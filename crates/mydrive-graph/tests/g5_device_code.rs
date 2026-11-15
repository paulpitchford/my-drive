//! Tests of the gate G5 experiment (`examples/g5_device_code.rs`), against a fake sign-in
//! authority and `mydrive-graph-sim`: WP-011 AC1 (argument parsing and output
//! formatting), AC2 (read scopes only, GET only), AC3 (no token, code or account
//! identifier in the report), and the lead's ruling (no Graph request before `yes`, also
//! after a device-code sign-in).

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../examples/g5_device_code.rs"]
#[allow(dead_code)]
mod g5;

#[path = "g1_g5_support.rs"]
mod support;

use std::time::Duration;

use axum::http::StatusCode;
use mydrive_graph_sim::GraphSim;
use serde_json::json;
use support::{
    ACCOUNT_NAME, CONSUMER_TID, DEVICE_CODE, FakeAuthority, REFRESH_TOKEN, Reply, ScriptedTerminal,
    TestResult, USER_CODE, WORK_TID, id_token,
};

use g5::g1::account::{AccountKind, Terminal};
use g5::{Args, Command, PollTiming, Report, parse_args};

impl Terminal for ScriptedTerminal {
    fn say(&mut self, text: &str) -> std::io::Result<()> {
        self.say_impl(text);
        Ok(())
    }

    fn ask(&mut self, prompt: &str) -> std::io::Result<Option<String>> {
        Ok(self.ask_impl(prompt))
    }
}

const FAST: PollTiming = PollTiming {
    default_interval: Duration::ZERO,
    slow_down_step: Duration::ZERO,
};

struct Setup {
    sim: GraphSim,
    authority: FakeAuthority,
}

async fn setup(tid: &str) -> Setup {
    let sim = GraphSim::start().await.unwrap();
    let authority = FakeAuthority::start(sim.token(), tid).await;
    Setup { sim, authority }
}

fn args(s: &Setup) -> Args {
    match parse_args([
        "--authority",
        s.authority.base(),
        "--graph-base",
        s.sim.base_url(),
    ]) {
        Ok(Command::Run(a)) => a,
        other => panic!("{other:?}"),
    }
}

async fn run(s: &Setup, term: &mut ScriptedTerminal) -> Report {
    g5::run(&args(s), FAST, term, "2026-11-15".to_owned()).await
}

fn pending() -> Reply {
    Reply::error(
        StatusCode::BAD_REQUEST,
        "authorization_pending",
        "AADSTS70016: pending",
    )
}

fn assert_no_secrets(report: &str, s: &Setup, tid: &str) {
    for f in [
        s.sim.token(),
        REFRESH_TOKEN,
        DEVICE_CODE,
        USER_CODE,
        &id_token(tid),
        tid,
        ACCOUNT_NAME,
        "fake-object-id",
        "login.example.invalid",
    ] {
        assert!(!report.contains(f), "report contains {f:?}:\n{report}");
    }
}

/// AC1: defaults, every option, help and bad command lines.
#[test]
fn parse_options() {
    let Ok(Command::Run(a)) = parse_args(Vec::<String>::new()) else {
        panic!()
    };
    assert_eq!(a, Args::default());
    assert_eq!(a.authority, "https://login.microsoftonline.com/common/");
    assert_eq!(a.graph_base, "https://graph.microsoft.com/v1.0");
    let Ok(Command::Run(a)) = parse_args([
        "--client-id=abc",
        "--authority",
        "http://127.0.0.1:1/t/",
        "--graph-base=http://127.0.0.1:2/v1.0",
    ]) else {
        panic!()
    };
    assert_eq!(a.client_id.as_deref(), Some("abc"));
    assert_eq!(a.authority, "http://127.0.0.1:1/t/");
    assert_eq!(a.graph_base, "http://127.0.0.1:2/v1.0");
    assert_eq!(parse_args(["--help"]), Ok(Command::Help));
    assert!(parse_args(["--min-size", "1"]).is_err());
    assert!(parse_args(["--authority"]).is_err());
    assert_eq!(
        g5::PollTiming::default().default_interval,
        Duration::from_secs(5)
    );
    assert_eq!(
        g5::PollTiming::default().slow_down_step,
        Duration::from_secs(5)
    );
}

/// AC2: the device-code request asks for exactly `Files.Read offline_access openid`,
/// sends no secret, and the poll uses the RFC 8628 grant.
#[tokio::test]
async fn requests_only_read_scopes() -> TestResult {
    let s = setup(CONSUMER_TID).await;
    let mut term = ScriptedTerminal::new().then_line("no\n");
    let _ = run(&s, &mut term).await;
    let forms = s.authority.forms();
    let (endpoint, form) = &forms[0];
    assert_eq!(endpoint, "devicecode");
    assert_eq!(form["scope"], "Files.Read offline_access openid");
    assert!(!form.contains_key("client_secret"));
    let (endpoint, form) = &forms[1];
    assert_eq!(endpoint, "token");
    assert_eq!(
        form["grant_type"],
        "urn:ietf:params:oauth:grant-type:device_code"
    );
    assert_eq!(form["device_code"], DEVICE_CODE);
    assert!(!form.contains_key("client_secret"));
    Ok(())
}

/// AC1, AC2, AC3 and the lead's ruling: pending, slow_down, then tokens. The user code is
/// shown on the terminal, the account kind and tenant are shown and confirmed, and only
/// then is one GET sent to Graph. The report holds no token, code or identifier.
#[tokio::test]
async fn full_run_personal() -> TestResult {
    let s = setup(CONSUMER_TID).await;
    s.authority.push_token_reply(pending());
    s.authority
        .push_token_reply(Reply::error(StatusCode::BAD_REQUEST, "slow_down", "slow"));
    s.authority.push_token_reply(pending());
    let mut term = ScriptedTerminal::new().then(|said, _| {
        // Before the confirmation, nothing may have reached Graph; checked below via
        // the log, and here by the order of what was said.
        assert!(said.contains("Signed in to a personal account"));
        Some("yes\n".to_owned())
    });
    let report = run(&s, &mut term).await;
    let text = report.render();
    assert!(report.completed(), "{text}");
    assert!(term.said.contains(USER_CODE), "{}", term.said);
    assert!(term.said.contains("https://login.example.invalid/device"));
    assert!(term.said.contains(CONSUMER_TID));
    assert_eq!((report.pending, report.slow_down), (2, 1));
    for line in [
        "# G5 report",
        "- date (UTC): 2026-11-15",
        "- scopes requested: Files.Read offline_access openid",
        "- device-code request: accepted",
        "- polls: 2 authorization_pending, 1 slow_down",
        "- sign-in: completed",
        "- tokens returned: access yes, refresh yes, ID yes",
        "- scopes granted: Files.Read openid offline_access",
        "- account kind: personal",
        "- account confirmed by the user: yes",
        // The simulator has no /me routes until WP-023.
        "- Graph check: GET /me/drive answered HTTP 404 (itemNotFound)",
    ] {
        assert!(
            text.lines().any(|l| l == line),
            "missing {line:?} in\n{text}"
        );
    }
    assert_no_secrets(&text, &s, CONSUMER_TID);
    let log = s.sim.requests();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].method, "GET");
    assert_eq!(log[0].path, "/v1.0/me/drive");
    let bearer = format!("Bearer {}", s.sim.token());
    assert_eq!(log[0].header("authorization"), Some(bearer.as_str()));
    Ok(())
}

/// Lead's ruling: after a device-code sign-in, nothing reaches Graph unless the user types
/// `yes`; a work tenant is shown as work.
#[tokio::test]
async fn no_graph_request_without_yes() -> TestResult {
    let s = setup(WORK_TID).await;
    for answer in [Some("no\n"), Some("Yes\n"), None] {
        let mut term = ScriptedTerminal::new();
        if let Some(a) = answer {
            term = term.then_line(a);
        }
        let report = run(&s, &mut term).await;
        assert_eq!(report.kind, Some(AccountKind::Work));
        assert!(!report.confirmed);
        assert!(term.said.contains("work account"));
        assert!(term.said.contains(WORK_TID));
        let text = report.render();
        assert!(text.contains("nothing was sent to Graph"), "{text}");
        assert_no_secrets(&text, &s, WORK_TID);
    }
    assert!(s.sim.requests().is_empty());
    Ok(())
}

/// AC1: a Graph that refuses the token is reported by status and code.
#[tokio::test]
async fn graph_refusal_is_reported() -> TestResult {
    let s = setup(CONSUMER_TID).await;
    s.sim.revoke_token(s.sim.token());
    let mut term = ScriptedTerminal::new().then_line("yes\n");
    let report = run(&s, &mut term).await;
    assert_eq!(
        report.graph.as_deref(),
        Some("GET /me/drive answered HTTP 401 (InvalidAuthenticationToken)")
    );
    Ok(())
}

/// AC1 and AC3: the device-code request refused (as abraunegg reports for personal
/// accounts) is reported by its codes; the description, which can name the account, goes
/// only to the terminal.
#[tokio::test]
async fn refused_device_code_reports_codes_only() -> TestResult {
    let s = setup(CONSUMER_TID).await;
    s.authority.set_device_reply(Reply {
        status: StatusCode::BAD_REQUEST,
        body: json!({
            "error": "unauthorized_client",
            "error_description": format!("AADSTS70002: The client is not authorised for {ACCOUNT_NAME}."),
            "error_codes": [70002, 50059],
        }),
    });
    let mut term = ScriptedTerminal::new();
    let report = run(&s, &mut term).await;
    let text = report.render();
    assert!(
        text.contains(
            "- device-code request: refused: HTTP 400 unauthorized_client AADSTS70002 AADSTS50059"
        ),
        "{text}"
    );
    assert!(text.contains("Stopped: the identity platform refused the device-code request"));
    assert_no_secrets(&text, &s, CONSUMER_TID);
    assert!(term.said.contains(ACCOUNT_NAME));
    assert_eq!(s.authority.forms().len(), 1, "no poll after a refusal");
    assert!(s.sim.requests().is_empty());
    Ok(())
}

/// AC1: each terminal poll error has its own plain outcome, and nothing reaches Graph.
#[tokio::test]
async fn poll_outcomes() -> TestResult {
    for (error, expected) in [
        ("authorization_declined", "declined by the user"),
        ("expired_token", "the code expired before sign-in finished"),
        (
            "bad_verification_code",
            "refused: HTTP 400 bad_verification_code AADSTS50126",
        ),
        (
            "invalid_grant",
            "refused: HTTP 400 invalid_grant AADSTS50126",
        ),
    ] {
        let s = setup(CONSUMER_TID).await;
        s.authority.push_token_reply(pending());
        s.authority.push_token_reply(Reply::error(
            StatusCode::BAD_REQUEST,
            error,
            &format!("AADSTS50126: bad for {ACCOUNT_NAME}"),
        ));
        let mut term = ScriptedTerminal::new().then_line("yes\n");
        let report = run(&s, &mut term).await;
        let text = report.render();
        assert_eq!(report.sign_in.as_deref(), Some(expected), "{text}");
        assert_eq!(term.asked, 0, "no confirmation without tokens");
        assert_no_secrets(&text, &s, CONSUMER_TID);
        assert!(s.sim.requests().is_empty());
    }
    Ok(())
}

/// AC1: a code whose lifetime has ended stops the poll.
#[tokio::test]
async fn expired_code_times_out() -> TestResult {
    let s = setup(CONSUMER_TID).await;
    s.authority.set_device_reply(Reply {
        status: StatusCode::OK,
        body: json!({
            "device_code": DEVICE_CODE,
            "user_code": USER_CODE,
            "verification_uri": "https://login.example.invalid/device",
            "expires_in": 0,
            "interval": 0,
        }),
    });
    let mut term = ScriptedTerminal::new();
    let report = run(&s, &mut term).await;
    assert_eq!(
        report.sign_in.as_deref(),
        Some("timed out: the code's lifetime ended")
    );
    assert_eq!(s.authority.forms().len(), 1, "no poll after the lifetime");
    Ok(())
}

/// AC1: tokens without an ID token stop before the confirmation and before Graph.
#[tokio::test]
async fn no_id_token_stops() -> TestResult {
    let s = setup(CONSUMER_TID).await;
    let mut body = support::granted(s.sim.token(), CONSUMER_TID);
    body.as_object_mut().unwrap().remove("id_token");
    s.authority.push_token_reply(Reply {
        status: StatusCode::OK,
        body,
    });
    let mut term = ScriptedTerminal::new().then_line("yes\n");
    let report = run(&s, &mut term).await;
    let text = report.render();
    assert!(
        text.contains("- tokens returned: access yes, refresh yes, ID no"),
        "{text}"
    );
    assert!(report.stopped.as_deref().unwrap().contains("no ID token"));
    assert_eq!(term.asked, 0);
    assert!(s.sim.requests().is_empty());
    Ok(())
}

/// ADR-0010 R6: the granted tokens never show through `Debug`.
#[test]
fn granted_debug_is_redacted() {
    let g = g5::Granted {
        access_token: mydrive_graph::auth::AccessToken::new("at-SECRET"),
        refresh_token: Some(mydrive_graph::auth::RefreshToken::new("rt-SECRET")),
        id_token: Some(mydrive_graph::auth::IdToken::new("id-SECRET")),
        scope: Some("Files.Read".into()),
    };
    let shown = format!("{g:?}");
    assert!(!shown.contains("SECRET"), "{shown}");
}
