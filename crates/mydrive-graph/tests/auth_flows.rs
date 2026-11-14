//! Sign-in and token tests (WP-009) against a fake authority on 127.0.0.1.
//!
//! Nothing here contacts Microsoft or a real keyring: the authority is an `axum` server in
//! this file, the browser is a raw TCP client, and the store is in memory.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::State as AxState;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Form, Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mydrive_core::ids::AccountId;
use mydrive_graph::auth::{
    AccessToken, AuthConfig, BoxFuture, ClientId, LoopbackSignIn, MemoryTokenStore, PasteSignIn,
    RefreshToken, RefreshingTokenProvider, Scopes, SignInError, StoreError, TokenClient,
    TokenError, TokenProvider, TokenStore,
};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use url::Url;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const CLIENT_ID: &str = "11111111-2222-3333-4444-555555555555";
const PASTE_REDIRECT: &str = "https://auth.example.invalid/nativeclient";

// ---------------------------------------------------------------------------------------
// Fake authority
// ---------------------------------------------------------------------------------------

/// A scripted reply from the fake token endpoint.
#[derive(Clone)]
enum Reply {
    /// 200 with these tokens.
    Tokens {
        access: String,
        refresh: Option<String>,
        expires_in: u64,
    },
    /// An OAuth error body with this status.
    Error {
        status: u16,
        error: &'static str,
        description: &'static str,
    },
}

#[derive(Default)]
struct FakeState {
    /// Every form received, in order.
    requests: Mutex<Vec<HashMap<String, String>>>,
    /// Replies to use, in order; when empty, numbered tokens are issued.
    script: Mutex<VecDeque<Reply>>,
    /// When set, each request waits for a permit before replying.
    gate: Mutex<Option<Arc<Semaphore>>>,
    issued: Mutex<u32>,
}

struct FakeAuthority {
    state: Arc<FakeState>,
    authority: String,
}

impl FakeAuthority {
    async fn start() -> Result<Self, std::io::Error> {
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

    fn config(&self) -> AuthConfig {
        match AuthConfig::with_authority(ClientId::from_config(Some(CLIENT_ID)), &self.authority) {
            Ok(c) => c,
            Err(e) => panic!("fake authority URL rejected: {e}"),
        }
    }

    fn client(&self) -> TokenClient {
        match TokenClient::new(&self.config()) {
            Ok(c) => c,
            Err(e) => panic!("token client: {e}"),
        }
    }

    fn push(&self, reply: Reply) {
        self.state
            .script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(reply);
    }

    fn requests(&self) -> Vec<HashMap<String, String>> {
        self.state
            .requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn count(&self) -> usize {
        self.state
            .requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

async fn token_endpoint(
    AxState(state): AxState<Arc<FakeState>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    state
        .requests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(form);
    let gate = state
        .gate
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(gate) = gate {
        let _permit = gate.acquire().await;
    }
    let reply = state
        .script
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .pop_front();
    let reply = reply.unwrap_or_else(|| {
        let mut n = state.issued.lock().unwrap_or_else(PoisonError::into_inner);
        *n += 1;
        Reply::Tokens {
            access: format!("fake-access-{n}"),
            refresh: Some(format!("fake-refresh-{n}")),
            expires_in: 3600,
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
                "expires_in": expires_in,
                "scope": "Files.ReadWrite.All User.Read",
                "id_token": "fake-id-token",
            });
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
async fn dead_authority() -> AuthConfig {
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

fn query(url: &Url) -> HashMap<String, String> {
    url.query_pairs().into_owned().collect()
}

/// Sends `GET <target>` to the loopback listener, as the browser would after the redirect,
/// and returns the raw response.
async fn browser_get(sign_in_port: u16, target: &str) -> std::io::Result<String> {
    let mut s = TcpStream::connect(("127.0.0.1", sign_in_port)).await?;
    let req = format!("GET {target} HTTP/1.1\r\nHost: localhost:{sign_in_port}\r\n\r\n");
    s.write_all(req.as_bytes()).await?;
    let mut out = String::new();
    s.read_to_string(&mut out).await?;
    Ok(out)
}

fn account(name: &str) -> AccountId {
    AccountId::new(name).unwrap_or_else(|e| panic!("account ID: {e}"))
}

// ---------------------------------------------------------------------------------------
// Loopback flow
// ---------------------------------------------------------------------------------------

/// AC1, ADR-0010 R1, ADR-0010 R5: the loopback flow completes against the fake authority
/// and returns tokens; the authorisation request carries an S256 challenge and the exact
/// initial scopes, and the token request carries the matching verifier.
#[tokio::test]
async fn loopback_flow_returns_tokens() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let sign_in = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    let port = sign_in.local_addr().port();
    let auth = sign_in.authorization_url().clone();
    let q = query(&auth);

    assert_eq!(auth.path(), "/tenant/oauth2/v2.0/authorize");
    assert_eq!(q["client_id"], CLIENT_ID);
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["redirect_uri"], format!("http://localhost:{port}"));
    assert_eq!(q["scope"], "Files.ReadWrite.All offline_access User.Read");
    assert_eq!(q["code_challenge_method"], "S256");
    let challenge = q["code_challenge"].clone();
    let state = q["state"].clone();
    assert!(!challenge.is_empty() && !state.is_empty());

    let browser = tokio::spawn(async move {
        browser_get(port, &format!("/?code=fake-code-1&state={state}")).await
    });
    let tokens = sign_in
        .complete(&fake.client(), Duration::from_secs(30))
        .await?;
    let page = browser.await??;

    assert!(page.starts_with("HTTP/1.1 200 OK"), "{page}");
    assert!(page.contains("You can close this tab"), "{page}");
    assert_eq!(tokens.access_token.expose_secret(), "fake-access-1");
    assert_eq!(
        tokens
            .refresh_token
            .as_ref()
            .map(RefreshToken::expose_secret),
        Some("fake-refresh-1")
    );

    let reqs = fake.requests();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r["grant_type"], "authorization_code");
    assert_eq!(r["code"], "fake-code-1");
    assert_eq!(r["client_id"], CLIENT_ID);
    assert_eq!(r["redirect_uri"], format!("http://localhost:{port}"));
    assert!(!r.contains_key("client_secret"));
    let verifier = &r["code_verifier"];
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        challenge,
        "the verifier sent must match the challenge in the authorisation URL"
    );
    Ok(())
}

/// ADR-0010 R2, AC9: the listener binds only 127.0.0.1, on a port the kernel picked, and
/// stops listening once the redirect has been accepted.
#[tokio::test]
async fn listener_is_loopback_only_and_closes() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let a = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    let b = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    assert_eq!(a.local_addr().ip(), std::net::Ipv4Addr::LOCALHOST);
    assert!(a.local_addr().ip().is_loopback());
    assert_ne!(a.local_addr().port(), 0);
    assert_ne!(a.local_addr().port(), b.local_addr().port());
    drop(b);

    let port = a.local_addr().port();
    let state = query(a.authorization_url())["state"].clone();
    let browser =
        tokio::spawn(async move { browser_get(port, &format!("/?code=c&state={state}")).await });
    a.complete(&fake.client(), Duration::from_secs(30)).await?;
    browser.await??;
    assert!(
        TcpStream::connect(("127.0.0.1", port)).await.is_err(),
        "the listener must be closed after the redirect"
    );
    Ok(())
}

/// ADR-0010 R2: requests that are not the redirect get a 400 and are ignored; the
/// redirect that follows is still accepted.
#[tokio::test]
async fn unrelated_requests_are_ignored() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let sign_in = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    let port = sign_in.local_addr().port();
    let state = query(sign_in.authorization_url())["state"].clone();
    let browser = tokio::spawn(async move {
        let favicon = browser_get(port, "/favicon.ico").await?;
        let bare = browser_get(port, "/").await?;
        let other_path = browser_get(port, &format!("/x?code=c&state={state}")).await?;
        let done = browser_get(port, &format!("/?code=c&state={state}")).await?;
        Ok::<_, std::io::Error>((favicon, bare, other_path, done))
    });
    sign_in
        .complete(&fake.client(), Duration::from_secs(30))
        .await?;
    let (favicon, bare, other_path, done) = browser.await??;
    assert!(favicon.starts_with("HTTP/1.1 400"), "{favicon}");
    assert!(bare.starts_with("HTTP/1.1 400"), "{bare}");
    assert!(other_path.starts_with("HTTP/1.1 400"), "{other_path}");
    assert!(done.starts_with("HTTP/1.1 200"), "{done}");
    assert_eq!(fake.count(), 1);
    Ok(())
}

/// AC2, ADR-0010 R2: a redirect with a wrong or missing `state` is rejected, the attempt
/// ends, and no token request is made.
#[tokio::test]
async fn wrong_or_missing_state_is_rejected_without_token_request() -> TestResult {
    let fake = FakeAuthority::start().await?;
    for target in ["/?code=c&state=not-the-state", "/?code=c"] {
        let sign_in = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
        let port = sign_in.local_addr().port();
        let target = target.to_owned();
        let browser = tokio::spawn(async move { browser_get(port, &target).await });
        let result = sign_in
            .complete(&fake.client(), Duration::from_secs(30))
            .await;
        let page = browser.await??;
        assert!(
            matches!(result, Err(SignInError::StateMismatch)),
            "{result:?}"
        );
        assert!(page.starts_with("HTTP/1.1 400"), "{page}");
        assert!(page.contains("does not belong to this attempt"), "{page}");
    }
    assert_eq!(fake.count(), 0, "no token request may be made");
    Ok(())
}

/// AC3: an error redirect is a typed error carrying the description.
#[tokio::test]
async fn error_redirect_is_typed_error_with_description() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let sign_in = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    let port = sign_in.local_addr().port();
    let state = query(sign_in.authorization_url())["state"].clone();
    let browser = tokio::spawn(async move {
        browser_get(
            port,
            &format!(
                "/?error=access_denied&error_description=The+user+declined+consent&state={state}"
            ),
        )
        .await
    });
    let result = sign_in
        .complete(&fake.client(), Duration::from_secs(30))
        .await;
    browser.await??;
    match result {
        Err(SignInError::Authorization { error, description }) => {
            assert_eq!(error, "access_denied");
            assert_eq!(description.as_deref(), Some("The user declined consent"));
        }
        other => panic!("expected an authorisation error, got {other:?}"),
    }
    assert_eq!(fake.count(), 0);
    Ok(())
}

/// The wait for the redirect ends with a timeout. The clock is paused, so no real time
/// passes.
#[tokio::test(start_paused = true)]
async fn loopback_times_out() -> TestResult {
    let config = AuthConfig::with_authority(
        ClientId::from_config(Some(CLIENT_ID)),
        "http://127.0.0.1:9/tenant/",
    )?;
    let client = TokenClient::new(&config)?;
    let sign_in = LoopbackSignIn::start(&config, Scopes::initial_sign_in()).await?;
    let port = sign_in.local_addr().port();
    let result = sign_in.complete(&client, Duration::from_secs(600)).await;
    assert!(
        matches!(result, Err(SignInError::TimedOut { .. })),
        "{result:?}"
    );
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Paste flow
// ---------------------------------------------------------------------------------------

fn paste_url(sign_in: &PasteSignIn, base: &str, extra: &str) -> String {
    let state = sign_in
        .authorization_url()
        .map(|u| query(u)["state"].clone())
        .unwrap_or_default();
    format!("{base}?code=fake-code-p&state={state}{extra}")
}

/// AC4, ADR-0010 R10: the paste flow accepts the full redirected URL, and rejects a URL for
/// a different redirect URI while staying pending.
#[tokio::test]
async fn paste_flow_accepts_full_url_and_rejects_other_redirect_uri() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let client = fake.client();
    let mut sign_in =
        PasteSignIn::start(&fake.config(), PASTE_REDIRECT, Scopes::initial_sign_in())?;
    let auth = sign_in.authorization_url().cloned().ok_or("not pending")?;
    assert_eq!(query(&auth)["redirect_uri"], PASTE_REDIRECT);
    assert_eq!(query(&auth)["code_challenge_method"], "S256");

    for other in [
        "https://evil.example.invalid/nativeclient",
        "https://auth.example.invalid/other",
        "http://auth.example.invalid/nativeclient",
        "https://auth.example.invalid:8443/nativeclient",
    ] {
        let pasted = paste_url(&sign_in, other, "");
        let r = sign_in.complete(&pasted, &client).await;
        assert!(
            matches!(r, Err(SignInError::WrongRedirectUri { .. })),
            "{other}: {r:?}"
        );
        assert!(sign_in.is_pending());
    }
    for junk in ["", "not a url", "code=abc"] {
        let r = sign_in.complete(junk, &client).await;
        assert!(
            matches!(r, Err(SignInError::MalformedRedirect { .. })),
            "{junk:?}: {r:?}"
        );
        assert!(sign_in.is_pending());
    }
    assert_eq!(fake.count(), 0);

    // Surrounding whitespace from a terminal paste is fine.
    let pasted = format!("  {}\n", paste_url(&sign_in, PASTE_REDIRECT, ""));
    let tokens = sign_in.complete(&pasted, &client).await?;
    assert_eq!(tokens.access_token.expose_secret(), "fake-access-1");
    let reqs = fake.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0]["code"], "fake-code-p");
    assert_eq!(reqs[0]["redirect_uri"], PASTE_REDIRECT);
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(reqs[0]["code_verifier"].as_bytes())),
        query(&auth)["code_challenge"]
    );
    assert!(!sign_in.is_pending());
    assert!(matches!(
        sign_in.complete(&pasted, &client).await,
        Err(SignInError::AttemptClosed)
    ));
    Ok(())
}

/// ADR-0010 R10: a pasted URL whose `state` does not match is rejected and cancels the
/// attempt, with no token request.
#[tokio::test]
async fn paste_with_wrong_state_cancels_attempt() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let client = fake.client();
    let mut sign_in =
        PasteSignIn::start(&fake.config(), PASTE_REDIRECT, Scopes::initial_sign_in())?;
    let good = paste_url(&sign_in, PASTE_REDIRECT, "");
    let bad = format!("{PASTE_REDIRECT}?code=fake-code-p&state=forged");
    assert!(matches!(
        sign_in.complete(&bad, &client).await,
        Err(SignInError::StateMismatch)
    ));
    assert!(!sign_in.is_pending());
    assert!(matches!(
        sign_in.complete(&good, &client).await,
        Err(SignInError::AttemptClosed)
    ));
    assert_eq!(fake.count(), 0);
    Ok(())
}

/// AC3 (paste): an error redirect is typed, and the attempt stays pending (F01).
#[tokio::test]
async fn paste_error_redirect_is_typed_and_keeps_attempt() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let mut sign_in =
        PasteSignIn::start(&fake.config(), PASTE_REDIRECT, Scopes::initial_sign_in())?;
    let state = query(sign_in.authorization_url().ok_or("not pending")?)["state"].clone();
    let pasted =
        format!("{PASTE_REDIRECT}?error=access_denied&error_description=declined&state={state}");
    match sign_in.complete(&pasted, &fake.client()).await {
        Err(SignInError::Authorization { error, description }) => {
            assert_eq!(error, "access_denied");
            assert_eq!(description.as_deref(), Some("declined"));
        }
        other => panic!("{other:?}"),
    }
    assert!(sign_in.is_pending());
    assert_eq!(fake.count(), 0);
    Ok(())
}

/// A pending paste attempt is discarded after 10 minutes (F01). Paused clock.
#[tokio::test(start_paused = true)]
async fn paste_attempt_expires() -> TestResult {
    let config = AuthConfig::with_authority(
        ClientId::from_config(Some(CLIENT_ID)),
        "http://127.0.0.1:9/tenant/",
    )?;
    let client = TokenClient::new(&config)?;
    let mut sign_in = PasteSignIn::start(&config, PASTE_REDIRECT, Scopes::initial_sign_in())?;
    let pasted = paste_url(&sign_in, PASTE_REDIRECT, "");
    tokio::time::advance(Duration::from_secs(10 * 60)).await;
    assert!(matches!(
        sign_in.complete(&pasted, &client).await,
        Err(SignInError::TimedOut { .. })
    ));
    assert!(!sign_in.is_pending());
    Ok(())
}

/// A token response without a refresh token fails the sign-in: the account could not stay
/// signed in.
#[tokio::test]
async fn sign_in_without_refresh_token_fails() -> TestResult {
    let fake = FakeAuthority::start().await?;
    fake.push(Reply::Tokens {
        access: "fake-access-x".into(),
        refresh: None,
        expires_in: 3600,
    });
    let mut sign_in =
        PasteSignIn::start(&fake.config(), PASTE_REDIRECT, Scopes::initial_sign_in())?;
    let pasted = paste_url(&sign_in, PASTE_REDIRECT, "");
    assert!(matches!(
        sign_in.complete(&pasted, &fake.client()).await,
        Err(SignInError::MissingRefreshToken)
    ));
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Token provider
// ---------------------------------------------------------------------------------------

fn resume(
    fake_client: TokenClient,
    store: Arc<MemoryTokenStore>,
    name: &str,
) -> RefreshingTokenProvider<MemoryTokenStore> {
    RefreshingTokenProvider::resume(account(name), Scopes::initial_sign_in(), fake_client, store)
}

async fn seeded_store(name: &str, token: &str) -> Arc<MemoryTokenStore> {
    let store = Arc::new(MemoryTokenStore::new());
    store
        .save(&account(name), &RefreshToken::new(token))
        .await
        .unwrap_or_else(|e| panic!("seed: {e}"));
    store
}

/// A cached token with more than the refresh margin left is reused; one inside the margin
/// is refreshed first.
#[tokio::test]
async fn refreshes_before_expiry() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = seeded_store("a", "seed").await;
    let provider = resume(fake.client(), store, "a");

    // Inside the 5-minute margin: every call refreshes.
    for n in 1..=2 {
        fake.push(Reply::Tokens {
            access: format!("short-{n}"),
            refresh: None,
            expires_in: 200,
        });
    }
    assert_eq!(provider.access_token().await?.expose_secret(), "short-1");
    assert_eq!(provider.access_token().await?.expose_secret(), "short-2");
    assert_eq!(fake.count(), 2);

    // An hour left: reused.
    let t = provider.access_token().await?;
    assert_eq!(t.expose_secret(), "fake-access-1");
    assert_eq!(provider.access_token().await?, t);
    assert_eq!(fake.count(), 3);
    let r = &fake.requests()[0];
    assert_eq!(r["grant_type"], "refresh_token");
    assert_eq!(r["refresh_token"], "seed");
    assert_eq!(r["scope"], "Files.ReadWrite.All offline_access User.Read");
    Ok(())
}

/// AC5: ten concurrent callers that all need a refresh cause exactly one refresh request,
/// and all get its token.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callers_share_one_refresh() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let gate = Arc::new(Semaphore::new(0));
    *fake
        .state
        .gate
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(gate.clone());
    let store = seeded_store("a", "seed").await;
    let provider = Arc::new(resume(fake.client(), store, "a"));

    let callers: Vec<_> = (0..10)
        .map(|_| {
            let p = provider.clone();
            tokio::spawn(async move { p.access_token().await })
        })
        .collect();
    // Hold the one refresh open until it has arrived, so the other callers pile up behind it.
    while fake.count() == 0 {
        tokio::task::yield_now().await;
    }
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    gate.add_permits(100);
    for c in callers {
        assert_eq!(c.await??.expose_secret(), "fake-access-1");
    }
    assert_eq!(fake.count(), 1, "exactly one refresh request");
    Ok(())
}

/// After a 401, `force_refresh` refreshes once even if several callers report the same
/// rejected token.
#[tokio::test]
async fn force_refresh_refreshes_once_per_rejected_token() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = seeded_store("a", "seed").await;
    let provider = resume(fake.client(), store, "a");
    let first = provider.access_token().await?;
    let second = provider.force_refresh(&first).await?;
    assert_ne!(first, second);
    assert_eq!(provider.force_refresh(&first).await?, second);
    assert_eq!(fake.count(), 2);
    Ok(())
}

/// AC6, ADR-0010 R8: a rolled refresh token replaces the stored one before the new access
/// token is handed out, and the next refresh uses it.
#[tokio::test]
async fn rolled_refresh_token_is_stored_before_hand_out() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = seeded_store("a", "seed").await;
    let provider = resume(fake.client(), store.clone(), "a");
    fake.push(Reply::Tokens {
        access: "a1".into(),
        refresh: Some("rolled-1".into()),
        expires_in: 200,
    });
    fake.push(Reply::Tokens {
        access: "a2".into(),
        refresh: Some("rolled-2".into()),
        expires_in: 200,
    });
    assert_eq!(provider.access_token().await?.expose_secret(), "a1");
    assert_eq!(
        store
            .get(&account("a"))
            .as_ref()
            .map(RefreshToken::expose_secret),
        Some("rolled-1")
    );
    assert_eq!(provider.access_token().await?.expose_secret(), "a2");
    assert_eq!(
        store
            .get(&account("a"))
            .as_ref()
            .map(RefreshToken::expose_secret),
        Some("rolled-2")
    );
    let reqs = fake.requests();
    assert_eq!(reqs[0]["refresh_token"], "seed");
    assert_eq!(reqs[1]["refresh_token"], "rolled-1");
    Ok(())
}

/// A store whose writes can be made to fail; reads come from the inner store.
#[derive(Default)]
struct FlakyStore {
    inner: MemoryTokenStore,
    fail_saves: Mutex<bool>,
}

impl TokenStore for FlakyStore {
    fn load<'a>(
        &'a self,
        account: &'a AccountId,
    ) -> BoxFuture<'a, Result<Option<RefreshToken>, StoreError>> {
        self.inner.load(account)
    }

    fn save<'a>(
        &'a self,
        account: &'a AccountId,
        token: &'a RefreshToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        if *self
            .fail_saves
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            Box::pin(async {
                Err(StoreError::Failed {
                    operation: "storing a refresh token",
                    reason: "test failure".into(),
                })
            })
        } else {
            self.inner.save(account, token)
        }
    }

    fn delete<'a>(&'a self, account: &'a AccountId) -> BoxFuture<'a, Result<(), StoreError>> {
        self.inner.delete(account)
    }
}

fn set_failing(store: &FlakyStore, fail: bool) {
    *store
        .fail_saves
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = fail;
}

fn stored(store: &FlakyStore) -> Option<String> {
    store
        .inner
        .get(&account("a"))
        .map(|t| t.expose_secret().to_owned())
}

/// AC6 (as reworded), F01 step 9, ADR-0010 R8: if storing the rolled refresh token fails,
/// the session continues: the access token is handed out, the stored old token is left
/// untouched, the new token is kept in memory and used for later refreshes, a non-fatal
/// warning is set, and the next successful write stores the newest token.
#[tokio::test]
async fn failed_store_write_keeps_session_and_newest_token() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = Arc::new(FlakyStore::default());
    store
        .inner
        .save(&account("a"), &RefreshToken::new("old"))
        .await?;
    set_failing(&store, true);
    let provider = RefreshingTokenProvider::resume(
        account("a"),
        Scopes::initial_sign_in(),
        fake.client(),
        store.clone(),
    );

    // The refresh is granted; the write fails; the session continues.
    let t1 = provider.access_token().await?;
    assert_eq!(t1.expose_secret(), "fake-access-1");
    assert!(!provider.needs_sign_in());
    assert!(matches!(
        provider.store_warning(),
        Some(StoreError::Failed { .. })
    ));
    assert_eq!(stored(&store).as_deref(), Some("old"));

    // The next refresh uses the in-memory token, not the stale stored one. The write
    // still fails; the stored token is still not touched.
    let t2 = provider.force_refresh(&t1).await?;
    assert_eq!(t2.expose_secret(), "fake-access-2");
    assert!(provider.store_warning().is_some());
    assert_eq!(stored(&store).as_deref(), Some("old"));

    // A refresh that is not granted hands out no access token.
    fake.push(Reply::Error {
        status: 503,
        error: "temporarily_unavailable",
        description: "try later",
    });
    let r = provider.force_refresh(&t2).await;
    assert!(matches!(r, Err(TokenError::Transient { .. })), "{r:?}");

    // The store recovers: the next refresh stores the newest token and clears the warning.
    set_failing(&store, false);
    let t4 = provider.force_refresh(&t2).await?;
    assert_eq!(t4.expose_secret(), "fake-access-3");
    assert!(provider.store_warning().is_none());
    assert_eq!(stored(&store).as_deref(), Some("fake-refresh-3"));

    let reqs = fake.requests();
    let used: Vec<&str> = reqs.iter().map(|r| r["refresh_token"].as_str()).collect();
    assert_eq!(
        used,
        ["old", "fake-refresh-1", "fake-refresh-2", "fake-refresh-2"],
        "each refresh used the newest refresh token held"
    );
    Ok(())
}

/// F01 step 9: while a refresh token is unsaved, the write is retried on use (at most every
/// `STORE_RETRY_INTERVAL`) without a refresh, and succeeds once the store recovers.
#[tokio::test]
async fn unsaved_refresh_token_is_retried_on_use() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = Arc::new(FlakyStore::default());
    store
        .inner
        .save(&account("a"), &RefreshToken::new("old"))
        .await?;
    set_failing(&store, true);
    let provider = RefreshingTokenProvider::resume(
        account("a"),
        Scopes::initial_sign_in(),
        fake.client(),
        store.clone(),
    );
    let t = provider.access_token().await?;
    assert!(provider.store_warning().is_some());

    set_failing(&store, false);
    // Within the retry interval the cached token is handed out without a write.
    assert_eq!(provider.access_token().await?, t);
    assert_eq!(stored(&store).as_deref(), Some("old"));

    // After the interval the next use retries the write. The clock is paused only now,
    // with no request in flight; the cached token is valid for an hour, so no network.
    tokio::time::pause();
    tokio::time::advance(mydrive_graph::auth::STORE_RETRY_INTERVAL).await;
    assert_eq!(provider.access_token().await?, t);
    assert_eq!(stored(&store).as_deref(), Some("fake-refresh-1"));
    assert!(provider.store_warning().is_none());
    assert_eq!(fake.count(), 1, "the retry made no refresh request");
    Ok(())
}

/// AC7, ADR-0010 R9: `invalid_grant` makes the account "needs sign-in", after which it
/// makes no more requests.
#[tokio::test]
async fn invalid_grant_means_needs_sign_in() -> TestResult {
    let fake = FakeAuthority::start().await?;
    fake.push(Reply::Error {
        status: 400,
        error: "invalid_grant",
        description: "AADSTS70008: The refresh token has expired.",
    });
    let store = seeded_store("a", "seed").await;
    let provider = resume(fake.client(), store.clone(), "a");
    let r = provider.access_token().await;
    assert!(matches!(&r, Err(e) if e.is_needs_sign_in()), "{r:?}");
    assert!(provider.needs_sign_in());
    let r = provider.access_token().await;
    assert!(matches!(&r, Err(e) if e.is_needs_sign_in()), "{r:?}");
    assert_eq!(
        fake.count(),
        1,
        "no requests once the account needs sign-in"
    );
    // The stored token is not deleted by a failed refresh; sign-out does that.
    assert!(store.get(&account("a")).is_some());
    Ok(())
}

/// AC7: a network failure, a 503 and a 429 are transient, not "needs sign-in".
#[tokio::test]
async fn transient_failures_are_not_needs_sign_in() -> TestResult {
    let store = seeded_store("a", "seed").await;
    let dead = TokenClient::new(&dead_authority().await)?;
    let provider = resume(dead, store.clone(), "a");
    let r = provider.access_token().await;
    assert!(matches!(r, Err(TokenError::Transient { .. })), "{r:?}");
    assert!(!provider.needs_sign_in());

    let fake = FakeAuthority::start().await?;
    fake.push(Reply::Error {
        status: 503,
        error: "temporarily_unavailable",
        description: "try later",
    });
    fake.push(Reply::Error {
        status: 429,
        error: "too_many_requests",
        description: "slow down",
    });
    let provider = resume(fake.client(), store, "a");
    for _ in 0..2 {
        let r = provider.access_token().await;
        assert!(matches!(r, Err(TokenError::Transient { .. })), "{r:?}");
        assert!(!provider.needs_sign_in());
    }
    assert_eq!(
        provider.access_token().await?.expose_secret(),
        "fake-access-1"
    );
    Ok(())
}

/// ADR-0010 R9: a failed refresh puts only that account into "needs sign-in"; another
/// account keeps getting tokens.
#[tokio::test]
async fn needs_sign_in_is_per_account() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = Arc::new(MemoryTokenStore::new());
    store
        .save(&account("a"), &RefreshToken::new("revoked"))
        .await?;
    store
        .save(&account("b"), &RefreshToken::new("good"))
        .await?;
    fake.push(Reply::Error {
        status: 400,
        error: "invalid_grant",
        description: "revoked",
    });
    let a = resume(fake.client(), store.clone(), "a");
    let b = resume(fake.client(), store, "b");
    assert!(a.access_token().await.is_err());
    assert!(a.needs_sign_in());
    assert!(b.access_token().await.is_ok());
    assert!(!b.needs_sign_in());
    Ok(())
}

/// A store that is never available, like a session with no Secret Service.
struct NoSecretService;

impl TokenStore for NoSecretService {
    fn load<'a>(
        &'a self,
        _: &'a AccountId,
    ) -> BoxFuture<'a, Result<Option<RefreshToken>, StoreError>> {
        Box::pin(async { Err(unavailable()) })
    }
    fn save<'a>(
        &'a self,
        _: &'a AccountId,
        _: &'a RefreshToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async { Err(unavailable()) })
    }
    fn delete<'a>(&'a self, _: &'a AccountId) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async { Err(unavailable()) })
    }
}

fn unavailable() -> StoreError {
    StoreError::Unavailable {
        reason: "org.freedesktop.secrets is not on the bus".into(),
    }
}

/// ADR-0010 R7: with no Secret Service, finishing sign-in fails with an error naming
/// `mydrive doctor`, and no provider (so no token) is kept.
#[tokio::test]
async fn sign_in_without_secret_service_fails_naming_doctor() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let mut sign_in =
        PasteSignIn::start(&fake.config(), PASTE_REDIRECT, Scopes::initial_sign_in())?;
    let pasted = paste_url(&sign_in, PASTE_REDIRECT, "");
    let tokens = sign_in.complete(&pasted, &fake.client()).await?;
    let r = RefreshingTokenProvider::signed_in(
        account("a"),
        tokens,
        Scopes::initial_sign_in(),
        fake.client(),
        Arc::new(NoSecretService),
    )
    .await;
    let e = r.err().ok_or("sign-in must fail")?;
    let msg = format!(
        "{e}: {}",
        std::error::Error::source(&e)
            .map(ToString::to_string)
            .unwrap_or_default()
    );
    assert!(msg.contains("mydrive doctor"), "{msg}");
    Ok(())
}

/// ADR-0010 R7: the Secret Service store reports a missing bus as unavailable, naming
/// `mydrive doctor`. The address points nowhere, so no real keyring is touched.
#[tokio::test]
async fn secret_service_store_unavailable_names_doctor() -> TestResult {
    let r = mydrive_graph::auth::SecretServiceStore::connect_to(
        "unix:path=/nonexistent/mydrive-test/bus",
    )
    .await;
    match r {
        Err(e @ StoreError::Unavailable { .. }) => {
            assert!(e.to_string().contains("mydrive doctor"), "{e}");
        }
        other => panic!("expected unavailable, got {other:?}"),
    }
    Ok(())
}

/// Signing in stores the refresh token and hands out the access token without a refresh.
#[tokio::test]
async fn signed_in_stores_refresh_token() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let mut sign_in =
        PasteSignIn::start(&fake.config(), PASTE_REDIRECT, Scopes::initial_sign_in())?;
    let pasted = paste_url(&sign_in, PASTE_REDIRECT, "");
    let tokens = sign_in.complete(&pasted, &fake.client()).await?;
    let store = Arc::new(MemoryTokenStore::new());
    let provider = RefreshingTokenProvider::signed_in(
        account("a"),
        tokens,
        Scopes::initial_sign_in(),
        fake.client(),
        store.clone(),
    )
    .await?;
    assert_eq!(
        store
            .get(&account("a"))
            .as_ref()
            .map(RefreshToken::expose_secret),
        Some("fake-refresh-1")
    );
    assert_eq!(
        provider.access_token().await?.expose_secret(),
        "fake-access-1"
    );
    assert_eq!(fake.count(), 1);
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Secrets never leak (AC8, ADR-0010 R6)
// ---------------------------------------------------------------------------------------

mod capture {
    use std::fmt::Write as _;
    use std::sync::{Arc, Mutex, PoisonError};

    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    /// A subscriber that writes every field of every span and event into one string.
    #[derive(Clone, Default)]
    pub(crate) struct Capture(pub(crate) Arc<Mutex<String>>);

    struct Visitor<'a>(&'a mut String);

    impl Visit for Visitor<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let _ = write!(self.0, "{}={value:?} ", field.name());
        }
    }

    impl Subscriber for Capture {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, span: &Attributes<'_>) -> Id {
            span.record(&mut Visitor(
                &mut self.0.lock().unwrap_or_else(PoisonError::into_inner),
            ));
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, values: &Record<'_>) {
            values.record(&mut Visitor(
                &mut self.0.lock().unwrap_or_else(PoisonError::into_inner),
            ));
        }
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut out = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = write!(out, "\n{} ", event.metadata().target());
            event.record(&mut Visitor(&mut out));
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }
}

/// AC8, ADR-0010 R6: across a sign-in, refreshes and failures, no token, code or verifier
/// appears in log events, `Debug` output or error messages.
#[tokio::test]
async fn no_secret_in_logs_debug_or_errors() -> TestResult {
    let capture = capture::Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let mut seen = String::new();

    let fake = FakeAuthority::start().await?;
    let sign_in = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    seen += &format!("{sign_in:?}");
    let port = sign_in.local_addr().port();
    let state = query(sign_in.authorization_url())["state"].clone();
    let browser = tokio::spawn(async move {
        browser_get(port, &format!("/?code=fake-code-1&state={state}")).await
    });
    let tokens = sign_in
        .complete(&fake.client(), Duration::from_secs(30))
        .await?;
    browser.await??;
    seen += &format!("{tokens:?} {:?}", fake.client());

    let store = Arc::new(MemoryTokenStore::new());
    fake.push(Reply::Tokens {
        access: "fake-access-rolled".into(),
        refresh: Some("fake-refresh-rolled".into()),
        expires_in: 200,
    });
    fake.push(Reply::Error {
        status: 400,
        error: "invalid_grant",
        description: "expired",
    });
    let provider = RefreshingTokenProvider::signed_in(
        account("a"),
        tokens,
        Scopes::initial_sign_in(),
        fake.client(),
        store.clone(),
    )
    .await?;
    let t = provider
        .force_refresh(&AccessToken::new("fake-access-1"))
        .await?;
    seen += &format!("{t:?} {provider:?} {store:?}");
    let e = provider
        .force_refresh(&t)
        .await
        .err()
        .ok_or("expected failure")?;
    seen += &format!("{e} {e:?}");

    let wrong = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    let port = wrong.local_addr().port();
    let browser =
        tokio::spawn(async move { browser_get(port, "/?code=fake-code-2&state=x").await });
    let e = wrong
        .complete(&fake.client(), Duration::from_secs(30))
        .await
        .err();
    browser.await??;
    seen += &format!(
        "{e:?} {}",
        e.as_ref().map(ToString::to_string).unwrap_or_default()
    );

    let logs = capture
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert!(
        logs.contains("sign-in redirect"),
        "the capture saw our events: {logs}"
    );
    let verifier = fake.requests()[0]["code_verifier"].clone();
    for secret in [
        "fake-access-1",
        "fake-refresh-1",
        "fake-id-token",
        "fake-code-1",
        "fake-code-2",
        "fake-access-rolled",
        "fake-refresh-rolled",
        verifier.as_str(),
    ] {
        assert!(!logs.contains(secret), "{secret} in logs:\n{logs}");
        assert!(!seen.contains(secret), "{secret} in output:\n{seen}");
    }
    Ok(())
}

/// ADR-0010 R3: no embedded web view. No web-view crate is anywhere in the dependency
/// lock file, so no code path can open one.
#[test]
fn no_embedded_web_view_dependency() {
    let lock =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock")).unwrap();
    for name in [
        "wry",
        "webkit2gtk",
        "webkit6",
        "tao",
        "webview2-com",
        "cef",
        "tauri",
    ] {
        assert!(
            !lock.contains(&format!("name = \"{name}\"")),
            "web-view crate {name} is a dependency"
        );
    }
}

/// ADR-0010 R6: the auth module never writes to disk. Tokens go only to a `TokenStore`.
#[test]
fn auth_module_has_no_file_writes() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/auth");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let src = std::fs::read_to_string(&path).unwrap();
        for api in ["std::fs", "tokio::fs", "File::create", "OpenOptions"] {
            assert!(!src.contains(api), "{} uses {api}", path.display());
        }
    }
}
