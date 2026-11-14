//! ADR-0020 R4: no message of any kind, in either direction, exceeds 1 MiB as one line.
//!
//! One test walks every method and notification in the catalogue and builds a worst-case
//! instance of each: the longest request ID, the longest paths, messages full of characters
//! that JSON must escape, and listings far larger than one line. A method or notification
//! added without a case here fails the test, so nothing can be added without a bound.

use mydrive_core::ids::{AccountId, DriveId, MountId, TenantId};
use mydrive_core::state::{EffectivePin, Hydration, ItemState, Modifiers, Pin, Sync};
use mydrive_ipc::codec::{MAX_LINE_BYTES, encode_line};
use mydrive_ipc::envelope::{ErrorResponse, Id, MAX_ID_BYTES, Notification, SuccessResponse};
use mydrive_ipc::error::{ErrorCode, RpcError};
use mydrive_ipc::methods::{
    AccountInfo, ActionOutcome, DaemonStatusResult, HelloParams, ItemInfo, ItemPathsParams,
    ItemStatusEntry, ItemStatusParams, ItemStatusTarget, ListParams, MAX_ACTION_PATH_BYTES,
    MAX_ACTION_PATHS, MAX_STATUS_PATH_BYTES, MAX_STATUS_PATHS, MethodCall, MountInfo,
    SUPPORTED_VERSIONS, Session, SkippedItem, StatusSubscribeResult, encoded_path_bytes, method,
};
use mydrive_ipc::paging::{
    ActionReport, KeysetSource, account_page, action_result, batch_page, directory_page,
    mount_page, status_changed_notifications,
};
use mydrive_ipc::path::{MAX_PATH_BYTES, ProtocolPath};
use mydrive_ipc::values::{
    AccountKind, AccountState, BlockedReason, ItemReason, MountState, RefusalReason,
};
use serde::Serialize;

/// The longest request ID, made of a character JSON escapes six times over.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_id() -> Id {
    Id::String("\u{1}".repeat(MAX_ID_BYTES))
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn assert_fits(message: &impl Serialize, what: &str) {
    let line = encode_line(message).unwrap_or_else(|e| panic!("{what}: {e}"));
    assert!(
        line.len() - 1 <= MAX_LINE_BYTES,
        "{what}: {} bytes",
        line.len() - 1
    );
}

/// A path of exactly `MAX_PATH_BYTES`, with a unique prefix, made of `fill`.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn long_path(prefix: &str, fill: char) -> ProtocolPath {
    let head = format!("/{prefix}/");
    let room = MAX_PATH_BYTES - head.len();
    let body: String = std::iter::repeat_n(fill, room / fill.len_utf8()).collect();
    ProtocolPath::new(format!("{head}{body}")).unwrap()
}

/// The largest item state: error status with a reason, an over-long message (capped on the
/// wire) and both modifiers.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_item() -> ItemInfo {
    let state = ItemState {
        hydration: Hydration::Partial,
        pin: Pin::Unpinned,
        sync: Sync::PendingDownload,
    };
    let modifiers = Modifiers {
        read_only: true,
        shortcut: true,
    };
    let mut item = ItemInfo::new(state, EffectivePin::Pinned, modifiers)
        .with_reason(ItemReason::HashMismatch, "x");
    item.message = Some("\u{1}".repeat(50_000));
    item
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_error() -> RpcError {
    let mut error = RpcError::refused(&RefusalReason::RemoteChanged, "x");
    error.message = "\u{1}".repeat(50_000);
    error
}

/// A source over a sorted vector, keyed by `key`.
struct VecSource<T> {
    rows: Vec<T>,
    key: fn(&T) -> String,
}

impl<T: Clone> KeysetSource<T> for VecSource<T> {
    fn rows_after(&mut self, after: Option<&str>, limit: usize) -> Result<Vec<T>, RpcError> {
        let key = self.key;
        Ok(self
            .rows
            .iter()
            .filter(|r| after.is_none_or(|a| key(r).as_str() > a))
            .take(limit)
            .cloned()
            .collect())
    }
}

/// Runs a full cursor loop, checking that every response and every request repeated with
/// its cursor fits one line. Returns the number of pages.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn loop_pages<R: Serialize>(
    what: &str,
    mut page: impl FnMut(Option<&str>) -> (R, Option<String>),
    request: impl Fn(Option<String>) -> MethodCall,
) -> usize {
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let req = request(cursor.clone()).to_request(worst_id()).unwrap();
        assert_fits(&req, &format!("{what} request, page {pages}"));
        let (result, next) = page(cursor.as_deref());
        pages += 1;
        assert_fits(
            &SuccessResponse::new(worst_id(), &result).unwrap(),
            &format!("{what} response, page {pages}"),
        );
        match next {
            Some(c) => cursor = Some(c),
            None => return pages,
        }
    }
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_hello() {
    // The client may list any number of versions; the error echoes them in a capped message.
    let mut session = Session::new();
    let params = HelloParams {
        client_name: "c".repeat(1_000),
        protocol_versions: (2..100_000).collect(),
    };
    let err = session.hello(&params, SUPPORTED_VERSIONS, &[]).unwrap_err();
    assert_fits(&ErrorResponse::new(Some(worst_id()), err), "hello error");
    let mut session = Session::new();
    let params = HelloParams {
        client_name: "mydrive-cli".into(),
        protocol_versions: vec![1],
    };
    let result = session.hello(&params, SUPPORTED_VERSIONS, &[]).unwrap();
    assert_fits(
        &SuccessResponse::new(worst_id(), &result).unwrap(),
        "hello result",
    );
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_daemon_status() {
    let result = DaemonStatusResult {
        daemon_version: "0.1.0".into(),
        protocol_versions: SUPPORTED_VERSIONS.to_vec(),
        ready: false,
    };
    assert_fits(
        &SuccessResponse::new(worst_id(), &result).unwrap(),
        "daemon.status",
    );
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_account_list() {
    let rows: Vec<AccountInfo> = (0..3_000)
        .map(|i| AccountInfo {
            account_id: AccountId::new(format!("{i:05}{}", "a".repeat(250))).unwrap(),
            kind: AccountKind::Other("\"".repeat(200)),
            tenant_id: Some(TenantId::new("\u{1}".repeat(200)).unwrap()),
            state: AccountState::Other("\"".repeat(200)),
        })
        .collect();
    let mut source = VecSource {
        rows,
        key: |a: &AccountInfo| a.account_id.as_str().to_owned(),
    };
    let pages = loop_pages(
        method::ACCOUNT_LIST,
        |c| {
            let p = account_page(&mut source, c, &worst_id()).unwrap();
            let next = p.next_cursor.clone();
            (p, next)
        },
        |cursor| MethodCall::AccountList(ListParams { cursor }),
    );
    assert!(pages > 1);
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_mount_list() {
    let rows: Vec<MountInfo> = (0..300)
        .map(|i| MountInfo {
            mount_id: MountId::new(format!("{i:06}{}", "m".repeat(58))).unwrap(),
            account_id: AccountId::new("\u{1}".repeat(300)).unwrap(),
            drive_id: DriveId::new("\"".repeat(2_000)).unwrap(),
            mount_point: long_path(&format!("{i:06}"), '"'),
            state: MountState::Blocked,
            blocked_reason: Some(BlockedReason::Other("\"".repeat(200))),
            blocked_message: Some("\u{1}".repeat(100_000)),
            read_only: true,
        })
        .collect();
    let mut source = VecSource {
        rows,
        key: |m: &MountInfo| m.mount_id.as_str().to_owned(),
    };
    let pages = loop_pages(
        method::MOUNT_LIST,
        |c| {
            let p = mount_page(&mut source, c, &worst_id()).unwrap();
            let next = p.next_cursor.clone();
            (p, next)
        },
        |cursor| MethodCall::MountList(ListParams { cursor }),
    );
    assert!(pages > 1);
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_item_status() {
    // A batch at the byte limit, of 4,096-byte paths full of characters JSON escapes.
    let mut paths = Vec::new();
    while paths.len() < MAX_STATUS_PATHS as usize {
        let p = long_path(&format!("{:05}", paths.len()), '"');
        paths.push(p);
        if encoded_path_bytes(&paths) > MAX_STATUS_PATH_BYTES {
            paths.pop();
            break;
        }
    }
    let params = |cursor| {
        MethodCall::ItemStatus(ItemStatusParams {
            target: ItemStatusTarget::Paths(paths.clone()),
            cursor,
        })
    };
    // The batch is accepted by the limits.
    let req = params(None).to_request(worst_id()).unwrap();
    MethodCall::from_request(&req).unwrap();
    let pages = loop_pages(
        "item.status batch",
        |c| {
            let p = batch_page(&paths, c, Some(u64::MAX), &worst_id(), |path| {
                ItemStatusEntry::known(path.clone(), worst_item())
            })
            .unwrap();
            let next = p.next_cursor.clone();
            (p, next)
        },
        params,
    );
    assert!(pages >= 1);

    // A directory at the longest path, with children of the longest names.
    let dir = long_path("d", '"');
    let dir_prefix = &dir.as_str()[..dir.as_str().len() - 300];
    let dir = ProtocolPath::new(dir_prefix.to_owned()).unwrap();
    let children: Vec<ItemStatusEntry> = (0..2_000)
        .map(|i| {
            let name = format!("{i:05}{}", "\"".repeat(250));
            let path = ProtocolPath::new(format!("{dir}/{name}")).unwrap();
            ItemStatusEntry::known(path, worst_item())
        })
        .collect();
    let dir_entry = ItemStatusEntry::known(dir.clone(), worst_item());
    let mut source = VecSource {
        rows: children,
        key: |e: &ItemStatusEntry| {
            let p = e.path.as_str();
            p[p.rfind('/').map_or(0, |i| i + 1)..].to_owned()
        },
    };
    let pages = loop_pages(
        "item.status directory",
        |c| {
            let p = directory_page(
                &dir,
                &dir_entry,
                &mut source,
                c,
                Some(u64::MAX),
                &worst_id(),
            )
            .unwrap();
            let next = p.next_cursor.clone();
            (p, next)
        },
        |cursor| {
            MethodCall::ItemStatus(ItemStatusParams {
                target: ItemStatusTarget::Directory(dir.clone()),
                cursor,
            })
        },
    );
    assert!(pages > 1);
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_action(name: &str) {
    let mut paths = Vec::new();
    while paths.len() < MAX_ACTION_PATHS as usize {
        paths.push(long_path(&format!("{:03}", paths.len()), 'a'));
        if encoded_path_bytes(&paths) > MAX_ACTION_PATH_BYTES {
            paths.pop();
            break;
        }
    }
    let call = MethodCall::parse(
        name,
        Some(
            &serde_json::to_value(ItemPathsParams {
                paths: paths.clone(),
            })
            .unwrap(),
        ),
    )
    .unwrap();
    assert_fits(
        &call.to_request(worst_id()).unwrap(),
        &format!("{name} request"),
    );
    let reports: Vec<ActionReport> = paths
        .iter()
        .enumerate()
        .map(|(i, path)| ActionReport {
            path: path.clone(),
            outcome: if i % 2 == 0 {
                ActionOutcome::Refused(worst_error())
            } else {
                ActionOutcome::Accepted {
                    skipped: (0..40)
                        .map(|j| SkippedItem {
                            path: long_path(&format!("{i:03}/{j:04}"), '"'),
                            error: worst_error(),
                        })
                        .collect(),
                    skipped_count: u64::MAX,
                    bytes_freed: Some(u64::MAX),
                }
            },
        })
        .collect();
    let result = action_result(&worst_id(), reports).unwrap();
    assert_fits(
        &SuccessResponse::new(worst_id(), &result).unwrap(),
        &format!("{name} result"),
    );
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_subscribe() {
    assert_fits(
        &SuccessResponse::new(worst_id(), &StatusSubscribeResult {}).unwrap(),
        "status.subscribe",
    );
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_status_changed() {
    let entries: Vec<ItemStatusEntry> = (0..500)
        .map(|i| ItemStatusEntry::known(long_path(&format!("{i:04}"), '"'), worst_item()))
        .collect();
    let refresh: Vec<ProtocolPath> = (0..500)
        .map(|i| long_path(&format!("r{i:04}"), '"'))
        .collect();
    let notes: Vec<Notification> =
        status_changed_notifications(entries, refresh, Some(u64::MAX)).unwrap();
    assert!(notes.len() > 1);
    for note in &notes {
        assert_fits(note, "status.changed");
    }
}

/// ADR-0020 R4: a worst-case instance of every method's request and response, and of every
/// notification, fits one line. Adding a method or notification without a case here fails.
#[test]
fn every_method_and_notification_is_bounded() {
    for name in method::ALL.iter().chain(method::NOTIFICATIONS) {
        match *name {
            method::HELLO => worst_hello(),
            method::DAEMON_STATUS => worst_daemon_status(),
            method::ACCOUNT_LIST => worst_account_list(),
            method::MOUNT_LIST => worst_mount_list(),
            method::ITEM_STATUS => worst_item_status(),
            method::ITEM_PIN
            | method::ITEM_UNPIN
            | method::ITEM_HYDRATE
            | method::ITEM_DEHYDRATE => {
                worst_action(name);
            }
            method::STATUS_SUBSCRIBE => worst_subscribe(),
            method::STATUS_CHANGED => worst_status_changed(),
            other => panic!("{other} has no worst-case bound; add one before adding the method"),
        }
    }
}

/// ADR-0020 R4: every error, with the longest ID and the longest message, fits one line.
#[test]
fn every_error_is_bounded() {
    for code in ErrorCode::ALL {
        let mut error = RpcError::new(*code, "\u{1}".repeat(50_000));
        error.message = "\u{1}".repeat(50_000);
        assert_fits(&ErrorResponse::new(Some(worst_id()), error), code.name());
    }
}
