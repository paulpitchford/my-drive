//! ADR-0020 R4: no message of any kind, in either direction, exceeds 1 MiB as one line.
//!
//! One test walks every method and notification in the catalogue and builds a worst-case
//! instance of each: the longest request ID, the longest paths, messages full of characters
//! that JSON must escape, and listings far larger than one line. A method or notification
//! added without a case here fails the test, so nothing can be added without a bound.

use std::collections::BTreeSet;
use std::path::PathBuf;

use mydrive_core::ids::{AccountId, DriveId, MountId, TenantId};
use mydrive_core::state::{EffectivePin, Hydration, ItemState, Modifiers, Pin, Sync};
use mydrive_ipc::codec::{MAX_LINE_BYTES, encode_line};
use mydrive_ipc::envelope::{ErrorResponse, Id, MAX_ID_BYTES, Notification, SuccessResponse};
use mydrive_ipc::error::{ErrorCode, RpcError};
use mydrive_ipc::methods::{
    AccountInfo, ActionOutcome, AttemptId, AuthorizationUrl, ConfirmationToken, DaemonStatusResult,
    HelloParams, ItemExportParams, ItemExportResult, ItemImportParams, ItemImportResult, ItemInfo,
    ItemPathsParams, ItemStatusEntry, ItemStatusParams, ItemStatusTarget, ListParams, LocalPath,
    LoginFinishParams, LoginFinishResult, LoginStartParams, LoginStartResult, LogoutParams,
    LogoutResult, MAX_ACTION_PATH_BYTES, MAX_ACTION_PATHS, MAX_ATTEMPT_ID_BYTES,
    MAX_AUTHORIZATION_URL_BYTES, MAX_PASTED_URL_BYTES, MAX_PAUSE_SECS, MAX_SHARING_LINK_BYTES,
    MAX_STATUS_PATH_BYTES, MAX_STATUS_PATHS, MethodCall, MountAddConfirmParams,
    MountAddConfirmResult, MountAddPreviewParams, MountAddPreviewResult, MountInfo,
    MountRemoveParams, MountRemoveResult, MountSource, MountTarget, PastedRedirect, ProposedMount,
    SUPPORTED_VERSIONS, Session, SharingLink, SkippedItem, StatusSubscribeParams,
    StatusSubscribeResult, SyncPauseParams, SyncPauseResult, SyncResumeParams, SyncResumeResult,
    encoded_path_bytes, method,
};
use mydrive_ipc::paging::{
    ActionReport, ConfirmationLedger, KeysetSource, account_page, action_result, batch_page,
    directory_page, fit_result, mount_page, status_changed_notifications,
};
use mydrive_ipc::path::{MAX_PATH_BYTES, ProtocolPath};
use mydrive_ipc::values::{
    AccountKind, AccountState, BlockedReason, DriveType, ItemReason, LocalFileReason, LoginMode,
    MountState, RefusalReason, SignInFailure, TransferMode,
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
            user_name: Some("\u{1}".repeat(2_000)),
            display_name: Some("\u{1}".repeat(2_000)),
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
            drive_type: Some(DriveType::Other("\"".repeat(200))),
            item_count: Some(u64::MAX),
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

/// A request that must parse, encoded with the longest ID, must fit.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn assert_request_fits(call: &MethodCall, what: &str) {
    let req = call.to_request(worst_id()).unwrap();
    assert_fits(&req, &format!("{what} request"));
    assert_eq!(&MethodCall::from_request(&req).unwrap(), call, "{what}");
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn assert_result_fits(result: &impl Serialize, what: &str) {
    assert_fits(
        &SuccessResponse::new(worst_id(), result).unwrap(),
        &format!("{what} result"),
    );
}

/// An identifier far longer than Graph ever sends, of characters JSON escapes six-fold.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn long_id() -> String {
    "\u{1}".repeat(4_000)
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_account() -> AccountInfo {
    AccountInfo {
        account_id: AccountId::new(long_id()).unwrap(),
        kind: AccountKind::Other("\"".repeat(200)),
        tenant_id: Some(TenantId::new(long_id()).unwrap()),
        state: AccountState::Other("\"".repeat(200)),
        user_name: Some("\u{1}".repeat(50_000)),
        display_name: Some("\u{1}".repeat(50_000)),
    }
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_mount_info() -> MountInfo {
    MountInfo {
        mount_id: MountId::new("m".repeat(64)).unwrap(),
        account_id: AccountId::new(long_id()).unwrap(),
        drive_id: DriveId::new("\"".repeat(4_000)).unwrap(),
        mount_point: long_path("mp", '"'),
        state: MountState::Other("\"".repeat(200)),
        blocked_reason: Some(BlockedReason::Other("\"".repeat(200))),
        blocked_message: Some("\u{1}".repeat(100_000)),
        read_only: true,
        drive_type: Some(DriveType::Other("\"".repeat(200))),
        item_count: Some(u64::MAX),
    }
}

/// Every error a sign-in can produce fits, for every reason.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn sign_in_errors_fit() {
    for reason in SignInFailure::KNOWN
        .iter()
        .map(|r| SignInFailure::from_wire(r))
        .chain([SignInFailure::Other("\"".repeat(10_000))])
    {
        let err = RpcError::sign_in_failed(&reason);
        assert_fits(&ErrorResponse::new(Some(worst_id()), err), "sign-in error");
    }
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_login_start() {
    assert_request_fits(
        &MethodCall::AccountLoginStart(LoginStartParams {
            mode: LoginMode::Headless,
        }),
        method::ACCOUNT_LOGIN_START,
    );
    // The longest URL the type admits.
    let head = "https://login.example/authorize?code_challenge_method=S256&\
                code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&state=";
    let url = format!(
        "{head}{}",
        "s".repeat(MAX_AUTHORIZATION_URL_BYTES - head.len())
    );
    let result = LoginStartResult {
        attempt_id: AttemptId::new("a".repeat(MAX_ATTEMPT_ID_BYTES)).unwrap(),
        authorization_url: AuthorizationUrl::new(url).unwrap(),
    };
    assert_result_fits(&result, method::ACCOUNT_LOGIN_START);
    sign_in_errors_fit();
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_login_finish() {
    // The longest pasted address, of a character JSON escapes.
    assert_request_fits(
        &MethodCall::AccountLoginFinish(LoginFinishParams {
            attempt_id: AttemptId::new("a".repeat(MAX_ATTEMPT_ID_BYTES)).unwrap(),
            redirect_url: Some(PastedRedirect::new("\"".repeat(MAX_PASTED_URL_BYTES)).unwrap()),
        }),
        method::ACCOUNT_LOGIN_FINISH,
    );
    let result = LoginFinishResult {
        account: worst_account(),
        proposed_mount: Some(ProposedMount {
            drive_id: DriveId::new("\"".repeat(4_000)).unwrap(),
            drive_type: DriveType::Other("\"".repeat(200)),
            mount_point: long_path("pm", '"'),
        }),
    };
    let result = fit_result(&worst_id(), result).unwrap();
    assert_result_fits(&result, method::ACCOUNT_LOGIN_FINISH);
    // An identifier too long for any line is refused with an error that fits.
    let mut huge = result;
    huge.account.account_id = AccountId::new("\"".repeat(MAX_LINE_BYTES)).unwrap();
    let err = fit_result(&worst_id(), huge).unwrap_err();
    assert_fits(
        &ErrorResponse::new(Some(worst_id()), err),
        "oversize result error",
    );
    sign_in_errors_fit();
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_logout() {
    assert_request_fits(
        &MethodCall::AccountLogout(LogoutParams {
            account_id: AccountId::new(long_id()).unwrap(),
            dry_run: true,
        }),
        method::ACCOUNT_LOGOUT,
    );
    let result = LogoutResult {
        account_id: AccountId::new(long_id()).unwrap(),
        signed_out: true,
        unsynced_files: u64::MAX,
    };
    assert_result_fits(&result, method::ACCOUNT_LOGOUT);
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_link() -> SharingLink {
    let head = "https://";
    SharingLink::new(format!(
        "{head}{}",
        "\"".repeat(MAX_SHARING_LINK_BYTES - head.len())
    ))
    .unwrap()
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_target() -> MountTarget {
    MountTarget {
        account_id: AccountId::new(long_id()).unwrap(),
        source: MountSource::SharingLink(worst_link()),
    }
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_mount_add_preview() {
    assert_request_fits(
        &MethodCall::MountAddPreview(MountAddPreviewParams {
            target: worst_target(),
        }),
        method::MOUNT_ADD_PREVIEW,
    );
    let mut ledger = ConfirmationLedger::new();
    let token = ledger.issue(&worst_target(), (), 0).unwrap();
    let result = MountAddPreviewResult {
        confirmation_token: token,
        account_id: AccountId::new(long_id()).unwrap(),
        drive_id: DriveId::new("\"".repeat(4_000)).unwrap(),
        drive_type: DriveType::Other("\"".repeat(200)),
        drive_name: "\u{1}".repeat(100_000),
        site_name: Some("\u{1}".repeat(100_000)),
        item_count: Some(u64::MAX),
        read_only: true,
        mount_point: long_path("pv", '"'),
    };
    let result = fit_result(&worst_id(), result).unwrap();
    assert_result_fits(&result, method::MOUNT_ADD_PREVIEW);
    let err = RpcError::already_mounted(
        &MountId::new("m".repeat(64)).unwrap(),
        "\u{1}".repeat(9_000),
    );
    assert_fits(
        &ErrorResponse::new(Some(worst_id()), err),
        "already-mounted",
    );
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_mount_add_confirm() {
    let mut ledger = ConfirmationLedger::new();
    let token = ledger.issue(&worst_target(), (), 0).unwrap();
    assert_request_fits(
        &MethodCall::MountAddConfirm(MountAddConfirmParams {
            target: worst_target(),
            confirmation_token: token.clone(),
            mount_point: Some(long_path("mp", '"')),
        }),
        method::MOUNT_ADD_CONFIRM,
    );
    let result = MountAddConfirmResult {
        mount: worst_mount_info(),
    };
    let result = fit_result(&worst_id(), result).unwrap();
    assert_result_fits(&result, method::MOUNT_ADD_CONFIRM);
    assert!(ledger.redeem(&token, &worst_target(), 1).is_ok());
    let err = ledger.redeem(&token, &worst_target(), 1).unwrap_err();
    assert_fits(
        &ErrorResponse::new(Some(worst_id()), err),
        "restart-preview",
    );
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_mount_remove() {
    assert_request_fits(
        &MethodCall::MountRemove(MountRemoveParams {
            mount_id: MountId::new("m".repeat(64)).unwrap(),
            dry_run: true,
        }),
        method::MOUNT_REMOVE,
    );
    let result = MountRemoveResult {
        mount_id: MountId::new("m".repeat(64)).unwrap(),
        removed: true,
        mount_point: long_path("rm", '"'),
        kept_files: u64::MAX,
        unsynced_files: u64::MAX,
    };
    assert_result_fits(&result, method::MOUNT_REMOVE);
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_pause_resume() {
    assert_request_fits(
        &MethodCall::SyncPause(SyncPauseParams {
            mount_id: Some(MountId::new("m".repeat(64)).unwrap()),
            duration_secs: Some(MAX_PAUSE_SECS),
        }),
        method::SYNC_PAUSE,
    );
    assert_request_fits(
        &MethodCall::SyncResume(SyncResumeParams {
            mount_id: Some(MountId::new("m".repeat(64)).unwrap()),
        }),
        method::SYNC_RESUME,
    );
    assert_result_fits(
        &SyncPauseResult {
            paused_mounts: u64::MAX,
            paused_until: Some(u64::MAX),
        },
        method::SYNC_PAUSE,
    );
    assert_result_fits(
        &SyncResumeResult {
            resumed_mounts: u64::MAX,
        },
        method::SYNC_RESUME,
    );
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn worst_import_export() {
    let local = LocalPath::new(long_path("local", '"').as_str()).unwrap();
    assert_request_fits(
        &MethodCall::ItemImport(ItemImportParams {
            source: local.clone(),
            destination: long_path("dest", '"'),
            mode: TransferMode::Replace,
        }),
        method::ITEM_IMPORT,
    );
    assert_request_fits(
        &MethodCall::ItemExport(ItemExportParams {
            source: long_path("src", '"'),
            destination: local.clone(),
            mode: TransferMode::Replace,
        }),
        method::ITEM_EXPORT,
    );
    assert_result_fits(
        &ItemImportResult {
            path: long_path("dest", '"'),
            item: worst_item(),
            bytes_copied: u64::MAX,
        },
        method::ITEM_IMPORT,
    );
    assert_result_fits(
        &ItemExportResult {
            path: long_path("src", '"'),
            destination: local,
            bytes_copied: u64::MAX,
        },
        method::ITEM_EXPORT,
    );
    let mut err = RpcError::local_file_error(&LocalFileReason::IoError, "x");
    err.message = "\u{1}".repeat(50_000);
    assert_fits(
        &ErrorResponse::new(Some(worst_id()), err),
        "local-file-error",
    );
}

/// The name of a request variant. The match has no wildcard, so adding a variant to
/// `MethodCall` does not compile until it is named here and given a sample below.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn variant_name(call: &MethodCall) -> &'static str {
    match call {
        MethodCall::Hello(_) => "Hello",
        MethodCall::DaemonStatus => "DaemonStatus",
        MethodCall::AccountList(_) => "AccountList",
        MethodCall::MountList(_) => "MountList",
        MethodCall::ItemStatus(_) => "ItemStatus",
        MethodCall::ItemPin(_) => "ItemPin",
        MethodCall::ItemUnpin(_) => "ItemUnpin",
        MethodCall::ItemHydrate(_) => "ItemHydrate",
        MethodCall::ItemDehydrate(_) => "ItemDehydrate",
        MethodCall::StatusSubscribe(_) => "StatusSubscribe",
        MethodCall::AccountLoginStart(_) => "AccountLoginStart",
        MethodCall::AccountLoginFinish(_) => "AccountLoginFinish",
        MethodCall::AccountLogout(_) => "AccountLogout",
        MethodCall::MountAddPreview(_) => "MountAddPreview",
        MethodCall::MountAddConfirm(_) => "MountAddConfirm",
        MethodCall::MountRemove(_) => "MountRemove",
        MethodCall::SyncPause(_) => "SyncPause",
        MethodCall::SyncResume(_) => "SyncResume",
        MethodCall::ItemImport(_) => "ItemImport",
        MethodCall::ItemExport(_) => "ItemExport",
    }
}

/// One call of each request variant.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn one_call_per_variant() -> Vec<MethodCall> {
    let paths = || ItemPathsParams {
        paths: vec![ProtocolPath::new("/m/a").unwrap()],
    };
    vec![
        MethodCall::Hello(HelloParams {
            client_name: "c".into(),
            protocol_versions: vec![1],
        }),
        MethodCall::DaemonStatus,
        MethodCall::AccountList(ListParams::default()),
        MethodCall::MountList(ListParams::default()),
        MethodCall::ItemStatus(ItemStatusParams {
            target: ItemStatusTarget::Directory(ProtocolPath::new("/m").unwrap()),
            cursor: None,
        }),
        MethodCall::ItemPin(paths()),
        MethodCall::ItemUnpin(paths()),
        MethodCall::ItemHydrate(paths()),
        MethodCall::ItemDehydrate(paths()),
        MethodCall::StatusSubscribe(StatusSubscribeParams::default()),
        MethodCall::AccountLoginStart(LoginStartParams {
            mode: LoginMode::Browser,
        }),
        MethodCall::AccountLoginFinish(LoginFinishParams {
            attempt_id: AttemptId::new("a").unwrap(),
            redirect_url: None,
        }),
        MethodCall::AccountLogout(LogoutParams {
            account_id: AccountId::new("a").unwrap(),
            dry_run: false,
        }),
        MethodCall::MountAddPreview(MountAddPreviewParams {
            target: MountTarget {
                account_id: AccountId::new("a").unwrap(),
                source: MountSource::OwnDrive,
            },
        }),
        MethodCall::MountAddConfirm(MountAddConfirmParams {
            target: MountTarget {
                account_id: AccountId::new("a").unwrap(),
                source: MountSource::OwnDrive,
            },
            confirmation_token: ConfirmationToken::new("c1.0.0").unwrap(),
            mount_point: None,
        }),
        MethodCall::MountRemove(MountRemoveParams {
            mount_id: MountId::new("m").unwrap(),
            dry_run: false,
        }),
        MethodCall::SyncPause(SyncPauseParams::default()),
        MethodCall::SyncResume(SyncResumeParams::default()),
        MethodCall::ItemImport(ItemImportParams {
            source: LocalPath::new("/a").unwrap(),
            destination: ProtocolPath::new("/m/a").unwrap(),
            mode: TransferMode::Create,
        }),
        MethodCall::ItemExport(ItemExportParams {
            source: ProtocolPath::new("/m/a").unwrap(),
            destination: LocalPath::new("/a").unwrap(),
            mode: TransferMode::Replace,
        }),
    ]
}

/// The variants `pub enum MethodCall` declares, read from its source, so a variant with no
/// sample in [`one_call_per_variant`] is noticed even if [`variant_name`] was updated.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn declared_variants() -> BTreeSet<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/methods.rs");
    let text = std::fs::read_to_string(&path).unwrap();
    let body = text
        .split_once("pub enum MethodCall {\n")
        .map(|(_, rest)| rest)
        .expect("src/methods.rs declares pub enum MethodCall");
    let body = &body[..body.find("\n}\n").expect("the enum is closed")];
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with('#'))
        .map(|l| {
            l.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .filter(|name| name.starts_with(|c: char| c.is_ascii_uppercase()))
        .collect()
}

/// Every request variant is in `method::ALL`, so the bound loop below reaches it: a variant
/// added to `MethodCall` but left out of the method list fails here.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn every_variant_is_in_the_method_list() {
    let calls = one_call_per_variant();
    let sampled: BTreeSet<String> = calls.iter().map(|c| variant_name(c).to_owned()).collect();
    let declared = declared_variants();
    assert!(declared.contains("Hello") && declared.contains("StatusSubscribe"));
    assert_eq!(
        sampled, declared,
        "every MethodCall variant needs a sample in one_call_per_variant"
    );
    for call in &calls {
        assert!(
            method::ALL.contains(&call.method()),
            "MethodCall::{} ({}) is missing from method::ALL, so it has no bound",
            variant_name(call),
            call.method()
        );
        let back = MethodCall::from_request(&call.to_request(1).unwrap()).unwrap();
        assert_eq!(variant_name(&back), variant_name(call));
    }
}

/// ADR-0020 R4: a worst-case instance of every method's request and response, and of every
/// notification, fits one line. Adding a method or notification without a case here fails,
/// and so does adding a request variant that is not in the method list.
#[test]
fn every_method_and_notification_is_bounded() {
    every_variant_is_in_the_method_list();
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
            method::ACCOUNT_LOGIN_START => worst_login_start(),
            method::ACCOUNT_LOGIN_FINISH => worst_login_finish(),
            method::ACCOUNT_LOGOUT => worst_logout(),
            method::MOUNT_ADD_PREVIEW => worst_mount_add_preview(),
            method::MOUNT_ADD_CONFIRM => worst_mount_add_confirm(),
            method::MOUNT_REMOVE => worst_mount_remove(),
            method::SYNC_PAUSE | method::SYNC_RESUME => worst_pause_resume(),
            method::ITEM_IMPORT | method::ITEM_EXPORT => worst_import_export(),
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
