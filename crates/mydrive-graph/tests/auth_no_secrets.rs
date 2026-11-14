//! AC8, ADR-0010 R6: no token, code or verifier appears in log events, `Debug` output or
//! error messages.
//!
//! This file is its own test binary with a single test, so it can install a *global*
//! subscriber before any tracing callsite is reached. (A per-thread subscriber misses
//! events whose callsites another test thread registered first, as "never interested".)
//! The test also checks that it captured events from every auth module it exercises, so
//! it cannot pass by capturing nothing.

#[path = "auth_fake_secret_service.rs"]
mod fake_secret_service;
#[path = "auth_support.rs"]
mod support;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mydrive_graph::auth::{
    AccessToken, LoopbackSignIn, PasteSignIn, RefreshingTokenProvider, Scopes, TokenProvider,
};
use support::{
    FakeAuthority, FlakyStore, PASTE_REDIRECT, SaveMode, TestResult, account, browser_get, lock,
    paste_url, query,
};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// A subscriber that writes every field of every span and event, with its target, into
/// one string.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<String>>);

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
        let mut out = lock(&self.0);
        let _ = write!(out, "\n[span {}] ", span.metadata().target());
        span.record(&mut Visitor(&mut out));
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, values: &Record<'_>) {
        values.record(&mut Visitor(&mut lock(&self.0)));
    }
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut out = lock(&self.0);
        let _ = write!(out, "\n[{}] ", event.metadata().target());
        event.record(&mut Visitor(&mut out));
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

/// AC8, ADR-0010 R6, review item 12: across the loopback and paste sign-ins, refreshes,
/// store-write failures and their warning, a rejected refresh, a forged redirect, and the
/// Secret Service store (save, load, replace, a dismissed prompt, failures), no secret reaches a log event, `Debug` output or an error message.
#[test]
fn no_secret_in_logs_debug_or_errors() -> TestResult {
    let capture = Capture::default();
    tracing::subscriber::set_global_default(capture.clone())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    let (seen, verifiers) = runtime.block_on(scenario())?;

    let logs = lock(&capture.0).clone();
    for module in ["loopback", "token", "provider", "secret_service"] {
        let target = format!("[mydrive_graph::auth::{module}]");
        assert!(
            logs.contains(&target),
            "no events captured from {target}; the capture is not working:\n{logs}"
        );
    }
    let mut secrets = vec![
        "fake-access-1".to_owned(),
        "fake-refresh-1".to_owned(),
        "fake-access-2".to_owned(),
        "fake-refresh-2".to_owned(),
        "fake-access-3".to_owned(),
        "fake-refresh-3".to_owned(),
        "fake-id-token".to_owned(),
        "fake-code-loopback".to_owned(),
        "fake-code-paste".to_owned(),
        "fake-code-forged".to_owned(),
        "seed-refresh-token".to_owned(),
        "ss-secret-1".to_owned(),
        "ss-secret-2".to_owned(),
        "ss-dismissed".to_owned(),
        "ss-failed".to_owned(),
    ];
    secrets.extend(verifiers);
    for secret in &secrets {
        assert!(!logs.contains(secret.as_str()), "{secret} in logs:\n{logs}");
        assert!(
            !seen.contains(secret.as_str()),
            "{secret} in output:\n{seen}"
        );
    }
    Ok(())
}

/// The real `SecretServiceStore` against the fake service: save, load, replace, a
/// dismissed prompt and failures, as a store and behind a provider. Returns what it
/// printed.
async fn secret_service_scenario() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    use fake_secret_service::{Keyring, LOGIN, PromptMode, fake_service, lock as keyring};
    use mydrive_graph::auth::{RefreshToken, SecretServiceStore, TokenStore};

    let mut seen = String::new();
    let (conn, state) = fake_service(Keyring {
        default_alias: Some(LOGIN.into()),
        ..Keyring::default()
    })
    .await?;
    let store = SecretServiceStore::with_connection(conn).await?;
    seen += &format!("{store:?}");
    let acct = account("ss");
    seen += &format!("{:?}", store.load(&acct).await?);
    store.save(&acct, &RefreshToken::new("ss-secret-1")).await?;
    store.save(&acct, &RefreshToken::new("ss-secret-2")).await?;
    let loaded = store.load(&acct).await?;
    assert_eq!(
        loaded.as_ref().map(RefreshToken::expose_secret),
        Some("ss-secret-2")
    );
    seen += &format!("{loaded:?}");

    // A dismissed prompt.
    {
        let mut k = keyring(&state);
        k.prompt_on_create = true;
        k.prompt_mode = PromptMode::Dismiss;
    }
    let e = store
        .save(&acct, &RefreshToken::new("ss-dismissed"))
        .await
        .err()
        .ok_or("the dismissed save must fail")?;
    assert!(
        matches!(e, mydrive_graph::auth::StoreError::Dismissed { .. }),
        "{e:?}"
    );
    seen += &format!("{e} {e:?}");

    // The service refuses to create the item.
    {
        let mut k = keyring(&state);
        k.prompt_on_create = false;
        k.fail_create = true;
    }
    let e = store
        .save(&acct, &RefreshToken::new("ss-failed"))
        .await
        .err()
        .ok_or("the failing save must fail")?;
    seen += &format!("{e} {e:?}");

    // Behind a provider: a refresh rolls the token, whose write the service refuses.
    let fake = FakeAuthority::start().await?;
    let provider = RefreshingTokenProvider::resume(
        acct.clone(),
        Scopes::initial_sign_in(),
        fake.client(),
        Arc::new(store),
    );
    let t = provider.access_token().await?;
    let w = provider.store_warning().ok_or("expected a store warning")?;
    seen += &format!("{t:?} {provider:?} {w:?} {}", w.error);
    Ok(seen)
}

async fn scenario() -> Result<(String, Vec<String>), Box<dyn std::error::Error + Send + Sync>> {
    let mut seen = String::new();
    let fake = FakeAuthority::start().await?;

    // Loopback sign-in, with a forged redirect first.
    let sign_in = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
    seen += &format!("{sign_in:?}");
    let port = sign_in.local_addr().port();
    let state = query(sign_in.authorization_url())["state"].clone();
    let browser = tokio::spawn(async move {
        browser_get(port, "/?code=fake-code-forged&state=forged").await?;
        browser_get(port, &format!("/?code=fake-code-loopback&state={state}")).await
    });
    let tokens = sign_in
        .complete(&fake.client(), Duration::from_secs(30))
        .await?;
    browser.await??;
    seen += &format!("{tokens:?} {:?}", fake.client());

    // Paste sign-in: a wrong address, an error redirect, then the real one.
    let mut paste = PasteSignIn::start(&fake.config(), PASTE_REDIRECT, Scopes::initial_sign_in())?;
    seen += &format!("{paste:?}");
    let wrong = paste_url(&paste, "https://other.example.invalid/x", "fake-code-paste");
    let e = paste.complete(&wrong, &fake.client()).await.err();
    seen += &format!(
        "{e:?} {}",
        e.as_ref().map(ToString::to_string).unwrap_or_default()
    );
    let pasted = paste_url(&paste, PASTE_REDIRECT, "fake-code-paste");
    let paste_tokens = paste.complete(&pasted, &fake.client()).await?;
    seen += &format!("{paste_tokens:?} {paste:?}");

    // Provider whose store writes fail, then are dismissed: the warning paths.
    let store = FlakyStore::seeded("a", "seed-refresh-token").await;
    store.set_mode(SaveMode::Fail);
    let provider = RefreshingTokenProvider::resume(
        account("a"),
        Scopes::initial_sign_in(),
        fake.client(),
        store.clone(),
    );
    let t = provider.access_token().await?;
    let w = provider.store_warning().ok_or("expected a store warning")?;
    seen += &format!("{t:?} {provider:?} {w:?} {}", w.error);
    // The dismissed path: a refresh rolls the token again, then a write is forced and
    // the keyring prompt is "dismissed".
    let t = provider.force_refresh(&t).await?;
    store.set_mode(SaveMode::Dismiss);
    let flushed = provider.flush().await;
    let w = provider.store_warning().ok_or("expected a store warning")?;
    assert!(
        matches!(w.error, mydrive_graph::auth::StoreError::Dismissed { .. }),
        "{w:?}"
    );
    seen += &format!("{t:?} {provider:?} {flushed:?} {w:?} {}", w.error);

    // A rejected refresh.
    fake.error(400, "invalid_grant");
    let e = provider
        .force_refresh(&t)
        .await
        .err()
        .ok_or("expected failure")?;
    seen += &format!("{e} {e:?} {provider:?}");
    let e = provider
        .force_refresh(&AccessToken::new("x"))
        .await
        .err()
        .ok_or("expected failure")?;
    seen += &format!("{e} {e:?}");

    seen += &secret_service_scenario().await?;

    let verifiers = fake
        .requests()
        .iter()
        .filter_map(|r| r.get("code_verifier").cloned())
        .collect::<Vec<_>>();
    assert_eq!(verifiers.len(), 2, "both sign-ins redeemed a code");
    Ok((seen, verifiers))
}
