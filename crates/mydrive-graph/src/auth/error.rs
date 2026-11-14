//! Error types for sign-in, token requests, token storage and the token provider.
//!
//! No variant carries a token, code, verifier or response body: messages hold only OAuth
//! error codes and descriptions, status codes and fixed reasons (ADR-0010 R6).

use std::time::Duration;

use mydrive_core::ids::AccountId;

/// A sign-in configuration value was invalid.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The authority base URL cannot be used.
    #[error("invalid sign-in authority {authority:?}: {reason}")]
    InvalidAuthority {
        /// The rejected value.
        authority: String,
        /// Why it was rejected.
        reason: &'static str,
    },
}

/// A request to the token endpoint failed.
#[derive(Debug, thiserror::Error)]
pub enum TokenRequestError {
    /// The request did not reach the token endpoint or no response came back.
    #[error("token request to the identity platform failed: {source}")]
    Network {
        /// The transport error, with its URL removed.
        #[source]
        source: reqwest::Error,
    },
    /// The token endpoint answered with an OAuth error.
    #[error(
        "the identity platform refused the token request (HTTP {status}): {error}{}",
        description.as_deref().map(|d| format!(": {d}")).unwrap_or_default()
    )]
    Rejected {
        /// The HTTP status.
        status: u16,
        /// The OAuth `error` code, for example `invalid_grant`.
        error: String,
        /// The `error_description`, if any.
        description: Option<String>,
    },
    /// The token endpoint answered with an HTTP error and no OAuth error body.
    #[error("the identity platform answered the token request with HTTP {status}")]
    Status {
        /// The HTTP status.
        status: u16,
    },
    /// The token endpoint answered 200 with a body we cannot use.
    #[error("the identity platform's token response is unusable: {reason}")]
    InvalidResponse {
        /// What was wrong with it.
        reason: &'static str,
    },
}

impl TokenRequestError {
    /// Whether this error means the refresh token (or code) is no longer usable and only an
    /// interactive sign-in can recover: `invalid_grant` (expired, revoked, password reset)
    /// or `interaction_required` (for example a new MFA or consent requirement).
    #[must_use]
    pub fn needs_sign_in(&self) -> bool {
        matches!(self, Self::Rejected { error, .. }
            if error == "invalid_grant" || error == "interaction_required")
    }

    /// Whether retrying later might succeed: transport failures, throttling and server
    /// errors.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Network { .. } => true,
            Self::Status { status } | Self::Rejected { status, .. } => {
                (*status == 429 || *status >= 500) && !self.needs_sign_in()
            }
            Self::InvalidResponse { .. } => false,
        }
    }
}

/// Signing in failed.
#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    /// The sign-in configuration is invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The loopback listener could not be started.
    #[error(
        "cannot listen on 127.0.0.1 for the sign-in redirect: {source}; \
         try `mydrive login --headless`"
    )]
    Bind {
        /// The socket error.
        #[source]
        source: std::io::Error,
    },
    /// Waiting for the loopback redirect failed at the socket level.
    #[error("the sign-in redirect listener failed: {source}")]
    Listener {
        /// The socket error.
        #[source]
        source: std::io::Error,
    },
    /// No redirect arrived in time, or the pending attempt expired.
    #[error("sign-in timed out after {} minutes; run the sign-in again", after.as_secs() / 60)]
    TimedOut {
        /// How long the attempt was kept.
        after: Duration,
    },
    /// A redirect carried a missing or wrong `state`. The attempt is cancelled.
    #[error("this sign-in response does not belong to this attempt; the attempt is cancelled")]
    StateMismatch,
    /// A pasted address is not a redirect to the URI this attempt used.
    #[error("the address is not the sign-in redirect to {expected}; paste the full address")]
    WrongRedirectUri {
        /// The redirect URI of this attempt.
        expected: String,
    },
    /// A redirect could not be understood.
    #[error("the sign-in redirect is malformed: {reason}")]
    MalformedRedirect {
        /// What was wrong.
        reason: &'static str,
    },
    /// The identity platform redirected with an error, for example `access_denied`.
    #[error(
        "sign-in was not completed: {error}{}",
        description.as_deref().map(|d| format!(": {d}")).unwrap_or_default()
    )]
    Authorization {
        /// The OAuth `error` code.
        error: String,
        /// The `error_description`, if any.
        description: Option<String>,
    },
    /// The attempt already finished or was cancelled.
    #[error("this sign-in attempt is no longer pending; start a new sign-in")]
    AttemptClosed,
    /// Redeeming the code failed.
    #[error("redeeming the sign-in code failed: {0}")]
    Token(#[source] TokenRequestError),
    /// The token response had no refresh token, so the account could not stay signed in.
    #[error("the identity platform returned no refresh token; was `offline_access` requested?")]
    MissingRefreshToken,
    /// The tokens could not be stored.
    #[error("cannot store the tokens of account {account}: {source}")]
    Store {
        /// The account being signed in.
        account: AccountId,
        /// The store error.
        #[source]
        source: StoreError,
    },
}

/// The token store failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// No Secret Service is reachable (ADR-0010 R7).
    #[error(
        "no Secret Service is available ({reason}); tokens are stored only there, so nothing \
         was saved. Run `mydrive doctor` to see how to provide one"
    )]
    Unavailable {
        /// What failed, without any secret.
        reason: String,
    },
    /// The user dismissed the keyring's unlock or confirmation prompt.
    #[error(
        "the keyring prompt was dismissed while {operation}; nothing was saved. \
         Run `mydrive doctor` for help"
    )]
    Dismissed {
        /// What was being done.
        operation: &'static str,
    },
    /// The store failed in some other way.
    #[error("token store failed while {operation}: {reason}")]
    Failed {
        /// What was being done.
        operation: &'static str,
        /// Why it failed, without any secret.
        reason: String,
    },
}

/// The token provider could not hand out an access token.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    /// The refresh token was rejected: the account is `needs-sign-in` and stays so until
    /// a new interactive sign-in. Not a transient failure.
    #[error("account {account} needs sign-in: its refresh token is no longer accepted")]
    NeedsSignIn {
        /// The account.
        account: AccountId,
    },
    /// Refreshing failed in a way that may succeed later (network, throttling, server).
    #[error("refreshing the access token of account {account} failed, will retry: {source}")]
    Transient {
        /// The account.
        account: AccountId,
        /// The request error.
        #[source]
        source: TokenRequestError,
    },
    /// The token endpoint refused the refresh for a reason other than an unusable refresh
    /// token, for example an unknown client ID.
    #[error("refreshing the access token of account {account} was refused: {source}")]
    Rejected {
        /// The account.
        account: AccountId,
        /// The request error.
        #[source]
        source: TokenRequestError,
    },
    /// The token store failed. When this follows a successful refresh, the old refresh
    /// token is still the stored one and no new access token was handed out.
    #[error("token store failed for account {account}: {source}")]
    Store {
        /// The account.
        account: AccountId,
        /// The store error.
        #[source]
        source: StoreError,
    },
}

impl TokenError {
    /// Whether this is the "needs sign-in" outcome rather than a failure that may pass.
    #[must_use]
    pub fn is_needs_sign_in(&self) -> bool {
        matches!(self, Self::NeedsSignIn { .. })
    }
}
