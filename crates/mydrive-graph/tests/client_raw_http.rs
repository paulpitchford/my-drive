//! Responses the Graph simulator does not produce, from a raw-TCP server: ADR-0011 R7
//! (RateLimit headers), content encodings, unframed and lying bodies, bad
//! `Content-Range`, connection resets, one refresh per call across redirects, redirects
//! with credentials, and an HTTP-date `Retry-After`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "client_raw_server.rs"]
mod raw;
#[path = "client_support.rs"]
mod support;

use std::sync::Arc;

use mydrive_core::ids::{DriveId, ItemId};
use mydrive_graph::client::{ByteRange, Download};
use mydrive_graph::{GraphClient, GraphErrorKind};
use raw::{RawServer, json, response};
use support::{FakeTokens, TestResult, read_all, test_user_agent};

const SIZE: u64 = 4096;

fn drive() -> DriveId {
    "b!raw".parse().unwrap()
}

fn item() -> ItemId {
    "01RAW".parse().unwrap()
}

fn client_for(server: &RawServer, tokens: Arc<FakeTokens>) -> GraphClient {
    GraphClient::new(&server.base_url(), tokens, test_user_agent()).unwrap()
}

async fn setup() -> (RawServer, GraphClient, Arc<FakeTokens>) {
    let server = RawServer::start().await;
    let tokens = FakeTokens::new("raw-token");
    let client = client_for(&server, tokens.clone());
    (server, client, tokens)
}

const RATELIMIT: [(&str, &str); 4] = [
    ("RateLimit-Limit", "1"),
    ("RateLimit-Remaining", "0"),
    ("RateLimit-Reset", "3600"),
    ("x-ms-throttle-limit-percentage", "0.99"),
];

/// ADR-0011 R7: RateLimit headers change nothing. A success carrying them is a plain
/// success with one request and no wait; a 429 carrying them but no `Retry-After` has no
/// duration; a 503 carrying only them is a server error, not throttling.
#[tokio::test]
async fn adr_0011_r7_ratelimit_headers_change_nothing() -> TestResult {
    let (server, client, _) = setup().await;
    server.push(json(
        "200 OK",
        &RATELIMIT,
        r#"{"id":"b!RAW","driveType":"business"}"#,
    ));
    let parsed = client.get_drive(&drive()).await?;
    assert_eq!(parsed.id, drive());
    assert_eq!(server.requests().len(), 1);

    server.push(json(
        "200 OK",
        &RATELIMIT,
        r#"{"value":[{"id":"a"}],"@odata.deltaLink":"x"}"#,
    ));
    let page = client.delta_from_root(&drive()).await?;
    assert_eq!(page.items.len(), 1);

    server.push(json("429 Too Many Requests", &RATELIMIT, "{}"));
    let err = client.get_drive(&drive()).await.unwrap_err();
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::Throttled {
                status: 429,
                retry_after: None,
                retry_after_raw: None
            }
        ),
        "{err}"
    );

    server.push(json("503 Service Unavailable", &RATELIMIT, "{}"));
    let err = client.get_drive(&drive()).await.unwrap_err();
    assert!(
        matches!(err.kind(), GraphErrorKind::ServerError { status: 503, .. }),
        "{err}"
    );
    assert_eq!(server.requests().len(), 4, "nothing was retried or delayed");
    assert_eq!(server.pending(), 0);
    Ok(())
}

/// A body compressed on the way (gzip by a proxy) is refused for whole and ranged
/// downloads, never reported as the file's bytes; every request asks for `identity`.
#[tokio::test]
async fn content_encoded_download_is_refused() -> TestResult {
    let (server, client, _) = setup().await;
    let gzip = [0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0];
    server.push(response(
        "200 OK",
        &[("Content-Encoding", "gzip"), ("Content-Length", "8")],
        &gzip,
    ));
    let err = client
        .download(&drive(), &item(), 8, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err.kind(), GraphErrorKind::UnsupportedEncoding { encoding } if encoding == "gzip"),
        "{err}"
    );

    server.push(response(
        "206 Partial Content",
        &[
            ("Content-Encoding", "gzip"),
            ("Content-Range", "bytes 0-7/4096"),
            ("Content-Length", "8"),
        ],
        &gzip,
    ));
    let err = client
        .download(&drive(), &item(), SIZE, ByteRange::new(0, 8))
        .await
        .unwrap_err();
    assert!(
        matches!(err.kind(), GraphErrorKind::UnsupportedEncoding { .. }),
        "{err}"
    );

    // A JSON body that is content-encoded is malformed.
    server.push(response(
        "200 OK",
        &[("Content-Encoding", "br"), ("Content-Length", "8")],
        &gzip,
    ));
    let err = client.get_drive(&drive()).await.unwrap_err();
    assert!(matches!(err.kind(), GraphErrorKind::Malformed(_)), "{err}");

    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(
            request.header("accept-encoding"),
            Some("identity"),
            "{request:?}"
        );
    }
    Ok(())
}

/// A whole-file body with no length that stops after 10 of 4096 bytes (connection
/// closed, not chunked) is truncated against the expected size, never complete.
#[tokio::test]
async fn unframed_short_whole_file_is_truncated() -> TestResult {
    let (server, client, _) = setup().await;
    server.push(response("200 OK", &[], &[7u8; 10]));
    let download = client.download(&drive(), &item(), SIZE, None).await?;
    let Download::WholeFile { body } = download else {
        panic!("{download:?}");
    };
    let err = body.bytes().await.unwrap_err();
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::Truncated {
                expected: SIZE,
                received: 10
            }
        ),
        "{err}"
    );
    // The same response is fine for a file that really is 10 bytes.
    server.push(response("200 OK", &[], &[7u8; 10]));
    assert_eq!(
        read_all(client.download(&drive(), &item(), 10, None).await?).await?,
        [7u8; 10]
    );

    // And an unframed body longer than the expected size is not the file either.
    server.push(response("200 OK", &[], &[7u8; 20]));
    let err = read_all(client.download(&drive(), &item(), 10, None).await?)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::SizeMismatch { expected: 10, .. }
        ),
        "{err}"
    );
    Ok(())
}

/// A 200 whose `Content-Length` is lower than the real size is a size mismatch before
/// any byte is handed out.
#[tokio::test]
async fn content_length_below_expected_size_is_a_mismatch() -> TestResult {
    let (server, client, _) = setup().await;
    server.push(response("200 OK", &[("Content-Length", "10")], &[1u8; 10]));
    let err = client
        .download(&drive(), &item(), SIZE, None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::SizeMismatch {
                expected: SIZE,
                reported: Some(10)
            }
        ),
        "{err}"
    );
    Ok(())
}

/// A `Content-Range` that does not match the request or the expected size is refused.
#[tokio::test]
async fn mismatched_content_range_is_refused() -> TestResult {
    let (server, client, _) = setup().await;
    let requested = ByteRange::new(100, 50);
    for (content_range, size_problem) in [
        ("bytes 0-49/4096", false),
        ("bytes 101-150/4096", false),
        ("bytes 100-199/4096", false),
        ("bytes 100-149/5000", true),
        ("bytes 100-149/*", true),
    ] {
        server.push(response(
            "206 Partial Content",
            &[("Content-Range", content_range), ("Content-Length", "50")],
            &[0u8; 50],
        ));
        let err = client
            .download(&drive(), &item(), SIZE, requested)
            .await
            .unwrap_err();
        if size_problem {
            assert!(
                matches!(
                    err.kind(),
                    GraphErrorKind::SizeMismatch { expected: SIZE, .. }
                ),
                "{content_range}: {err}"
            );
        } else {
            assert!(
                matches!(err.kind(), GraphErrorKind::Malformed(_)),
                "{content_range}: {err}"
            );
        }
    }

    // A 206 to a request without Range, or with a Content-Length that disagrees.
    server.push(response(
        "206 Partial Content",
        &[
            ("Content-Range", "bytes 0-9/4096"),
            ("Content-Length", "10"),
        ],
        &[0u8; 10],
    ));
    let err = client
        .download(&drive(), &item(), SIZE, None)
        .await
        .unwrap_err();
    assert!(matches!(err.kind(), GraphErrorKind::Malformed(_)), "{err}");
    server.push(response(
        "206 Partial Content",
        &[("Content-Range", "bytes 0-9/4096"), ("Content-Length", "9")],
        &[0u8; 9],
    ));
    let err = client
        .download(&drive(), &item(), SIZE, ByteRange::new(0, 10))
        .await
        .unwrap_err();
    assert!(matches!(err.kind(), GraphErrorKind::Malformed(_)), "{err}");
    Ok(())
}

/// A 206 that ends before the requested end is accepted only when it ends at the file's
/// last byte.
#[tokio::test]
async fn short_206_only_at_end_of_file() -> TestResult {
    let (server, client, _) = setup().await;
    server.push(response(
        "206 Partial Content",
        &[
            ("Content-Range", "bytes 0-49/4096"),
            ("Content-Length", "50"),
        ],
        &[0u8; 50],
    ));
    let err = client
        .download(&drive(), &item(), SIZE, ByteRange::new(0, 100))
        .await
        .unwrap_err();
    assert!(matches!(err.kind(), GraphErrorKind::Malformed(_)), "{err}");

    server.push(response(
        "206 Partial Content",
        &[
            ("Content-Range", "bytes 50-59/60"),
            ("Content-Length", "10"),
        ],
        &[3u8; 10],
    ));
    match client
        .download(&drive(), &item(), 60, ByteRange::new(50, 100))
        .await?
    {
        Download::Range { start, end, body } => {
            assert_eq!((start, end), (50, 59));
            assert_eq!(body.bytes().await?, [3u8; 10]);
        }
        other => panic!("{other:?}"),
    }
    Ok(())
}

/// A connection reset in the middle of the body is an error, never a short success.
#[tokio::test]
async fn connection_reset_mid_body_is_an_error() -> TestResult {
    let (server, client, _) = setup().await;
    for range in [None, ByteRange::new(0, 4096)] {
        let mut headers = vec![("Content-Length", "4096")];
        let status = if range.is_some() {
            headers.push(("Content-Range", "bytes 0-4095/4096"));
            "206 Partial Content"
        } else {
            "200 OK"
        };
        server.push(response(status, &headers, &[9u8; 100]).reset());
        // The reset may arrive before or after the head is read; either way the
        // download fails.
        let outcome = match client.download(&drive(), &item(), SIZE, range).await {
            Ok(download) => read_all(download).await.map(|b| b.len()),
            Err(e) => Err(e),
        };
        let err = outcome.expect_err("a reset body must not read as complete");
        assert!(
            matches!(
                err.kind(),
                GraphErrorKind::Truncated { expected: SIZE, .. } | GraphErrorKind::Transport(_)
            ),
            "{range:?}: {err}"
        );
    }
    Ok(())
}

/// At most one forced refresh per call, even across a same-origin redirect chain:
/// 401, refresh, 302 to the Graph origin, 401 again is unauthorised without a second
/// refresh, and the scripted 200 after it is never requested.
#[tokio::test]
async fn one_forced_refresh_per_call_across_redirects() -> TestResult {
    let (server, client, tokens) = setup().await;
    tokens.refresh_to("fresh-token");
    let again = format!("{}/drives/b!raw/items/01RAW/content2", server.base_url());
    server.push(json(
        "401 Unauthorized",
        &[],
        r#"{"error":{"code":"InvalidAuthenticationToken"}}"#,
    ));
    server.push(response(
        "302 Found",
        &[("Location", again.as_str()), ("Content-Length", "0")],
        b"",
    ));
    server.push(json(
        "401 Unauthorized",
        &[],
        r#"{"error":{"code":"InvalidAuthenticationToken"}}"#,
    ));
    server.push(response("200 OK", &[("Content-Length", "4")], b"data"));
    let err = client
        .download(&drive(), &item(), 4, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err.kind(), GraphErrorKind::Unauthorized { .. }),
        "{err}"
    );
    assert_eq!(
        tokens.rejected(),
        ["raw-token"],
        "exactly one forced refresh"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2].header("authorization"),
        Some("Bearer fresh-token")
    );
    assert_eq!(server.pending(), 1);
    Ok(())
}

/// A redirect whose `Location` carries credentials is refused, and not followed.
#[tokio::test]
async fn redirect_with_userinfo_is_refused() -> TestResult {
    let (server, client, _) = setup().await;
    let target = server.origin().replace("http://", "http://user:secret@");
    let location = format!("{target}/v1.0/elsewhere");
    server.push(response(
        "302 Found",
        &[("Location", location.as_str()), ("Content-Length", "0")],
        b"",
    ));
    let err = client
        .download(&drive(), &item(), 4, None)
        .await
        .unwrap_err();
    assert!(matches!(err.kind(), GraphErrorKind::Malformed(_)), "{err}");
    assert!(!err.to_string().contains("secret"));
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// A `Retry-After` that is an HTTP-date is kept raw for the scheduler.
#[tokio::test]
async fn http_date_retry_after_is_kept_raw() -> TestResult {
    let (server, client, _) = setup().await;
    let date = "Wed, 21 Oct 2026 07:28:00 GMT";
    server.push(json(
        "503 Service Unavailable",
        &[("Retry-After", date)],
        "{}",
    ));
    let err = client.get_drive(&drive()).await.unwrap_err();
    match err.kind() {
        GraphErrorKind::Throttled {
            status: 503,
            retry_after: None,
            retry_after_raw: Some(raw),
        } => assert_eq!(raw, date),
        other => panic!("{other}"),
    }
    Ok(())
}

/// One bad item in a delta page does not fail the page: it is reported with its ID.
#[tokio::test]
async fn delta_page_with_a_bad_item_still_arrives() -> TestResult {
    let (server, client, _) = setup().await;
    server.push(json(
        "200 OK",
        &[],
        r#"{"value":[{"id":"ok"},{"id":"bad","parentReference":{"id":""}},
            {"id":"odd","file":{"hashes":{"quickXorHash":"zz"}}}],"@odata.deltaLink":"d"}"#,
    ));
    let page = client.delta_from_root(&drive()).await?;
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[1].warnings.len(), 1);
    assert_eq!(page.problems.len(), 1);
    assert_eq!(page.problems[0].id.as_str(), "bad");
    assert_eq!(page.problems[0].index, 1);
    Ok(())
}
