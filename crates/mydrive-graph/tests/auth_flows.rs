//! Sign-in tests (WP-009): the loopback and paste flows against a fake authority on
//! 127.0.0.1. Nothing here contacts Microsoft or a real keyring.

#[path = "auth_support.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mydrive_core::ids::AccountId;
use mydrive_graph::auth::{
    AuthConfig, BoxFuture, ClientId, LoopbackSignIn, MemoryTokenStore, PasteSignIn, RefreshToken,
    RefreshingTokenProvider, Scopes, SignInError, StoreError, TokenClient, TokenStore,
};
use sha2::{Digest as _, Sha256};
use support::{
    CLIENT_ID, FakeAuthority, PASTE_REDIRECT, TestResult, account, browser_get, browser_get_at,
    paste_url, query,
};
use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------------------
// Loopback flow
// ---------------------------------------------------------------------------------------

/// AC1, ADR-0010 R1, ADR-0010 R5, review item 12: the loopback flow completes against the
/// fake authority and returns tokens; the authorisation request carries an S256 challenge
/// and the exact initial scopes but never the verifier, and the token request carries the
/// matching verifier.
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
    assert!(!q.contains_key("code_verifier"));
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
    assert!(
        !auth.as_str().contains(verifier.as_str()),
        "the verifier must never be in the authorisation URL"
    );
    Ok(())
}

/// Whether this machine can bind the IPv6 loopback address.
async fn ipv6_loopback_available() -> bool {
    TcpListener::bind("[::1]:0").await.is_ok()
}

/// ADR-0010 R2, AC9, review item 5: the listener binds only loopback addresses, on a port
/// the kernel picked: 127.0.0.1 and, where IPv6 loopback exists, [::1] on the same port.
/// It stops listening on both once the redirect has been accepted.
#[tokio::test]
async fn listener_is_loopback_only_and_closes() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let a = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    let b = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    assert_eq!(a.local_addr().ip(), std::net::Ipv4Addr::LOCALHOST);
    assert_ne!(a.local_addr().port(), 0);
    assert_ne!(a.local_addr().port(), b.local_addr().port());
    for addr in a.local_addrs() {
        assert!(addr.ip().is_loopback(), "{addr}");
        assert_eq!(addr.port(), a.local_addr().port());
    }
    if ipv6_loopback_available().await {
        assert_eq!(a.local_addrs().len(), 2, "{:?}", a.local_addrs());
        assert_eq!(a.local_addrs()[1].ip(), std::net::Ipv6Addr::LOCALHOST);
    } else {
        eprintln!("IPv6 loopback unavailable here; checked IPv4 only");
    }
    drop(b);

    let addrs = a.local_addrs().to_vec();
    let port = a.local_addr().port();
    let state = query(a.authorization_url())["state"].clone();
    let browser =
        tokio::spawn(async move { browser_get(port, &format!("/?code=c&state={state}")).await });
    a.complete(&fake.client(), Duration::from_secs(30)).await?;
    browser.await??;
    for addr in addrs {
        assert!(
            TcpStream::connect(addr).await.is_err(),
            "the listener on {addr} must be closed after the redirect"
        );
    }
    Ok(())
}

/// Review item 5: the redirect is accepted over `[::1]` too, so the IPv6 side of
/// `localhost` belongs to this sign-in.
#[tokio::test]
async fn redirect_over_ipv6_loopback_is_accepted() -> TestResult {
    if !ipv6_loopback_available().await {
        eprintln!("IPv6 loopback unavailable here; nothing to check");
        return Ok(());
    }
    let fake = FakeAuthority::start().await?;
    let sign_in = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    let v6 = *sign_in
        .local_addrs()
        .get(1)
        .ok_or("no [::1] listener although IPv6 loopback is available")?;
    let state = query(sign_in.authorization_url())["state"].clone();
    let browser =
        tokio::spawn(async move { browser_get_at(v6, &format!("/?code=c6&state={state}")).await });
    sign_in
        .complete(&fake.client(), Duration::from_secs(30))
        .await?;
    assert!(browser.await??.starts_with("HTTP/1.1 200"));
    assert_eq!(fake.requests()[0]["code"], "c6");
    Ok(())
}

/// Review item 5: the redirect host is a configuration value, `localhost` by default.
#[tokio::test]
async fn redirect_host_is_configurable() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let config = fake.config().with_loopback_host("127.0.0.1")?;
    let sign_in = LoopbackSignIn::start(&config, Scopes::initial_sign_in()).await?;
    let port = sign_in.local_addr().port();
    assert_eq!(
        query(sign_in.authorization_url())["redirect_uri"],
        format!("http://127.0.0.1:{port}")
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

/// AC2, ADR-0010 R2, review item 3: redirects with a wrong or missing `state` get a 400,
/// cause no token request, and do not cancel the attempt; the real redirect that follows
/// is accepted, exactly once.
#[tokio::test]
async fn wrong_or_missing_state_is_ignored_and_real_redirect_accepted() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let sign_in = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    let port = sign_in.local_addr().port();
    let state = query(sign_in.authorization_url())["state"].clone();
    let browser = tokio::spawn(async move {
        let wrong = browser_get(port, "/?code=forged&state=not-the-state").await?;
        let missing = browser_get(port, "/?code=forged").await?;
        let error_wrong = browser_get(port, "/?error=access_denied&state=not-the-state").await?;
        let real = browser_get(port, &format!("/?code=real&state={state}")).await?;
        Ok::<_, std::io::Error>([wrong, missing, error_wrong, real])
    });
    let tokens = sign_in
        .complete(&fake.client(), Duration::from_secs(30))
        .await?;
    let [wrong, missing, error_wrong, real] = browser.await??;
    for page in [&wrong, &missing, &error_wrong] {
        assert!(page.starts_with("HTTP/1.1 400"), "{page}");
    }
    assert!(wrong.contains("does not belong to this attempt"), "{wrong}");
    assert!(real.starts_with("HTTP/1.1 200"), "{real}");
    assert_eq!(tokens.access_token.expose_secret(), "fake-access-1");
    let reqs = fake.requests();
    assert_eq!(reqs.len(), 1, "exactly one token request");
    assert_eq!(reqs[0]["code"], "real");
    Ok(())
}

/// AC2, review item 3: a wrong `state` alone never completes the attempt and never causes
/// a token request; the attempt only ends by timing out.
#[tokio::test]
async fn wrong_state_alone_times_out_without_token_request() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let sign_in = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    let port = sign_in.local_addr().port();
    let client = fake.client();
    let attempt =
        tokio::spawn(async move { sign_in.complete(&client, Duration::from_secs(2)).await });
    let page = browser_get(port, "/?code=forged&state=not-the-state").await?;
    assert!(page.starts_with("HTTP/1.1 400"), "{page}");
    let result = attempt.await?;
    assert!(
        matches!(result, Err(SignInError::TimedOut { .. })),
        "{result:?}"
    );
    assert_eq!(fake.count(), 0, "no token request may be made");
    Ok(())
}

/// Review item 4: an idle connection and a half-sent request do not delay the real
/// redirect. The attempt's timeout is shorter than the per-connection read deadline, so a
/// listener that served connections one at a time would time out.
#[tokio::test]
async fn idle_connections_do_not_delay_the_redirect() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let sign_in = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    let port = sign_in.local_addr().port();
    let state = query(sign_in.authorization_url())["state"].clone();

    let idle = TcpStream::connect(("127.0.0.1", port)).await?;
    let mut partial = TcpStream::connect(("127.0.0.1", port)).await?;
    partial
        .write_all(b"GET /?code=x HTTP/1.1\r\nHost: loc")
        .await?;

    let client = fake.client();
    let timeout = mydrive_graph::auth::CONNECTION_READ_TIMEOUT * 3 / 4;
    let attempt = tokio::spawn(async move { sign_in.complete(&client, timeout).await });
    let page = browser_get(port, &format!("/?code=real&state={state}")).await?;
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    let tokens = attempt.await??;
    assert_eq!(tokens.access_token.expose_secret(), "fake-access-1");
    // The idle connection's read deadline has not even passed: the server has neither
    // answered nor closed it. A listener that read connections one at a time would have
    // waited it out (and answered it) before reaching the redirect.
    let mut buf = [0u8; 16];
    assert!(
        matches!(idle.try_read(&mut buf), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "the redirect waited for the idle connection"
    );
    drop((idle, partial));
    Ok(())
}

/// Second review item 2: beyond `MAX_CONNECTIONS` connections in flight, extra
/// connections are closed at once rather than held; once the idle ones time out, the real
/// redirect is accepted.
#[tokio::test]
async fn connection_flood_is_capped() -> TestResult {
    use tokio::io::AsyncReadExt as _;
    let fake = FakeAuthority::start().await?;
    let sign_in = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    let port = sign_in.local_addr().port();
    let state = query(sign_in.authorization_url())["state"].clone();
    let client = fake.client();
    let attempt =
        tokio::spawn(async move { sign_in.complete(&client, Duration::from_secs(20)).await });

    let mut idle = Vec::new();
    for _ in 0..mydrive_graph::auth::MAX_CONNECTIONS {
        idle.push(TcpStream::connect(("127.0.0.1", port)).await?);
    }
    let mut extra = TcpStream::connect(("127.0.0.1", port)).await?;
    let mut buf = [0u8; 16];
    let closed = tokio::time::timeout(
        mydrive_graph::auth::CONNECTION_READ_TIMEOUT / 2,
        extra.read(&mut buf),
    )
    .await;
    assert!(
        matches!(closed, Ok(Ok(0)) | Ok(Err(_))),
        "the extra connection must be closed at once, got {closed:?}"
    );

    // The idle connections time out and free their slots; then the redirect gets in.
    let page = loop {
        match browser_get(port, &format!("/?code=real&state={state}")).await {
            Ok(page) if page.starts_with("HTTP/1.1 200") => break page,
            _ => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    };
    assert!(page.contains("You can close this tab"));
    attempt.await??;
    drop(idle);
    Ok(())
}

/// AC3: an error redirect with the matching state is a typed error carrying the
/// description.
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

/// AC4, ADR-0010 R10, review item 12: the paste flow accepts the full redirected URL, and
/// rejects a URL for a different redirect URI while staying pending; the authorisation
/// URL never carries the verifier.
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
        let pasted = paste_url(&sign_in, other, "fake-code-p");
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
    let pasted = format!("  {}\n", paste_url(&sign_in, PASTE_REDIRECT, "fake-code-p"));
    let tokens = sign_in.complete(&pasted, &client).await?;
    assert_eq!(tokens.access_token.expose_secret(), "fake-access-1");
    let reqs = fake.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0]["code"], "fake-code-p");
    assert_eq!(reqs[0]["redirect_uri"], PASTE_REDIRECT);
    let verifier = reqs[0]["code_verifier"].clone();
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        query(&auth)["code_challenge"]
    );
    assert!(!auth.as_str().contains(verifier.as_str()));
    assert!(!sign_in.is_pending());
    assert!(matches!(
        sign_in.complete(&pasted, &client).await,
        Err(SignInError::AttemptClosed)
    ));
    Ok(())
}

/// ADR-0010 R10: a pasted URL whose `state` does not match is rejected and cancels the
/// attempt, with no token request. (Unlike the loopback, a paste comes from the user, so
/// a mismatch means they pasted the wrong address.)
#[tokio::test]
async fn paste_with_wrong_state_cancels_attempt() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let client = fake.client();
    let mut sign_in =
        PasteSignIn::start(&fake.config(), PASTE_REDIRECT, Scopes::initial_sign_in())?;
    let good = paste_url(&sign_in, PASTE_REDIRECT, "fake-code-p");
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
    let pasted = paste_url(&sign_in, PASTE_REDIRECT, "fake-code-p");
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
    fake.tokens("fake-access-x", None, Some(3600));
    let mut sign_in =
        PasteSignIn::start(&fake.config(), PASTE_REDIRECT, Scopes::initial_sign_in())?;
    let pasted = paste_url(&sign_in, PASTE_REDIRECT, "fake-code-p");
    assert!(matches!(
        sign_in.complete(&pasted, &fake.client()).await,
        Err(SignInError::MissingRefreshToken)
    ));
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Storing the sign-in
// ---------------------------------------------------------------------------------------

/// A store that is never available, like a session with no Secret Service.
struct NoSecretService;

fn unavailable() -> StoreError {
    StoreError::Unavailable {
        reason: "org.freedesktop.secrets is not on the bus".into(),
    }
}

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

/// ADR-0010 R7: with no Secret Service, finishing sign-in fails with an error naming
/// `mydrive doctor`, and no provider (so no token) is kept.
#[tokio::test]
async fn sign_in_without_secret_service_fails_naming_doctor() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let mut sign_in =
        PasteSignIn::start(&fake.config(), PASTE_REDIRECT, Scopes::initial_sign_in())?;
    let pasted = paste_url(&sign_in, PASTE_REDIRECT, "fake-code-p");
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
    let pasted = paste_url(&sign_in, PASTE_REDIRECT, "fake-code-p");
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
    use mydrive_graph::auth::TokenProvider as _;
    assert_eq!(
        provider.access_token().await?.expose_secret(),
        "fake-access-1"
    );
    assert_eq!(fake.count(), 1);
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Structural rules
// ---------------------------------------------------------------------------------------

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
