//! Content downloads against the simulator: AC2, AC3, AC6, AC10 (both download modes,
//! `Range` honoured and ignored), ADR-0011 R10, R11.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "client_support.rs"]
mod support;

use mydrive_graph::client::{ByteRange, Download};
use mydrive_graph::{GraphError, GraphErrorKind};
use mydrive_graph_sim::{
    DownloadMode, DriveOptions, DriveType, Fault, Origin, RangeMode, StatusCode,
};
use support::{FakeTokens, TestResult, body, client, client_with, count, read_all, sim_with};

const SIZE: u64 = 4096;

fn content() -> Vec<u8> {
    (0..4096u32).map(|n| (n % 251) as u8).collect()
}

fn options(download: DownloadMode, range: RangeMode) -> DriveOptions {
    DriveOptions {
        download,
        range,
        ..DriveOptions::default()
    }
}

const MODES: [DownloadMode; 2] = [DownloadMode::Redirect, DownloadMode::Direct];

/// AC2, ADR-0011 R11, AC10 (`Range` ignored, both download modes): a ranged download from
/// a server that ignores `Range` is reported as the whole file from offset 0, never as the
/// requested range, and the body is the whole file.
#[tokio::test]
async fn ac2_adr_0011_r11_ignored_range_is_whole_file() -> TestResult {
    for mode in MODES {
        let (sim, drive, root) =
            sim_with(DriveType::Personal, options(mode, RangeMode::Ignored)).await;
        let file = sim.create_file(&drive, &root, "f", content())?;
        let download = client(&sim)
            .download(&drive, &file, SIZE, ByteRange::new(100, 50))
            .await?;
        let Download::WholeFile { body } = download else {
            panic!("{mode:?}: an ignored range must be the whole file: {download:?}");
        };
        assert_eq!(body.expected_len(), SIZE);
        assert_eq!(body.bytes().await?, content(), "{mode:?}");
        // The range was asked for, so this is the ignored-range case and not a plain
        // whole-file download.
        assert!(
            sim.requests()
                .iter()
                .any(|r| r.header("range") == Some("bytes=100-149"))
        );
    }
    Ok(())
}

/// AC10 (`Range` honoured, both download modes), ADR-0011 R10: a ranged download gets
/// exactly the requested bytes, confirmed by `Content-Range`; a range past the end of the
/// file is reported as the shorter range that ends at the file's last byte.
#[tokio::test]
async fn honoured_range_is_reported_as_the_confirmed_range() -> TestResult {
    for mode in MODES {
        let (sim, drive, root) =
            sim_with(DriveType::Business, options(mode, RangeMode::Honoured)).await;
        let file = sim.create_file(&drive, &root, "f", content())?;
        let client = client(&sim);

        match client
            .download(&drive, &file, SIZE, ByteRange::new(100, 50))
            .await?
        {
            Download::Range { start, end, body } => {
                assert_eq!((start, end), (100, 149), "{mode:?}");
                assert_eq!(body.expected_len(), 50);
                assert_eq!(body.bytes().await?, content()[100..150], "{mode:?}");
            }
            other => panic!("{mode:?}: {other:?}"),
        }

        match client
            .download(&drive, &file, SIZE, ByteRange::new(4090, 100))
            .await?
        {
            Download::Range { start, end, body } => {
                assert_eq!((start, end), (4090, 4095));
                assert_eq!(body.bytes().await?, content()[4090..]);
            }
            other => panic!("{other:?}"),
        }

        let whole = client.download(&drive, &file, SIZE, None).await?;
        let Download::WholeFile { body } = whole else {
            panic!("{whole:?}");
        };
        assert_eq!(body.bytes().await?, content());
    }
    Ok(())
}

/// The expected size is checked against what the server says: a stale size (the file
/// changed remotely) is an error for both whole and ranged downloads, never a success.
#[tokio::test]
async fn stale_expected_size_is_a_size_mismatch() -> TestResult {
    for mode in MODES {
        let (sim, drive, root) =
            sim_with(DriveType::Personal, options(mode, RangeMode::Honoured)).await;
        let file = sim.create_file(&drive, &root, "f", content())?;
        let client = client(&sim);
        for (expected, range) in [
            (SIZE + 1, None),
            (SIZE - 1, None),
            (SIZE + 10, ByteRange::new(0, 10)),
        ] {
            let err = client
                .download(&drive, &file, expected, range)
                .await
                .unwrap_err();
            assert!(
                matches!(
                    err.kind(),
                    GraphErrorKind::SizeMismatch { expected: e, reported: Some(SIZE) } if *e == expected
                ),
                "{mode:?} {expected} {range:?}: {err}"
            );
        }
        // An empty file downloads as an empty whole file.
        let empty = sim.create_file(&drive, &root, "empty", Vec::new())?;
        assert!(
            read_all(client.download(&drive, &empty, 0, None).await?)
                .await?
                .is_empty()
        );
    }
    Ok(())
}

/// AC3, ADR-0011 R10: in redirect mode the bearer token goes to `/content` and not to the
/// redirected download origin, which still gets the `Range`; in direct mode the token is
/// sent to `/content` and no other origin is contacted. Every request asks for the
/// identity encoding.
#[tokio::test]
async fn ac3_adr_0011_r10_no_bearer_token_to_redirect_host() -> TestResult {
    for mode in MODES {
        let (sim, drive, root) =
            sim_with(DriveType::Personal, options(mode, RangeMode::Honoured)).await;
        let file = sim.create_file(&drive, &root, "f", content())?;
        read_all(
            client(&sim)
                .download(&drive, &file, SIZE, ByteRange::new(0, 10))
                .await?,
        )
        .await?;
        let log = sim.requests();
        let bearer = format!("Bearer {}", sim.token());
        let graph: Vec<_> = log.iter().filter(|r| r.origin == Origin::Graph).collect();
        assert_eq!(graph.len(), 1, "{mode:?}: {log:?}");
        assert!(graph[0].path.ends_with("/content"));
        assert_eq!(graph[0].header("authorization"), Some(bearer.as_str()));
        let redirected: Vec<_> = log
            .iter()
            .filter(|r| r.origin == Origin::Download)
            .collect();
        match mode {
            DownloadMode::Redirect => {
                assert_eq!(graph[0].status, 302);
                assert_eq!(redirected.len(), 1, "{log:?}");
                assert_eq!(redirected[0].header("authorization"), None);
                assert_eq!(redirected[0].header("range"), Some("bytes=0-9"));
                assert_eq!(redirected[0].status, 206);
            }
            DownloadMode::Direct => {
                assert_eq!(graph[0].status, 206);
                assert!(redirected.is_empty(), "{log:?}");
            }
        }
    }
    Ok(())
}

/// Drains a download chunk by chunk, returning the bytes that arrived and how it ended.
async fn drain(download: Download) -> (usize, Result<(), GraphError>) {
    let mut body = body(download);
    let mut received = 0;
    loop {
        match body.chunk().await {
            Ok(Some(chunk)) => received += chunk.len(),
            Ok(None) => return (received, Ok(())),
            Err(e) => {
                // Every later read fails too: a failed download never turns into an end.
                assert!(body.chunk().await.is_err());
                return (received, Err(e));
            }
        }
    }
}

/// AC6: a truncated body is reported as an error, never as a short successful read, for
/// whole and ranged downloads on both origins.
#[tokio::test]
async fn ac6_truncated_body_is_an_error() -> TestResult {
    for mode in MODES {
        for range in [None, ByteRange::new(0, 1000)] {
            let (sim, drive, root) =
                sim_with(DriveType::Personal, options(mode, RangeMode::Honoured)).await;
            let file = sim.create_file(&drive, &root, "f", content())?;
            let path = match mode {
                DownloadMode::Redirect => "/download",
                DownloadMode::Direct => "/content",
            };
            sim.push_fault(Fault::truncate_body(100).on_path(path));
            let download = client(&sim).download(&drive, &file, SIZE, range).await?;
            let expected = if range.is_some() { 1000 } else { SIZE };
            let (received, ended) = drain(download).await;
            assert_eq!(received, 100, "{mode:?} {range:?}");
            let err = ended.expect_err("a truncated body must not read as complete");
            assert!(
                matches!(
                    err.kind(),
                    GraphErrorKind::Truncated { expected: e, received: 100 } if *e == expected
                ),
                "{mode:?} {range:?}: {err}"
            );
            assert_eq!(sim.pending_faults(), 0);
        }
    }
    Ok(())
}

/// AC6: a truncated JSON body (a delta page) is an error too.
#[tokio::test]
async fn ac6_truncated_json_is_an_error() -> TestResult {
    let (sim, drive, root) = sim_with(DriveType::Business, DriveOptions::default()).await;
    for n in 0..20 {
        sim.create_file(&drive, &root, &format!("file {n}"), vec![1])?;
    }
    sim.push_fault(Fault::truncate_body(50).on_path("/delta"));
    let err = client(&sim).delta_from_root(&drive).await.unwrap_err();
    assert!(
        matches!(err.kind(), GraphErrorKind::Truncated { received: 50, .. }),
        "{err}"
    );
    Ok(())
}

/// The redirected host refusing its pre-authenticated URL is not a token problem: no
/// token refresh, and a distinct error.
#[tokio::test]
async fn rejected_download_url_is_not_a_token_refresh() -> TestResult {
    let (sim, drive, root) = sim_with(DriveType::Personal, DriveOptions::default()).await;
    let file = sim.create_file(&drive, &root, "f", content())?;
    let tokens = FakeTokens::new(sim.token());
    sim.push_fault(Fault::status(StatusCode::UNAUTHORIZED).on_path("/download"));
    let err = client_with(&sim, tokens.clone())
        .download(&drive, &file, SIZE, None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::DownloadUrlRejected { status: 401 }
        ),
        "{err}"
    );
    assert!(tokens.rejected().is_empty());
    assert_eq!(count(&sim, Origin::Download), 1);
    Ok(())
}

/// Errors from either origin map to the typed error: 404 on `/content`, 403 and 503 on
/// the redirected host, 416 for a range outside the file.
#[tokio::test]
async fn download_errors_are_typed() -> TestResult {
    let (sim, drive, root) = sim_with(DriveType::Personal, DriveOptions::default()).await;
    let file = sim.create_file(&drive, &root, "f", content())?;
    let client = client(&sim);

    let missing = client
        .download(&drive, &"nope".parse()?, SIZE, None)
        .await
        .unwrap_err();
    assert!(
        matches!(missing.kind(), GraphErrorKind::NotFound { .. }),
        "{missing}"
    );

    sim.push_fault(Fault::status(StatusCode::FORBIDDEN).on_path("/download"));
    let forbidden = client
        .download(&drive, &file, SIZE, None)
        .await
        .unwrap_err();
    match forbidden.kind() {
        GraphErrorKind::Forbidden { codes } => assert_eq!(codes.primary(), Some("accessDenied")),
        other => panic!("{other}"),
    }

    sim.push_fault(
        Fault::status(StatusCode::SERVICE_UNAVAILABLE)
            .retry_after(4)
            .on_path("/download"),
    );
    let throttled = client
        .download(&drive, &file, SIZE, None)
        .await
        .unwrap_err();
    assert!(
        matches!(throttled.kind(), GraphErrorKind::Throttled { status: 503, retry_after: Some(d), .. } if d.as_secs() == 4),
        "{throttled}"
    );

    let outside = client
        .download(&drive, &file, SIZE, ByteRange::new(5000, 10))
        .await
        .unwrap_err();
    assert!(
        matches!(
            outside.kind(),
            GraphErrorKind::UnexpectedStatus { status: 416, .. }
        ),
        "{outside}"
    );
    Ok(())
}

/// The response's `ETag` header is passed on when the server sends one.
#[tokio::test]
async fn content_etag_header_is_reported() -> TestResult {
    for with_etag in [false, true] {
        let opts = DriveOptions {
            content_etag_header: with_etag,
            download: DownloadMode::Direct,
            ..DriveOptions::default()
        };
        let (sim, drive, root) = sim_with(DriveType::Business, opts).await;
        let file = sim.create_file(&drive, &root, "f", content())?;
        let client = client(&sim);
        let item = client.get_item(&drive, &file).await?;
        let download = body(client.download(&drive, &file, SIZE, None).await?);
        if with_etag {
            assert_eq!(download.etag(), item.etag.as_deref());
        } else {
            assert_eq!(download.etag(), None);
        }
    }
    Ok(())
}
