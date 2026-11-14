//! Handing out valid access tokens, refreshing before expiry (F01, token refresh).
//!
//! # Concurrency and cancellation
//!
//! A refresh (reading the stored refresh token, the token request, and writing the rolled
//! refresh token) runs in a spawned task that the caller cannot cancel; callers only wait
//! for its result. Dropping a caller's future therefore never loses a granted refresh
//! token. The rolled token becomes the in-memory newest before the store write starts, and
//! is marked as saved only when the write succeeds.
//!
//! No keyring or network I/O happens while the state lock is held, and the fast path
//! (a fresh cached token) never waits for I/O. Retries of an unsaved refresh token run in
//! the background with backoff.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use mydrive_core::ids::AccountId;
use tokio::sync::watch;
use tokio::time::Instant;

use super::config::Scopes;
use super::error::{SignInError, StoreError, TokenError};
use super::secret::{AccessToken, RefreshToken};
use super::store::{BoxFuture, TokenStore};
use super::token::{TokenClient, TokenSet};

/// A cached access token is refreshed once less than this is left (F01: 5 minutes), or
/// less than half its lifetime if that is shorter.
pub const REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);

/// First delay before retrying a failed write of the newest refresh token; it doubles on
/// each failure up to [`STORE_RETRY_MAX`].
pub const STORE_RETRY_INITIAL: Duration = Duration::from_secs(30);

/// Longest delay between retries of a failed store write.
pub const STORE_RETRY_MAX: Duration = Duration::from_secs(15 * 60);

/// After the user dismisses a keyring prompt, no write is retried for this long, so they
/// are not prompted again and again. The warning stays visible meanwhile.
pub const STORE_RETRY_AFTER_DISMISSED: Duration = Duration::from_secs(60 * 60);

/// After a failed refresh, callers get the same failure without a new request for this
/// long; it doubles on each consecutive failure up to [`REFRESH_RETRY_MAX`].
pub const REFRESH_RETRY_INITIAL: Duration = Duration::from_secs(5);

/// Longest time a refresh failure is reused.
pub const REFRESH_RETRY_MAX: Duration = Duration::from_secs(5 * 60);

/// How long a refresh waits for the store write before handing out the access token anyway
/// (the write carries on in the background).
const STORE_WRITE_WAIT: Duration = Duration::from_secs(10);

/// Gives the Graph client a valid access token for one account.
pub trait TokenProvider: Send + Sync {
    /// A token that is not near expiry, refreshing first if needed. Concurrent callers that
    /// all need a refresh share one refresh request.
    fn access_token(&self) -> BoxFuture<'_, Result<AccessToken, TokenError>>;

    /// Refreshes because Graph answered 401 to `rejected`, although it looked valid (F01).
    /// If another caller has already replaced `rejected`, the newer token is returned
    /// without a second refresh.
    fn force_refresh<'a>(
        &'a self,
        rejected: &'a AccessToken,
    ) -> BoxFuture<'a, Result<AccessToken, TokenError>>;
}

/// The access token held in memory, with deadlines on both clocks.
struct Cached {
    token: AccessToken,
    deadline: Instant,
    wall_deadline: SystemTime,
    refresh_at: Instant,
    refresh_at_wall: SystemTime,
}

impl Cached {
    fn new(token: AccessToken, deadline: Instant, wall_deadline: SystemTime) -> Self {
        let now = Instant::now();
        let lifetime = deadline.saturating_duration_since(now);
        let threshold = REFRESH_MARGIN.min(lifetime / 2);
        Self {
            token,
            deadline,
            wall_deadline,
            refresh_at: deadline.checked_sub(threshold).unwrap_or(now),
            refresh_at_wall: wall_deadline
                .checked_sub(threshold)
                .unwrap_or(wall_deadline),
        }
    }

    /// Not yet due for refresh on either clock.
    fn is_fresh(&self) -> bool {
        Instant::now() < self.refresh_at && SystemTime::now() < self.refresh_at_wall
    }

    /// Not expired on either clock. The monotonic clock stops during suspend; the wall
    /// clock does not.
    fn is_valid(&self) -> bool {
        Instant::now() < self.deadline && SystemTime::now() < self.wall_deadline
    }
}

type Outcome = Result<AccessToken, TokenError>;

/// Everything guarded by the state lock. The lock is never held across an `.await`.
#[derive(Default)]
struct State {
    cache: Option<Cached>,
    /// The newest refresh token known, once one has been loaded or granted.
    newest: Option<RefreshToken>,
    /// `newest` has not been written to the store yet.
    unsaved: bool,
    /// The refresh in progress, if any.
    inflight: Option<watch::Receiver<Option<Outcome>>>,
    /// The last refresh failure and until when it is reused.
    failure: Option<(TokenError, Instant)>,
    refresh_backoff: Duration,
    /// A store write is running.
    writer_running: bool,
    /// No store write before this.
    next_store_attempt: Option<Instant>,
    store_backoff: Duration,
    warning: Option<StoreError>,
}

struct Inner<S: TokenStore + ?Sized> {
    account: AccountId,
    scopes: Scopes,
    client: TokenClient,
    store: Arc<S>,
    state: Mutex<State>,
    needs_sign_in: AtomicBool,
}

/// A [`TokenProvider`] that refreshes through a [`TokenClient`] and keeps the refresh token
/// in a [`TokenStore`]. See the module documentation for its concurrency guarantees.
pub struct RefreshingTokenProvider<S: TokenStore + ?Sized + 'static> {
    inner: Arc<Inner<S>>,
}

impl<S: TokenStore + ?Sized + 'static> fmt::Debug for RefreshingTokenProvider<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefreshingTokenProvider")
            .field("account", &self.inner.account)
            .field("scopes", &self.inner.scopes)
            .field("needs_sign_in", &self.needs_sign_in())
            .field("store_warning", &self.store_warning())
            .finish_non_exhaustive()
    }
}

impl<S: TokenStore + ?Sized + 'static> RefreshingTokenProvider<S> {
    fn build(account: AccountId, scopes: Scopes, client: TokenClient, store: Arc<S>) -> Self {
        Self {
            inner: Arc::new(Inner {
                account,
                scopes,
                client,
                store,
                state: Mutex::new(State::default()),
                needs_sign_in: AtomicBool::new(false),
            }),
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
            .clone()
            .ok_or(SignInError::MissingRefreshToken)?;
        if let Err(source) = store.save(&account, &refresh).await {
            tracing::warn!(%account, error = %source, "cannot store tokens after sign-in");
            return Err(SignInError::Store { account, source });
        }
        tracing::info!(%account, "signed in; refresh token stored");
        let provider = Self::build(account, scopes, client, store);
        {
            let mut st = provider.inner.lock();
            st.newest = Some(refresh);
            st.cache = Some(Cached::new(
                tokens.access_token,
                tokens.expires_at,
                tokens.expires_at_wall,
            ));
        }
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
        &self.inner.account
    }

    /// Whether the account is `needs-sign-in`: its refresh token was rejected, and no
    /// request is made for it until a new sign-in builds a new provider (ADR-0010 R9).
    #[must_use]
    pub fn needs_sign_in(&self) -> bool {
        self.inner.needs_sign_in.load(Ordering::Acquire)
    }

    /// The non-fatal keyring warning: `Some` while the newest refresh token could not be
    /// stored and is held only in memory (F01 step 9). Tokens are still handed out; the
    /// write is retried in the background, and this clears once it succeeds. A
    /// [`StoreError::Dismissed`] here means the user dismissed the keyring prompt; it is
    /// not retried for [`STORE_RETRY_AFTER_DISMISSED`]. For `mydrive doctor` and status.
    #[must_use]
    pub fn store_warning(&self) -> Option<StoreError> {
        self.inner.lock().warning.clone()
    }
}

impl<S: TokenStore + ?Sized + 'static> Inner<S> {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn needs_sign_in_error(&self) -> TokenError {
        TokenError::NeedsSignIn {
            account: self.account.clone(),
        }
    }

    async fn get(self: &Arc<Self>, rejected: Option<&AccessToken>) -> Outcome {
        let mut rx = {
            let mut st = self.lock();
            if self.needs_sign_in.load(Ordering::Acquire) {
                return Err(self.needs_sign_in_error());
            }
            if let Some(c) = st.cache.as_ref() {
                let usable = match rejected {
                    None => c.is_fresh(),
                    Some(r) => *r != c.token && c.is_valid(),
                };
                if usable {
                    let token = c.token.clone();
                    // Never waits: a due retry runs in the background.
                    drop(self.start_writer(&mut st));
                    return Ok(token);
                }
            }
            match st.inflight.clone() {
                Some(rx) => rx,
                None => {
                    if let Some((e, until)) = &st.failure
                        && Instant::now() < *until
                    {
                        return Err(e.clone());
                    }
                    let (tx, rx) = watch::channel(None);
                    st.inflight = Some(rx.clone());
                    tokio::spawn(Arc::clone(self).run_refresh(tx));
                    rx
                }
            }
        };
        let outcome = rx
            .wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|v| (*v).clone());
        outcome.unwrap_or_else(|| {
            Err(TokenError::RefreshAborted {
                account: self.account.clone(),
            })
        })
    }

    /// Starts the background store writer if a write is due and none is running.
    fn start_writer(self: &Arc<Self>, st: &mut State) -> Option<tokio::task::JoinHandle<()>> {
        let due = st.next_store_attempt.is_none_or(|t| Instant::now() >= t);
        if !st.unsaved || st.writer_running || !due {
            return None;
        }
        st.writer_running = true;
        Some(tokio::spawn(Arc::clone(self).write_newest()))
    }

    /// Writes the newest refresh token until the store holds it, or a write fails.
    async fn write_newest(self: Arc<Self>) {
        let account = &self.account;
        loop {
            let token = {
                let mut st = self.lock();
                match (&st.newest, st.unsaved) {
                    (Some(t), true) => t.clone(),
                    _ => {
                        st.writer_running = false;
                        return;
                    }
                }
            };
            let result = self.store.save(account, &token).await;
            let mut st = self.lock();
            match result {
                Ok(()) if st.newest.as_ref() == Some(&token) => {
                    if st.warning.is_some() {
                        tracing::info!(%account, "refresh token stored after an earlier failure");
                    }
                    st.unsaved = false;
                    st.warning = None;
                    st.store_backoff = Duration::ZERO;
                    st.next_store_attempt = None;
                    st.writer_running = false;
                    return;
                }
                // A newer token arrived during the write: store that one too.
                Ok(()) => {}
                Err(e) => {
                    let delay = if matches!(e, StoreError::Dismissed { .. }) {
                        STORE_RETRY_AFTER_DISMISSED
                    } else {
                        let next =
                            (st.store_backoff * 2).clamp(STORE_RETRY_INITIAL, STORE_RETRY_MAX);
                        st.store_backoff = next;
                        next
                    };
                    tracing::warn!(
                        %account,
                        error = %e,
                        retry_in_secs = delay.as_secs(),
                        "cannot store the newest refresh token; keeping it in memory"
                    );
                    st.next_store_attempt = Some(Instant::now() + delay);
                    st.warning = Some(e);
                    st.writer_running = false;
                    return;
                }
            }
        }
    }

    /// One refresh, in its own task so a dropped caller cannot cancel it.
    async fn run_refresh(self: Arc<Self>, tx: watch::Sender<Option<Outcome>>) {
        let outcome = self.refresh().await;
        {
            let mut st = self.lock();
            st.inflight = None;
            match &outcome {
                Ok(_) => {
                    st.failure = None;
                    st.refresh_backoff = Duration::ZERO;
                }
                Err(e) if e.is_needs_sign_in() => {}
                Err(e) => {
                    let backoff =
                        (st.refresh_backoff * 2).clamp(REFRESH_RETRY_INITIAL, REFRESH_RETRY_MAX);
                    st.refresh_backoff = backoff;
                    st.failure = Some((e.clone(), Instant::now() + backoff));
                }
            }
        }
        // Nobody may be waiting any more; that is fine.
        let _ = tx.send(Some(outcome));
    }

    async fn refresh(self: &Arc<Self>) -> Outcome {
        let account = &self.account;
        let known = self.lock().newest.clone();
        let old = match known {
            Some(t) => t,
            None => match self.store.load(account).await {
                Ok(Some(t)) => {
                    let mut st = self.lock();
                    st.newest.get_or_insert_with(|| t.clone()).clone()
                }
                Ok(None) => {
                    tracing::warn!(%account, "no stored refresh token; account needs sign-in");
                    self.needs_sign_in.store(true, Ordering::Release);
                    return Err(self.needs_sign_in_error());
                }
                Err(source) => {
                    tracing::warn!(%account, error = %source, "cannot read the refresh token");
                    return Err(TokenError::Store {
                        account: account.clone(),
                        source,
                    });
                }
            },
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
        // ADR-0010 R8, F01 step 9: the rolled refresh token becomes the newest in memory
        // before the store write starts, so it is used from now on whatever happens to the
        // write. The write gets a bounded wait before the access token is handed out; if it
        // fails, the session continues with a warning and the write is retried.
        let writer = {
            let mut st = self.lock();
            if let Some(new) = tokens.refresh_token.clone()
                && new != old
            {
                st.newest = Some(new);
                st.unsaved = true;
            }
            self.start_writer(&mut st)
        };
        if let Some(writer) = writer {
            let _ = tokio::time::timeout(STORE_WRITE_WAIT, writer).await;
        }
        let token = tokens.access_token.clone();
        self.lock().cache = Some(Cached::new(
            tokens.access_token,
            tokens.expires_at,
            tokens.expires_at_wall,
        ));
        tracing::debug!(%account, "access token refreshed");
        Ok(token)
    }
}

impl<S: TokenStore + ?Sized + 'static> TokenProvider for RefreshingTokenProvider<S> {
    fn access_token(&self) -> BoxFuture<'_, Result<AccessToken, TokenError>> {
        Box::pin(async move { self.inner.get(None).await })
    }

    fn force_refresh<'a>(
        &'a self,
        rejected: &'a AccessToken,
    ) -> BoxFuture<'a, Result<AccessToken, TokenError>> {
        Box::pin(async move { self.inner.get(Some(rejected)).await })
    }
}
