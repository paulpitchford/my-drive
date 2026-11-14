//! Runs the shell and Python cursor loops from `docs/protocol/README.md` against a small
//! fake daemon: a full listing over several pages, a daemon that closes the connection in
//! the middle of the loop, a reply that is not JSON, and a refused `hello`. Each client must
//! finish, fail cleanly with a clear message, and never spin.
//!
//! Needs `bash`, `jq`, `socat`, `python3` and `timeout`; without them the test is skipped
//! with a message saying which tool is missing. Set `MYDRIVE_REQUIRE_README_TOOLS=1` to turn
//! that skip into a failure; CI does this, so the examples cannot go unchecked there.

use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The fake daemon: serves one connection on a Unix socket, then exits. It ends itself with
/// an alarm after the bound (argument 3), so no wait on it can hang.
const FAKE_DAEMON: &str = r#"
import json, signal, socket, sys

path, mode, bound = sys.argv[1], sys.argv[2], int(sys.argv[3])
signal.alarm(bound)  # the default action of SIGALRM ends the process
srv = socket.socket(socket.AF_UNIX)
srv.bind(path)
srv.listen(1)
print("ready", flush=True)
conn, _ = srv.accept()
lines = conn.makefile("rb")
TOTAL, PAGE = 1000, 300
pages = 0

def send(obj):
    conn.sendall((json.dumps(obj, separators=(",", ":")) + "\n").encode())

for raw in lines:
    req = json.loads(raw)
    rid = req.get("id")
    if req["method"] == "hello":
        if mode == "hello-error":
            send({"jsonrpc": "2.0", "id": rid,
                  "error": {"code": 1009, "message": "no protocol version in common"}})
        else:
            send({"jsonrpc": "2.0", "id": rid,
                  "result": {"protocol_version": 1, "capabilities": [], "limits": {}}})
        continue
    pages += 1
    if mode == "close-mid" and pages == 2:
        conn.close()
        break
    if mode == "not-json" and pages == 2:
        conn.sendall(b"this is not JSON\n")
        continue
    params = req["params"]
    start = int(params["cursor"][1:]) if params.get("cursor") else 0
    end = min(start + PAGE, TOTAL)
    item = {"status": "online-only", "modifiers": [], "hydration": "placeholder",
            "pin": "inherit", "effective_pin": "unpinned", "sync": "in-sync"}
    result = {"entries": [{"path": f"{params['directory']}/f{i:04}", "item": item}
                          for i in range(start, end)]}
    if end < TOTAL:
        result["next_cursor"] = f"c{end}"
    send({"jsonrpc": "2.0", "id": rid, "result": result})
"#;

/// Environment variable that turns "a tool is missing" from a skip into a failure.
const REQUIRE_ENV: &str = "MYDRIVE_REQUIRE_README_TOOLS";

/// How long any one process of the test may run before it is killed, in seconds. Every
/// wait in the test is bounded by it, so a stuck client or daemon fails instead of hanging.
const BOUND_SECS: &str = "30";

/// Returns true if the test should run. Skips loudly, or fails if `REQUIRE_ENV` is `1`.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn tools_available_or_skip(test: &str) -> bool {
    let Some(tool) = missing_tool() else {
        return true;
    };
    assert!(
        std::env::var_os(REQUIRE_ENV).is_none_or(|value| value != "1"),
        "{test}: {REQUIRE_ENV}=1 but `{tool}` is not installed"
    );
    // Written to the real stderr, so the skip shows even when the harness captures output.
    let _ = writeln!(
        io::stderr().lock(),
        "SKIPPED {test}: `{tool}` is not installed. Install it or set {REQUIRE_ENV}=1 to make \
         this a failure."
    );
    false
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn missing_tool() -> Option<&'static str> {
    let probes: [(&str, &str); 5] = [
        ("bash", "--version"),
        ("jq", "--version"),
        ("socat", "-V"),
        ("python3", "--version"),
        ("timeout", "--version"),
    ];
    probes.into_iter().find_map(|(tool, arg)| {
        let ok = Command::new(tool)
            .arg(arg)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        (!ok).then_some(tool)
    })
}

/// The first fenced block of `language` in the catalogue.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn catalogue_block(language: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/protocol/README.md");
    let text = std::fs::read_to_string(path).expect("read docs/protocol/README.md");
    let fence = format!("```{language}\n");
    let start = text
        .find(&fence)
        .unwrap_or_else(|| panic!("no ```{language} block"));
    let body = &text[start + fence.len()..];
    body[..body.find("\n```").expect("closed block") + 1].to_owned()
}

/// Starts the fake daemon in `mode`, runs `client` against it, and returns its output.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn run(work: &Path, client: &[&str], mode: &str) -> Output {
    let runtime = work.join(format!("run-{mode}-{}", client[0]));
    std::fs::create_dir_all(runtime.join("mydrive")).unwrap();
    let socket = runtime.join("mydrive").join("daemon.sock");
    // The daemon bounds its own life with an alarm, so the read of "ready" below ends too.
    let mut daemon = Command::new("python3")
        .arg("-I")
        .arg(work.join("fake_daemon.py"))
        .arg(&socket)
        .arg(mode)
        .arg(BOUND_SECS)
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the fake daemon");
    let mut ready = String::new();
    BufReader::new(daemon.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready.trim(), "ready");
    let output = Command::new("timeout")
        .arg(BOUND_SECS)
        .args(client)
        .arg("/home/user/OneDrive/Pictures")
        .env("XDG_RUNTIME_DIR", &runtime)
        .current_dir(work)
        .output()
        .expect("run the client");
    let _ = daemon.kill();
    let _ = daemon.wait();
    output
}

/// Checks that a client run failed cleanly: it ended by itself, with a failure status, a
/// clear message on stderr, and no Python traceback.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn assert_fails_cleanly(out: &Output, client: &str, case: &str, message: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        out.status.code(),
        Some(124),
        "{client}, {case}: spun until the timeout"
    );
    assert!(!out.status.success(), "{client}, {case}: must fail");
    assert!(
        stderr.contains(message),
        "{client}, {case}: unclear failure, expected {message:?}: {stderr}"
    );
    assert!(
        !stderr.contains("Traceback"),
        "{client}, {case}: crashed instead of reporting: {stderr}"
    );
}

#[test]
fn readme_cursor_loops_run_and_fail_cleanly() {
    if !tools_available_or_skip("readme_cursor_loops_run_and_fail_cleanly") {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    std::fs::write(work.path().join("fake_daemon.py"), FAKE_DAEMON).unwrap();
    std::fs::write(work.path().join("list-dir.sh"), catalogue_block("sh")).unwrap();
    std::fs::write(work.path().join("list_dir.py"), catalogue_block("python")).unwrap();
    // The shell loop streams each page as it arrives; the Python loop prints only a complete
    // listing. So after a failure on page two the shell has printed page one (which is why
    // the catalogue tells consumers to check its exit status) and Python has printed nothing.
    let clients: [(&str, Vec<&str>, usize); 2] = [
        ("shell", vec!["bash", "list-dir.sh"], 300),
        ("python", vec!["python3", "-I", "list_dir.py"], 0),
    ];
    for (name, client, printed_before_failure) in &clients {
        let out = run(work.path(), client, "ok");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{name}: listing failed: {stderr}");
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 1000, "{name}: every entry over every page");
        assert!(
            lines[999].contains("/home/user/OneDrive/Pictures/f0999"),
            "{name}"
        );

        for (mode, message) in [
            ("close-mid", "connection closed"),
            ("not-json", "unreadable reply"),
        ] {
            let out = run(work.path(), client, mode);
            assert_fails_cleanly(&out, name, mode, message);
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).lines().count(),
                *printed_before_failure,
                "{name}, {mode}: entries printed before the failure"
            );
        }

        let out = run(work.path(), client, "hello-error");
        assert_fails_cleanly(&out, name, "hello-error", "error 1009");
    }
}
