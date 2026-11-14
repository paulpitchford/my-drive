//! Sign-in and token storage (ADR-0010, F01).
//!
//! - [`LoopbackSignIn`]: the system-browser flow with a `127.0.0.1` loopback redirect.
//! - [`PasteSignIn`]: the headless flow, completing from a pasted redirect URL.
//! - [`TokenClient`]: code redemption and refresh against an injectable authority.
//! - [`TokenProvider`] and [`RefreshingTokenProvider`]: valid access tokens for the Graph
//!   client, refreshed before expiry, one refresh at a time.
//! - [`TokenStore`], with [`MemoryTokenStore`] and [`SecretServiceStore`].
//!
//! This module never opens a browser or a web view (ADR-0010 R3); the CLI opens the user's
//! own browser. No token, code or verifier is logged or shown by `Debug` (ADR-0010 R6).

mod attempt;
mod config;
mod error;
mod loopback;
mod pkce;
mod provider;
mod secret;
mod secret_service;
mod store;
mod token;

pub use attempt::{ATTEMPT_LIFETIME, PasteSignIn, PendingSignIn, RedirectUri};
pub use config::{
    AuthConfig, ClientId, DEFAULT_AUTHORITY, DEFAULT_CLIENT_ID, INITIAL_SCOPES,
    NATIVE_CLIENT_REDIRECT_URI, SITE_SEARCH_SCOPE, Scopes,
};
pub use error::{ConfigError, SignInError, StoreError, TokenError, TokenRequestError};
pub use loopback::{LOOPBACK_TIMEOUT, LoopbackSignIn};
pub use pkce::{CODE_CHALLENGE_METHOD, State};
pub use provider::{REFRESH_MARGIN, RefreshingTokenProvider, TokenProvider};
pub use secret::{AccessToken, AuthorizationCode, IdToken, PkceVerifier, RefreshToken};
pub use secret_service::SecretServiceStore;
pub use store::{BoxFuture, MemoryTokenStore, TokenStore};
pub use token::{TokenClient, TokenSet};
