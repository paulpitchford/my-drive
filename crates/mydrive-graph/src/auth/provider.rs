//! Handing out valid access tokens, refreshing before expiry (F01, token refresh).

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mydrive_core::ids::AccountId;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::config::Scopes;
use super::error::{SignInError, TokenError};
use super::secret::AccessToken;
use super::store::{BoxFuture, TokenStore};
use super::token::{TokenClient, TokenSet};

/// A cached access token is refreshed once it has less than this left (F01: 5 minutes).
pub const REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);

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

/// A [`TokenProvider`] that refreshes through a [`TokenClient`] and keeps the refresh token
/// in a [`TokenStore`].
///
/// The refresh token is read from the store for each refresh, so the store stays the only
/// place it lives. Refreshes are serialised by one lock: callers that arrive while a
/// refresh is running wait for it and then use its result.
pub struct RefreshingTokenProvider<S: TokenStore + ?Sized> {
    account: AccountId,
    scopes: Scopes,
    client: TokenClient,
    store: Arc<S>,
    cache: Mutex<Option<Cached>>,
    needs_sign_in: AtomicBool,
}

impl<S: TokenStore + ?Sized> fmt::Debug for RefreshingTokenProvider<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefreshingTokenProvider")
            .field("account", &self.account)
            .field("scopes", &self.scopes)
            .field("needs_sign_in", &self.needs_sign_in())
            .finish_non_exhaustive()
    }
}

impl<S: TokenStore + ?Sized> RefreshingTokenProvider<S> {
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
        Ok(Self {
            cache: Mutex::new(Some(Cached {
                token: tokens.access_token,
                expires_at: tokens.expires_at,
            })),
            account,
            scopes,
            client,
            store,
            needs_sign_in: AtomicBool::new(false),
        })
    }

    /// A provider for an account signed in earlier, for example after a daemon restart
    /// (J01 V7). The first call refreshes with the stored refresh token.
    #[must_use]
    pub fn resume(account: AccountId, scopes: Scopes, client: TokenClient, store: Arc<S>) -> Self {
        Self {
            account,
            scopes,
            client,
            store,
            cache: Mutex::new(None),
            needs_sign_in: AtomicBool::new(false),
        }
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

    fn needs_sign_in_error(&self) -> TokenError {
        TokenError::NeedsSignIn {
            account: self.account.clone(),
        }
    }

    async fn get(&self, rejected: Option<&AccessToken>) -> Result<AccessToken, TokenError> {
        let mut cache = self.cache.lock().await;
        if self.needs_sign_in() {
            return Err(self.needs_sign_in_error());
        }
        if let Some(c) = cache.as_ref() {
            let fresh = Instant::now() + REFRESH_MARGIN < c.expires_at;
            let replaced = rejected.is_some_and(|r| *r != c.token);
            if (fresh && rejected.is_none()) || (replaced && Instant::now() < c.expires_at) {
                return Ok(c.token.clone());
            }
        }
        *cache = None;
        let refreshed = self.refresh().await?;
        let token = refreshed.token.clone();
        *cache = Some(refreshed);
        Ok(token)
    }

    /// One refresh. Called with the cache lock held, so at most one runs per account.
    async fn refresh(&self) -> Result<Cached, TokenError> {
        let account = &self.account;
        let store_error = |source| TokenError::Store {
            account: account.clone(),
            source,
        };
        let Some(old) = self.store.load(account).await.map_err(store_error)? else {
            tracing::warn!(%account, "no stored refresh token; account needs sign-in");
            self.needs_sign_in.store(true, Ordering::Release);
            return Err(self.needs_sign_in_error());
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
        // access token is handed out. If that fails the old one stays stored and usable.
        if let Some(new) = &tokens.refresh_token
            && *new != old
            && let Err(source) = self.store.save(account, new).await
        {
            tracing::warn!(%account, error = %source, "cannot store rolled refresh token");
            return Err(store_error(source));
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
