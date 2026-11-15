//! ADR-0010 R6, WP-016 AC3 and AC4: no result, notification or error of the protocol can
//! carry a sign-in secret, the pasted redirect address, or a sharing link.
//!
//! The tests plant recognisable secrets (a PKCE verifier, an authorisation code, access,
//! refresh and ID tokens, the pasted address and a sharing link), try to push each one
//! through every value type a response can carry, build every new result and every error the
//! protocol layer produces from inputs that contain them, and search the encoded bytes.

use std::collections::BTreeSet;

use mydrive_core::ids::{AccountId, DriveId, MountId, TenantId};
use mydrive_core::state::{EffectivePin, Hydration, ItemState, Modifiers, Pin, Sync};
use mydrive_ipc::codec::encode_line;
use mydrive_ipc::envelope::{ErrorResponse, Id, SuccessResponse};
use mydrive_ipc::error::{ErrorCode, RpcError};
use mydrive_ipc::methods::{
    AccountInfo, AttemptId, AuthorizationUrl, ConfirmationToken, HelloParams, ItemExportResult,
    ItemImportResult, ItemInfo, LocalPath, LoginFinishResult, LoginStartResult, LogoutResult,
    MethodCall, MountAddConfirmResult, MountAddPreviewResult, MountInfo, MountPauseResult,
    MountRemoveResult, MountResumeResult, MountSource, MountTarget, PastedRedirect, ProposedMount,
    SUPPORTED_VERSIONS, Session, SharingLink, method,
};
use mydrive_ipc::paging::ConfirmationLedger;
use mydrive_ipc::path::ProtocolPath;
use mydrive_ipc::values::{
    AccountKind, AccountState, DriveType, ItemReason, MountState, SignInFailure,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// A PKCE verifier: 43 characters, the shortest RFC 7636 allows.
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CODE: &str = "M.C507_BAY.2.U.PLANTED-AUTHORIZATION-CODE-6f1e";
const ACCESS_TOKEN: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiJ9.PLANTED-ACCESS-TOKEN";
const REFRESH_TOKEN: &str = "M.R3_BAY.PLANTED-REFRESH-TOKEN-0aa1";
const ID_TOKEN: &str = "eyJhbGciOiJub25lIn0.PLANTED-ID-TOKEN";
const STATE: &str = "c3RhdGUtZm9yLXRoaXMtYXR0ZW1wdA";
const LINK: &str = "https://example.sharepoint.com/:f:/s/Finance/PLANTED-SHARING-LINK-EjK3pQ2";

fn pasted() -> String {
    format!(
        "https://login.microsoftonline.com/common/oauth2/nativeclient?code={CODE}&state={STATE}"
    )
}

/// Every planted secret, and distinctive parts of the composite ones.
fn secrets() -> Vec<String> {
    let link_hex: String = LINK.bytes().map(|b| format!("{b:02x}")).collect();
    vec![
        VERIFIER.into(),
        VERIFIER.to_ascii_lowercase(),
        CODE.into(),
        "PLANTED-AUTHORIZATION-CODE".into(),
        ACCESS_TOKEN.into(),
        REFRESH_TOKEN.into(),
        ID_TOKEN.into(),
        pasted(),
        LINK.into(),
        "PLANTED-SHARING-LINK".into(),
        link_hex[..32].to_owned(),
    ]
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn assert_clean(bytes: &[u8], what: &str) {
    let text = String::from_utf8_lossy(bytes);
    for secret in secrets() {
        assert!(!text.contains(&secret), "{what} carries {secret:?}: {text}");
    }
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn result_line(result: &impl Serialize) -> Vec<u8> {
    encode_line(&SuccessResponse::new(Id::Number(1), result).unwrap()).unwrap()
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn error_line(error: RpcError) -> Vec<u8> {
    encode_line(&ErrorResponse::new(Some(Id::Number(1)), error)).unwrap()
}

/// Base64url without padding, as PKCE uses.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// The S256 challenge of the planted verifier.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn challenge() -> String {
    base64url(&Sha256::digest(VERIFIER.as_bytes()))
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn authorize_url(extra: &str) -> String {
    format!(
        "https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id=00000000-0000-\
         0000-0000-000000000000&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A53211&\
         scope=Files.ReadWrite.All%20offline_access%20User.Read&state={STATE}&code_challenge={}&\
         code_challenge_method=S256{extra}",
        challenge()
    )
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn item() -> ItemInfo {
    let state = ItemState {
        hydration: Hydration::Full,
        pin: Pin::Inherit,
        sync: Sync::PendingUpload,
    };
    ItemInfo::new(state, EffectivePin::Unpinned, Modifiers::default())
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn link_target() -> MountTarget {
    MountTarget {
        account_id: AccountId::new("acct-work-1").unwrap(),
        source: MountSource::SharingLink(SharingLink::new(LINK).unwrap()),
    }
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn mount() -> MountInfo {
    MountInfo {
        mount_id: MountId::new("example-finance-documents").unwrap(),
        account_id: AccountId::new("acct-work-1").unwrap(),
        drive_id: DriveId::new("b!financedriveid").unwrap(),
        mount_point: ProtocolPath::new("/home/user/Example/Finance - Documents").unwrap(),
        state: MountState::Resyncing,
        blocked_reason: None,
        blocked_message: None,
        read_only: false,
        drive_type: Some(DriveType::DocumentLibrary),
        item_count: Some(0),
        pause: None,
    }
}

/// The RFC 7636 example: our base64url and digest are the real S256 transform.
#[test]
fn the_planted_challenge_is_the_s256_of_the_verifier() {
    assert_eq!(challenge(), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    assert_eq!(challenge().len(), 43);
}

/// WP-016 AC3, ADR-0010 R6: no value type a result carries can hold a sign-in secret. Each
/// secret is pushed into every validated field that a daemon might misuse, and refused; the
/// refusal does not repeat it.
#[test]
fn adr_0010_r6_value_types_refuse_secrets() {
    for secret in [VERIFIER, CODE, ACCESS_TOKEN, REFRESH_TOKEN, ID_TOKEN] {
        for err in [
            AttemptId::new(secret).err(),
            AttemptId::new(secret.to_ascii_lowercase()).err(),
            ConfirmationToken::new(secret).err(),
            ConfirmationToken::new(secret.to_ascii_lowercase()).err(),
        ] {
            let err = err.unwrap_or_else(|| panic!("{secret} accepted as an ID or token"));
            assert!(!err.to_string().contains(secret));
        }
    }
    // Secrets in the authorisation URL: as their own parameters, in any case, or with the
    // verifier as a `plain` challenge.
    let smuggled = smuggled_urls();
    for url in smuggled {
        let err = AuthorizationUrl::new(url.clone()).unwrap_err();
        assert_clean(err.to_string().as_bytes(), "AuthorizationUrl error");
        assert!(serde_json::from_value::<AuthorizationUrl>(json!(url)).is_err());
    }
}

/// Planted authorisation URLs: each smuggles a secret past a rule `AuthorizationUrl` checks.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn smuggled_urls() -> Vec<String> {
    vec![
        authorize_url(&format!("&code_verifier={VERIFIER}")),
        authorize_url(&format!("&Code_Verifier={VERIFIER}")),
        authorize_url(&format!("&code={CODE}")),
        authorize_url(&format!(";code={CODE}")),
        authorize_url(&format!("&x=1;code={CODE}")),
        authorize_url(&format!("&access_token={ACCESS_TOKEN}")),
        authorize_url(&format!("&refresh_token={REFRESH_TOKEN}")),
        authorize_url(&format!("&id_token={ID_TOKEN}")),
        authorize_url(&format!("&token={ACCESS_TOKEN}")),
        authorize_url(&format!("&code%5Fverifier={VERIFIER}")),
        authorize_url(&format!("#code={CODE}")),
        authorize_url("")
            .replace(&challenge(), VERIFIER)
            .replace("code_challenge_method=S256", "code_challenge_method=plain"),
        authorize_url("").replace("https://", &format!("https://{CODE}@")),
        authorize_url("").replace("/authorize?", &format!("/authorize;code={CODE}?")),
    ]
}

/// A value of a checked type built from planted material: the first planted attempt the type
/// accepts, or else `fallback`. If a check ever lets a secret through, the secret flows into
/// the result and the search of the encoded bytes finds it.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn from_planted<T, E>(attempts: impl IntoIterator<Item = Result<T, E>>, fallback: T) -> T {
    attempts
        .into_iter()
        .find_map(Result::ok)
        .unwrap_or(fallback)
}

/// WP-016 AC3, ADR-0010 R6: every result added by WP-016 is built with each of its checked
/// fields made from planted secrets, and its encoded bytes carry none of them.
///
/// What the types protect, and so what this test can catch: `attempt_id` and
/// `confirmation_token` (too short and in too small an alphabet for a verifier, code or
/// token), `authorization_url` (no credential parameter, S256 only), and the confirmation
/// token the ledger derives from a planted sharing link. The other text fields of these
/// results (identifiers, paths, open-set values, display names, messages) are not protected
/// by any type: [`unprotected_fields_accept_any_text`] shows they would carry a secret, and
/// the catalogue says so. Keeping secrets out of them is the daemon's rule, tested there.
#[test]
fn adr_0010_r6_no_new_result_carries_a_secret() {
    let mut covered = BTreeSet::new();
    // The credentials themselves, as given and lowercased (the ID alphabets are lowercase).
    let planted: Vec<String> = [VERIFIER, CODE, ACCESS_TOKEN, REFRESH_TOKEN, ID_TOKEN, LINK]
        .into_iter()
        .map(str::to_owned)
        .chain([pasted()])
        .flat_map(|s| [s.to_ascii_lowercase(), s])
        .collect();
    let attempt_id = from_planted(
        planted.iter().cloned().map(AttemptId::new),
        AttemptId::new("attempt-7f3a").unwrap(),
    );
    let url = from_planted(
        smuggled_urls().into_iter().map(AuthorizationUrl::new),
        AuthorizationUrl::new(authorize_url("")).unwrap(),
    );
    assert!(url.as_str().contains(&challenge()));
    assert!(url.as_str().contains(STATE));
    let mut ledger = ConfirmationLedger::new();
    let token = from_planted(
        planted.iter().cloned().map(ConfirmationToken::new),
        ledger.issue(&link_target(), "payload", 0).unwrap(),
    );
    let account = AccountInfo {
        account_id: AccountId::new("acct-personal-1").unwrap(),
        kind: AccountKind::Personal,
        tenant_id: Some(TenantId::new("9188040d-6c67-4c5b-b112-36a304b66dad").unwrap()),
        state: AccountState::SignedIn,
        user_name: Some("sam@example.com".into()),
        display_name: Some("Sam Example".into()),
    };
    let mount = mount();
    for name in method::ALL {
        let line = match *name {
            // Results that existed before WP-016 carry no new field that could hold one.
            method::HELLO
            | method::DAEMON_STATUS
            | method::ACCOUNT_LIST
            | method::MOUNT_LIST
            | method::ITEM_STATUS
            | method::ITEM_PIN
            | method::ITEM_UNPIN
            | method::ITEM_HYDRATE
            | method::ITEM_DEHYDRATE
            | method::STATUS_SUBSCRIBE => continue,
            // Checked fields: both, from planted material.
            method::ACCOUNT_LOGIN_START => result_line(&LoginStartResult {
                attempt_id: attempt_id.clone(),
                authorization_url: url.clone(),
            }),
            // No checked field: identifiers, a path and display names only.
            method::ACCOUNT_LOGIN_FINISH => result_line(&LoginFinishResult {
                account: account.clone(),
                proposed_mount: Some(ProposedMount {
                    drive_id: DriveId::new("0123456789abcdef").unwrap(),
                    drive_type: DriveType::Personal,
                    mount_point: ProtocolPath::new("/home/user/OneDrive").unwrap(),
                }),
            }),
            // No checked field: an identifier and a count.
            method::ACCOUNT_LOGOUT => result_line(&LogoutResult {
                account_id: account.account_id.clone(),
                signed_out: true,
                unsynced_files: 3,
            }),
            // Checked: the token, derived by the ledger from a planted link, or a planted
            // token if `ConfirmationToken` ever accepted one.
            method::MOUNT_ADD_PREVIEW => result_line(&MountAddPreviewResult {
                confirmation_token: token.clone(),
                account_id: AccountId::new("acct-work-1").unwrap(),
                drive_id: DriveId::new("b!financedriveid").unwrap(),
                drive_type: DriveType::DocumentLibrary,
                drive_name: "Documents".into(),
                site_name: Some("Finance".into()),
                item_count: Some(8_400),
                read_only: false,
                mount_point: mount.mount_point.clone(),
            }),
            // No checked field: the mount's identifiers, path and names.
            method::MOUNT_ADD_CONFIRM => result_line(&MountAddConfirmResult {
                mount: mount.clone(),
            }),
            // No checked field: an identifier, a path and counts.
            method::MOUNT_REMOVE => result_line(&MountRemoveResult {
                mount_id: mount.mount_id.clone(),
                removed: true,
                mount_point: mount.mount_point.clone(),
                kept_files: 140,
                unsynced_files: 0,
            }),
            // Counts and a time: no text at all.
            method::MOUNT_PAUSE => result_line(&MountPauseResult {
                paused_mounts: 4,
                paused_until: Some(1_794_790_800),
            }),
            method::MOUNT_RESUME => result_line(&MountResumeResult { resumed_mounts: 4 }),
            // No checked field: paths, item state and a count.
            method::ITEM_IMPORT => result_line(&ItemImportResult {
                path: ProtocolPath::new("/home/user/OneDrive/a.pdf").unwrap(),
                item: item(),
                bytes_copied: 1,
            }),
            method::ITEM_EXPORT => result_line(&ItemExportResult {
                path: ProtocolPath::new("/home/user/OneDrive/a.pdf").unwrap(),
                destination: LocalPath::new("/home/user/a.pdf").unwrap(),
                bytes_copied: 1,
            }),
            other => panic!("{other} has no planted-secret case; add one (ADR-0010 R6)"),
        };
        assert_clean(&line, name);
        covered.insert(*name);
    }
    assert_eq!(covered.len(), 10, "every WP-016 method is covered");
    // No notification was added; `status.changed` carries item state only.
    assert_eq!(method::NOTIFICATIONS, [method::STATUS_CHANGED]);
    // The confirmation token holds nothing of the link.
    assert_clean(token.as_str().as_bytes(), "confirmation token");
}

/// WP-016 AC3, stated plainly: the protocol types do **not** protect the free-text and
/// identifier fields of results. Each accepts a planted secret in a form its rule allows, and
/// the encoded result carries it. This pins down what the catalogue's "What the types check"
/// paragraph says is the daemon's rule, not the types'. If a type starts refusing one of
/// these, move the field to [`adr_0010_r6_no_new_result_carries_a_secret`] and the
/// catalogue's checked list.
#[test]
fn unprotected_fields_accept_any_text() {
    let lowercase_verifier: String = VERIFIER
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let carried = [
        (
            "account_id",
            result_line(&LogoutResult {
                account_id: AccountId::new(CODE).unwrap(),
                signed_out: true,
                unsynced_files: 0,
            }),
            CODE.to_owned(),
        ),
        (
            "user_name",
            result_line(&AccountInfo {
                user_name: Some(ACCESS_TOKEN.into()),
                ..mount_account()
            }),
            ACCESS_TOKEN.to_owned(),
        ),
        (
            "tenant_id",
            result_line(&AccountInfo {
                tenant_id: Some(TenantId::new(REFRESH_TOKEN).unwrap()),
                ..mount_account()
            }),
            REFRESH_TOKEN.to_owned(),
        ),
        (
            "mount_id",
            result_line(&MountInfo {
                mount_id: MountId::new(lowercase_verifier.clone()).unwrap(),
                ..mount()
            }),
            lowercase_verifier,
        ),
        (
            "drive_id",
            result_line(&MountInfo {
                drive_id: DriveId::new(ID_TOKEN).unwrap(),
                ..mount()
            }),
            // Drive IDs are kept in lowercase.
            ID_TOKEN.to_ascii_lowercase(),
        ),
        (
            "mount_point",
            result_line(&MountInfo {
                mount_point: ProtocolPath::new(format!("/home/user/{CODE}")).unwrap(),
                ..mount()
            }),
            CODE.to_owned(),
        ),
        (
            "drive_type (open set)",
            result_line(&MountInfo {
                drive_type: Some(DriveType::from_wire(CODE)),
                ..mount()
            }),
            CODE.to_owned(),
        ),
        (
            "blocked_message",
            result_line(&MountInfo {
                blocked_message: Some(LINK.into()),
                ..mount()
            }),
            LINK.to_owned(),
        ),
        (
            "export destination",
            result_line(&ItemExportResult {
                path: ProtocolPath::new("/home/user/OneDrive/a.pdf").unwrap(),
                destination: LocalPath::new(format!("/home/user/{CODE}")).unwrap(),
                bytes_copied: 1,
            }),
            CODE.to_owned(),
        ),
        (
            "item message",
            result_line(&item().with_reason(ItemReason::DownloadFailed, CODE)),
            CODE.to_owned(),
        ),
        (
            "error message",
            error_line(RpcError::new(ErrorCode::Internal, CODE)),
            CODE.to_owned(),
        ),
    ];
    for (field, line, secret) in carried {
        let text = String::from_utf8_lossy(&line);
        assert!(
            text.contains(&secret),
            "{field} no longer carries arbitrary text; it is now checked"
        );
    }
    let catalogue = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/protocol/README.md"),
    )
    .unwrap()
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ");
    for needle in [
        "Only three values a client receives are checked by the protocol types",
        "The types cannot tell any of these from a secret",
    ] {
        assert!(
            catalogue.contains(needle),
            "the catalogue must say {needle:?}"
        );
    }
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn mount_account() -> AccountInfo {
    AccountInfo {
        account_id: AccountId::new("acct-personal-1").unwrap(),
        kind: AccountKind::Personal,
        tenant_id: None,
        state: AccountState::SignedIn,
        user_name: None,
        display_name: None,
    }
}

/// WP-016 AC3: every error the protocol layer builds for sign-in and for a confirmation is
/// free of the planted secrets, whatever reason it carries.
#[test]
fn sign_in_and_confirmation_errors_carry_no_secret() {
    for reason in SignInFailure::KNOWN
        .iter()
        .map(|r| SignInFailure::from_wire(r))
        .chain([SignInFailure::Other("future".into())])
    {
        assert_clean(
            &error_line(RpcError::sign_in_failed(&reason)),
            "sign-in error",
        );
    }
    let mut ledger = ConfirmationLedger::new();
    let token = ledger.issue(&link_target(), (), 0).unwrap();
    let other = MountTarget {
        account_id: AccountId::new("acct-work-2").unwrap(),
        ..link_target()
    };
    let err = ledger.claim(&token, &other, 0).unwrap_err();
    assert_clean(&error_line(err), "confirmation refusal");
    let claim = ledger.claim(&token, &link_target(), 0).unwrap();
    let err = ledger.claim(&token, &link_target(), 0).unwrap_err();
    assert_clean(&error_line(err), "confirmation while claimed");
    assert_clean(format!("{claim:?}").as_bytes(), "claim Debug");
    ledger.release(claim, 0);
    let claim = ledger.claim(&token, &link_target(), 0).unwrap();
    let _ = ledger.consume(claim, mount());
    let err = ledger.claim(&token, &link_target(), 0).unwrap_err();
    assert_clean(&error_line(err), "second confirmation");
    assert_clean(format!("{ledger:?}").as_bytes(), "ledger Debug");
}

/// Variants of a parameter object with `secret` placed in every field, in several shapes,
/// and as a key, plus whole-params shapes that hold it.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn planted_variants(valid: &Value, secret: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let map = valid.as_object().unwrap();
    let shapes = [
        json!(secret),
        json!(format!("{secret} ")),
        json!(format!("{secret}\n")),
        json!(format!("{secret}{}", "x".repeat(9_000))),
        json!([secret]),
        json!({"value": secret}),
        json!({ (secret): 1 }),
    ];
    for key in map.keys() {
        for shape in &shapes {
            let mut variant = map.clone();
            variant.insert(key.clone(), shape.clone());
            out.push(Value::Object(variant));
            // The same with every other field removed, so the planted one is what fails.
            let mut alone = serde_json::Map::new();
            alone.insert(key.clone(), shape.clone());
            out.push(Value::Object(alone));
        }
    }
    let mut keyed = map.clone();
    keyed.insert(secret.to_owned(), json!(secret));
    out.push(Value::Object(keyed));
    out.push(json!([secret]));
    out.push(json!(secret));
    out
}

/// WP-016 AC4, ADR-0010 R6: the pasted redirect address, with a planted code, never appears
/// in any response to `account.login.finish`: not in an `invalid-params` error for any shape
/// of request it could be sent in, not in the `hello-required` refusal, not in a sign-in
/// error, and not in the `Debug` form of the decoded request. The same holds for a sharing
/// link in `mount.add.preview` and `mount.add.confirm`.
#[test]
fn adr_0010_r6_pasted_address_and_link_never_reach_a_response() {
    let cases = [
        (
            method::ACCOUNT_LOGIN_FINISH,
            json!({"attempt_id": "attempt-7f3b", "redirect_url": pasted()}),
            pasted(),
        ),
        (
            method::MOUNT_ADD_PREVIEW,
            json!({"account_id": "acct-work-1", "target": "sharing-link", "sharing_link": LINK}),
            LINK.to_owned(),
        ),
        (
            method::MOUNT_ADD_CONFIRM,
            json!({"account_id": "acct-work-1", "target": "sharing-link", "sharing_link": LINK,
                   "confirmation_token": "c1.0000000000000000.0000000000000000",
                   "mount_point": "/home/user/Example/Finance"}),
            LINK.to_owned(),
        ),
    ];
    let before_hello = Session::new();
    let mut checked = 0;
    for (name, valid, secret) in cases {
        assert!(method::REDACTED.contains(&name));
        let call = MethodCall::parse(name, Some(&valid)).unwrap();
        assert_clean(
            format!("{call:?}").as_bytes(),
            "Debug of the decoded request",
        );
        let err = before_hello.admit(&call).unwrap_err();
        assert_clean(&error_line(err), "hello-required");
        for variant in planted_variants(&valid, &secret) {
            match MethodCall::parse(name, Some(&variant)) {
                Ok(call) => {
                    // Accepted. Either the secret sits in its own redacted field, or a field
                    // that takes any string (an account ID) took it; then it is that field's
                    // value, not a credential, and `unknown-account` does not echo it.
                    let mut params = call.params().unwrap().unwrap();
                    let own = params.as_object_mut().unwrap().remove(
                        if name == method::ACCOUNT_LOGIN_FINISH {
                            "redirect_url"
                        } else {
                            "sharing_link"
                        },
                    );
                    let elsewhere = params.to_string().contains(secret.as_str());
                    if elsewhere {
                        assert!(
                            params["account_id"]
                                .as_str()
                                .is_some_and(|a| a.contains(secret.as_str())),
                            "{params}"
                        );
                        assert_clean(&error_line(RpcError::unknown_account()), "unknown-account");
                    } else {
                        assert!(own.is_some() || !format!("{call:?}").contains(secret.as_str()));
                        assert_clean(format!("{call:?}").as_bytes(), "Debug of a decoded request");
                    }
                }
                Err(err) => {
                    assert_eq!(err.code, -32602, "{name} {variant}");
                    assert_clean(&error_line(err), &format!("{name} error for {variant}"));
                    checked += 1;
                }
            }
        }
        // The value accepted, the daemon may still refuse the sign-in; each refusal is fixed.
        let mut session = Session::new();
        session
            .hello(
                &HelloParams {
                    client_name: "c".into(),
                    protocol_versions: vec![1],
                },
                SUPPORTED_VERSIONS,
                &[],
            )
            .unwrap();
        assert!(session.admit(&call).is_ok());
    }
    assert!(checked > 100, "only {checked} refusals were checked");
    // The validating constructors do not repeat what they refuse.
    for bad in [
        format!("{} ", pasted()),
        format!("{}\t", pasted()),
        "x".repeat(9_000),
    ] {
        let err = PastedRedirect::new(bad).unwrap_err();
        assert_clean(err.to_string().as_bytes(), "PastedRedirect error");
    }
    for bad in [LINK.replace("https", "http"), format!("{LINK} ")] {
        let err = SharingLink::new(bad).unwrap_err();
        assert_clean(err.to_string().as_bytes(), "SharingLink error");
    }
}
