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
use mydrive_graph::client::{ByteRange, Download, MAX_CODE_LEN, MAX_RETRY_AFTER};
use mydrive_graph::model::{DeltaEntry, ModelError};
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
    assert_eq!(page.items().count(), 1);

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

/// Reads a download to its end and returns the error and the bytes handed out first.
async fn fail_after(download: Download) -> (u64, mydrive_graph::GraphError) {
    let mut body = support::body(download);
    let mut handed_out = 0;
    loop {
        match body.chunk().await {
            Ok(Some(chunk)) => handed_out += chunk.len() as u64,
            Ok(None) => panic!("a short body must not end in success"),
            Err(e) => return (handed_out, e),
        }
    }
}

fn chunked(body: &[u8], terminator: bool) -> Vec<u8> {
    let mut out = format!("{:x}\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out.extend_from_slice(b"\r\n");
    if terminator {
        out.extend_from_slice(b"0\r\n\r\n");
    }
    out
}

/// The `Truncated` / `SizeMismatch` contract. A chunked body with a proper terminator
/// that is shorter than the expected size is a complete, different version: every byte
/// must be discarded, so it is `SizeMismatch`, never `Truncated`. The same for a short
/// unframed body (a close with no length), and for a chunked body longer than expected.
#[tokio::test]
async fn short_or_long_unverified_bodies_are_size_mismatches() -> TestResult {
    let (server, client, _) = setup().await;
    let te = [("Transfer-Encoding", "chunked")];

    // v2 of 3000 bytes served for an expected v1 of 4096, chunked with a terminator.
    server.push(response("200 OK", &te, &chunked(&[2u8; 3000], true)));
    let download = client.download(&drive(), &item(), SIZE, None).await?;
    assert!(matches!(&download, Download::WholeFile { body } if !body.is_verified()));
    let (_, err) = fail_after(download).await;
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::SizeMismatch {
                expected: SIZE,
                reported: Some(3000)
            }
        ),
        "{err}"
    );

    // Chunked, cut off without a terminator: not verified, so still SizeMismatch.
    server.push(response("200 OK", &te, &chunked(&[2u8; 100], false)));
    let (_, err) = fail_after(client.download(&drive(), &item(), SIZE, None).await?).await;
    assert!(
        matches!(err.kind(), GraphErrorKind::SizeMismatch { .. }),
        "{err}"
    );

    // Unframed (no length, connection closed) and short.
    server.push(response("200 OK", &[], &[7u8; 10]));
    let (_, err) = fail_after(client.download(&drive(), &item(), SIZE, None).await?).await;
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

    // Chunked and longer than expected.
    server.push(response("200 OK", &te, &chunked(&[2u8; 5000], true)));
    let (_, err) = fail_after(client.download(&drive(), &item(), SIZE, None).await?).await;
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::SizeMismatch { expected: SIZE, .. }
        ),
        "{err}"
    );

    // A 206 whose chunked body ends cleanly short of its range.
    server.push(response(
        "206 Partial Content",
        &[
            ("Transfer-Encoding", "chunked"),
            ("Content-Range", "bytes 0-99/4096"),
        ],
        &chunked(&[1u8; 50], true),
    ));
    let (_, err) = fail_after(
        client
            .download(&drive(), &item(), SIZE, ByteRange::new(0, 100))
            .await?,
    )
    .await;
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::SizeMismatch { expected: 100, .. }
        ),
        "{err}"
    );

    // An unframed body of exactly the expected size is fine.
    server.push(response("200 OK", &[], &[7u8; 10]));
    assert_eq!(
        read_all(client.download(&drive(), &item(), 10, None).await?).await?,
        [7u8; 10]
    );
    Ok(())
}

/// A verified head (matching `Content-Length`, or a matching `Content-Range`) followed
/// by a connection failure is `Truncated`: the bytes handed out may be kept.
#[tokio::test]
async fn verified_head_then_reset_is_truncated() -> TestResult {
    let (server, client, _) = setup().await;
    for range in [None, ByteRange::new(0, 4096)] {
        let mut headers = vec![("Content-Length", "4096")];
        let status = if range.is_some() {
            headers.push(("Content-Range", "bytes 0-4095/4096"));
            "206 Partial Content"
        } else {
            "200 OK"
        };
        // A clean close after part of a verified body: hyper reports the early end.
        server.push(response(status, &headers, &[9u8; 100]));
        let download = client.download(&drive(), &item(), SIZE, range).await?;
        assert!(support::body_ref(&download).is_verified());
        let (handed_out, err) = fail_after(download).await;
        assert!(
            matches!(
                err.kind(),
                GraphErrorKind::Truncated { expected: SIZE, received } if *received == handed_out
            ),
            "{range:?}: {err}"
        );
    }
    Ok(())
}

/// An unframed body of exactly the expected size is accepted; one longer than the
/// expected size is a size mismatch.
#[tokio::test]
async fn unframed_long_whole_file_is_a_mismatch() -> TestResult {
    let (server, client, _) = setup().await;
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
    let order: Vec<_> = page
        .entries
        .iter()
        .map(|e| (e.id().as_str(), matches!(e, DeltaEntry::Item(_))))
        .collect();
    assert_eq!(order, [("ok", true), ("bad", false), ("odd", true)]);
    assert_eq!(page.items().nth(1).unwrap().warnings.len(), 1);
    let problem = page.problems().next().unwrap();
    assert_eq!(problem.id.as_str(), "bad");
    assert!(!problem.deleted);
    Ok(())
}

/// Server strings never reach errors verbatim: a field holding a URL with a token-like
/// query is named, not echoed, and an error code with a newline and a fake log line is
/// reduced to a short printable form.
#[tokio::test]
async fn server_strings_are_not_echoed_into_errors() -> TestResult {
    let (server, client, _) = setup().await;
    let secret = "https://h.invalid/x?access_token=SECRETVALUE";
    server.push(json(
        "200 OK",
        &[],
        &serde_json::json!({"id": "x", "size": secret}).to_string(),
    ));
    let err = client.get_item(&drive(), &item()).await.unwrap_err();
    let text = format!("{err} {err:?}");
    assert!(text.contains("size"), "{text}");
    assert!(!text.contains("SECRETVALUE"), "{text}");

    server.push(json(
        "200 OK",
        &[],
        &serde_json::json!({"value": [{"id": "x", "size": secret}], "@odata.deltaLink": "d"})
            .to_string(),
    ));
    let page = client.delta_from_root(&drive()).await?;
    let problem = page.problems().next().unwrap();
    assert!(
        !problem.reason.contains("SECRETVALUE"),
        "{}",
        problem.reason
    );

    let code = "notFound\n2026-11-14T00:00:00Z ERROR mydrive: fake line SECRETVALUE";
    server.push(json(
        "404 Not Found",
        &[],
        &serde_json::json!({"error": {"code": code}}).to_string(),
    ));
    let err = client.get_drive(&drive()).await.unwrap_err();
    let GraphErrorKind::NotFound { codes } = err.kind() else {
        panic!("{err}");
    };
    let kept = codes.primary().unwrap();
    assert!(
        !kept.contains('\n') && kept.len() <= MAX_CODE_LEN,
        "{kept:?}"
    );
    assert!(!err.to_string().contains('\n'));
    Ok(())
}

/// A range starting at or past the expected size, or any range of an empty file, is
/// refused without a request; a 416 naming another size is a size mismatch.
#[tokio::test]
async fn range_outside_the_file_is_refused() -> TestResult {
    let (server, client, _) = setup().await;
    for (size, start) in [(SIZE, SIZE), (SIZE, SIZE + 1), (0, 0)] {
        let err = client
            .download(&drive(), &item(), size, ByteRange::new(start, 10))
            .await
            .unwrap_err();
        assert!(
            matches!(err.kind(), GraphErrorKind::RangeOutsideFile { start: s, size: z } if *s == start && *z == size),
            "{err}"
        );
    }
    assert!(server.requests().is_empty());

    server.push(json(
        "416 Range Not Satisfiable",
        &[("Content-Range", "bytes */100")],
        "{}",
    ));
    let err = client
        .download(&drive(), &item(), SIZE, ByteRange::new(200, 10))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::SizeMismatch {
                expected: SIZE,
                reported: Some(100)
            }
        ),
        "{err}"
    );
    server.push(json(
        "416 Range Not Satisfiable",
        &[("Content-Range", "bytes */4096")],
        "{}",
    ));
    let err = client
        .download(&drive(), &item(), SIZE, ByteRange::new(200, 10))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::UnexpectedStatus { status: 416, .. }
        ),
        "{err}"
    );
    Ok(())
}

/// Two `Content-Encoding` lines, `identity` then `gzip`, are refused: every line counts.
/// Likewise two `Transfer-Encoding` lines.
#[tokio::test]
async fn two_line_encodings_are_refused() -> TestResult {
    let (server, client, _) = setup().await;
    let gzip = [0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0];
    server.push(response(
        "200 OK",
        &[
            ("Content-Encoding", "identity"),
            ("Content-Encoding", "gzip"),
            ("Content-Length", "8"),
        ],
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
            ("Content-Encoding", "identity"),
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

    // A JSON body with the two-line form is malformed, not parsed.
    server.push(response(
        "200 OK",
        &[
            ("Content-Encoding", "identity"),
            ("Content-Encoding", "br"),
            ("Content-Length", "8"),
        ],
        &gzip,
    ));
    let err = client.get_drive(&drive()).await.unwrap_err();
    assert!(matches!(err.kind(), GraphErrorKind::Malformed(_)), "{err}");

    // Two Transfer-Encoding lines, whatever the second says.
    for second in ["chunked", "gzip"] {
        server.push(response(
            "200 OK",
            &[
                ("Transfer-Encoding", "chunked"),
                ("Transfer-Encoding", second),
            ],
            b"8\r\n\x1f\x8b\x08\x00\x00\x00\x00\x00\r\n0\r\n\r\n",
        ));
        let err = client
            .download(&drive(), &item(), 8, None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err.kind(),
                GraphErrorKind::UnsupportedTransferEncoding { .. }
            ),
            "{second}: {err}"
        );
    }
    Ok(())
}

/// Over the wire, a damaged delta entry is a problem, never a normal item, and a
/// `"deleted": null` entry is reported with its `deleted` key.
#[tokio::test]
async fn damaged_delta_entries_are_problems() -> TestResult {
    let (server, client, _) = setup().await;
    server.push(response(
        "200 OK",
        &[("Content-Type", "application/json")],
        br#"{"value":[{"id":"a","deleted":null},{"id":"b","deleted":{},"deleted":null},
            {"id":"c","file":{},"folder":{}},{"id":"d","root":5}],"@odata.deltaLink":"x"}"#,
    ));
    let page = client.delta_from_root(&drive()).await?;
    assert_eq!(page.items().count(), 0);
    let problems: Vec<_> = page
        .problems()
        .map(|p| (p.id.as_str(), p.deleted))
        .collect();
    assert_eq!(
        problems,
        [("a", true), ("b", true), ("c", false), ("d", false)]
    );
    Ok(())
}

/// A `Transfer-Encoding` other than `chunked` is refused.
#[tokio::test]
async fn non_chunked_transfer_encoding_is_refused() -> TestResult {
    let (server, client, _) = setup().await;
    server.push(response(
        "200 OK",
        &[("Transfer-Encoding", "gzip, chunked")],
        b"0\r\n\r\n",
    ));
    let err = client
        .download(&drive(), &item(), 0, None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::UnsupportedTransferEncoding { .. }
        ),
        "{err}"
    );
    Ok(())
}

/// A `Retry-After` beyond one day is capped, with the raw value kept.
#[tokio::test]
async fn huge_retry_after_is_capped() -> TestResult {
    let (server, client, _) = setup().await;
    server.push(json(
        "429 Too Many Requests",
        &[("Retry-After", "999999999999999999999")],
        "{}",
    ));
    let err = client.get_drive(&drive()).await.unwrap_err();
    match err.kind() {
        GraphErrorKind::Throttled {
            retry_after: Some(d),
            retry_after_raw: Some(raw),
            ..
        } => {
            assert_eq!(*d, MAX_RETRY_AFTER);
            assert_eq!(raw, "999999999999999999999");
        }
        other => panic!("{other}"),
    }
    Ok(())
}

/// A hostile item ID (very long, with a newline, an escape sequence and a token-like
/// query) on a damaged item: the error keeps the ID exactly as data, but its `Display`
/// and `Debug` show it only reduced to short printable ASCII.
#[tokio::test]
async fn hostile_item_id_is_not_shown_raw_in_errors() -> TestResult {
    let (server, client, _) = setup().await;
    let hostile = format!(
        "{}\n2026-11-14 ERROR fake\u{1b}[31m?access_token=SECRETTOKEN{}",
        "A".repeat(200),
        "B".repeat(3000)
    );
    for get_root in [false, true] {
        server.push(json(
            "200 OK",
            &[],
            &serde_json::json!({"id": hostile, "size": "not a number"}).to_string(),
        ));
        let err = if get_root {
            client.get_root(&drive()).await.unwrap_err()
        } else {
            client.get_item(&drive(), &item()).await.unwrap_err()
        };
        let GraphErrorKind::Model(ModelError::Item { id, reason }) = err.kind() else {
            panic!("{err}");
        };
        assert_eq!(
            id.as_str(),
            hostile,
            "the ID stays available exactly, as data"
        );
        assert!(reason.contains("size"));
        for text in [
            err.to_string(),
            format!("{err:?}"),
            format!("{:?}", err.kind()),
        ] {
            assert!(!text.contains('\n') && !text.contains('\u{1b}'), "{text:?}");
            assert!(!text.contains("SECRETTOKEN"), "{text}");
            assert!(text.len() < 1000, "{} characters", text.len());
        }
    }

    // The operation's IDs are shown the same way.
    let hostile_item = ItemId::new(hostile.clone())?;
    server.push(json("404 Not Found", &[], "{}"));
    let err = client.get_item(&drive(), &hostile_item).await.unwrap_err();
    for text in [err.to_string(), format!("{err:?}")] {
        assert!(
            !text.contains('\n') && !text.contains("SECRETTOKEN"),
            "{text:?}"
        );
        assert!(text.len() < 1000, "{} characters", text.len());
    }
    assert_eq!(
        err.operation(),
        &mydrive_graph::client::Operation::GetItem {
            drive: drive(),
            item: hostile_item
        }
    );
    Ok(())
}

/// Any key repeated at page level fails the page, not only `value` and the links.
#[tokio::test]
async fn any_repeated_page_level_key_fails_the_page() -> TestResult {
    let (server, client, _) = setup().await;
    for body in [
        r#"{"@odata.context":"a","@odata.context":"b","value":[],"@odata.deltaLink":"d"}"#,
        r#"{"unknown":1,"value":[],"unknown":2,"@odata.deltaLink":"d"}"#,
    ] {
        server.push(json("200 OK", &[], body));
        let err = client.delta_from_root(&drive()).await.unwrap_err();
        assert!(
            matches!(err.kind(), GraphErrorKind::Model(_)),
            "{body}: {err}"
        );
    }
    Ok(())
}

/// A `Content-Length` equal to the expected size does not verify the head when
/// `Transfer-Encoding: chunked` is also present: a connection failing part-way is a
/// `SizeMismatch`, never `Truncated`.
#[tokio::test]
async fn content_length_with_chunked_is_not_a_verified_head() -> TestResult {
    let (server, client, _) = setup().await;
    let mut partial = b"1000\r\n".to_vec();
    partial.extend_from_slice(&[5u8; 100]);
    server.push(response(
        "200 OK",
        &[("Content-Length", "4096"), ("Transfer-Encoding", "chunked")],
        &partial,
    ));
    let download = client.download(&drive(), &item(), SIZE, None).await?;
    assert!(!support::body_ref(&download).is_verified());
    let (_, err) = fail_after(download).await;
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::SizeMismatch { expected: SIZE, .. }
        ),
        "{err}"
    );
    Ok(())
}
