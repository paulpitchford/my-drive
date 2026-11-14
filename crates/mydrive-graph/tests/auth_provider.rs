//! Token provider tests (WP-009): refresh timing, concurrency, cancellation safety, store
//! failures and "needs sign-in", against a fake authority on 127.0.0.1.
//!
//! Tests that need time to pass pause the tokio clock only while no request is in flight,
//! and resume it before the next request, so request timeouts never fire early.

#[path = "auth_support.rs"]
mod support;

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use mydrive_graph::auth::{
    AccessToken, MemoryTokenStore, RefreshToken, RefreshingTokenProvider, STORE_RETRY_INITIAL,
    Scopes, StoreError, TokenClient, TokenError, TokenProvider, TokenSet, TokenStore,
};
use support::{
    FakeAuthority, FlakyStore, LoadMode, SaveMode, TestResult, account, dead_authority, resume,
    seeded_store, settle,
};
use tokio::time::Instant;

/// Moves the paused-able tokio clock forward by `by`, with no request in flight.
async fn jump(by: Duration) {
    tokio::time::pause();
    tokio::time::advance(by).await;
    tokio::time::resume();
}

fn token_set(access: &str, refresh: &str, mono: Duration, wall: SystemTime) -> TokenSet {
    TokenSet {
        access_token: AccessToken::new(access),
        expires_at: Instant::now() + mono,
        expires_at_wall: wall,
        refresh_token: Some(RefreshToken::new(refresh)),
        id_token: None,
        scope: None,
    }
}

async fn signed_in(
    fake: &FakeAuthority,
    tokens: TokenSet,
) -> Result<RefreshingTokenProvider<MemoryTokenStore>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(RefreshingTokenProvider::signed_in(
        account("a"),
        tokens,
        Scopes::initial_sign_in(),
        fake.client(),
        Arc::new(MemoryTokenStore::new()),
    )
    .await?)
}

// ---------------------------------------------------------------------------------------
// When to refresh
// ---------------------------------------------------------------------------------------

/// Review item 8: a lifetime at or under the refresh margin, or no `expires_in` at all,
/// does not cause a refresh on every call.
#[tokio::test]
async fn short_or_missing_lifetime_does_not_refresh_every_call() -> TestResult {
    let fake = FakeAuthority::start().await?;
    fake.tokens("short", None, Some(200));
    let p = resume(fake.client(), seeded_store("a", "seed").await, "a");
    assert_eq!(p.access_token().await?.expose_secret(), "short");
    assert_eq!(p.access_token().await?.expose_secret(), "short");
    assert_eq!(fake.count(), 1);

    let fake = FakeAuthority::start().await?;
    fake.tokens("no-expiry", None, None);
    let p = resume(fake.client(), seeded_store("a", "seed").await, "a");
    assert_eq!(p.access_token().await?.expose_secret(), "no-expiry");
    assert_eq!(p.access_token().await?.expose_secret(), "no-expiry");
    assert_eq!(fake.count(), 1);

    let fake = FakeAuthority::start().await?;
    fake.tokens("zero", None, Some(0));
    let p = resume(fake.client(), seeded_store("a", "seed").await, "a");
    assert_eq!(p.access_token().await?.expose_secret(), "zero");
    assert_eq!(p.access_token().await?.expose_secret(), "zero");
    assert_eq!(fake.count(), 1);
    Ok(())
}

/// F01: a cached token is reused until less than the refresh margin is left, then
/// refreshed first.
#[tokio::test]
async fn refreshes_before_expiry() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let far = SystemTime::now() + Duration::from_secs(24 * 3600);
    let p = signed_in(
        &fake,
        token_set("initial", "r0", Duration::from_secs(3600), far),
    )
    .await?;
    jump(Duration::from_secs(3600 - 301)).await;
    assert_eq!(p.access_token().await?.expose_secret(), "initial");
    assert_eq!(fake.count(), 0);
    jump(Duration::from_secs(2)).await;
    assert_eq!(p.access_token().await?.expose_secret(), "fake-access-1");
    assert_eq!(fake.refresh_tokens_sent(), ["r0"]);
    let r = &fake.requests()[0];
    assert_eq!(r["grant_type"], "refresh_token");
    assert_eq!(r["scope"], "Files.ReadWrite.All offline_access User.Read");
    Ok(())
}

/// Review item 10: a token whose wall-clock deadline has passed counts as expired even if
/// the monotonic clock (which stops during suspend) says it is fresh.
#[tokio::test]
async fn wall_clock_expiry_forces_refresh() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let past = SystemTime::now()
        .checked_sub(Duration::from_secs(1))
        .ok_or("clock")?;
    let p = signed_in(
        &fake,
        token_set("stale", "r0", Duration::from_secs(3600), past),
    )
    .await?;
    assert_eq!(p.access_token().await?.expose_secret(), "fake-access-1");
    assert_eq!(fake.count(), 1);
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Concurrency
// ---------------------------------------------------------------------------------------

/// AC5: ten concurrent callers that all need a refresh cause exactly one refresh request,
/// and all get its token.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callers_share_one_refresh() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let gate = fake.hold();
    let provider = Arc::new(resume(fake.client(), seeded_store("a", "seed").await, "a"));

    let callers: Vec<_> = (0..10)
        .map(|_| {
            let p = provider.clone();
            tokio::spawn(async move { p.access_token().await })
        })
        .collect();
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

/// Review item 9: callers waiting on a refresh that fails transiently all share that
/// failure, and callers arriving just after it get it too, without a request each.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transient_failure_is_shared_not_repeated() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let gate = fake.hold();
    fake.error(503, "temporarily_unavailable");
    let provider = Arc::new(resume(fake.client(), seeded_store("a", "seed").await, "a"));
    let callers: Vec<_> = (0..10)
        .map(|_| {
            let p = provider.clone();
            tokio::spawn(async move { p.access_token().await })
        })
        .collect();
    while fake.count() == 0 {
        tokio::task::yield_now().await;
    }
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    gate.add_permits(100);
    for c in callers {
        let r = c.await?;
        assert!(matches!(r, Err(TokenError::Transient { .. })), "{r:?}");
    }
    let r = provider.access_token().await;
    assert!(matches!(r, Err(TokenError::Transient { .. })), "{r:?}");
    assert_eq!(fake.count(), 1, "one request for all of them");
    assert!(!provider.needs_sign_in());
    Ok(())
}

/// After a 401, `force_refresh` refreshes once even if several callers report the same
/// rejected token.
#[tokio::test]
async fn force_refresh_refreshes_once_per_rejected_token() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let provider = resume(fake.client(), seeded_store("a", "seed").await, "a");
    let first = provider.access_token().await?;
    let second = provider.force_refresh(&first).await?;
    assert_ne!(first, second);
    assert_eq!(provider.force_refresh(&first).await?, second);
    assert_eq!(fake.count(), 2);
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Cancellation safety (review item 1)
// ---------------------------------------------------------------------------------------

/// Review item 1: a caller dropped while the refresh request is in flight does not lose
/// the rolled refresh token; the next refresh uses it, not the old one.
#[tokio::test]
async fn caller_dropped_mid_refresh_keeps_rolled_token() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let gate = fake.hold();
    let store = FlakyStore::seeded("a", "old").await;
    let provider = resume(fake.client(), store.clone(), "a");

    let dropped = tokio::time::timeout(Duration::from_millis(300), provider.access_token()).await;
    assert!(
        dropped.is_err(),
        "the caller must have been dropped mid-refresh"
    );
    gate.add_permits(100);

    let t1 = provider.access_token().await?;
    assert_eq!(t1.expose_secret(), "fake-access-1");
    let t2 = provider.force_refresh(&t1).await?;
    assert_eq!(t2.expose_secret(), "fake-access-2");
    assert_eq!(fake.refresh_tokens_sent(), ["old", "fake-refresh-1"]);
    assert_eq!(store.stored("a").as_deref(), Some("fake-refresh-2"));
    Ok(())
}

/// Review item 1: a caller dropped while the rolled token is being stored does not lose
/// it, even when that write then fails; the next refresh uses it.
#[tokio::test]
async fn caller_dropped_mid_save_keeps_rolled_token() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = FlakyStore::seeded("a", "old").await;
    let write_gate = store.hold();
    store.set_mode(SaveMode::Fail);
    let provider = resume(fake.client(), store.clone(), "a");

    let dropped = tokio::time::timeout(Duration::from_millis(300), provider.access_token()).await;
    assert!(
        dropped.is_err(),
        "the caller must have been dropped mid-save"
    );
    assert!(settle(|| store.saves() == 1).await);
    write_gate.add_permits(100);

    let t1 = provider.access_token().await?;
    assert_eq!(t1.expose_secret(), "fake-access-1");
    let t2 = provider.force_refresh(&t1).await?;
    assert_eq!(t2.expose_secret(), "fake-access-2");
    assert_eq!(fake.refresh_tokens_sent(), ["old", "fake-refresh-1"]);
    assert_eq!(
        store.stored("a").as_deref(),
        Some("old"),
        "never a partial write"
    );
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Storing the rolled refresh token
// ---------------------------------------------------------------------------------------

/// AC6, ADR-0010 R8: a rolled refresh token replaces the stored one before the new access
/// token is handed out, and the next refresh uses it.
#[tokio::test]
async fn rolled_refresh_token_is_stored_before_hand_out() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = seeded_store("a", "seed").await;
    let provider = resume(fake.client(), store.clone(), "a");
    let t1 = provider.access_token().await?;
    assert_eq!(
        store
            .get(&account("a"))
            .as_ref()
            .map(RefreshToken::expose_secret),
        Some("fake-refresh-1")
    );
    provider.force_refresh(&t1).await?;
    assert_eq!(
        store
            .get(&account("a"))
            .as_ref()
            .map(RefreshToken::expose_secret),
        Some("fake-refresh-2")
    );
    assert_eq!(fake.refresh_tokens_sent(), ["seed", "fake-refresh-1"]);
    Ok(())
}

/// AC6 (as reworded), F01 step 9, ADR-0010 R8: if storing the rolled refresh token fails,
/// the session continues: the access token is handed out, the stored old token is left
/// untouched, the new token is kept in memory and used for later refreshes, a non-fatal
/// warning is set, no access token comes from a refresh that was not granted, and the next
/// successful write stores the newest token.
#[tokio::test]
async fn failed_store_write_keeps_session_and_newest_token() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = FlakyStore::seeded("a", "old").await;
    store.set_mode(SaveMode::Fail);
    let provider = resume(fake.client(), store.clone(), "a");

    let t1 = provider.access_token().await?;
    assert_eq!(t1.expose_secret(), "fake-access-1");
    assert!(!provider.needs_sign_in());
    assert!(matches!(
        provider.store_warning().map(|w| w.error),
        Some(StoreError::Failed { .. })
    ));
    assert_eq!(store.stored("a").as_deref(), Some("old"));

    // Within the retry backoff: the refresh uses the in-memory token, and no write is tried.
    let t2 = provider.force_refresh(&t1).await?;
    assert_eq!(t2.expose_secret(), "fake-access-2");
    assert_eq!(store.saves(), 1);
    assert_eq!(store.stored("a").as_deref(), Some("old"));

    // A refresh that is not granted hands out no access token.
    fake.error(503, "temporarily_unavailable");
    let r = provider.force_refresh(&t2).await;
    assert!(matches!(r, Err(TokenError::Transient { .. })), "{r:?}");

    // The store recovers; after the backoffs, the next refresh stores the newest token.
    store.set_mode(SaveMode::Succeed);
    jump(STORE_RETRY_INITIAL + Duration::from_secs(1)).await;
    let t4 = provider.force_refresh(&t2).await?;
    assert_eq!(t4.expose_secret(), "fake-access-3");
    assert!(provider.store_warning().is_none());
    assert_eq!(store.stored("a").as_deref(), Some("fake-refresh-3"));
    assert_eq!(
        fake.refresh_tokens_sent(),
        ["old", "fake-refresh-1", "fake-refresh-2", "fake-refresh-2"],
        "each refresh used the newest refresh token held"
    );
    Ok(())
}

/// F01 step 9, review item 2: an unsaved refresh token is retried in the background after
/// the backoff, triggered by an ordinary call that needs no refresh.
#[tokio::test]
async fn unsaved_refresh_token_is_retried_in_background() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = FlakyStore::seeded("a", "old").await;
    store.set_mode(SaveMode::Fail);
    let provider = resume(fake.client(), store.clone(), "a");
    let t = provider.access_token().await?;
    assert!(provider.store_warning().is_some());

    store.set_mode(SaveMode::Succeed);
    assert_eq!(provider.access_token().await?, t);
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert_eq!(store.saves(), 1, "no retry inside the backoff");

    jump(STORE_RETRY_INITIAL + Duration::from_secs(1)).await;
    assert_eq!(provider.access_token().await?, t);
    assert!(settle(|| store.stored("a").as_deref() == Some("fake-refresh-1")).await);
    assert!(provider.store_warning().is_none());
    assert_eq!(fake.count(), 1, "the retry made no refresh request");
    Ok(())
}

/// Review item 2: handing out a fresh cached token never waits for the keyring, even
/// while a background write is stuck.
#[tokio::test]
async fn fast_path_never_waits_for_keyring() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = FlakyStore::seeded("a", "old").await;
    store.set_mode(SaveMode::Fail);
    let provider = resume(fake.client(), store.clone(), "a");
    let t = provider.access_token().await?;

    store.set_mode(SaveMode::Succeed);
    let stuck = store.hold();
    jump(STORE_RETRY_INITIAL + Duration::from_secs(1)).await;
    for _ in 0..3 {
        let got = tokio::time::timeout(Duration::from_secs(1), provider.access_token()).await;
        assert_eq!(got.map_err(|_| "access_token waited for the keyring")??, t);
    }
    assert!(settle(|| store.saves() == 2).await, "the retry started");
    stuck.add_permits(10);
    assert!(settle(|| store.stored("a").as_deref() == Some("fake-refresh-1")).await);
    Ok(())
}

/// Review item 2: after the user dismisses the keyring prompt, the write is not retried
/// for a long interval (no repeated prompts); the dismissal stays visible as the warning.
#[tokio::test]
async fn dismissed_prompt_is_not_retried_soon() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = FlakyStore::seeded("a", "old").await;
    store.set_mode(SaveMode::Dismiss);
    let provider = resume(fake.client(), store.clone(), "a");
    let t = provider.access_token().await?;
    assert!(matches!(
        provider.store_warning().map(|w| w.error),
        Some(StoreError::Dismissed { .. })
    ));
    assert_eq!(store.saves(), 1);

    store.set_mode(SaveMode::Succeed);
    // Well past every ordinary backoff, while the access token is still fresh.
    jump(Duration::from_secs(20 * 60)).await;
    assert_eq!(provider.access_token().await?, t);
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert_eq!(store.saves(), 1, "no prompt again so soon");
    assert!(matches!(
        provider.store_warning().map(|w| w.error),
        Some(StoreError::Dismissed { .. })
    ));

    // After the long interval (the token is also due by then), the write happens.
    jump(Duration::from_secs(41 * 60)).await;
    let t2 = provider.access_token().await?;
    assert_ne!(t2, t);
    assert_eq!(store.saves(), 2);
    assert_eq!(store.stored("a").as_deref(), Some("fake-refresh-2"));
    assert!(provider.store_warning().is_none());
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Needs sign-in versus transient
// ---------------------------------------------------------------------------------------

/// AC7, ADR-0010 R9: `invalid_grant` makes the account "needs sign-in", after which it
/// makes no more requests.
#[tokio::test]
async fn invalid_grant_means_needs_sign_in() -> TestResult {
    let fake = FakeAuthority::start().await?;
    fake.error(400, "invalid_grant");
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

/// `interaction_required` also means "needs sign-in" (accepted by the lead).
#[tokio::test]
async fn interaction_required_means_needs_sign_in() -> TestResult {
    let fake = FakeAuthority::start().await?;
    fake.error(400, "interaction_required");
    let provider = resume(fake.client(), seeded_store("a", "seed").await, "a");
    assert!(provider.access_token().await.is_err());
    assert!(provider.needs_sign_in());
    Ok(())
}

/// AC7: a network failure, a 503 and a 429 are transient, not "needs sign-in"; after the
/// failure backoff the next call refreshes again and succeeds.
#[tokio::test]
async fn transient_failures_are_not_needs_sign_in() -> TestResult {
    let dead = TokenClient::new(&dead_authority().await)?;
    let provider = resume(dead, seeded_store("a", "seed").await, "a");
    let r = provider.access_token().await;
    assert!(matches!(r, Err(TokenError::Transient { .. })), "{r:?}");
    assert!(!provider.needs_sign_in());

    for (status, error) in [(503, "temporarily_unavailable"), (429, "too_many_requests")] {
        let fake = FakeAuthority::start().await?;
        fake.error(status, error);
        let provider = resume(fake.client(), seeded_store("a", "seed").await, "a");
        let r = provider.access_token().await;
        assert!(matches!(r, Err(TokenError::Transient { .. })), "{r:?}");
        assert!(!provider.needs_sign_in());
        jump(mydrive_graph::auth::REFRESH_RETRY_INITIAL + Duration::from_secs(1)).await;
        assert_eq!(
            provider.access_token().await?.expose_secret(),
            "fake-access-1"
        );
    }
    Ok(())
}

/// Other OAuth errors are `Rejected`: neither transient nor "needs sign-in".
#[tokio::test]
async fn other_oauth_errors_are_rejected() -> TestResult {
    let fake = FakeAuthority::start().await?;
    fake.error(400, "invalid_client");
    let provider = resume(fake.client(), seeded_store("a", "seed").await, "a");
    let r = provider.access_token().await;
    assert!(matches!(r, Err(TokenError::Rejected { .. })), "{r:?}");
    assert!(!provider.needs_sign_in());
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
    fake.error(400, "invalid_grant");
    let a = resume(fake.client(), store.clone(), "a");
    let b = resume(fake.client(), store, "b");
    assert!(a.access_token().await.is_err());
    assert!(a.needs_sign_in());
    assert!(b.access_token().await.is_ok());
    assert!(!b.needs_sign_in());
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Liveness (second review)
// ---------------------------------------------------------------------------------------

/// Second review item 1: a panic in the store while a refresh reads the token does not
/// wedge the provider; once the failure backoff passes, the next call refreshes.
#[tokio::test]
async fn panic_in_refresh_task_is_recovered() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = FlakyStore::seeded("a", "old").await;
    store.set_load_mode(LoadMode::PanicOnce);
    let provider = resume(fake.client(), store.clone(), "a");
    let r = provider.access_token().await;
    assert!(matches!(r, Err(TokenError::RefreshAborted { .. })), "{r:?}");
    jump(mydrive_graph::auth::REFRESH_RETRY_INITIAL + Duration::from_secs(1)).await;
    assert_eq!(
        provider.access_token().await?.expose_secret(),
        "fake-access-1"
    );
    Ok(())
}

/// Second review item 1: a panic in the store while writing does not leave the writer
/// marked as running: a warning appears, with a retry time, and a retry succeeds.
#[tokio::test]
async fn panic_in_writer_is_recovered() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = FlakyStore::seeded("a", "old").await;
    store.set_mode(SaveMode::PanicOnce);
    let provider = resume(fake.client(), store.clone(), "a");
    provider.access_token().await?;
    let w = provider.store_warning().ok_or("expected a warning")?;
    assert!(w.retry_in.is_some(), "{w:?}");
    assert_eq!(store.stored("a").as_deref(), Some("old"));
    provider.retry_store_now();
    assert!(settle(|| store.stored("a").as_deref() == Some("fake-refresh-1")).await);
    assert!(provider.store_warning().is_none());
    Ok(())
}

/// Second review item 3: an early refresh (inside the margin) that fails transiently hands
/// out the still-valid cached token; once it has expired, the failure is returned.
#[tokio::test]
async fn early_refresh_failure_keeps_valid_cached_token() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let far = SystemTime::now() + Duration::from_secs(24 * 3600);
    let p = signed_in(
        &fake,
        token_set("initial", "r0", Duration::from_secs(3600), far),
    )
    .await?;
    jump(Duration::from_secs(3600 - 200)).await;
    fake.error(503, "temporarily_unavailable");
    assert_eq!(p.access_token().await?.expose_secret(), "initial");
    assert_eq!(p.access_token().await?.expose_secret(), "initial");
    assert_eq!(fake.count(), 1, "the failure is reused, not repeated");
    jump(Duration::from_secs(300)).await;
    fake.error(503, "temporarily_unavailable");
    let r = p.access_token().await;
    assert!(matches!(r, Err(TokenError::Transient { .. })), "{r:?}");
    Ok(())
}

/// Second review item 4: `reset_backoff` ends the failure backoff, and `force_refresh`
/// bypasses it.
#[tokio::test]
async fn reset_backoff_and_force_refresh_bypass_the_failure_cache() -> TestResult {
    let fake = FakeAuthority::start().await?;
    fake.error(503, "temporarily_unavailable");
    let provider = resume(fake.client(), seeded_store("a", "seed").await, "a");
    assert!(provider.access_token().await.is_err());
    assert!(provider.access_token().await.is_err());
    assert_eq!(fake.count(), 1);
    provider.reset_backoff();
    assert_eq!(
        provider.access_token().await?.expose_secret(),
        "fake-access-1"
    );
    assert_eq!(fake.count(), 2);

    let fake = FakeAuthority::start().await?;
    fake.error(503, "temporarily_unavailable");
    let provider = resume(fake.client(), seeded_store("a", "seed").await, "a");
    assert!(provider.access_token().await.is_err());
    let t = provider
        .force_refresh(&AccessToken::new("rejected"))
        .await?;
    assert_eq!(t.expose_secret(), "fake-access-1");
    assert_eq!(fake.count(), 2);
    Ok(())
}

/// Second review item 5: after `close`, a write the old provider had started never lands,
/// so it cannot overwrite a new sign-in's token; the closed provider hands out nothing.
#[tokio::test]
async fn closed_provider_never_writes_again() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = FlakyStore::seeded("a", "old").await;
    let gate = store.hold();
    let old = resume(fake.client(), store.clone(), "a");
    // The refresh is granted; its write waits (as on a keyring prompt).
    let dropped = tokio::time::timeout(Duration::from_millis(300), old.access_token()).await;
    assert!(dropped.is_err());
    assert!(settle(|| store.saves() == 1).await);

    old.close().await;
    // The new sign-in's token lands in the store.
    store
        .inner
        .save(&account("a"), &RefreshToken::new("new-chain"))
        .await?;
    gate.add_permits(100);
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    assert_eq!(store.stored("a").as_deref(), Some("new-chain"));
    let r = old.access_token().await;
    assert!(matches!(r, Err(TokenError::Closed { .. })), "{r:?}");
    old.retry_store_now();
    old.flush().await?;
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    assert_eq!(store.stored("a").as_deref(), Some("new-chain"));
    assert_eq!(store.saves(), 1, "no write after close");
    Ok(())
}

/// Second review item 6: `flush` writes an unsaved token at once, ignoring the backoff,
/// and the warning reports when the next automatic retry is due.
#[tokio::test]
async fn flush_writes_unsaved_token_now() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = FlakyStore::seeded("a", "old").await;
    store.set_mode(SaveMode::Fail);
    let provider = resume(fake.client(), store.clone(), "a");
    provider.access_token().await?;
    let w = provider.store_warning().ok_or("expected a warning")?;
    let retry_in = w.retry_in.ok_or("expected a retry time")?;
    assert!(
        retry_in <= STORE_RETRY_INITIAL && retry_in > STORE_RETRY_INITIAL / 2,
        "{retry_in:?}"
    );

    let still_failing = provider.flush().await;
    assert!(
        matches!(still_failing, Err(StoreError::Failed { .. })),
        "{still_failing:?}"
    );
    store.set_mode(SaveMode::Succeed);
    provider.flush().await?;
    assert_eq!(store.stored("a").as_deref(), Some("fake-refresh-1"));
    assert!(provider.store_warning().is_none());
    assert_eq!(store.saves(), 3);
    Ok(())
}

/// Second review item 6: `retry_store_now` retries at once, even inside the long pause
/// after a dismissed prompt.
#[tokio::test]
async fn retry_store_now_ignores_the_pause() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = FlakyStore::seeded("a", "old").await;
    store.set_mode(SaveMode::Dismiss);
    let provider = resume(fake.client(), store.clone(), "a");
    provider.access_token().await?;
    let w = provider.store_warning().ok_or("expected a warning")?;
    assert!(matches!(w.error, StoreError::Dismissed { .. }));
    assert!(w.retry_in.ok_or("expected a retry time")? > Duration::from_secs(50 * 60));
    store.set_mode(SaveMode::Succeed);
    provider.retry_store_now();
    assert!(settle(|| store.stored("a").as_deref() == Some("fake-refresh-1")).await);
    Ok(())
}

/// Second review item 7: a dismissed prompt while reading the refresh token is not asked
/// again for a long time, not even by `force_refresh`; `reset_backoff` (a user action)
/// tries again.
#[tokio::test]
async fn dismissed_read_is_not_retried_soon() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = FlakyStore::seeded("a", "old").await;
    store.set_load_mode(LoadMode::Dismiss);
    let provider = resume(fake.client(), store.clone(), "a");
    let r = provider.access_token().await;
    assert!(
        matches!(
            r,
            Err(TokenError::Store {
                source: StoreError::Dismissed { .. },
                ..
            })
        ),
        "{r:?}"
    );
    assert_eq!(store.loads(), 1);
    jump(Duration::from_secs(20 * 60)).await;
    assert!(provider.access_token().await.is_err());
    assert!(
        provider
            .force_refresh(&AccessToken::new("x"))
            .await
            .is_err()
    );
    assert_eq!(store.loads(), 1, "no new prompt so soon");

    store.set_load_mode(LoadMode::Normal);
    provider.reset_backoff();
    assert_eq!(
        provider.access_token().await?.expose_secret(),
        "fake-access-1"
    );
    assert_eq!(store.loads(), 2);
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Final verification gaps
// ---------------------------------------------------------------------------------------

/// Review item 10 on the `force_refresh` path: after a 401 on another token, a cached
/// token whose wall-clock deadline has passed is not handed back; a refresh happens.
#[tokio::test]
async fn force_refresh_does_not_return_wall_clock_expired_token() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let past = SystemTime::now()
        .checked_sub(Duration::from_secs(1))
        .ok_or("clock")?;
    let p = signed_in(
        &fake,
        token_set("stale", "r0", Duration::from_secs(3600), past),
    )
    .await?;
    let t = p
        .force_refresh(&AccessToken::new("some-older-token"))
        .await?;
    assert_eq!(t.expose_secret(), "fake-access-1");
    assert_eq!(fake.count(), 1);
    Ok(())
}

/// Review items 3 and 10: when an early refresh fails transiently, a cached token whose
/// wall-clock deadline has passed is not handed out instead.
#[tokio::test]
async fn transient_failure_does_not_return_wall_clock_expired_token() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let past = SystemTime::now()
        .checked_sub(Duration::from_secs(1))
        .ok_or("clock")?;
    let p = signed_in(
        &fake,
        token_set("stale", "r0", Duration::from_secs(3600), past),
    )
    .await?;
    fake.error(503, "temporarily_unavailable");
    let r = p.access_token().await;
    assert!(matches!(r, Err(TokenError::Transient { .. })), "{r:?}");
    let r = p.access_token().await;
    assert!(matches!(r, Err(TokenError::Transient { .. })), "{r:?}");
    Ok(())
}

/// AC6, ADR-0010 R8: while the write of the rolled refresh token is in progress (within
/// the documented 10-second wait), the new access token is not handed out; it is handed
/// out once the store holds the new refresh token.
#[tokio::test]
async fn access_token_waits_for_the_store_write() -> TestResult {
    let fake = FakeAuthority::start().await?;
    let store = FlakyStore::seeded("a", "old").await;
    let gate = store.hold();
    let provider = Arc::new(resume(fake.client(), store.clone(), "a"));
    let caller = {
        let p = provider.clone();
        tokio::spawn(async move { p.access_token().await })
    };
    assert!(settle(|| store.saves() == 1).await, "the write started");
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    assert!(
        !caller.is_finished(),
        "the access token was handed out before the refresh token was stored"
    );
    assert_eq!(store.stored("a").as_deref(), Some("old"));
    gate.add_permits(1);
    let t = caller.await??;
    assert_eq!(t.expose_secret(), "fake-access-1");
    assert_eq!(store.stored("a").as_deref(), Some("fake-refresh-1"));
    Ok(())
}
