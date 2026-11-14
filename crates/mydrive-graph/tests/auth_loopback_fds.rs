//! Second review item 2: running out of file descriptors while a sign-in waits does not
//! end the attempt (`accept` fails with `EMFILE`; the listener pauses and retries).
//!
//! The test re-runs this binary in a child process with a low descriptor limit, set with
//! the shell's `ulimit -n`, so it cannot disturb other tests. It is its own binary for
//! that reason.

#[path = "auth_support.rs"]
mod support;

use std::time::Duration;

use mydrive_graph::auth::{LoopbackSignIn, Scopes};
use support::{FakeAuthority, TestResult, browser_get, query};

const CHILD: &str = "MYDRIVE_TEST_FD_CHILD";

#[test]
fn accept_errors_do_not_end_the_sign_in() -> TestResult {
    if std::env::var_os(CHILD).is_some() {
        return child();
    }
    let exe = std::env::current_exe()?;
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(
            "ulimit -n 128 && exec \"$0\" --exact accept_errors_do_not_end_the_sign_in --nocapture",
        )
        .arg(exe)
        .env(CHILD, "1")
        .status()?;
    assert!(status.success(), "the child run failed: {status}");
    Ok(())
}

fn child() -> TestResult {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let fake = FakeAuthority::start().await?;
        let sign_in = LoopbackSignIn::start(&fake.config(), Scopes::initial_sign_in()).await?;
        let port = sign_in.local_addr().port();
        let state = query(sign_in.authorization_url())["state"].clone();
        let client = fake.client();
        let attempt =
            tokio::spawn(async move { sign_in.complete(&client, Duration::from_secs(20)).await });
        tokio::task::yield_now().await;

        // Use up every descriptor but one, connect with that one, and let the listener's
        // `accept` fail with EMFILE for a while.
        let mut hog = Vec::new();
        while let Ok(f) = std::fs::File::open("/dev/null") {
            hog.push(f);
            if hog.len() > 100_000 {
                return Err("the descriptor limit was not applied".into());
            }
        }
        drop(hog.pop());
        let pending = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
        for _ in 0..5 {
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        assert!(
            !attempt.is_finished(),
            "the attempt ended while accept failed"
        );
        drop(hog);
        drop(pending);

        let page = browser_get(port, &format!("/?code=real&state={state}")).await?;
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        attempt.await??;
        Ok(())
    })
}
