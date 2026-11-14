//! Accounts, mounts and delta tokens.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Fixture, MOUNT, ROOT, account, mount, mount_id};
use mydrive_core::ids::{AccountId, MountId, TenantId};
use mydrive_store::{Account, Queries, StoreError};

#[test]
fn accounts_round_trip_and_upsert() {
    let fx = Fixture::new();
    let store = fx.open();
    store.upsert_account(&account()).unwrap();
    assert_eq!(
        store.account(&account().account_id).unwrap(),
        Some(account())
    );
    let work = Account {
        tenant_id: Some(TenantId::new("tenant-1").unwrap()),
        ..account()
    };
    store.upsert_account(&work).unwrap();
    assert_eq!(store.account(&account().account_id).unwrap(), Some(work));
    assert_eq!(
        store.account(&AccountId::new("nobody").unwrap()).unwrap(),
        None
    );
}

#[test]
fn mounts_round_trip() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut second = mount("m2", "folder-x", "/home/u/Org/Site - Docs");
    second.read_only = true;
    second.delta_token = Some("initial".to_owned());
    store.insert_mount(&second).unwrap();

    assert_eq!(
        store.mount(&MountId::new("m2").unwrap()).unwrap(),
        Some(second.clone())
    );
    let all = store.mounts().unwrap();
    assert_eq!(all, [mount(MOUNT, ROOT, "/home/u/OneDrive"), second]);
    assert_eq!(store.mount(&MountId::new("absent").unwrap()).unwrap(), None);
}

/// ADR-0007 R2: each mount has exactly one delta token, stored against its `mount_id`.
#[test]
fn adr_0007_r2_delta_token_is_per_mount() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store
        .insert_mount(&mount("m2", "other-root", "/home/u/B"))
        .unwrap();
    let m2 = MountId::new("m2").unwrap();

    assert_eq!(store.delta_token(&mount_id()).unwrap(), None);
    store.set_delta_token(&mount_id(), Some("t-a")).unwrap();
    store.set_delta_token(&m2, Some("t-b")).unwrap();
    store.set_delta_token(&mount_id(), Some("t-a2")).unwrap();
    assert_eq!(
        store.delta_token(&mount_id()).unwrap().as_deref(),
        Some("t-a2")
    );
    assert_eq!(store.delta_token(&m2).unwrap().as_deref(), Some("t-b"));
    store.set_delta_token(&m2, None).unwrap();
    assert_eq!(store.delta_token(&m2).unwrap(), None);

    let absent = MountId::new("absent").unwrap();
    assert!(matches!(
        store.delta_token(&absent).unwrap_err(),
        StoreError::MountNotFound { .. }
    ));
    assert!(matches!(
        store.set_delta_token(&absent, Some("t")).unwrap_err(),
        StoreError::MountNotFound { .. }
    ));
}

/// ADR-0007 R4: a mount whose root (`drive_id`, `item_id`) equals an existing mount's root
/// is refused with an error naming the existing mount.
#[test]
fn adr_0007_r4_same_root_cannot_be_mounted_twice() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let err = store
        .insert_mount(&mount("m2", ROOT, "/home/u/Elsewhere"))
        .unwrap_err();
    assert!(
        matches!(&err, StoreError::MountRootTaken { existing, .. } if existing.as_str() == MOUNT),
        "{err}"
    );
    assert!(err.to_string().contains(MOUNT), "{err}");
    assert_eq!(store.mounts().unwrap().len(), 1);
}

/// Lead ruling 6: `Mount`'s `Debug` never shows the delta token.
#[test]
fn mount_debug_redacts_the_delta_token() {
    let mut m = mount(MOUNT, ROOT, "/home/u/OneDrive");
    m.delta_token = Some("secret-token-value".to_owned());
    let shown = format!("{m:?}");
    assert!(!shown.contains("secret-token-value"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");
    assert!(shown.contains(MOUNT), "{shown}");
}

#[test]
fn mount_insert_refusals() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let err = store.insert_mount(&mount(MOUNT, "x", "/p")).unwrap_err();
    assert!(matches!(err, StoreError::MountExists { .. }), "{err}");

    let mut orphan = mount("m3", "y", "/q");
    orphan.account_id = AccountId::new("nobody").unwrap();
    let err = store.insert_mount(&orphan).unwrap_err();
    assert!(matches!(err, StoreError::AccountNotFound { .. }), "{err}");
}

#[test]
fn blocked_reason_is_set_and_cleared() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store
        .set_mount_blocked_reason(&mount_id(), Some("unmanaged device policy"))
        .unwrap();
    assert_eq!(
        store
            .mount(&mount_id())
            .unwrap()
            .unwrap()
            .blocked_reason
            .as_deref(),
        Some("unmanaged device policy")
    );
    store.set_mount_blocked_reason(&mount_id(), None).unwrap();
    assert_eq!(
        store.mount(&mount_id()).unwrap().unwrap().blocked_reason,
        None
    );
}
