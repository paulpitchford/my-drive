//! A pending sign-in attempt: the authorisation URL, and checking the redirect it ends in.
//!
//! Both the loopback flow and the paste flow end here, so the `state` and redirect-URI
//! checks exist once.

use std::fmt;
use std::time::Duration;

use tokio::time::Instant;
use url::Url;

use super::config::{AuthConfig, Scopes};
use super::error::SignInError;
use super::pkce::{CODE_CHALLENGE_METHOD, State};
use super::secret::{AuthorizationCode, PkceVerifier};
use super::token::{TokenClient, TokenSet};

/// How long a pending attempt is kept before it is discarded (F01).
pub const ATTEMPT_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// A redirect URI: the exact text sent to the authority, and its parsed form for comparing.
#[derive(Clone, Debug)]
pub struct RedirectUri {
    text: String,
    url: Url,
}

impl RedirectUri {
    /// Parses a redirect URI.
    ///
    /// # Errors
    ///
    /// [`SignInError::MalformedRedirect`] if `text` is not an absolute URL without query or
    /// fragment.
    pub fn parse(text: &str) -> Result<Self, SignInError> {
        let url = Url::parse(text).map_err(|_| SignInError::MalformedRedirect {
            reason: "the redirect URI is not an absolute URL",
        })?;
        if url.query().is_some() || url.fragment().is_some() || url.cannot_be_a_base() {
            return Err(SignInError::MalformedRedirect {
                reason: "the redirect URI has a query or fragment",
            });
        }
        Ok(Self {
            text: text.to_owned(),
            url,
        })
    }

    /// The URI exactly as sent in the `redirect_uri` parameter.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether `candidate` is a redirect to this URI: same scheme, host, port and path.
    /// An empty path and `/` are the same.
    #[must_use]
    pub fn is_target_of(&self, candidate: &Url) -> bool {
        let path = |u: &Url| {
            let p = u.path();
            if p.is_empty() {
                "/".to_owned()
            } else {
                p.to_owned()
            }
        };
        candidate.scheme() == self.url.scheme()
            && candidate.host_str().map(str::to_ascii_lowercase)
                == self.url.host_str().map(str::to_ascii_lowercase)
            && candidate.port_or_known_default() == self.url.port_or_known_default()
            && path(candidate) == path(&self.url)
    }
}

/// The secret parts of one sign-in attempt, owned by the daemon until the code is redeemed.
pub struct PendingSignIn {
    client: AuthConfig,
    redirect_uri: RedirectUri,
    scopes: Scopes,
    state: State,
    verifier: PkceVerifier,
    authorization_url: Url,
    started: Instant,
}

impl fmt::Debug for PendingSignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingSignIn")
            .field("client_id", self.client.client_id())
            .field("redirect_uri", &self.redirect_uri.as_str())
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl PendingSignIn {
    /// Starts an attempt: fresh PKCE verifier and `state`, and the authorisation URL that
    /// carries the `S256` challenge (ADR-0010 R1).
    ///
    /// # Errors
    ///
    /// [`SignInError::Config`] if the authority's endpoint cannot be built.
    pub fn new(
        config: &AuthConfig,
        redirect_uri: RedirectUri,
        scopes: Scopes,
    ) -> Result<Self, SignInError> {
        let state = State::generate();
        let verifier = PkceVerifier::generate();
        let mut authorization_url = config.authorize_endpoint()?;
        authorization_url
            .query_pairs_mut()
            .append_pair("client_id", config.client_id().as_str())
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect_uri.as_str())
            .append_pair("response_mode", "query")
            .append_pair("scope", &scopes.to_param())
            .append_pair("state", state.as_str())
            .append_pair("code_challenge", &verifier.challenge())
            .append_pair("code_challenge_method", CODE_CHALLENGE_METHOD);
        Ok(Self {
            client: config.clone(),
            redirect_uri,
            scopes,
            state,
            verifier,
            authorization_url,
            started: Instant::now(),
        })
    }

    /// The URL the user opens in their own browser. It holds no secret: the challenge is
    /// public and the `state` is only useful together with this attempt.
    #[must_use]
    pub fn authorization_url(&self) -> &Url {
        &self.authorization_url
    }

    /// The redirect URI of this attempt.
    #[must_use]
    pub fn redirect_uri(&self) -> &RedirectUri {
        &self.redirect_uri
    }

    /// The scopes requested.
    #[must_use]
    pub fn scopes(&self) -> &Scopes {
        &self.scopes
    }

    /// The PKCE verifier, for the token request.
    pub(crate) fn verifier(&self) -> &PkceVerifier {
        &self.verifier
    }

    /// Whether the attempt is older than [`ATTEMPT_LIFETIME`].
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.started.elapsed() >= ATTEMPT_LIFETIME
    }

    /// Checks a redirect against this attempt and takes the code from it.
    ///
    /// The checks run in this order: the target is this attempt's redirect URI; `state`
    /// is present exactly once and matches; then an `error` is reported, or the code is
    /// returned.
    ///
    /// # Errors
    ///
    /// - [`SignInError::WrongRedirectUri`] if `url` is not a redirect to this attempt's URI.
    /// - [`SignInError::StateMismatch`] if `state` is missing, repeated or wrong.
    /// - [`SignInError::Authorization`] if the authority redirected with an error.
    /// - [`SignInError::MalformedRedirect`] if there is neither a code nor an error.
    pub fn accept_redirect(&self, url: &Url) -> Result<AuthorizationCode, SignInError> {
        if !self.redirect_uri.is_target_of(url) {
            return Err(SignInError::WrongRedirectUri {
                expected: self.redirect_uri.as_str().to_owned(),
            });
        }
        let mut state = Vec::new();
        let mut code = Vec::new();
        let mut error = Vec::new();
        let mut description = None;
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "state" => state.push(v.into_owned()),
                "code" => code.push(v.into_owned()),
                "error" => error.push(v.into_owned()),
                "error_description" => description = Some(v.into_owned()),
                _ => {}
            }
        }
        match state.as_slice() {
            [s] if self.state.matches(s) => {}
            _ => return Err(SignInError::StateMismatch),
        }
        match (code.as_slice(), error.as_slice()) {
            (_, [e]) => Err(SignInError::Authorization {
                error: e.clone(),
                description,
            }),
            ([c], []) if !c.is_empty() => Ok(AuthorizationCode::new(c.clone())),
            _ => Err(SignInError::MalformedRedirect {
                reason: "it has no single authorisation code or error",
            }),
        }
    }

    /// Redeems `code` for tokens with this attempt's verifier (ADR-0010 R1), consuming the
    /// attempt.
    ///
    /// # Errors
    ///
    /// [`SignInError::Token`] if the token request fails, and
    /// [`SignInError::MissingRefreshToken`] if the response has no refresh token.
    pub async fn redeem(
        self,
        code: AuthorizationCode,
        client: &TokenClient,
    ) -> Result<TokenSet, SignInError> {
        let tokens = client
            .redeem_code(&code, &self)
            .await
            .map_err(SignInError::Token)?;
        if tokens.refresh_token.is_none() {
            return Err(SignInError::MissingRefreshToken);
        }
        Ok(tokens)
    }
}

/// The headless sign-in (J14): the user opens the URL on any device and pastes back the
/// address the browser was redirected to (ADR-0010 R10).
#[derive(Debug)]
pub struct PasteSignIn {
    pending: Option<PendingSignIn>,
}

impl PasteSignIn {
    /// Starts a headless attempt with `redirect_uri`, for example
    /// [`NATIVE_CLIENT_REDIRECT_URI`](super::config::NATIVE_CLIENT_REDIRECT_URI).
    ///
    /// # Errors
    ///
    /// [`SignInError::MalformedRedirect`] or [`SignInError::Config`] if the inputs are bad.
    pub fn start(
        config: &AuthConfig,
        redirect_uri: &str,
        scopes: Scopes,
    ) -> Result<Self, SignInError> {
        let redirect_uri = RedirectUri::parse(redirect_uri)?;
        Ok(Self {
            pending: Some(PendingSignIn::new(config, redirect_uri, scopes)?),
        })
    }

    /// The URL to print for the user, while the attempt is pending.
    #[must_use]
    pub fn authorization_url(&self) -> Option<&Url> {
        self.pending.as_ref().map(PendingSignIn::authorization_url)
    }

    /// Whether the attempt can still take a pasted address.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Checks a pasted address and, if it is good, redeems its code.
    ///
    /// A malformed address, one for another redirect URI, or an `error` redirect leaves the
    /// attempt pending so the user can paste again (F01). A `state` mismatch cancels it.
    /// An expired attempt is discarded. Once a code has been sent to the token endpoint the
    /// attempt is closed whatever the outcome, since a code is redeemable only once.
    ///
    /// # Errors
    ///
    /// As [`PendingSignIn::accept_redirect`] and [`PendingSignIn::redeem`], plus
    /// [`SignInError::TimedOut`] for an expired attempt and [`SignInError::AttemptClosed`]
    /// if it is no longer pending.
    pub async fn complete(
        &mut self,
        pasted: &str,
        client: &TokenClient,
    ) -> Result<TokenSet, SignInError> {
        let code = self.accept(pasted)?;
        let pending = self.pending.take().ok_or(SignInError::AttemptClosed)?;
        pending.redeem(code, client).await
    }

    /// Checks a pasted address without redeeming it.
    fn accept(&mut self, pasted: &str) -> Result<AuthorizationCode, SignInError> {
        let pending = self.pending.as_ref().ok_or(SignInError::AttemptClosed)?;
        if pending.is_expired() {
            self.pending = None;
            return Err(SignInError::TimedOut {
                after: ATTEMPT_LIFETIME,
            });
        }
        let url = Url::parse(pasted.trim()).map_err(|_| SignInError::MalformedRedirect {
            reason: "it is not a full address (it must start with http:// or https://)",
        })?;
        match pending.accept_redirect(&url) {
            Err(SignInError::StateMismatch) => {
                self.pending = None;
                Err(SignInError::StateMismatch)
            }
            other => other,
        }
    }
}
