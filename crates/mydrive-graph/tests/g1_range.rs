//! Tests of the gate G1 experiment (`examples/g1_range.rs`), against `mydrive-graph-sim`
//! and a fake sign-in authority: WP-011 AC1 (argument parsing and output formatting), AC2
//! (read scopes only, GET only), AC3 (no token, pre-authenticated URL or account
//! identifier in the report), and the lead's ruling (no Graph request before `yes`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../examples/g1_range.rs"]
#[allow(dead_code)]
mod g1;

#[path = "g1_g5_support.rs"]
mod support;

use std::time::{Duration, UNIX_EPOCH};

use axum::http::StatusCode;
use mydrive_core::ids::DriveId;
use mydrive_graph_sim::{DownloadMode, DriveOptions, DriveType, GraphSim, Origin, RangeMode};
use support::{
    ACCOUNT_NAME, AUTH_CODE, CONSUMER_TID, FakeAuthority, PASTE_REDIRECT, REFRESH_TOKEN, Reply,
    ScriptedTerminal, TestResult, WORK_TID, id_token,
};

use g1::account::{self, AccountKind, Terminal};
use g1::{Args, Command, Report, UsageError, parse_args};

impl Terminal for ScriptedTerminal {
    fn say(&mut self, text: &str) -> std::io::Result<()> {
        self.say_impl(text);
        Ok(())
    }

    fn ask(&mut self, prompt: &str) -> std::io::Result<Option<String>> {
        Ok(self.ask_impl(prompt))
    }
}

const FILE_NAME: &str = "holiday-video-unique-name.bin";
const SIZE: usize = 3 * 1024 * 1024;

fn content() -> Vec<u8> {
    (0..SIZE).map(|n| (n % 251) as u8).collect()
}

struct Setup {
    sim: GraphSim,
    authority: FakeAuthority,
    drive: DriveId,
    item: String,
}

async fn setup(options: DriveOptions, tid: &str) -> Setup {
    let sim = GraphSim::start().await.unwrap();
    let drive = sim.add_drive_with(DriveType::Personal, options).unwrap();
    let root = sim.root_id(&drive).unwrap();
    sim.create_file(&drive, &root, "small.txt", b"tiny".to_vec())
        .unwrap();
    let item = sim
        .create_file(&drive, &root, FILE_NAME, content())
        .unwrap();
    let authority = FakeAuthority::start(sim.token(), tid).await;
    Setup {
        sim,
        authority,
        drive,
        item: item.as_str().to_owned(),
    }
}

fn args(s: &Setup) -> Args {
    match parse_args([
        "--authority",
        s.authority.base(),
        "--graph-base",
        s.sim.base_url(),
        "--drive-id",
        s.drive.as_str(),
        "--redirect-uri",
        PASTE_REDIRECT,
        "--min-size",
        "1048576",
    ]) {
        Ok(Command::Run(a)) => a,
        other => panic!("{other:?}"),
    }
}

async fn run(s: &Setup, term: &mut ScriptedTerminal) -> Report {
    g1::run(&args(s), term, "2026-11-15".to_owned()).await
}

/// Everything that must never appear in a report: tokens, codes, the pre-authenticated
/// URL's origin and key, drive and item IDs, the file name, the tenant, the account name.
fn assert_no_secrets(report: &str, s: &Setup, tid: &str) {
    let origin = s.sim.download_origin().trim_start_matches("http://");
    let forbidden = [
        s.sim.token(),
        REFRESH_TOKEN,
        AUTH_CODE,
        &id_token(tid),
        origin,
        "tempauth",
        s.drive.as_str(),
        &s.item,
        FILE_NAME,
        tid,
        ACCOUNT_NAME,
        "fake-object-id",
    ];
    for f in forbidden {
        assert!(!report.contains(f), "report contains {f:?}:\n{report}");
    }
}

// ---------------------------------------------------------------------------------------
// AC1: argument parsing.

/// AC1: with no options, the defaults: the native-client redirect, the real authority and
/// Graph, and the documented sizes.
#[test]
fn parse_defaults() {
    let Ok(Command::Run(a)) = parse_args(Vec::<String>::new()) else {
        panic!()
    };
    assert_eq!(a, Args::default());
    assert_eq!(a.min_size, 5 * 1024 * 1024);
    assert_eq!(a.range_len, 65536);
    assert_eq!(a.max_pages, 50);
    assert_eq!(
        a.redirect_uri,
        "https://login.microsoftonline.com/common/oauth2/nativeclient"
    );
    assert_eq!(a.authority, "https://login.microsoftonline.com/common/");
    assert_eq!(a.graph_base, "https://graph.microsoft.com/v1.0");
    assert_eq!(a.drive_id, None);
    assert_eq!(a.client_id, None);
}

/// AC1: every option, in both `--name value` and `--name=value` form.
#[test]
fn parse_every_option() {
    let Ok(Command::Run(a)) = parse_args([
        "--min-size=10",
        "--range-len",
        "20",
        "--max-pages=3",
        "--redirect-uri",
        "http://127.0.0.1:5000",
        "--client-id=abc",
        "--authority",
        "http://127.0.0.1:1/t/",
        "--graph-base=http://127.0.0.1:2/v1.0",
        "--drive-id",
        "d1",
    ]) else {
        panic!()
    };
    assert_eq!(a.min_size, 10);
    assert_eq!(a.range_len, 20);
    assert_eq!(a.max_pages, 3);
    assert_eq!(a.redirect_uri, "http://127.0.0.1:5000");
    assert_eq!(a.client_id.as_deref(), Some("abc"));
    assert_eq!(a.authority, "http://127.0.0.1:1/t/");
    assert_eq!(a.graph_base, "http://127.0.0.1:2/v1.0");
    assert_eq!(a.drive_id.as_deref(), Some("d1"));
}

/// AC1: help, unknown options, missing and bad values.
#[test]
fn parse_rejects_bad_command_lines() {
    assert_eq!(parse_args(["--help"]), Ok(Command::Help));
    assert_eq!(parse_args(["--min-size", "1", "-h"]), Ok(Command::Help));
    assert_eq!(
        parse_args(["--write"]),
        Err(UsageError::Unknown("--write".into()))
    );
    assert_eq!(
        parse_args(["--min-size"]),
        Err(UsageError::MissingValue("--min-size"))
    );
    for bad in [
        vec!["--min-size", "x"],
        vec!["--min-size", "0"],
        vec!["--range-len", "0"],
        vec!["--range-len", "16777217"],
        vec!["--max-pages", "0"],
        vec!["--drive-id", " "],
    ] {
        assert!(
            matches!(parse_args(bad.clone()), Err(UsageError::BadValue { .. })),
            "{bad:?}"
        );
    }
    assert!(parse_args(["--range-len", "16777216"]).is_ok());
    assert!(g1::USAGE.contains("--min-size"));
}

/// AC1: the date in the report is the UTC calendar date.
#[test]
fn utc_date_is_the_calendar_date() {
    assert_eq!(g1::utc_date(UNIX_EPOCH), "1970-01-01");
    let t = UNIX_EPOCH + Duration::from_secs(1_794_787_199); // 2026-11-15T23:59:59Z
    assert_eq!(g1::utc_date(t), "2026-11-15");
    let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
    assert_eq!(g1::utc_date(leap), "2000-02-29");
}

/// AC1: the range is taken from the middle of the file, at most 1 MiB in, on a 4 KiB
/// boundary, and cut at the file's end.
#[test]
fn pick_range_stays_inside_the_file() {
    assert_eq!(g1::pick_range(0, 10), None);
    assert_eq!(g1::pick_range(100, 10), Some((0, 9)));
    assert_eq!(g1::pick_range(100, 1000), Some((0, 99)));
    assert_eq!(g1::pick_range(20_000, 100), Some((8192, 8291)));
    assert_eq!(
        g1::pick_range(10 * 1024 * 1024, 65536),
        Some((1024 * 1024, 1024 * 1024 + 65535))
    );
}

// ---------------------------------------------------------------------------------------
// The account check (lead's ruling).

/// The consumer tenant is personal, any other tenant is work; the kind is derived, never
/// typed.
#[test]
fn tid_maps_to_the_account_kind() {
    let personal = account::tenant_from_id_token(&id_token(CONSUMER_TID)).unwrap();
    assert_eq!(personal.kind(), AccountKind::Personal);
    assert_eq!(personal.as_str(), CONSUMER_TID);
    let work = account::tenant_from_id_token(&id_token(WORK_TID)).unwrap();
    assert_eq!(work.kind(), AccountKind::Work);
    let upper = account::tenant_from_id_token(&id_token(&CONSUMER_TID.to_uppercase())).unwrap();
    assert_eq!(upper.kind(), AccountKind::Personal);
}

/// A token without a usable `tid` gives an error, never a guessed kind, and the error
/// does not repeat the token.
#[test]
fn bad_id_tokens_are_refused() {
    use account::ClaimError;
    let t = account::tenant_from_id_token;
    assert_eq!(t("no-dots"), Err(ClaimError::NotAJwt));
    assert_eq!(t("a.b.c.d"), Err(ClaimError::NotAJwt));
    assert_eq!(t("a.!!!.c"), Err(ClaimError::NotBase64));
    assert_eq!(
        t(&support::jwt(&serde_json::json!([1, 2]))),
        Err(ClaimError::NotJson)
    );
    assert_eq!(
        t(&support::jwt(&serde_json::json!({"oid": "x"}))),
        Err(ClaimError::NoTid)
    );
    assert_eq!(
        t(&support::jwt(&serde_json::json!({"tid": 7}))),
        Err(ClaimError::NoTid)
    );
    for bad in [
        "common",
        "9188040d6c674c5bb11236a304b66dad",
        "9188040d-6c67-4c5b-b112-36a304b66daz",
        "\u{1b}[2J",
    ] {
        assert_eq!(
            t(&support::jwt(&serde_json::json!({ "tid": bad }))),
            Err(ClaimError::BadTid),
            "{bad}"
        );
    }
    // A payload with base64 padding is still read.
    let claims = br#"{"tid":"9188040d-6c67-4c5b-b112-36a304b66dad"}"#;
    let padded = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE, claims);
    assert!(t(&format!("e30.{padded}.sig")).is_ok());
}

/// Only the exact answer `yes` goes on; the prompt names the kind and the tenant.
#[test]
fn only_yes_confirms() {
    let tenant = account::tenant_from_id_token(&id_token(WORK_TID)).unwrap();
    for (answer, expected) in [
        (Some("yes\n"), true),
        (Some("  yes  \n"), true),
        (Some("y\n"), false),
        (Some("YES\n"), false),
        (Some("yes please\n"), false),
        (Some("\n"), false),
        (None, false),
    ] {
        let mut term = ScriptedTerminal::new();
        if let Some(a) = answer {
            term = term.then_line(a);
        }
        assert_eq!(
            account::confirm(&mut term, &tenant).unwrap(),
            expected,
            "{answer:?}"
        );
        assert!(term.said.contains("work account"), "{}", term.said);
        assert!(term.said.contains(WORK_TID), "{}", term.said);
        assert_eq!(term.asked, 1);
    }
}

#[test]
fn aadsts_codes_are_extracted_without_the_text() {
    let d = format!(
        "AADSTS50020: User account '{ACCOUNT_NAME}' does not exist. AADSTS50020 again, AADSTS700016."
    );
    assert_eq!(
        account::aadsts_codes(&d),
        vec!["AADSTS50020", "AADSTS700016"]
    );
    assert!(account::aadsts_codes("nothing here AADSTS").is_empty());
    assert_eq!(account::printable("a\u{1b}[31mb\n", 10), "a[31mb");
}

/// Lead's ruling: when the user does not type `yes`, nothing is sent to Graph, and the
/// report says so.
#[tokio::test]
async fn no_graph_request_without_yes() -> TestResult {
    let s = setup(DriveOptions::default(), WORK_TID).await;
    for answer in ["no\n", "y\n", ""] {
        let mut term = ScriptedTerminal::new().then_paste();
        if !answer.is_empty() {
            term = term.then_line(answer);
        }
        let report = run(&s, &mut term).await;
        assert!(!report.completed());
        assert!(!report.confirmed);
        assert_eq!(report.kind, Some(AccountKind::Work));
        assert!(s.sim.requests().is_empty(), "{:?}", s.sim.requests());
        let text = report.render();
        assert!(text.contains("nothing was sent to Graph"), "{text}");
        assert!(text.contains("- account kind: work"), "{text}");
        assert_no_secrets(&text, &s, WORK_TID);
    }
    Ok(())
}

/// Lead's ruling: an ID token without a `tid` stops the run before Graph.
#[tokio::test]
async fn no_tid_stops_before_graph() -> TestResult {
    let s = setup(DriveOptions::default(), CONSUMER_TID).await;
    let mut body = support::granted(s.sim.token(), CONSUMER_TID);
    body["id_token"] = serde_json::Value::String(support::jwt(&serde_json::json!({"oid": "x"})));
    s.authority.push_token_reply(Reply {
        status: StatusCode::OK,
        body,
    });
    let mut term = ScriptedTerminal::new().then_paste().then_line("yes\n");
    let report = run(&s, &mut term).await;
    assert!(report.stopped.as_deref().unwrap().contains("no tid claim"));
    assert_eq!(term.asked, 1, "the confirmation was never asked");
    assert!(s.sim.requests().is_empty());
    Ok(())
}

// ---------------------------------------------------------------------------------------
// AC2: read scopes only, GET only.

/// AC2: the experiment asks for exactly `Files.Read offline_access openid`, in the
/// authorisation URL and in the token request, and nothing that writes.
#[tokio::test]
async fn requests_only_read_scopes() -> TestResult {
    assert_eq!(g1::SCOPES, ["Files.Read", "offline_access", "openid"]);
    let s = setup(DriveOptions::default(), CONSUMER_TID).await;
    let mut term = ScriptedTerminal::new().then_paste().then_line("no\n");
    let _ = run(&s, &mut term).await;
    assert_eq!(
        support::shown_scope(&term.said).as_deref(),
        Some("Files.Read offline_access openid")
    );
    let forms = s.authority.forms();
    assert_eq!(forms.len(), 1);
    let (endpoint, form) = &forms[0];
    assert_eq!(endpoint, "token");
    assert_eq!(form["scope"], "Files.Read offline_access openid");
    assert!(!form.contains_key("client_secret"));
    for f in std::iter::once(&form["scope"]) {
        assert!(!f.contains("Write") && !f.contains("Sites") && !f.contains("User."));
    }
    Ok(())
}

/// ADR-0010 R1: the experiment's sign-in still carries an `S256` PKCE challenge, and the
/// token request carries the verifier.
#[tokio::test]
async fn sign_in_uses_pkce() -> TestResult {
    let s = setup(DriveOptions::default(), CONSUMER_TID).await;
    let mut term = ScriptedTerminal::new().then_paste().then_line("no\n");
    let _ = run(&s, &mut term).await;
    assert!(term.said.contains("code_challenge_method=S256"));
    let (_, form) = &s.authority.forms()[0];
    assert!(form.get("code_verifier").is_some_and(|v| v.len() >= 43));
    Ok(())
}

// ---------------------------------------------------------------------------------------
// The full run against the simulator.

/// AC1, AC2, AC3 and ADR-0011 R10: today's documented behaviour (302 to a
/// pre-authenticated URL that honours `Range`). Way 1 follows the redirect without the
/// bearer token and gets 206; way 2 reports the 302 and stops; way 3 is not tried. Every
/// request is a GET; the report holds no secret or identifier.
#[tokio::test]
async fn redirect_mode_range_honoured() -> TestResult {
    let s = setup(DriveOptions::default(), CONSUMER_TID).await;
    let mut term = ScriptedTerminal::new().then_paste().then_line("yes\n");
    let report = run(&s, &mut term).await;
    let text = report.render();
    assert!(report.completed(), "{text}");
    assert!(term.said.contains("personal account"));
    assert!(term.said.contains(CONSUMER_TID));

    assert!(text.contains("- account kind: personal"), "{text}");
    assert!(
        text.contains("- account confirmed by the user: yes"),
        "{text}"
    );
    assert!(text.contains(&format!("- file: {SIZE} bytes")), "{text}");
    let (start, end) = report.range.unwrap();
    assert_eq!((start, end), (1024 * 1024, 1024 * 1024 + 65535));
    assert!(text.contains("- range requested: bytes=1048576-1114111 (65536 bytes)"));

    let way1 = &report.ways[0];
    assert_eq!(way1.hops.len(), 2);
    assert_eq!(way1.hops[0].status, Some(302));
    assert!(way1.hops[0].bearer);
    let loc = way1.hops[0].location.unwrap();
    assert!(loc.usable && !loc.same_origin && loc.has_query);
    assert_eq!(way1.hops[1].status, Some(206));
    assert!(
        !way1.hops[1].bearer,
        "ADR-0011 R10: no bearer to the redirect host"
    );
    assert!(
        way1.hops[1]
            .headers
            .contains(&("content-range", "bytes 1048576-1114111/3145728".to_owned()))
    );
    let expected = content()[start as usize..=end as usize].to_vec();
    assert_eq!(way1.range_bytes.as_ref(), Some(&expected));

    let way2 = &report.ways[1];
    assert_eq!(way2.hops.len(), 1);
    assert_eq!(way2.hops[0].status, Some(302));
    assert_eq!(way2.outcome(), "302, a redirect, not followed");
    assert_eq!(report.ways[2].outcome(), "not tried: no header documented");

    let reference = report.reference.as_ref().unwrap();
    assert_eq!(reference.hops[1].status, Some(200));
    assert_eq!(reference.range_bytes.as_ref(), Some(&expected));
    let body = reference.hops[1].body.unwrap();
    assert!(!body.complete, "the reference stops at the range end");
    assert!(body.read > end && body.read < SIZE as u64);

    assert!(
        text.contains("Range on /content with the bearer token: answered 302"),
        "{text}"
    );
    assert!(
        text.contains("Range on the redirect target: honoured (206)"),
        "{text}"
    );
    assert!(text.contains("Way 1: Range, redirects followed (bearer token to the Graph origin only): 206, the range; bytes match the reference"), "{text}");
    assert!(text.contains("## Way 3: Range with the header that opts into the post-April-2027 behaviour\n\nnot tried: no header documented"), "{text}");
    assert_no_secrets(&text, &s, CONSUMER_TID);

    let log = s.sim.requests();
    assert!(!log.is_empty());
    for r in &log {
        assert_eq!(r.method, "GET", "AC2: {r:?}");
        if r.origin == Origin::Download {
            assert_eq!(r.header("authorization"), None, "ADR-0011 R10: {r:?}");
        }
    }
    Ok(())
}

/// ADR-0011 R8: every request the experiment sends carries the `ISV|...` User-Agent.
#[tokio::test]
async fn every_request_carries_the_user_agent() -> TestResult {
    let s = setup(DriveOptions::default(), CONSUMER_TID).await;
    let mut term = ScriptedTerminal::new().then_paste().then_line("yes\n");
    let report = run(&s, &mut term).await;
    assert!(report.completed());
    for r in s.sim.requests() {
        assert!(
            r.header("user-agent")
                .is_some_and(|u| u.starts_with("ISV|my-drive|mydrive/")),
            "{r:?}"
        );
    }
    Ok(())
}

/// AC1: the post-April-2027 shape (content straight from `/content`), `Range` honoured.
/// Way 2 then gets 206 itself, with the bearer token, and the summary says so.
#[tokio::test]
async fn direct_mode_range_honoured() -> TestResult {
    let s = setup(
        DriveOptions {
            download: DownloadMode::Direct,
            content_etag_header: true,
            ..DriveOptions::default()
        },
        WORK_TID,
    )
    .await;
    let mut term = ScriptedTerminal::new().then_paste().then_line("yes\n");
    let report = run(&s, &mut term).await;
    let text = report.render();
    assert!(report.completed(), "{text}");
    for way in &report.ways[..2] {
        assert_eq!(way.hops.len(), 1);
        assert_eq!(way.hops[0].status, Some(206));
        assert_eq!(way.hops[0].etag, g1::EtagRelation::SameAsItem);
    }
    assert!(text.contains("- account kind: work"));
    assert!(
        text.contains("Range on /content with the bearer token: answered 206"),
        "{text}"
    );
    assert!(
        text.contains("Content responses carry: ETag header equal to the item's eTag"),
        "{text}"
    );
    assert!(!text.contains("Range on the redirect target"), "{text}");
    assert!(text.contains("Way 2: Range, bearer token, redirects disabled: 206, the range; bytes match the reference"), "{text}");
    assert_no_secrets(&text, &s, WORK_TID);
    Ok(())
}

/// AC1 and ADR-0011 R11: a server that ignores `Range` answers 200; the report says the
/// range was ignored, and the range bytes are taken from offset `start` of the whole body,
/// which still match the reference.
#[tokio::test]
async fn range_ignored_is_reported() -> TestResult {
    let s = setup(
        DriveOptions {
            download: DownloadMode::Direct,
            range: RangeMode::Ignored,
            ..DriveOptions::default()
        },
        CONSUMER_TID,
    )
    .await;
    let mut term = ScriptedTerminal::new().then_paste().then_line("yes\n");
    let report = run(&s, &mut term).await;
    let text = report.render();
    assert_eq!(report.ways[1].hops[0].status, Some(200));
    assert_eq!(
        report.ways[1].outcome(),
        "200, the whole file: Range was ignored"
    );
    let (start, end) = report.range.unwrap();
    assert_eq!(
        report.ways[1].range_bytes.as_deref(),
        Some(&content()[start as usize..=end as usize])
    );
    assert!(
        text.contains("answered 200: content directly, Range ignored"),
        "{text}"
    );
    assert!(text.contains("bytes match the reference"), "{text}");
    assert!(
        text.contains("Content responses carry: no ETag header"),
        "{text}"
    );
    Ok(())
}

/// AC1: with no file large enough, the run stops with a plain reason and requests no
/// content.
#[tokio::test]
async fn no_large_file_stops() -> TestResult {
    let s = setup(DriveOptions::default(), CONSUMER_TID).await;
    let mut a = args(&s);
    a.min_size = 100 * 1024 * 1024;
    let mut term = ScriptedTerminal::new().then_paste().then_line("yes\n");
    let report = g1::run(&a, &mut term, "2026-11-15".into()).await;
    let text = report.render();
    assert!(
        text.contains("Stopped: no file of at least 104857600 bytes in the drive"),
        "{text}"
    );
    assert!(
        s.sim
            .requests()
            .iter()
            .all(|r| !r.path.ends_with("/content"))
    );
    Ok(())
}

/// AC1: delta paging stops at `--max-pages`.
#[tokio::test]
async fn max_pages_bounds_the_search() -> TestResult {
    let s = setup(
        DriveOptions {
            delta_page_size: 1,
            ..DriveOptions::default()
        },
        CONSUMER_TID,
    )
    .await;
    let mut a = args(&s);
    a.min_size = 100 * 1024 * 1024;
    a.max_pages = 2;
    let mut term = ScriptedTerminal::new().then_paste().then_line("yes\n");
    let report = g1::run(&a, &mut term, "2026-11-15".into()).await;
    assert_eq!(
        report.stopped.as_deref(),
        Some("no file of at least 104857600 bytes in the first 2 delta pages")
    );
    let deltas = s
        .sim
        .requests()
        .iter()
        .filter(|r| r.path.contains("delta"))
        .count();
    assert_eq!(deltas, 2);
    Ok(())
}

/// AC3: a refused sign-in is reported by its codes only. The description, which can name
/// the account, goes to the terminal and never into the report.
#[tokio::test]
async fn refused_sign_in_reports_codes_only() -> TestResult {
    let s = setup(DriveOptions::default(), CONSUMER_TID).await;
    s.authority.push_token_reply(Reply::error(
        StatusCode::BAD_REQUEST,
        "invalid_grant",
        &format!("AADSTS65001: The user {ACCOUNT_NAME} has not consented. Trace ID: abc"),
    ));
    let mut term = ScriptedTerminal::new().then_paste();
    let report = run(&s, &mut term).await;
    let text = report.render();
    assert!(
        text.contains("- sign-in: token request refused (HTTP 400): invalid_grant AADSTS65001"),
        "{text}"
    );
    assert!(text.contains("Stopped: sign-in did not complete"), "{text}");
    assert_no_secrets(&text, &s, CONSUMER_TID);
    assert!(
        term.said.contains(ACCOUNT_NAME),
        "the description is on the terminal"
    );
    assert!(s.sim.requests().is_empty());
    Ok(())
}

/// AC1: a pasted address that is not the redirect leaves the attempt pending, so the user
/// can paste again.
#[tokio::test]
async fn a_bad_paste_can_be_retried() -> TestResult {
    let s = setup(DriveOptions::default(), CONSUMER_TID).await;
    let mut term = ScriptedTerminal::new()
        .then_line("not an address\n")
        .then_paste()
        .then_line("no\n");
    let report = run(&s, &mut term).await;
    assert_eq!(report.sign_in.as_deref(), Some("completed"));
    assert!(term.said.contains("Try again"));
    Ok(())
}

/// Without `--drive-id` the drive is looked up with `GET /me/drive`. The simulator has no
/// `/me` routes until WP-023, so this checks the failure is reported plainly; the success
/// path is untested end to end until then.
#[tokio::test]
async fn me_drive_failure_is_reported() -> TestResult {
    let s = setup(DriveOptions::default(), CONSUMER_TID).await;
    let mut a = args(&s);
    a.drive_id = None;
    let mut term = ScriptedTerminal::new().then_paste().then_line("yes\n");
    let report = g1::run(&a, &mut term, "2026-11-15".into()).await;
    let text = report.render();
    assert!(
        text.contains("- drive: GET /me/drive answered HTTP 404 (itemNotFound)"),
        "{text}"
    );
    let log = s.sim.requests();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].path, "/v1.0/me/drive");
    assert_eq!(log[0].method, "GET");
    Ok(())
}

/// AC1: output formatting of a fixed report, independent of the network.
#[test]
fn render_fixed_report() {
    let hop = g1::Hop {
        target: g1::Target::OtherOrigin,
        bearer: false,
        range: Some("bytes=0-9".into()),
        status: Some(206),
        error: None,
        headers: vec![("content-range", "bytes 0-9/100".into())],
        etag: g1::EtagRelation::Differs,
        location: None,
        error_codes: vec![],
        body: Some(g1::BodyInfo {
            read: 10,
            complete: true,
            failed: false,
        }),
    };
    let report = Report {
        date: "2026-11-15".into(),
        sign_in: Some("completed".into()),
        scopes_granted: Some("Files.Read openid".into()),
        kind: Some(AccountKind::Personal),
        confirmed: true,
        range: Some((0, 9)),
        ways: vec![g1::Way {
            title: "Way 1: test",
            hops: vec![hop],
            not_tried: None,
            range_bytes: Some(vec![1; 10]),
        }],
        ..Report::default()
    };
    let text = report.render();
    let expected_lines = [
        "# G1 report",
        "- date (UTC): 2026-11-15",
        "- scopes requested: Files.Read offline_access openid",
        "- sign-in: completed",
        "- scopes granted: Files.Read openid",
        "- account kind: personal",
        "- range requested: bytes=0-9 (10 bytes)",
        "## Way 1: test",
        "1. GET to another origin; bearer token sent: no; Range: bytes=0-9 -> 206",
        "   - content-range: bytes 0-9/100",
        "   - etag: present, differs from the item's eTag",
        "   - body: 10 bytes read, to its end",
        "Result: 206, the range; no reference bytes to compare with",
        "- Range on /content with the bearer token: answered 206: Range honoured on /content itself",
        "- Range on the redirect target: honoured (206)",
    ];
    for line in expected_lines {
        assert!(
            text.lines().any(|l| l == line),
            "missing {line:?} in\n{text}"
        );
    }
    assert!(report.completed());
}

/// AC1: the `/me/drive` body gives the drive ID (used, never reported) and `driveType`.
/// Tested on fixed input: the simulator has no `/me` routes until WP-023.
#[test]
fn me_drive_body_is_parsed() {
    let (id, kind) = g1::parse_my_drive(br#"{"id":"B!AbC","driveType":"personal"}"#).unwrap();
    assert_eq!(id.as_str(), "b!abc");
    assert_eq!(kind, "personal");
    let (_, kind) = g1::parse_my_drive(br#"{"id":"x"}"#).unwrap();
    assert_eq!(kind, "not stated");
    assert!(g1::parse_my_drive(b"nope").is_err());
    assert!(g1::parse_my_drive(br#"{"driveType":"business"}"#).is_err());
    assert!(g1::parse_my_drive(br#"{"id":""}"#).is_err());
}
