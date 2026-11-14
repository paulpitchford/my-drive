//! `SecretServiceStore` against the fake Secret Service in `auth_fake_secret_service.rs`.
//! No session bus and no real keyring is involved.

#[path = "auth_fake_secret_service.rs"]
mod fake_secret_service;
#[path = "auth_support.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use fake_secret_service::{
    Keyring, LOGIN, OTHER, PromptMode, StoredItem, account, eventually, fake_service, lock,
    our_attributes, our_item,
};
use mydrive_graph::auth::{
    PROMPT_TIMEOUT, RefreshToken, RefreshingTokenProvider, STORE_RETRY_AFTER_DISMISSED, Scopes,
    SecretServiceStore, StoreError, TokenProvider, TokenStore,
};

use support::{TestResult, bounded};

/// ADR-0010 R6, review item 11: tokens round-trip through the default collection with a
/// schema attribute, and saving replaces the account's one entry.
#[tokio::test]
async fn round_trip_in_default_collection_with_schema() -> TestResult {
    bounded(async {
        let (conn, keyring) = fake_service(Keyring {
            default_alias: Some(LOGIN.into()),
            ..Keyring::default()
        })
        .await?;
        let store = SecretServiceStore::with_connection(conn).await?;
        assert!(store.load(&account()).await?.is_none());
        store
            .save(&account(), &RefreshToken::new("dummy-1"))
            .await?;
        store
            .save(&account(), &RefreshToken::new("dummy-2"))
            .await?;
        assert_eq!(
            store
                .load(&account())
                .await?
                .as_ref()
                .map(RefreshToken::expose_secret),
            Some("dummy-2")
        );
        {
            let k = lock(&keyring);
            assert_eq!(k.items.len(), 1, "one entry per account");
            assert_eq!(k.items[0].collection, LOGIN);
            assert_eq!(k.items[0].attributes, our_attributes());
        }
        store.delete(&account()).await?;
        assert!(store.load(&account()).await?.is_none());
        Ok(())
    })
    .await
}

/// Review item 11: an item outside the default collection, or one from another
/// application without our schema attribute, is never read as our token.
#[tokio::test]
async fn ignores_other_collections_and_other_schemas() -> TestResult {
    bounded(async {
        let mut other_app = our_attributes();
        other_app.remove("xdg:schema");
        let (conn, _) = fake_service(Keyring {
            default_alias: Some(LOGIN.into()),
            items: vec![
                StoredItem {
                    path: format!("{OTHER}/1"),
                    collection: OTHER.into(),
                    attributes: our_attributes(),
                    value: b"from-other-collection".to_vec(),
                },
                StoredItem {
                    path: format!("{LOGIN}/2"),
                    collection: LOGIN.into(),
                    attributes: other_app,
                    value: b"from-other-application".to_vec(),
                },
            ],
            next: 10,
            ..Keyring::default()
        })
        .await?;
        let store = SecretServiceStore::with_connection(conn).await?;
        assert!(store.load(&account()).await?.is_none());
        Ok(())
    })
    .await
}

/// Review item 11, ADR-0010 R7: no default collection is an error naming `mydrive doctor`.
#[tokio::test]
async fn no_default_collection_names_doctor() -> TestResult {
    bounded(async {
        let (conn, _) = fake_service(Keyring::default()).await?;
        let store = SecretServiceStore::with_connection(conn).await?;
        let e = store
            .save(&account(), &RefreshToken::new("dummy"))
            .await
            .err()
            .ok_or("save must fail")?;
        assert!(matches!(e, StoreError::Unavailable { .. }), "{e:?}");
        assert!(e.to_string().contains("mydrive doctor"), "{e}");
        let e = store.load(&account()).await.err().ok_or("load must fail")?;
        assert!(e.to_string().contains("mydrive doctor"), "{e}");
        Ok(())
    })
    .await
}

/// Review item 11, ADR-0010 R7: when the service cannot create the item, the error names
/// `mydrive doctor`.
#[tokio::test]
async fn create_failure_names_doctor() -> TestResult {
    bounded(async {
        let (conn, _) = fake_service(Keyring {
            default_alias: Some(LOGIN.into()),
            fail_create: true,
            ..Keyring::default()
        })
        .await?;
        let store = SecretServiceStore::with_connection(conn).await?;
        let e = store
            .save(&account(), &RefreshToken::new("dummy"))
            .await
            .err()
            .ok_or("save must fail")?;
        let shown = e.to_string();
        assert!(shown.contains("mydrive doctor"), "{shown}");
        assert!(!shown.contains("dummy"), "{shown}");
        Ok(())
    })
    .await
}

/// Review item 9 (J01 V4): a write that needs a prompt completes when the user accepts.
#[tokio::test]
async fn prompt_that_completes_stores_the_token() -> TestResult {
    bounded(async {
        let (conn, keyring) = fake_service(Keyring {
            default_alias: Some(LOGIN.into()),
            prompt_on_create: true,
            ..Keyring::default()
        })
        .await?;
        let store = SecretServiceStore::with_connection(conn).await?;
        store
            .save(&account(), &RefreshToken::new("dummy-1"))
            .await?;
        assert_eq!(lock(&keyring).prompts_shown, 1);
        assert_eq!(
            store
                .load(&account())
                .await?
                .as_ref()
                .map(RefreshToken::expose_secret),
            Some("dummy-1")
        );
        Ok(())
    })
    .await
}

/// Review item 9 (J01 V4): a locked keyring is unlocked through its prompt on read.
#[tokio::test]
async fn locked_item_is_unlocked_on_read() -> TestResult {
    bounded(async {
        let (conn, keyring) = fake_service(Keyring {
            default_alias: Some(LOGIN.into()),
            locked: true,
            items: vec![our_item("dummy-locked")],
            next: 10,
            ..Keyring::default()
        })
        .await?;
        let store = SecretServiceStore::with_connection(conn).await?;
        assert_eq!(
            store
                .load(&account())
                .await?
                .as_ref()
                .map(RefreshToken::expose_secret),
            Some("dummy-locked")
        );
        assert_eq!(lock(&keyring).prompts_shown, 1);
        Ok(())
    })
    .await
}

/// Review item 9: a dismissed prompt is `StoreError::Dismissed`, naming `mydrive doctor`.
#[tokio::test]
async fn dismissed_prompt_is_dismissed_error() -> TestResult {
    bounded(async {
        let (conn, _) = fake_service(Keyring {
            default_alias: Some(LOGIN.into()),
            prompt_on_create: true,
            prompt_mode: PromptMode::Dismiss,
            ..Keyring::default()
        })
        .await?;
        let store = SecretServiceStore::with_connection(conn).await?;
        let e = store
            .save(&account(), &RefreshToken::new("dummy"))
            .await
            .err()
            .ok_or("save must fail")?;
        assert!(matches!(e, StoreError::Dismissed { .. }), "{e:?}");
        assert!(e.to_string().contains("mydrive doctor"), "{e}");
        Ok(())
    })
    .await
}

/// Review item 9, WP-013: an unanswered prompt times out after the configured prompt
/// timeout, not the default, with an error naming `mydrive doctor`, and is dismissed so it
/// cannot complete the write later.
///
/// The tokio clock is paused and moves only when nothing else can run, so the wait is
/// measured exactly and takes no real time: the save must give up after the configured
/// timeout, neither before it nor at the 5-minute default.
#[tokio::test(start_paused = true)]
async fn unanswered_prompt_times_out_and_is_dismissed() -> TestResult {
    bounded(async {
        let configured = Duration::from_secs(7);
        assert!(configured < PROMPT_TIMEOUT);
        let (conn, keyring) = fake_service(Keyring {
            default_alias: Some(LOGIN.into()),
            prompt_on_create: true,
            prompt_mode: PromptMode::Never,
            ..Keyring::default()
        })
        .await?;
        let store = SecretServiceStore::with_connection(conn)
            .await?
            .with_prompt_timeout(configured);
        let started = tokio::time::Instant::now();
        let e = store
            .save(&account(), &RefreshToken::new("dummy"))
            .await
            .err()
            .ok_or("save must fail")?;
        let waited = started.elapsed();
        assert!(
            waited >= configured && waited < configured + Duration::from_secs(1),
            "the save gave up after {waited:?}; the configured prompt timeout is {configured:?}"
        );
        assert_eq!(lock(&keyring).prompts_shown, 1);
        assert!(matches!(e, StoreError::Failed { .. }), "{e:?}");
        assert!(e.to_string().contains("not answered"), "{e}");
        assert!(e.to_string().contains("mydrive doctor"), "{e}");
        assert!(eventually(&keyring, |k| k.dismiss_calls == 1).await);
        assert!(lock(&keyring).items.is_empty());
        Ok(())
    })
    .await
}

/// Review items 5 and 9: a write abandoned while its prompt is showing (for example by
/// `RefreshingTokenProvider::close`) dismisses the prompt.
#[tokio::test]
async fn abandoned_prompt_is_dismissed() -> TestResult {
    bounded(async {
        let (conn, keyring) = fake_service(Keyring {
            default_alias: Some(LOGIN.into()),
            prompt_on_create: true,
            prompt_mode: PromptMode::Never,
            ..Keyring::default()
        })
        .await?;
        let store = SecretServiceStore::with_connection(conn).await?;
        let token = RefreshToken::new("dummy");
        let abandoned =
            tokio::time::timeout(Duration::from_millis(300), store.save(&account(), &token)).await;
        assert!(abandoned.is_err());
        assert!(eventually(&keyring, |k| k.dismiss_calls == 1).await);
        assert!(lock(&keyring).items.is_empty());
        Ok(())
    })
    .await
}

/// Review items 7 and 9: a dismissed prompt on write, through the provider, gives a
/// `Dismissed` warning and no new prompt for a long time.
#[tokio::test]
async fn dismissed_write_prompt_pauses_the_provider() -> TestResult {
    bounded(async {
        let fake = support::FakeAuthority::start().await?;
        let (conn, keyring) = fake_service(Keyring {
            default_alias: Some(LOGIN.into()),
            prompt_on_create: true,
            prompt_mode: PromptMode::Dismiss,
            items: vec![our_item("seed")],
            next: 10,
            ..Keyring::default()
        })
        .await?;
        let store = Arc::new(SecretServiceStore::with_connection(conn).await?);
        let provider = RefreshingTokenProvider::resume(
            account(),
            Scopes::initial_sign_in(),
            fake.client(),
            store,
        );
        let t = provider.access_token().await?;
        let w = provider.store_warning().ok_or("expected a warning")?;
        assert!(matches!(w.error, StoreError::Dismissed { .. }), "{w:?}");
        let retry_in = w.retry_in.ok_or("expected a retry time")?;
        assert!(retry_in > STORE_RETRY_AFTER_DISMISSED - Duration::from_secs(60));
        assert_eq!(lock(&keyring).prompts_shown, 1);

        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(20 * 60)).await;
        tokio::time::resume();
        assert_eq!(provider.access_token().await?, t);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(lock(&keyring).prompts_shown, 1, "no new prompt so soon");
        assert_eq!(lock(&keyring).create_calls, 1);
        Ok(())
    })
    .await
}
