//! The fake sign-in authority: the authorise step, the token endpoint with its PKCE check,
//! refresh-token rotation and revocation, and ID tokens.
//!
//! It is served on its own listener (`GraphSim::authority_url`), standing in for
//! `login.microsoftonline.com`, behind the request log and the fault layer. Its state lives
//! in the identity registry (`identity::Directory`). Documented in `docs/identity.md`.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use axum::extract::{Form, Path, Query, State as AxState};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::GraphSim;
use crate::identity::{Account, Directory, IdentityError};
use crate::json::percent_encode;
use crate::server::Shared;
use crate::time::format_timestamp;

/// Lifetime of an issued access token, in seconds, as `expires_in` reports it. The
/// identity platform documents a random lifetime of 60 to 90 minutes; the simulator uses
/// the shortest whole hour.
pub const ACCESS_TOKEN_LIFETIME_SECS: u64 = 3600;

/// How long an authorisation code can be redeemed, in seconds of the simulator's clock:
/// "about 10 minutes" in the authorization code flow documentation.
pub const CODE_LIFETIME_SECS: i64 = 600;

/// How long a refresh token lasts, in seconds of the simulator's clock: 90 days, as the
/// refresh token documentation gives.
pub const REFRESH_TOKEN_LIFETIME_SECS: i64 = 90 * 24 * 60 * 60;

/// Whether a refresh token still works after it has been used once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OldRefreshToken {
    /// The used token answers `invalid_grant`: only the newest one works. WP-023's
    /// default, the case that punishes a client which does not store the new token.
    Refused,
    /// The used token keeps working until it expires, as the refresh token
    /// documentation says ("not revoked on use").
    Accepted,
}

/// When the token endpoint includes an `id_token`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdTokenIssue {
    /// Only when the granted scopes include `openid`, as documented.
    WithOpenidScope,
    /// On every successful token response.
    Always,
}

/// What redeeming an authorisation code a second time does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReusedCode {
    /// The request is refused and every token issued from the code's first redemption
    /// (and refreshes of it) is revoked, as RFC 6749 section 4.1.2 says a server SHOULD.
    /// Whether the identity platform does this is unverified; the default, the harder
    /// case.
    RevokeIssuedTokens,
    /// The request is refused; tokens already issued keep working.
    RefuseOnly,
}

/// An authorisation code waiting to be redeemed.
#[derive(Debug, Clone)]
struct CodeRecord {
    client_id: String,
    redirect_uri: String,
    /// The challenge and its method (`S256` or `plain`), if the request had one.
    challenge: Option<(String, String)>,
    scopes: Vec<String>,
    object_id: String,
    issued_at: i64,
}

/// Where a refresh token stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshStatus {
    /// The newest token of its chain.
    Live,
    /// Used once; a newer one replaced it.
    Rolled,
    /// Revoked by the test.
    Revoked,
}

#[derive(Debug, Clone)]
struct RefreshRecord {
    client_id: String,
    /// The scopes granted at the authorise step that started the chain. A refresh may ask
    /// for these or fewer, never more.
    granted: Vec<String>,
    object_id: String,
    issued_at: i64,
    status: RefreshStatus,
    /// The authorisation code the chain started from.
    chain: String,
}

/// The authority's part of the identity registry. Its `Debug` shows counts only, never a
/// code or token.
#[derive(Default)]
pub(crate) struct AuthorityState {
    /// The account the next authorise request signs in as.
    signing_in: Option<String>,
    /// An error the next authorise request redirects with instead of a code.
    deny_next: Option<(String, String)>,
    codes: HashMap<String, CodeRecord>,
    /// Codes already presented once, whatever the outcome.
    used_codes: HashSet<String>,
    refresh_tokens: HashMap<String, RefreshRecord>,
    /// Access tokens issued in each chain, keyed by the code it started from.
    chain_access: HashMap<String, Vec<String>>,
    next_secret: u64,
}

impl fmt::Debug for AuthorityState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorityState")
            .field("signing_in", &self.signing_in)
            .field("pending_codes", &self.codes.len())
            .field("used_codes", &self.used_codes.len())
            .field("refresh_tokens", &self.refresh_tokens.len())
            .field("chains", &self.chain_access.len())
            .finish_non_exhaustive()
    }
}

/// Makes a secret in the shape the request log knows how to mask:
/// `<prefix><instance>-<n>.<secret part>` (see `log::ISSUED_SECRET_PREFIXES`). The secret
/// part is derived, not random: the simulator is not a security boundary, and derived
/// values keep runs reproducible.
fn mint(directory: &mut Directory, prefix: &str) -> String {
    directory.authority.next_secret += 1;
    let label = format!("{}-{}", directory.instance, directory.authority.next_secret);
    let digest = Sha256::digest(format!("{prefix}{label}:mydrive-graph-sim").as_bytes());
    format!("{prefix}{label}.{}", URL_SAFE_NO_PAD.encode(digest))
}

/// The authority's routes. The first path segment is the tenant (`common`,
/// `organizations`, `consumers` or a tenant ID); any value is accepted.
pub(crate) fn routes() -> Router<Arc<Shared>> {
    Router::new()
        .route("/{tenant}/oauth2/v2.0/authorize", get(authorize))
        .route("/{tenant}/oauth2/v2.0/token", post(token))
}

/// The OAuth error body the identity platform uses on the token endpoint.
fn oauth_error(
    status: StatusCode,
    error: &str,
    code: u32,
    description: &str,
    now: i64,
) -> Response {
    (
        status,
        Json(json!({
            "error": error,
            "error_description": format!("AADSTS{code}: {description}"),
            "error_codes": [code],
            "timestamp": format_timestamp(now),
            "trace_id": "00000000-0000-4000-8000-000000000000",
            "correlation_id": "00000000-0000-4000-8000-000000000000",
        })),
    )
        .into_response()
}

/// The answer for a path the authority does not serve.
pub(crate) async fn no_route() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "invalid_request", "error_description": "Simulator: no such authority endpoint." })),
    )
        .into_response()
}

/// A `302` to `redirect_uri` with the given query parameters added.
fn redirect(redirect_uri: &str, params: &[(&str, &str)]) -> Response {
    let mut location = redirect_uri.to_owned();
    let mut separator = if location.contains('?') { '&' } else { '?' };
    for (name, value) in params {
        location.push(separator);
        location.push_str(name);
        location.push('=');
        location.push_str(&percent_encode(value));
        separator = '&';
    }
    match HeaderValue::from_str(&location) {
        Ok(value) => {
            let mut response = StatusCode::FOUND.into_response();
            response.headers_mut().insert(header::LOCATION, value);
            response
        }
        Err(_) => bad_authorize("the redirect_uri is not a usable URL"),
    }
}

/// An authorise request that cannot be redirected: the identity platform shows an error
/// page instead of redirecting to an untrusted URI.
fn bad_authorize(reason: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": "invalid_request", "error_description": format!("Simulator: {reason}.") })),
    )
        .into_response()
}

fn split_scopes(scope: Option<&String>) -> Vec<String> {
    scope
        .map(|s| s.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn has_scope(scopes: &[String], wanted: &str) -> bool {
    scopes.iter().any(|s| s.eq_ignore_ascii_case(wanted))
}

/// The `S256` challenge of a verifier, as RFC 7636 defines it.
fn s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

// ---- authorise -------------------------------------------------------------------------

async fn authorize(
    AxState(shared): AxState<Arc<Shared>>,
    Path(_tenant): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let (Some(client_id), Some(redirect_uri)) = (
        query.get("client_id").filter(|v| !v.is_empty()),
        query.get("redirect_uri").filter(|v| !v.is_empty()),
    ) else {
        return bad_authorize("client_id and redirect_uri are required");
    };
    if !(redirect_uri.starts_with("http://") || redirect_uri.starts_with("https://"))
        || redirect_uri.contains('#')
    {
        return bad_authorize("the redirect_uri is not an absolute http(s) URL");
    }
    let state = query.get("state").map(String::as_str);
    let with_state = |mut params: Vec<(&'static str, String)>| {
        if let Some(state) = state {
            params.push(("state", state.to_owned()));
        }
        params
    };
    let now = shared.lock().clock;
    let mut directory = shared.directory();
    if let Some((error, description)) = directory.authority.deny_next.take() {
        let params = with_state(vec![("error", error), ("error_description", description)]);
        return redirect_owned(redirect_uri, &params);
    }
    if query.get("response_type").map(String::as_str) != Some("code") {
        let params = with_state(vec![
            ("error", "unsupported_response_type".to_owned()),
            (
                "error_description",
                "AADSTS70005: response_type must be code.".to_owned(),
            ),
        ]);
        return redirect_owned(redirect_uri, &params);
    }
    let challenge = match query.get("code_challenge") {
        None => None,
        Some(challenge) => {
            let method = query
                .get("code_challenge_method")
                .map_or("plain", String::as_str);
            if method != "S256" && method != "plain" {
                let params = with_state(vec![
                    ("error", "invalid_request".to_owned()),
                    (
                        "error_description",
                        "AADSTS501491: code_challenge_method must be S256 or plain.".to_owned(),
                    ),
                ]);
                return redirect_owned(redirect_uri, &params);
            }
            Some((challenge.clone(), method.to_owned()))
        }
    };
    let Some(object_id) = directory.authority.signing_in.clone() else {
        return bad_authorize("no account is signing in; call GraphSim::sign_in_as first");
    };
    let code = mint(&mut directory, "sim-code-");
    directory.authority.codes.insert(
        code.clone(),
        CodeRecord {
            client_id: client_id.clone(),
            redirect_uri: redirect_uri.clone(),
            challenge,
            scopes: split_scopes(query.get("scope")),
            object_id,
            issued_at: now,
        },
    );
    redirect_owned(redirect_uri, &with_state(vec![("code", code)]))
}

fn redirect_owned(redirect_uri: &str, params: &[(&'static str, String)]) -> Response {
    let borrowed: Vec<(&str, &str)> = params.iter().map(|(n, v)| (*n, v.as_str())).collect();
    redirect(redirect_uri, &borrowed)
}

// ---- token -----------------------------------------------------------------------------

/// What a token request ended in, before the access token is registered with the Graph
/// origin.
enum Grant {
    Issued {
        body: Value,
        access_token: String,
    },
    /// Refused; `revoke` lists access tokens the Graph origin must stop accepting.
    Refused {
        response: Response,
        revoke: Vec<String>,
    },
}

impl Grant {
    fn refused(response: Response) -> Self {
        Self::Refused {
            response,
            revoke: Vec::new(),
        }
    }
}

async fn token(
    AxState(shared): AxState<Arc<Shared>>,
    Path(_tenant): Path<String>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let now = shared.lock().clock;
    let grant = {
        let mut directory = shared.directory();
        match form.get("grant_type").map(String::as_str) {
            Some("authorization_code") => redeem_code(&mut directory, &form, now),
            Some("refresh_token") => redeem_refresh(&mut directory, &form, now),
            _ => Grant::refused(oauth_error(
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                70003,
                "The app requested an unsupported grant type.",
                now,
            )),
        }
    };
    match grant {
        Grant::Issued { body, access_token } => {
            // The directory lock is released; now the drives' lock, so the Graph origin
            // accepts the new access token.
            shared.lock().tokens.insert(access_token);
            (StatusCode::OK, Json(body)).into_response()
        }
        Grant::Refused { response, revoke } => {
            if !revoke.is_empty() {
                let mut state = shared.lock();
                for token in &revoke {
                    state.tokens.remove(token);
                }
            }
            response
        }
    }
}

fn invalid_grant(code: u32, description: &str, now: i64) -> Grant {
    Grant::refused(oauth_error(
        StatusCode::BAD_REQUEST,
        "invalid_grant",
        code,
        description,
        now,
    ))
}

fn redeem_code(directory: &mut Directory, form: &HashMap<String, String>, now: i64) -> Grant {
    let Some(code) = form.get("code") else {
        return Grant::refused(oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            900_144,
            "The request body must contain the parameter 'code'.",
            now,
        ));
    };
    // A code is redeemable once: any attempt uses it up, whatever the outcome.
    let Some(record) = directory.authority.codes.remove(code) else {
        if directory.authority.used_codes.contains(code) {
            return reused_code(directory, code, now);
        }
        return invalid_grant(
            70000,
            "The provided authorization code is not valid: unknown or already redeemed.",
            now,
        );
    };
    directory.authority.used_codes.insert(code.clone());
    if now - record.issued_at > CODE_LIFETIME_SECS {
        return invalid_grant(70008, "The provided authorization code has expired.", now);
    }
    if form.get("client_id") != Some(&record.client_id) {
        return invalid_grant(
            70000,
            "The authorization code was issued to another client.",
            now,
        );
    }
    if form.get("redirect_uri") != Some(&record.redirect_uri) {
        return invalid_grant(
            50148,
            "The redirect_uri does not match the one in the authorization request.",
            now,
        );
    }
    if let Some((challenge, method)) = &record.challenge {
        let matches = form.get("code_verifier").is_some_and(|verifier| {
            if method == "S256" {
                s256(verifier) == *challenge
            } else {
                verifier == challenge
            }
        });
        if !matches {
            return invalid_grant(
                501_481,
                "The code_verifier does not match the code_challenge supplied in the \
                 authorization request.",
                now,
            );
        }
    }
    issue(
        directory,
        Issue {
            object_id: &record.object_id,
            client_id: &record.client_id,
            requested: record.scopes.clone(),
            granted: record.scopes,
            chain: code,
        },
        now,
    )
}

/// A code presented again: refused, and under [`ReusedCode::RevokeIssuedTokens`] every
/// token of the chain it started is revoked.
fn reused_code(directory: &mut Directory, code: &str, now: i64) -> Grant {
    let response = oauth_error(
        StatusCode::BAD_REQUEST,
        "invalid_grant",
        54005,
        "OAuth2 Authorization code was already redeemed.",
        now,
    );
    if directory.options.reused_code == ReusedCode::RefuseOnly {
        return Grant::refused(response);
    }
    for record in directory.authority.refresh_tokens.values_mut() {
        if record.chain == code {
            record.status = RefreshStatus::Revoked;
        }
    }
    let revoke = directory
        .authority
        .chain_access
        .remove(code)
        .unwrap_or_default();
    Grant::Refused { response, revoke }
}

fn redeem_refresh(directory: &mut Directory, form: &HashMap<String, String>, now: i64) -> Grant {
    let Some(refresh) = form.get("refresh_token") else {
        return Grant::refused(oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            900_144,
            "The request body must contain the parameter 'refresh_token'.",
            now,
        ));
    };
    let Some(record) = directory.authority.refresh_tokens.get(refresh).cloned() else {
        return invalid_grant(9_002_313, "The refresh token is malformed or unknown.", now);
    };
    match record.status {
        RefreshStatus::Live => {}
        RefreshStatus::Rolled
            if directory.options.old_refresh_token == OldRefreshToken::Accepted => {}
        RefreshStatus::Rolled => {
            return invalid_grant(
                70000,
                "The refresh token was already used and has been replaced.",
                now,
            );
        }
        RefreshStatus::Revoked => {
            return invalid_grant(
                50173,
                "The provided grant has expired due to it being revoked.",
                now,
            );
        }
    }
    if now - record.issued_at > REFRESH_TOKEN_LIFETIME_SECS {
        return invalid_grant(
            700_082,
            "The refresh token has expired due to inactivity.",
            now,
        );
    }
    if form.get("client_id") != Some(&record.client_id) {
        return invalid_grant(
            70000,
            "The refresh token was issued to another client.",
            now,
        );
    }
    let requested = match form.get("scope") {
        Some(_) => split_scopes(form.get("scope")),
        None => record.granted.clone(),
    };
    // A refresh cannot widen the grant: a scope not granted at the authorise step needs a
    // new authorise step (incremental consent, ADR-0010 R5). Nothing is issued or rolled.
    if let Some(extra) = requested.iter().find(|s| !has_scope(&record.granted, s)) {
        return invalid_grant(
            65001,
            &format!(
                "The user or administrator has not consented to use the application \
                 with scope '{extra}'. Send an interactive authorization request."
            ),
            now,
        );
    }
    if let Some(entry) = directory.authority.refresh_tokens.get_mut(refresh)
        && entry.status == RefreshStatus::Live
    {
        entry.status = RefreshStatus::Rolled;
    }
    issue(
        directory,
        Issue {
            object_id: &record.object_id,
            client_id: &record.client_id,
            requested,
            granted: record.granted,
            chain: &record.chain,
        },
        now,
    )
}

/// What to issue: for whom, the scopes asked for now, and the chain's grant.
struct Issue<'a> {
    object_id: &'a str,
    client_id: &'a str,
    requested: Vec<String>,
    granted: Vec<String>,
    chain: &'a str,
}

/// Issues an access token, a refresh token when `offline_access` was granted, and an ID
/// token as the switch says.
fn issue(directory: &mut Directory, request: Issue<'_>, now: i64) -> Grant {
    let Issue {
        object_id,
        client_id,
        requested: scopes,
        granted,
        chain,
    } = request;
    let Some(account) = directory.account(object_id).cloned() else {
        return invalid_grant(50034, "The user account does not exist.", now);
    };
    let access_token = mint(directory, "sim-at-");
    directory.bind_token(access_token.clone(), object_id.to_owned());
    directory
        .authority
        .chain_access
        .entry(chain.to_owned())
        .or_default()
        .push(access_token.clone());
    // `offline_access` and the OpenID scopes are not listed in the granted `scope`.
    let listed: Vec<&str> = scopes
        .iter()
        .map(String::as_str)
        .filter(|s| {
            !["offline_access", "openid", "profile", "email"]
                .iter()
                .any(|o| s.eq_ignore_ascii_case(o))
        })
        .collect();
    let mut body = json!({
        "token_type": "Bearer",
        "scope": listed.join(" "),
        "expires_in": ACCESS_TOKEN_LIFETIME_SECS,
        "ext_expires_in": ACCESS_TOKEN_LIFETIME_SECS,
        "access_token": access_token,
    });
    if has_scope(&scopes, "offline_access") {
        let refresh = mint(directory, "sim-rt-");
        directory.authority.refresh_tokens.insert(
            refresh.clone(),
            RefreshRecord {
                client_id: client_id.to_owned(),
                granted,
                object_id: object_id.to_owned(),
                issued_at: now,
                status: RefreshStatus::Live,
                chain: chain.to_owned(),
            },
        );
        body["refresh_token"] = json!(refresh);
    }
    let id_token = match directory.options.id_token {
        IdTokenIssue::WithOpenidScope => has_scope(&scopes, "openid"),
        IdTokenIssue::Always => true,
    };
    if id_token {
        body["id_token"] = json!(id_token_for(
            &account,
            client_id,
            &directory.issuer_origin,
            now
        ));
    }
    Grant::Issued { body, access_token }
}

/// The claims of an ID token for `account`, as the ID token claims reference lists them
/// for v2.0 tokens.
pub(crate) fn id_token_claims(
    account: &Account,
    client_id: &str,
    issuer_origin: &str,
    now: i64,
) -> Value {
    let subject = URL_SAFE_NO_PAD.encode(Sha256::digest(
        format!("{}:{client_id}", account.object_id).as_bytes(),
    ));
    json!({
        "ver": "2.0",
        "iss": format!("{issuer_origin}/{}/v2.0", account.tenant_claim()),
        "aud": client_id,
        "iat": now,
        "nbf": now,
        "exp": now + ACCESS_TOKEN_LIFETIME_SECS as i64,
        "sub": subject,
        "oid": account.object_id,
        "tid": account.tenant_claim(),
        "name": account.display_name,
        "preferred_username": account.user_name,
    })
}

/// An unsecured JWT (`alg: none`, empty signature, RFC 7519 section 6) carrying the
/// account's claims. The client never validates it; a client that starts validating will
/// refuse it, rather than trust a signature nobody checked.
fn id_token_for(account: &Account, client_id: &str, issuer_origin: &str, now: i64) -> String {
    let header = json!({ "typ": "JWT", "alg": "none" });
    let claims = id_token_claims(account, client_id, issuer_origin, now);
    format!(
        "{}.{}.",
        URL_SAFE_NO_PAD.encode(header.to_string().as_bytes()),
        URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes())
    )
}

// ---- control API -----------------------------------------------------------------------

impl GraphSim {
    /// The authority base URL to give a client, `http://127.0.0.1:<port>/common/`,
    /// standing in for `https://login.microsoftonline.com/common/`.
    #[must_use]
    pub fn authority_url(&self) -> String {
        format!("{}/common/", self.shared.authority_base)
    }

    /// The authority's origin, `http://127.0.0.1:<port>`.
    #[must_use]
    pub fn authority_origin(&self) -> &str {
        &self.shared.authority_base
    }

    /// Chooses the account that "signs in in the browser": every later authorise request
    /// redirects with a code for it.
    ///
    /// # Errors
    ///
    /// [`IdentityError::UnknownAccount`] if the account does not exist here.
    pub fn sign_in_as(&self, account: &Account) -> Result<(), IdentityError> {
        let mut directory = self.shared.directory();
        if directory.account(&account.object_id).is_none() {
            return Err(IdentityError::UnknownAccount {
                object_id: account.object_id.clone(),
            });
        }
        directory.authority.signing_in = Some(account.object_id.clone());
        Ok(())
    }

    /// Makes the next authorise request redirect with `error` and `error_description`
    /// instead of a code, as when the user cancels (`access_denied`) or the organisation
    /// refuses the app.
    pub fn deny_next_sign_in(&self, error: &str, description: &str) {
        self.shared.directory().authority.deny_next =
            Some((error.to_owned(), description.to_owned()));
    }

    /// Revokes every refresh token issued to `account`, as an administrator's "revoke
    /// sessions" or a password reset does: later refreshes answer `invalid_grant`. Access
    /// tokens already issued keep working until revoked with [`GraphSim::revoke_token`],
    /// as they do until they expire.
    pub fn revoke_refresh_tokens(&self, account: &Account) {
        for record in self
            .shared
            .directory()
            .authority
            .refresh_tokens
            .values_mut()
        {
            if record.object_id == account.object_id {
                record.status = RefreshStatus::Revoked;
            }
        }
    }

    /// Revokes one refresh token; returns whether the authority had issued it.
    pub fn revoke_refresh_token(&self, refresh_token: &str) -> bool {
        match self
            .shared
            .directory()
            .authority
            .refresh_tokens
            .get_mut(refresh_token)
        {
            Some(record) => {
                record.status = RefreshStatus::Revoked;
                true
            }
            None => false,
        }
    }
}
