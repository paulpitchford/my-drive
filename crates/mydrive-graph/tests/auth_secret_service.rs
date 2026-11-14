//! The one test that uses a real Secret Service (WP-009 permits exactly one ignored test).
//!
//! It is excluded from `cargo test` because CI has no keyring. Run it by hand, in a desktop
//! session with an unlocked keyring (gnome-keyring or KWallet), with:
//!
//! ```text
//! cargo test -p mydrive-graph --test auth_secret_service -- --ignored
//! ```
//!
//! It writes, reads, replaces and deletes one item labelled
//! `my-drive refresh token (mydrive-keyring-self-test)` holding a dummy value, and leaves
//! nothing behind when it passes. The keyring may show its unlock prompt.

use mydrive_core::ids::AccountId;
use mydrive_graph::auth::{RefreshToken, SecretServiceStore, TokenStore};

/// ADR-0010 R6: tokens round-trip through the Secret Service, one entry per account.
#[tokio::test]
#[ignore = "needs a real Secret Service; run by hand with --ignored"]
async fn secret_service_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let store = SecretServiceStore::connect().await?;
    let account = AccountId::new("mydrive-keyring-self-test")?;

    store.delete(&account).await?;
    assert!(store.load(&account).await?.is_none());

    store
        .save(&account, &RefreshToken::new("dummy-value-1"))
        .await?;
    let loaded = store.load(&account).await?;
    assert_eq!(
        loaded.as_ref().map(RefreshToken::expose_secret),
        Some("dummy-value-1")
    );

    store
        .save(&account, &RefreshToken::new("dummy-value-2"))
        .await?;
    let loaded = store.load(&account).await?;
    assert_eq!(
        loaded.as_ref().map(RefreshToken::expose_secret),
        Some("dummy-value-2"),
        "saving replaces the account's entry"
    );

    store.delete(&account).await?;
    assert!(store.load(&account).await?.is_none());
    Ok(())
}
