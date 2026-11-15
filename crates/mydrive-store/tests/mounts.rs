//! Accounts, mounts, their constraints and the derived mount state.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Fixture, MOUNT, ROOT, account, finish_run_with_token, mount, mount_id, raw};
use mydrive_core::ids::{AccountId, MountId, TenantId};
use mydrive_core::names::AccountKind;
use mydrive_store::{
    Account, AccountState, BlockedReason, DeltaRunKind, DriveType, MountAccess, MountBlock,
    MountPause, MountRule, MountState, MountStateFacts, PageLink, Queries, ReadOnlyReason,
    StoreError, Timestamp, derive_mount_state,
};

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
        kind: AccountKind::Work,
        username: Some("user@example.invalid".to_owned()),
        display_name: Some("Example User".to_owned()),
        org_name: Some("Example Org".to_owned()),
        needs_sign_in: true,
        refuses_hash_and_percent: true,
        ..account()
    };
    store.upsert_account(&work).unwrap();
    assert_eq!(store.account(&account().account_id).unwrap(), Some(work));
    assert_eq!(
        store.account(&AccountId::new("nobody").unwrap()).unwrap(),
        None
    );
}

/// WP-017 "Accounts": the listing is in byte order of account ID, and the two flags have
/// their own setters; the state is derived from the sign-in flag.
#[test]
fn accounts_are_listed_in_byte_order_and_flags_are_set() {
    let fx = Fixture::new();
    let store = fx.open();
    for id in ["b", "B", "a", "é", "z"] {
        store
            .upsert_account(&Account {
                account_id: AccountId::new(id).unwrap(),
                ..account()
            })
            .unwrap();
    }
    let ids: Vec<String> = store
        .accounts()
        .unwrap()
        .into_iter()
        .map(|a| a.account_id.as_str().to_owned())
        .collect();
    assert_eq!(ids, ["B", "a", "b", "z", "é"]);

    let a = AccountId::new("a").unwrap();
    store.set_account_needs_sign_in(&a, true).unwrap();
    store
        .set_account_refuses_hash_and_percent(&a, true)
        .unwrap();
    let got = store.account(&a).unwrap().unwrap();
    assert!(got.needs_sign_in && got.refuses_hash_and_percent);
    assert_eq!(got.state(), AccountState::NeedsSignIn);
    store.set_account_needs_sign_in(&a, false).unwrap();
    assert_eq!(
        store.account(&a).unwrap().unwrap().state(),
        AccountState::SignedIn
    );

    let nobody = AccountId::new("nobody").unwrap();
    assert!(matches!(
        store.set_account_needs_sign_in(&nobody, true).unwrap_err(),
        StoreError::AccountNotFound { .. }
    ));
    let empty = Account {
        display_name: Some(String::new()),
        ..account()
    };
    assert!(matches!(
        store.upsert_account(&empty).unwrap_err(),
        StoreError::InvalidAccount { .. }
    ));
}

#[test]
fn mounts_round_trip() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut second = mount("m2", "folder-x", "/home/u/Org/Site - Docs");
    second.drive_type = Some(DriveType::DocumentLibrary);
    second.display_name = Some("Docs".to_owned());
    second.read_only = true;
    second.read_only_reason = Some(ReadOnlyReason::LibraryReadOnly);
    second.paused = true;
    second.paused_until = Some(Timestamp::from_unix_seconds(1_800_000_000));
    second.blocked_reason = Some(BlockedReason::OrganisationPolicy);
    second.blocked_message = Some("Unmanaged device".to_owned());
    second.delta_run = Some(DeltaRunKind::Initial);
    store.insert_mount(&second).unwrap();

    assert_eq!(
        store.mount(&MountId::new("m2").unwrap()).unwrap(),
        Some(second.clone())
    );
    let all = store.mounts().unwrap();
    assert_eq!(all, [mount(MOUNT, ROOT, "/home/u/OneDrive"), second]);
    assert_eq!(store.mount(&MountId::new("absent").unwrap()).unwrap(), None);
}

/// ADR-0007 R2: each mount has exactly one live delta token, stored against its
/// `mount_id`; finishing a run on one mount never changes another's.
#[test]
fn adr_0007_r2_delta_token_is_per_mount() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store
        .insert_mount(&mount("m2", "other-root", "/home/u/B"))
        .unwrap();
    let m2 = MountId::new("m2").unwrap();

    assert_eq!(store.delta_token(&mount_id()).unwrap(), None);
    finish_run_with_token(&store, &mount_id(), "t-a");
    finish_run_with_token(&store, &m2, "t-b");
    finish_run_with_token(&store, &mount_id(), "t-a2");
    assert_eq!(
        store.delta_token(&mount_id()).unwrap().as_deref(),
        Some("t-a2")
    );
    assert_eq!(store.delta_token(&m2).unwrap().as_deref(), Some("t-b"));

    let absent = MountId::new("absent").unwrap();
    assert!(matches!(
        store.delta_token(&absent).unwrap_err(),
        StoreError::MountNotFound { .. }
    ));
    assert!(matches!(
        store
            .begin_delta_run(&absent, DeltaRunKind::Initial)
            .unwrap_err(),
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

/// Lead ruling 6: `Mount`'s `Debug` never shows the delta token or either run link.
#[test]
fn mount_debug_redacts_the_delta_token_and_links() {
    let mut m = mount(MOUNT, ROOT, "/home/u/OneDrive");
    m.delta_token = Some("secret-token-value".to_owned());
    m.delta_run = Some(DeltaRunKind::Incremental);
    m.delta_next_link = Some("https://graph.example/next?token=secret-next".to_owned());
    let shown = format!("{m:?}");
    assert!(!shown.contains("secret-token-value"), "{shown}");
    assert!(!shown.contains("secret-next"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");
    assert!(shown.contains(MOUNT), "{shown}");
    m.delta_next_link = None;
    m.delta_staged_link = Some("https://graph.example/delta?token=secret-staged".to_owned());
    let shown = format!("{m:?}");
    assert!(!shown.contains("secret-staged"), "{shown}");
}

/// A page link carries a delta token, so its `Debug` never shows it either.
#[test]
fn page_link_debug_redacts_the_link() {
    for link in [
        mydrive_store::PageLink::Next("https://graph.example/next?token=secret-next".to_owned()),
        mydrive_store::PageLink::Delta("https://graph.example/delta?token=secret-delta".to_owned()),
    ] {
        let shown = format!("{link:?}");
        assert!(!shown.contains("secret"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
    }
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

/// WP-017 AC7: every mount constraint, given through the API, is a typed
/// `InvalidMount` naming the rule, and nothing is written.
#[test]
fn ac7_mount_constraints_are_typed_errors_through_the_api() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let base = || mount("m2", "r2", "/home/u/M2");
    let cases: Vec<(&str, mydrive_store::Mount, MountRule)> = vec![
        (
            "next link without a run",
            mydrive_store::Mount {
                delta_next_link: Some("n".to_owned()),
                ..base()
            },
            MountRule::LinkWithoutRun,
        ),
        (
            "staged link without a run",
            mydrive_store::Mount {
                delta_staged_link: Some("s".to_owned()),
                ..base()
            },
            MountRule::LinkWithoutRun,
        ),
        (
            "both links",
            mydrive_store::Mount {
                delta_run: Some(DeltaRunKind::Initial),
                delta_next_link: Some("n".to_owned()),
                delta_staged_link: Some("s".to_owned()),
                ..base()
            },
            MountRule::BothLinks,
        ),
        (
            "blocked message without a reason",
            mydrive_store::Mount {
                blocked_message: Some("why".to_owned()),
                ..base()
            },
            MountRule::BlockedMessageWithoutReason,
        ),
        (
            "paused-until without paused",
            mydrive_store::Mount {
                paused_until: Some(Timestamp::from_unix_seconds(5)),
                ..base()
            },
            MountRule::PausedUntilWithoutPause,
        ),
        (
            "read-only reason without read-only",
            mydrive_store::Mount {
                read_only_reason: Some(ReadOnlyReason::NoWritePermission),
                ..base()
            },
            MountRule::ReadOnlyReasonWithoutReadOnly,
        ),
        (
            "no drive type",
            mydrive_store::Mount {
                drive_type: None,
                ..base()
            },
            MountRule::DriveTypeRequired,
        ),
        (
            "a live token on a new mount",
            mydrive_store::Mount {
                delta_token: Some("t".to_owned()),
                ..base()
            },
            MountRule::NewMountHasDeltaState,
        ),
        (
            "a next link on a new mount",
            mydrive_store::Mount {
                delta_run: Some(DeltaRunKind::Initial),
                delta_next_link: Some("n".to_owned()),
                ..base()
            },
            MountRule::NewMountHasDeltaState,
        ),
        (
            "a resync on a new mount",
            mydrive_store::Mount {
                delta_run: Some(DeltaRunKind::ResyncApply),
                ..base()
            },
            MountRule::NewMountRunNotInitial,
        ),
        (
            "an incremental run without a token",
            mydrive_store::Mount {
                delta_run: Some(DeltaRunKind::Incremental),
                ..base()
            },
            MountRule::IncrementalWithoutToken,
        ),
        (
            "an empty display name",
            mydrive_store::Mount {
                display_name: Some(String::new()),
                ..base()
            },
            MountRule::EmptyText,
        ),
    ];
    for (what, m, rule) in cases {
        let err = store.insert_mount(&m).unwrap_err();
        assert!(
            matches!(err, StoreError::InvalidMount { rule: r, .. } if r == rule),
            "{what}: {err}"
        );
        assert_eq!(store.mounts().unwrap().len(), 1, "{what}: nothing written");
    }

    // Through the run operations.
    assert!(matches!(
        store
            .begin_delta_run(&mount_id(), DeltaRunKind::Incremental)
            .unwrap_err(),
        StoreError::InvalidMount {
            rule: MountRule::IncrementalWithoutToken,
            ..
        }
    ));
    assert!(matches!(
        store
            .stage_delta_page(&mount_id(), &[], &PageLink::Next("n".to_owned()))
            .unwrap_err(),
        StoreError::NoDeltaRun { .. }
    ));
    finish_run_with_token(&store, &mount_id(), "t");
    assert!(matches!(
        store
            .begin_delta_run(&mount_id(), DeltaRunKind::Initial)
            .unwrap_err(),
        StoreError::InvalidMount {
            rule: MountRule::InitialWithToken,
            ..
        }
    ));
    store
        .begin_delta_run(&mount_id(), DeltaRunKind::Incremental)
        .unwrap();
    assert!(matches!(
        store
            .stage_delta_page(&mount_id(), &[], &PageLink::Next(String::new()))
            .unwrap_err(),
        StoreError::InvalidMount {
            rule: MountRule::EmptyText,
            ..
        }
    ));
    store
        .stage_delta_page(&mount_id(), &[], &PageLink::Delta("d".to_owned()))
        .unwrap();
    // With the staged link recorded, a next link would make both: refused.
    assert!(matches!(
        store
            .stage_delta_page(&mount_id(), &[], &PageLink::Next("n".to_owned()))
            .unwrap_err(),
        StoreError::DeltaRunComplete { .. }
    ));
    assert!(matches!(
        store
            .set_mount_blocked(
                &mount_id(),
                Some(&MountBlock {
                    reason: BlockedReason::TenantRefused,
                    message: Some(String::new()),
                }),
            )
            .unwrap_err(),
        StoreError::InvalidMount {
            rule: MountRule::EmptyText,
            ..
        }
    ));
}

/// WP-017 AC7: the same constraints hold in the table itself, whatever writes the row.
#[test]
fn ac7_mount_constraints_are_table_checks() {
    let fx = Fixture::new();
    drop(fx.open_with_mount());
    let conn = raw(&fx.path());
    for (what, set) in [
        ("next link without a run", "delta_next_link = 'n'"),
        ("staged link without a run", "delta_staged_link = 's'"),
        (
            "both links",
            "delta_run = 'initial', delta_next_link = 'n', delta_staged_link = 's'",
        ),
        ("blocked message without a reason", "blocked_message = 'm'"),
        ("paused-until without paused", "paused_until = 5"),
        (
            "read-only reason without read-only",
            "read_only_reason = 'library-read-only'",
        ),
        ("incremental without a token", "delta_run = 'incremental'"),
        (
            "initial with a token",
            "delta_token = 't', delta_run = 'initial'",
        ),
        ("an unknown blocked reason", "blocked_reason = 'other'"),
        ("an unknown run", "delta_run = 'other'"),
        ("an unknown drive type", "drive_type = 'other'"),
    ] {
        let err = conn
            .execute(
                &format!("UPDATE mounts SET {set} WHERE mount_id = 'm1'"),
                [],
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("CHECK constraint failed"),
            "{what}: {err}"
        );
    }
}

/// The mount setters write their facts, and only theirs.
#[test]
fn mount_setters_write_their_facts() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let m = mount_id();
    store
        .set_mount_paused(&m, MountPause::PausedUntil(Timestamp::from_unix_seconds(9)))
        .unwrap();
    let got = store.mount(&m).unwrap().unwrap();
    assert_eq!(
        (got.paused, got.paused_until),
        (true, Some(Timestamp::from_unix_seconds(9)))
    );
    store.set_mount_paused(&m, MountPause::Paused).unwrap();
    assert_eq!(store.mount(&m).unwrap().unwrap().paused_until, None);
    store.set_mount_paused(&m, MountPause::Running).unwrap();
    assert!(!store.mount(&m).unwrap().unwrap().paused);

    let block = MountBlock {
        reason: BlockedReason::OrganisationPolicy,
        message: Some("unmanaged device policy".to_owned()),
    };
    store.set_mount_blocked(&m, Some(&block)).unwrap();
    let got = store.mount(&m).unwrap().unwrap();
    assert_eq!(
        (got.blocked_reason, got.blocked_message.as_deref()),
        (
            Some(BlockedReason::OrganisationPolicy),
            Some("unmanaged device policy")
        )
    );
    store.set_mount_blocked(&m, None).unwrap();
    let got = store.mount(&m).unwrap().unwrap();
    assert_eq!((got.blocked_reason, got.blocked_message), (None, None));

    store
        .set_mount_read_only(
            &m,
            MountAccess::ReadOnly(Some(ReadOnlyReason::NoWritePermission)),
        )
        .unwrap();
    let got = store.mount(&m).unwrap().unwrap();
    assert_eq!(
        (got.read_only, got.read_only_reason),
        (true, Some(ReadOnlyReason::NoWritePermission))
    );
    store
        .set_mount_read_only(&m, MountAccess::ReadWrite)
        .unwrap();
    let got = store.mount(&m).unwrap().unwrap();
    assert_eq!((got.read_only, got.read_only_reason), (false, None));

    store
        .set_mount_drive_info(&m, DriveType::Business, Some("OneDrive - Example"))
        .unwrap();
    let got = store.mount(&m).unwrap().unwrap();
    assert_eq!(
        (got.drive_type, got.display_name.as_deref()),
        (Some(DriveType::Business), Some("OneDrive - Example"))
    );

    // Nothing else changed.
    assert_eq!(
        got,
        mydrive_store::Mount {
            drive_type: Some(DriveType::Business),
            display_name: Some("OneDrive - Example".to_owned()),
            ..mount(MOUNT, ROOT, "/home/u/OneDrive")
        }
    );
    let absent = MountId::new("absent").unwrap();
    assert!(matches!(
        store
            .set_mount_paused(&absent, MountPause::Paused)
            .unwrap_err(),
        StoreError::MountNotFound { .. }
    ));
}

/// WP-017 AC9: the derived state follows the documented precedence (blocked, then
/// paused, then resyncing when there is no live token or the run is a resync, else
/// active), for every combination of the facts.
#[test]
fn ac9_mount_state_is_derived_by_precedence_for_every_combination() {
    let runs = [
        None,
        Some(DeltaRunKind::Initial),
        Some(DeltaRunKind::Incremental),
        Some(DeltaRunKind::ResyncApply),
        Some(DeltaRunKind::ResyncUpload),
    ];
    let mut seen = 0;
    for blocked in [false, true] {
        for paused in [false, true] {
            for has_live_token in [false, true] {
                for run in runs {
                    let facts = MountStateFacts {
                        blocked,
                        paused,
                        has_live_token,
                        run,
                    };
                    let resync = matches!(
                        run,
                        Some(DeltaRunKind::ResyncApply | DeltaRunKind::ResyncUpload)
                    );
                    let expected = if blocked {
                        MountState::Blocked
                    } else if paused {
                        MountState::Paused
                    } else if !has_live_token || resync {
                        MountState::Resyncing
                    } else {
                        MountState::Active
                    };
                    assert_eq!(derive_mount_state(facts), expected, "{facts:?}");
                    seen += 1;
                }
            }
        }
    }
    assert_eq!(seen, 40);
    // The four values by name, so the table above cannot drift into agreeing with itself.
    let facts = |blocked, paused, has_live_token, run| MountStateFacts {
        blocked,
        paused,
        has_live_token,
        run,
    };
    assert_eq!(
        derive_mount_state(facts(true, true, false, Some(DeltaRunKind::ResyncApply))),
        MountState::Blocked
    );
    assert_eq!(
        derive_mount_state(facts(false, true, false, None)),
        MountState::Paused
    );
    assert_eq!(
        derive_mount_state(facts(false, false, true, Some(DeltaRunKind::ResyncUpload))),
        MountState::Resyncing
    );
    assert_eq!(
        derive_mount_state(facts(false, false, true, Some(DeltaRunKind::Incremental))),
        MountState::Active
    );
    assert_eq!(MountState::Resyncing.as_str(), "resyncing");
}

/// The stored mount's `state()` reads its own facts: a new mount is resyncing until its
/// first run is finished, then active; paused and blocked take precedence.
#[test]
fn a_stored_mount_reports_its_derived_state() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let state = || store.mount(&mount_id()).unwrap().unwrap().state();
    assert_eq!(state(), MountState::Resyncing);
    finish_run_with_token(&store, &mount_id(), "t");
    assert_eq!(state(), MountState::Active);
    store
        .begin_delta_run(&mount_id(), DeltaRunKind::ResyncApply)
        .unwrap();
    assert_eq!(state(), MountState::Resyncing);
    store
        .set_mount_paused(&mount_id(), MountPause::Paused)
        .unwrap();
    assert_eq!(state(), MountState::Paused);
    store
        .set_mount_blocked(
            &mount_id(),
            Some(&MountBlock {
                reason: BlockedReason::TenantRefused,
                message: None,
            }),
        )
        .unwrap();
    assert_eq!(state(), MountState::Blocked);
}
