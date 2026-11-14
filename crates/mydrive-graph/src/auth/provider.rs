//! Handing out valid access tokens, refreshing before expiry (F01, token refresh).

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use mydrive_core::ids::AccountId;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::config::Scopes;
use super::error::{SignInError, StoreError, TokenError};
use super::secret::{AccessToken, RefreshToken};
use super::store::{BoxFuture, TokenStore};
use super::token::{TokenClient, TokenSet};

/// A cached access token is refreshed once it has less than this left (F01: 5 minutes).
pub const REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);

/// While a refresh token is waiting to be stored, the write is retried on use at most this
/// often, so a failing keyring (or a dismissed prompt) is not hit on every Graph request.
/// It is always retried before a refresh.
pub const STORE_RETRY_INTERVAL: Duration = Duration::from_secs(60);

/// Gives the Graph client a valid access token for one account.
pub trait TokenProvider: Send + Sync {
    /// A token valid for at least [`REFRESH_MARGIN`] more, refreshing first if needed.
    /// Concurrent callers that all need a refresh share one refresh request.
    fn access_token(&self) -> BoxFuture<'_, Result<AccessToken, TokenError>>;

    /// Refreshes because Graph answered 401 to `rejected`, although it looked valid (F01).
    /// If another caller has already replaced `rejected`, the newer token is returned
    /// without a second refresh.
    fn force_refresh<'a>(
        &'a self,
        rejected: &'a AccessToken,
    ) -> BoxFuture<'a, Result<AccessToken, TokenError>>;
}

/// The access token held in memory.
struct Cached {
    token: AccessToken,
    expires_at: Instant,
}

/// Everything guarded by the refresh lock.
#[derive(Default)]
struct Session {
    cache: Option<Cached>,
    /// The newest refresh token, when writing it to the store failed. It is what later
    /// refreshes use, and the write is retried until it succeeds (F01 step 9).
    unsaved: Option<RefreshToken>,
    /// When the last write of `unsaved` was attempted.
    last_store_attempt: Option<Instant>,
}

/// A [`TokenProvider`] that refreshes through a [`TokenClient`] and keeps the refresh token
/// in a [`TokenStore`].
///
/// The refresh token is read from the store for each refresh, unless a newer one is held in
/// memory because storing it failed. Refreshes are serialised by one lock: callers that
/// arrive while a refresh is running wait for it and then use its result.
pub struct RefreshingTokenProvider<S: TokenStore + ?Sized> {
    account: AccountId,
    scopes: Scopes,
    client: TokenClient,
    store: Arc<S>,
    session: Mutex<Session>,
    needs_sign_in: AtomicBool,
    store_warning: std::sync::Mutex<Option<StoreError>>,
}

impl<S: TokenStore + ?Sized> fmt::Debug for RefreshingTokenProvider<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefreshingTokenProvider")
            .field("account", &self.account)
            .field("scopes", &self.scopes)
            .field("needs_sign_in", &self.needs_sign_in())
            .field("store_warning", &self.store_warning())
            .finish_non_exhaustive()
    }
}

impl<S: TokenStore + ?Sized> RefreshingTokenProvider<S> {
    fn build(account: AccountId, scopes: Scopes, client: TokenClient, store: Arc<S>) -> Self {
        Self {
            account,
            scopes,
            client,
            store,
            session: Mutex::new(Session::default()),
            needs_sign_in: AtomicBool::new(false),
            store_warning: std::sync::Mutex::new(None),
        }
    }

    /// Finishes a sign-in: stores the refresh token from `tokens`, then keeps the access
    /// token in memory. If storing fails nothing is kept (ADR-0010 R7).
    ///
    /// # Errors
    ///
    /// [`SignInError::MissingRefreshToken`] if `tokens` has no refresh token, and
    /// [`SignInError::Store`] if the store refuses it.
    pub async fn signed_in(
        account: AccountId,
        tokens: TokenSet,
        scopes: Scopes,
        client: TokenClient,
        store: Arc<S>,
    ) -> Result<Self, SignInError> {
        let refresh = tokens
            .refresh_token
            .as_ref()
            .ok_or(SignInError::MissingRefreshToken)?;
        if let Err(source) = store.save(&account, refresh).await {
            tracing::warn!(%account, error = %source, "cannot store tokens after sign-in");
            return Err(SignInError::Store { account, source });
        }
        tracing::info!(%account, "signed in; refresh token stored");
        let provider = Self::build(account, scopes, client, store);
        provider.session.lock().await.cache = Some(Cached {
            token: tokens.access_token,
            expires_at: tokens.expires_at,
        });
        Ok(provider)
    }

    /// A provider for an account signed in earlier, for example after a daemon restart
    /// (J01 V7). The first call refreshes with the stored refresh token.
    #[must_use]
    pub fn resume(account: AccountId, scopes: Scopes, client: TokenClient, store: Arc<S>) -> Self {
        Self::build(account, scopes, client, store)
    }

    /// The account this provider serves.
    #[must_use]
    pub fn account(&self) -> &AccountId {
        &self.account
    }

    /// Whether the account is `needs-sign-in`: its refresh token was rejected, and no
    /// request is made for it until a new sign-in builds a new provider (ADR-0010 R9).
    #[must_use]
    pub fn needs_sign_in(&self) -> bool {
        self.needs_sign_in.load(Ordering::Acquire)
    }

    /// The non-fatal keyring warning: `Some` while the newest refresh token could not be
    /// stored and is held only in memory (F01 step 9). Tokens are still handed out; the
    /// write is retried, and this clears once it succeeds. For `mydrive doctor` and status.
    #[must_use]
    pub fn store_warning(&self) -> Option<StoreError> {
        self.store_warning
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_store_warning(&self, warning: Option<StoreError>) {
        *self
            .store_warning
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = warning;
    }

    fn needs_sign_in_error(&self) -> TokenError {
        TokenError::NeedsSignIn {
            account: self.account.clone(),
        }
    }

    async fn get(&self, rejected: Option<&AccessToken>) -> Result<AccessToken, TokenError> {
        let mut session = self.session.lock().await;
        if self.needs_sign_in() {
            return Err(self.needs_sign_in_error());
        }
        if let Some(c) = session.cache.as_ref() {
            let fresh = Instant::now() + REFRESH_MARGIN < c.expires_at;
            let replaced = rejected.is_some_and(|r| *r != c.token);
            if (fresh && rejected.is_none()) || (replaced && Instant::now() < c.expires_at) {
                let token = c.token.clone();
                let due = session
                    .last_store_attempt
                    .is_none_or(|t| t.elapsed() >= STORE_RETRY_INTERVAL);
                if due {
                    self.retry_store(&mut session).await;
                }
                return Ok(token);
            }
        }
        session.cache = None;
        self.retry_store(&mut session).await;
        let refreshed = self.refresh(&mut session).await?;
        let token = refreshed.token.clone();
        session.cache = Some(refreshed);
        Ok(token)
    }

    /// Writes the unsaved refresh token, if there is one. Failure is not an error for the
    /// caller: the token stays in memory and the warning stays set.
    async fn retry_store(&self, session: &mut Session) {
        if let Some(token) = session.unsaved.take() {
            self.store_newest(session, token).await;
        }
    }

    /// Stores `token` as the newest refresh token. On failure it is kept in memory as
    /// `unsaved` and the warning is set; the stored (older) token is left as it was.
    async fn store_newest(&self, session: &mut Session, token: RefreshToken) {
        let account = &self.account;
        session.last_store_attempt = Some(Instant::now());
        match self.store.save(account, &token).await {
            Ok(()) => {
                if self.store_warning().is_some() {
                    tracing::info!(%account, "refresh token stored after an earlier failure");
                }
                session.unsaved = None;
                self.set_store_warning(None);
            }
            Err(e) => {
                tracing::warn!(
                    %account,
                    error = %e,
                    "cannot store the newest refresh token; keeping it in memory and retrying"
                );
                session.unsaved = Some(token);
                self.set_store_warning(Some(e));
            }
        }
    }

    /// One refresh. Called with the refresh lock held, so at most one runs per account.
    async fn refresh(&self, session: &mut Session) -> Result<Cached, TokenError> {
        let account = &self.account;
        let old = match session.unsaved.clone() {
            Some(t) => t,
            None => {
                let loaded =
                    self.store
                        .load(account)
                        .await
                        .map_err(|source| TokenError::Store {
                            account: account.clone(),
                            source,
                        })?;
                let Some(t) = loaded else {
                    tracing::warn!(%account, "no stored refresh token; account needs sign-in");
                    self.needs_sign_in.store(true, Ordering::Release);
                    return Err(self.needs_sign_in_error());
                };
                t
            }
        };
        let tokens = match self.client.refresh(&old, &self.scopes).await {
            Ok(tokens) => tokens,
            Err(e) if e.needs_sign_in() => {
                tracing::warn!(%account, error = %e, "refresh token rejected; account needs sign-in");
                self.needs_sign_in.store(true, Ordering::Release);
                return Err(self.needs_sign_in_error());
            }
            Err(source) if source.is_transient() => {
                tracing::warn!(%account, error = %source, "token refresh failed; will retry");
                return Err(TokenError::Transient {
                    account: account.clone(),
                    source,
                });
            }
            Err(source) => {
                tracing::warn!(%account, error = %source, "token refresh refused");
                return Err(TokenError::Rejected {
                    account: account.clone(),
                    source,
                });
            }
        };
        // ADR-0010 R8: the rolled refresh token replaces the stored one before the new
        // access token is handed out. If the write fails, the server may already have
        // retired the old token, so the new one is kept in memory and used from now on, the
        // write is retried, and the session continues with a warning (F01 step 9).
        if let Some(new) = tokens.refresh_token.clone()
            && new != old
        {
            self.store_newest(session, new).await;
        }
        tracing::debug!(%account, "access token refreshed");
        Ok(Cached {
            token: tokens.access_token,
            expires_at: tokens.expires_at,
        })
    }
}

impl<S: TokenStore + ?Sized> TokenProvider for RefreshingTokenProvider<S> {
    fn access_token(&self) -> BoxFuture<'_, Result<AccessToken, TokenError>> {
        Box::pin(self.get(None))
    }

    fn force_refresh<'a>(
        &'a self,
        rejected: &'a AccessToken,
    ) -> BoxFuture<'a, Result<AccessToken, TokenError>> {
        Box::pin(self.get(Some(rejected)))
    }
}
