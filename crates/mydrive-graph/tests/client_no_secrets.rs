//! AC7: tokens and pre-authenticated URLs never appear in `Debug` output, error messages or
//! log events of the Graph client.
//!
//! This file is its own test binary with a single test, so it can install a *global*
//! subscriber before any tracing callsite is reached (a per-thread subscriber misses
//! events whose callsites another thread registered first). The test also checks that it
//! captured the client's events, so it cannot pass by capturing nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "client_support.rs"]
mod support;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use mydrive_graph::client::ByteRange;
use mydrive_graph_sim::{DownloadMode, DriveOptions, DriveType, Fault, GraphSim, StatusCode};
use support::{FakeTokens, TestResult, client_with, full_delta, read_all};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

const STALE: &str = "stale-secret-token";

fn lock(m: &Mutex<String>) -> MutexGuard<'_, String> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A subscriber that writes every field of every span and event, with its target and
/// level, into one string.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<String>>);

struct Visitor<'a>(&'a mut String);

impl Visit for Visitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }
}

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, span: &Attributes<'_>) -> Id {
        let mut out = lock(&self.0);
        let _ = write!(out, "\n[span {}] ", span.metadata().target());
        span.record(&mut Visitor(&mut out));
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, values: &Record<'_>) {
        values.record(&mut Visitor(&mut lock(&self.0)));
    }
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut out = lock(&self.0);
        let _ = write!(out, "\n[{}] ", event.metadata().target());
        event.record(&mut Visitor(&mut out));
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

/// Exercises every client path that handles a token or a pre-authenticated URL and
/// returns the `Debug` and `Display` text of everything it got back.
async fn scenario(sim: &GraphSim) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut seen = String::new();
    let redirect = sim.add_drive(DriveType::Personal)?;
    let direct = sim.add_drive_with(
        DriveType::Business,
        DriveOptions {
            download: DownloadMode::Direct,
            ..DriveOptions::default()
        },
    )?;
    let r_root = sim.root_id(&redirect)?;
    let d_root = sim.root_id(&direct)?;
    let r_file = sim.create_file(&redirect, &r_root, "r", vec![1; 4096])?;
    let d_file = sim.create_file(&direct, &d_root, "d", vec![2; 4096])?;

    // A stale token refreshed through a 401, on a redirected download.
    let tokens = FakeTokens::new(STALE);
    tokens.refresh_to(sim.token());
    let client = client_with(sim, tokens);
    let _ = writeln!(seen, "{client:?}");
    let download = client
        .download(&redirect, &r_file, 4096, ByteRange::new(0, 100))
        .await?;
    let _ = writeln!(seen, "{download:?}");
    read_all(download).await?;
    let download = client.download(&direct, &d_file, 4096, None).await?;
    let _ = writeln!(seen, "{download:?}");
    read_all(download).await?;

    // A truncated redirected download, and a refused pre-authenticated URL.
    sim.push_fault(Fault::truncate_body(10).on_path("/download"));
    let download = client.download(&redirect, &r_file, 4096, None).await?;
    let err = read_all(download).await.unwrap_err();
    let _ = writeln!(seen, "{err:?} {err}");
    sim.push_fault(Fault::status(StatusCode::UNAUTHORIZED).on_path("/download"));
    let err = client
        .download(&redirect, &r_file, 4096, None)
        .await
        .unwrap_err();
    let _ = writeln!(seen, "{err:?} {err}");

    // Items whose JSON carries @microsoft.graph.downloadUrl, and delta.
    let item = client.get_item(&redirect, &r_file).await?;
    let _ = writeln!(seen, "{item:?}");
    let (items, link, _) = full_delta(&client, &redirect, None).await;
    let _ = writeln!(seen, "{items:?} {link:?}");

    // Errors: resync, throttling, a second 401.
    sim.push_fault(
        Fault::status(StatusCode::GONE)
            .code("resyncChangesApplyDifferences")
            .location(format!("{}/drives/{redirect}/root/delta", sim.base_url()))
            .on_path("/delta"),
    );
    let err = client.delta_from_link(&redirect, &link).await.unwrap_err();
    let _ = writeln!(seen, "{err:?} {err}");
    sim.push_fault(Fault::status(StatusCode::TOO_MANY_REQUESTS).retry_after(3));
    let err = client.get_drive(&redirect).await.unwrap_err();
    let _ = writeln!(seen, "{err:?} {err}");
    let bad = FakeTokens::new(STALE);
    bad.refresh_to("another-stale-secret");
    let err = client_with(sim, bad)
        .download(&redirect, &r_file, 4096, None)
        .await
        .unwrap_err();
    let _ = writeln!(seen, "{err:?} {err}");
    Ok(seen)
}

/// AC7, ADR-0011 R10: across refreshes, redirected and direct downloads, truncation,
/// refused download URLs, resyncs and throttling, no bearer token and no
/// pre-authenticated URL reaches a log event, `Debug` output or an error message.
#[test]
fn ac7_no_token_or_preauthenticated_url_in_logs_or_debug() -> TestResult {
    let capture = Capture::default();
    tracing::subscriber::set_global_default(capture.clone())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let (seen, token, download_origin) = runtime.block_on(async {
        let sim = GraphSim::start().await?;
        let seen = scenario(&sim).await?;
        // The pre-authenticated URLs the simulator handed out are in its request log.
        assert!(
            sim.requests()
                .iter()
                .any(|r| r.query.as_deref().is_some_and(|q| q.contains("tempauth")))
        );
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((
            seen,
            sim.token().to_owned(),
            sim.download_origin().to_owned(),
        ))
    })?;

    let logs = lock(&capture.0).clone();
    assert!(logs.contains("[mydrive_graph::client]"), "{logs}");
    assert!(logs.contains("following download redirect"), "{logs}");
    assert!(logs.contains("forcing one token refresh"), "{logs}");
    for (what, text) in [("logs", &logs), ("debug and errors", &seen)] {
        for secret in [
            token.as_str(),
            STALE,
            "another-stale-secret",
            "tempauth",
            download_origin.as_str(),
            "Bearer",
        ] {
            assert!(!text.contains(secret), "{what} contain {secret:?}:\n{text}");
        }
    }
    Ok(())
}
