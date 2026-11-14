//! Runs the shell and Python cursor loops from `docs/protocol/README.md` against a small
//! fake daemon: a full listing over several pages, a daemon that closes the connection in
//! the middle of the loop, and a refused `hello`. Each client must finish, fail cleanly with
//! a clear message, and never spin.
//!
//! Needs `bash`, `jq`, `socat`, `python3` and `timeout`; without them the test is skipped
//! with a message saying which tool is missing.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The fake daemon: serves one connection on a Unix socket, then exits.
const FAKE_DAEMON: &str = r#"
import json, socket, sys

path, mode = sys.argv[1], sys.argv[2]
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
    let mut daemon = Command::new("python3")
        .arg("-I")
        .arg(work.join("fake_daemon.py"))
        .arg(&socket)
        .arg(mode)
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the fake daemon");
    let mut ready = String::new();
    BufReader::new(daemon.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready.trim(), "ready");
    let output = Command::new("timeout")
        .arg("30")
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

#[test]
fn readme_cursor_loops_run_and_fail_cleanly() {
    if let Some(tool) = missing_tool() {
        eprintln!("skipping readme_cursor_loops_run_and_fail_cleanly: `{tool}` is not installed");
        return;
    }
    let work = tempfile::tempdir().unwrap();
    std::fs::write(work.path().join("fake_daemon.py"), FAKE_DAEMON).unwrap();
    std::fs::write(work.path().join("list-dir.sh"), catalogue_block("sh")).unwrap();
    std::fs::write(work.path().join("list_dir.py"), catalogue_block("python")).unwrap();
    let clients: [(&str, Vec<&str>); 2] = [
        ("shell", vec!["bash", "list-dir.sh"]),
        ("python", vec!["python3", "-I", "list_dir.py"]),
    ];
    for (name, client) in &clients {
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

        let out = run(work.path(), client, "close-mid");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_ne!(
            out.status.code(),
            Some(124),
            "{name}: spun until the timeout"
        );
        assert!(
            !out.status.success(),
            "{name}: a closed connection must fail"
        );
        assert!(
            stderr.contains("connection closed"),
            "{name}: unclear failure on close: {stderr}"
        );

        let out = run(work.path(), client, "hello-error");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_ne!(
            out.status.code(),
            Some(124),
            "{name}: spun until the timeout"
        );
        assert!(!out.status.success(), "{name}: a refused hello must fail");
        assert!(
            stderr.contains("error 1009"),
            "{name}: unclear failure on hello: {stderr}"
        );
    }
}
