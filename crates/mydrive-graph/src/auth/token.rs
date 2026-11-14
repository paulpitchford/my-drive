//! The token endpoint: redeeming a code and refreshing, against an injectable authority.

use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use tokio::time::Instant;
use url::Url;

use super::attempt::PendingSignIn;
use super::config::{AuthConfig, ClientId, Scopes};
use super::error::{SignInError, TokenRequestError};
use super::secret::{AccessToken, AuthorizationCode, IdToken, RefreshToken};

/// The longest a token request may take before it counts as a network failure.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Assumed lifetime when the response omits `expires_in`. Short, so we refresh early rather
/// than send an expired token.
const FALLBACK_LIFETIME: Duration = Duration::from_secs(5 * 60);

/// Upper bound on a reported lifetime, so a bogus `expires_in` cannot overflow the clock.
const MAX_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

/// Tokens returned by the token endpoint.
#[derive(Clone)]
pub struct TokenSet {
    /// The access token.
    pub access_token: AccessToken,
    /// When the access token expires, on the tokio clock.
    pub expires_at: Instant,
    /// The refresh token, if one was returned. A refresh that returns none keeps the old one.
    pub refresh_token: Option<RefreshToken>,
    /// The ID token, if one was returned.
    pub id_token: Option<IdToken>,
    /// The scopes granted, as the endpoint reported them.
    pub scope: Option<String>,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &self.access_token)
            .field("expires_at", &self.expires_at)
            .field("refresh_token", &self.refresh_token)
            .field("id_token", &self.id_token)
            .field("scope", &self.scope)
            .finish()
    }
}

/// The success body of the token endpoint. Fields not listed are ignored.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: Option<String>,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    scope: Option<String>,
}

/// The error body of the token endpoint.
#[derive(Deserialize)]
struct ErrorResponse {
    error: String,
    error_description: Option<String>,
}

/// A client for one authority's token endpoint. Public client: no secret is ever sent.
#[derive(Clone)]
pub struct TokenClient {
    http: reqwest::Client,
    token_endpoint: Url,
    client_id: ClientId,
}

impl fmt::Debug for TokenClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenClient")
            .field("token_endpoint", &self.token_endpoint.as_str())
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

impl TokenClient {
    /// A client for the token endpoint of `config`'s authority.
    ///
    /// # Errors
    ///
    /// [`SignInError::Config`] if the endpoint cannot be built, and
    /// [`SignInError::Token`] if the HTTP client cannot be created.
    pub fn new(config: &AuthConfig) -> Result<Self, SignInError> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| {
                SignInError::Token(TokenRequestError::Network {
                    source: e.without_url(),
                })
            })?;
        Ok(Self {
            http,
            token_endpoint: config.token_endpoint()?,
            client_id: config.client_id().clone(),
        })
    }

    /// Redeems an authorisation code with the attempt's PKCE verifier (ADR-0010 R1).
    ///
    /// # Errors
    ///
    /// Any [`TokenRequestError`].
    pub async fn redeem_code(
        &self,
        code: &AuthorizationCode,
        attempt: &PendingSignIn,
    ) -> Result<TokenSet, TokenRequestError> {
        let scope = attempt.scopes().to_param();
        let form = [
            ("client_id", self.client_id.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code.expose_secret()),
            ("redirect_uri", attempt.redirect_uri().as_str()),
            ("code_verifier", attempt.verifier().expose_secret()),
            ("scope", scope.as_str()),
        ];
        tracing::debug!("redeeming authorisation code");
        self.post(&form).await
    }

    /// Exchanges a refresh token for a new access token (and usually a new refresh token).
    ///
    /// # Errors
    ///
    /// Any [`TokenRequestError`]; see [`TokenRequestError::needs_sign_in`].
    pub async fn refresh(
        &self,
        refresh_token: &RefreshToken,
        scopes: &Scopes,
    ) -> Result<TokenSet, TokenRequestError> {
        let scope = scopes.to_param();
        let form = [
            ("client_id", self.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.expose_secret()),
            ("scope", scope.as_str()),
        ];
        self.post(&form).await
    }

    async fn post(&self, form: &[(&str, &str)]) -> Result<TokenSet, TokenRequestError> {
        let network = |e: reqwest::Error| TokenRequestError::Network {
            source: e.without_url(),
        };
        let response = self
            .http
            .post(self.token_endpoint.clone())
            .form(form)
            .send()
            .await
            .map_err(network)?;
        let status = response.status();
        let body = response.bytes().await.map_err(network)?;
        if !status.is_success() {
            // Only the error code and description are kept; the rest of the body is dropped.
            return Err(match serde_json::from_slice::<ErrorResponse>(&body) {
                Ok(e) => TokenRequestError::Rejected {
                    status: status.as_u16(),
                    error: e.error,
                    description: e.error_description,
                },
                Err(_) => TokenRequestError::Status {
                    status: status.as_u16(),
                },
            });
        }
        let parsed: TokenResponse =
            serde_json::from_slice(&body).map_err(|_| TokenRequestError::InvalidResponse {
                reason: "the body is not a token response",
            })?;
        if parsed.access_token.is_empty() {
            return Err(TokenRequestError::InvalidResponse {
                reason: "the access token is empty",
            });
        }
        if let Some(t) = &parsed.token_type
            && !t.eq_ignore_ascii_case("bearer")
        {
            return Err(TokenRequestError::InvalidResponse {
                reason: "the token type is not Bearer",
            });
        }
        let lifetime = parsed
            .expires_in
            .map_or(FALLBACK_LIFETIME, Duration::from_secs)
            .min(MAX_LIFETIME);
        Ok(TokenSet {
            access_token: AccessToken::new(parsed.access_token),
            expires_at: Instant::now() + lifetime,
            refresh_token: parsed
                .refresh_token
                .filter(|t| !t.is_empty())
                .map(RefreshToken::new),
            id_token: parsed.id_token.map(IdToken::new),
            scope: parsed.scope,
        })
    }
}
