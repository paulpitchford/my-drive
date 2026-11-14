//! Shared helpers for the auth tests: a fake authority on 127.0.0.1, a fake browser, and
//! a token store whose writes can be made to fail, hang or be "dismissed".
//!
//! Included by the `auth_*` test files with `#[path = "auth_support.rs"] mod support;`.
//! Cargo also builds it as a test binary of its own, with no tests; each including file
//! uses only part of it, hence the `dead_code` allowance.
#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::State as AxState;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Form, Json, Router};
use mydrive_core::ids::AccountId;
use mydrive_graph::auth::{
    AuthConfig, BoxFuture, ClientId, MemoryTokenStore, PasteSignIn, RefreshToken,
    RefreshingTokenProvider, Scopes, StoreError, TokenClient, TokenStore,
};
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use url::Url;

pub(crate) type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

pub(crate) const CLIENT_ID: &str = "11111111-2222-3333-4444-555555555555";
pub(crate) const PASTE_REDIRECT: &str = "https://auth.example.invalid/nativeclient";

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------------------
// Fake authority
// ---------------------------------------------------------------------------------------

/// A scripted reply from the fake token endpoint.
#[derive(Clone)]
pub(crate) enum Reply {
    /// 200 with these tokens.
    Tokens {
        access: String,
        refresh: Option<String>,
        expires_in: Option<u64>,
    },
    /// An OAuth error body with this status.
    Error {
        status: u16,
        error: &'static str,
        description: &'static str,
    },
}

#[derive(Default)]
pub(crate) struct FakeState {
    /// Every form received, in order.
    pub(crate) requests: Mutex<Vec<HashMap<String, String>>>,
    /// Replies to use, in order; when empty, numbered tokens are issued.
    pub(crate) script: Mutex<VecDeque<Reply>>,
    /// When set, each request waits for a permit before replying.
    pub(crate) gate: Mutex<Option<Arc<Semaphore>>>,
    pub(crate) issued: Mutex<u32>,
}

pub(crate) struct FakeAuthority {
    pub(crate) state: Arc<FakeState>,
    pub(crate) authority: String,
}

impl FakeAuthority {
    pub(crate) async fn start() -> Result<Self, std::io::Error> {
        let state = Arc::new(FakeState::default());
        let app = Router::new()
            .route("/tenant/oauth2/v2.0/token", post(token_endpoint))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            state,
            authority: format!("http://{addr}/tenant/"),
        })
    }

    pub(crate) fn config(&self) -> AuthConfig {
        AuthConfig::with_authority(ClientId::from_config(Some(CLIENT_ID)), &self.authority)
            .unwrap_or_else(|e| panic!("fake authority URL rejected: {e}"))
    }

    pub(crate) fn client(&self) -> TokenClient {
        TokenClient::new(&self.config()).unwrap_or_else(|e| panic!("token client: {e}"))
    }

    pub(crate) fn push(&self, reply: Reply) {
        lock(&self.state.script).push_back(reply);
    }

    pub(crate) fn tokens(&self, access: &str, refresh: Option<&str>, expires_in: Option<u64>) {
        self.push(Reply::Tokens {
            access: access.to_owned(),
            refresh: refresh.map(str::to_owned),
            expires_in,
        });
    }

    pub(crate) fn error(&self, status: u16, error: &'static str) {
        self.push(Reply::Error {
            status,
            error,
            description: "scripted by the test",
        });
    }

    /// Makes every request wait for a permit from the returned semaphore.
    pub(crate) fn hold(&self) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        *lock(&self.state.gate) = Some(gate.clone());
        gate
    }

    pub(crate) fn requests(&self) -> Vec<HashMap<String, String>> {
        lock(&self.state.requests).clone()
    }

    /// The `refresh_token` sent in each refresh request, in order.
    pub(crate) fn refresh_tokens_sent(&self) -> Vec<String> {
        self.requests()
            .iter()
            .filter_map(|r| r.get("refresh_token").cloned())
            .collect()
    }

    pub(crate) fn count(&self) -> usize {
        lock(&self.state.requests).len()
    }
}

async fn token_endpoint(
    AxState(state): AxState<Arc<FakeState>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    lock(&state.requests).push(form);
    let gate = lock(&state.gate).clone();
    if let Some(gate) = gate
        && let Ok(permit) = gate.acquire().await
    {
        permit.forget();
    }
    let reply = lock(&state.script).pop_front();
    let reply = reply.unwrap_or_else(|| {
        let mut n = lock(&state.issued);
        *n += 1;
        Reply::Tokens {
            access: format!("fake-access-{n}"),
            refresh: Some(format!("fake-refresh-{n}")),
            expires_in: Some(3600),
        }
    });
    match reply {
        Reply::Tokens {
            access,
            refresh,
            expires_in,
        } => {
            let mut body = json!({
                "token_type": "Bearer",
                "access_token": access,
                "scope": "Files.ReadWrite.All User.Read",
                "id_token": "fake-id-token",
            });
            if let Some(e) = expires_in {
                body["expires_in"] = json!(e);
            }
            if let Some(r) = refresh {
                body["refresh_token"] = json!(r);
            }
            Json(body).into_response()
        }
        Reply::Error {
            status,
            error,
            description,
        } => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"error": error, "error_description": description})),
        )
            .into_response(),
    }
}

/// A token endpoint where nothing listens: connections are refused.
pub(crate) async fn dead_authority() -> AuthConfig {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|e| panic!("bind: {e}"));
    let addr = listener
        .local_addr()
        .unwrap_or_else(|e| panic!("local_addr: {e}"));
    drop(listener);
    AuthConfig::with_authority(
        ClientId::from_config(Some(CLIENT_ID)),
        &format!("http://{addr}/tenant/"),
    )
    .unwrap_or_else(|e| panic!("authority: {e}"))
}

// ---------------------------------------------------------------------------------------
// Fake browser
// ---------------------------------------------------------------------------------------

pub(crate) fn query(url: &Url) -> HashMap<String, String> {
    url.query_pairs().into_owned().collect()
}

/// Sends `GET <target>` to the loopback listener at `addr`, as the browser would after the
/// redirect, and returns the raw response.
pub(crate) async fn browser_get_at(
    addr: std::net::SocketAddr,
    target: &str,
) -> std::io::Result<String> {
    let mut s = TcpStream::connect(addr).await?;
    let req = format!(
        "GET {target} HTTP/1.1\r\nHost: localhost:{}\r\n\r\n",
        addr.port()
    );
    s.write_all(req.as_bytes()).await?;
    let mut out = String::new();
    s.read_to_string(&mut out).await?;
    Ok(out)
}

/// As [`browser_get_at`], on `127.0.0.1:<port>`.
pub(crate) async fn browser_get(port: u16, target: &str) -> std::io::Result<String> {
    browser_get_at(([127, 0, 0, 1], port).into(), target).await
}

pub(crate) fn account(name: &str) -> AccountId {
    AccountId::new(name).unwrap_or_else(|e| panic!("account ID: {e}"))
}

pub(crate) fn paste_url(sign_in: &PasteSignIn, base: &str, code: &str) -> String {
    let state = sign_in
        .authorization_url()
        .map(|u| query(u)["state"].clone())
        .unwrap_or_default();
    format!("{base}?code={code}&state={state}")
}

// ---------------------------------------------------------------------------------------
// Stores
// ---------------------------------------------------------------------------------------

pub(crate) async fn seeded_store(name: &str, token: &str) -> Arc<MemoryTokenStore> {
    let store = Arc::new(MemoryTokenStore::new());
    store
        .save(&account(name), &RefreshToken::new(token))
        .await
        .unwrap_or_else(|e| panic!("seed: {e}"));
    store
}

pub(crate) fn resume<S: TokenStore + 'static>(
    client: TokenClient,
    store: Arc<S>,
    name: &str,
) -> RefreshingTokenProvider<S> {
    RefreshingTokenProvider::resume(account(name), Scopes::initial_sign_in(), client, store)
}

/// What [`FlakyStore::save`] does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SaveMode {
    Succeed,
    Fail,
    Dismiss,
    /// Panic in the next write, then succeed.
    PanicOnce,
}

/// What [`FlakyStore::load`] does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LoadMode {
    Normal,
    Dismiss,
    /// Panic in the next read, then read normally.
    PanicOnce,
}

/// A store whose writes can fail, be dismissed, or wait on a gate. Reads come from the
/// inner in-memory store.
pub(crate) struct FlakyStore {
    pub(crate) inner: MemoryTokenStore,
    mode: Mutex<SaveMode>,
    load_mode: Mutex<LoadMode>,
    gate: Mutex<Option<Arc<Semaphore>>>,
    saves: AtomicUsize,
    loads: AtomicUsize,
}

impl FlakyStore {
    pub(crate) async fn seeded(name: &str, token: &str) -> Arc<Self> {
        let store = Arc::new(Self {
            inner: MemoryTokenStore::new(),
            mode: Mutex::new(SaveMode::Succeed),
            load_mode: Mutex::new(LoadMode::Normal),
            gate: Mutex::new(None),
            saves: AtomicUsize::new(0),
            loads: AtomicUsize::new(0),
        });
        store
            .inner
            .save(&account(name), &RefreshToken::new(token))
            .await
            .unwrap_or_else(|e| panic!("seed: {e}"));
        store
    }

    pub(crate) fn set_mode(&self, mode: SaveMode) {
        *lock(&self.mode) = mode;
    }

    pub(crate) fn set_load_mode(&self, mode: LoadMode) {
        *lock(&self.load_mode) = mode;
    }

    /// How many reads were attempted.
    pub(crate) fn loads(&self) -> usize {
        self.loads.load(Ordering::SeqCst)
    }

    /// Makes every write wait for a permit from the returned semaphore.
    pub(crate) fn hold(&self) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        *lock(&self.gate) = Some(gate.clone());
        gate
    }

    /// How many writes were attempted.
    pub(crate) fn saves(&self) -> usize {
        self.saves.load(Ordering::SeqCst)
    }

    /// The stored token for `name`.
    pub(crate) fn stored(&self, name: &str) -> Option<String> {
        self.inner
            .get(&account(name))
            .map(|t| t.expose_secret().to_owned())
    }
}

impl TokenStore for FlakyStore {
    fn load<'a>(
        &'a self,
        account: &'a AccountId,
    ) -> BoxFuture<'a, Result<Option<RefreshToken>, StoreError>> {
        Box::pin(async move {
            self.loads.fetch_add(1, Ordering::SeqCst);
            let mode = std::mem::replace(&mut *lock(&self.load_mode), LoadMode::Normal);
            match mode {
                LoadMode::Normal => self.inner.load(account).await,
                LoadMode::Dismiss => {
                    *lock(&self.load_mode) = LoadMode::Dismiss;
                    Err(StoreError::Dismissed {
                        operation: "reading a refresh token",
                    })
                }
                LoadMode::PanicOnce => panic!("test store panics while reading"),
            }
        })
    }

    fn save<'a>(
        &'a self,
        account: &'a AccountId,
        token: &'a RefreshToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.saves.fetch_add(1, Ordering::SeqCst);
            let gate = lock(&self.gate).clone();
            if let Some(gate) = gate
                && let Ok(permit) = gate.acquire().await
            {
                permit.forget();
            }
            let mode = *lock(&self.mode);
            match mode {
                SaveMode::Succeed => self.inner.save(account, token).await,
                SaveMode::PanicOnce => {
                    *lock(&self.mode) = SaveMode::Succeed;
                    panic!("test store panics while writing")
                }
                SaveMode::Fail => Err(StoreError::Failed {
                    operation: "storing a refresh token",
                    reason: "test failure".into(),
                }),
                SaveMode::Dismiss => Err(StoreError::Dismissed {
                    operation: "storing a refresh token",
                }),
            }
        })
    }

    fn delete<'a>(&'a self, account: &'a AccountId) -> BoxFuture<'a, Result<(), StoreError>> {
        self.inner.delete(account)
    }
}

/// Yields until `cond` holds, up to a bound; returns whether it held. Lets spawned
/// background work run without sleeping.
pub(crate) async fn settle(mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..1000 {
        if cond() {
            return true;
        }
        tokio::task::yield_now().await;
    }
    cond()
}
