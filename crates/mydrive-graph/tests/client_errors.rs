//! Error mapping against the simulator's fault queue: AC4, AC8, and the other statuses.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "client_support.rs"]
mod support;

use std::time::Duration;

use mydrive_graph::GraphErrorKind;
use mydrive_graph::client::{Operation, Resync};
use mydrive_graph::model::{DeltaContinuation, DeltaLink};
use mydrive_graph_sim::{DriveOptions, DriveType, Fault, Method, StatusCode};
use support::{TestResult, client, full_delta, sim_with};

/// AC4, ADR-0011 R6 (client side): a 429 or 503 with `Retry-After` maps to the throttled
/// error carrying that duration, and the client sends exactly one request: it neither
/// waits nor retries.
#[tokio::test]
async fn ac4_throttled_with_retry_after_and_no_retry() -> TestResult {
    let (sim, drive, _) = sim_with(DriveType::Business, DriveOptions::default()).await;
    let client = client(&sim);
    for (status, secs) in [
        (StatusCode::TOO_MANY_REQUESTS, 7),
        (StatusCode::SERVICE_UNAVAILABLE, 120),
    ] {
        sim.clear_requests();
        sim.push_fault(Fault::status(status).retry_after(secs).on_path("/delta"));
        let err = client.delta_from_root(&drive).await.unwrap_err();
        match err.kind() {
            GraphErrorKind::Throttled {
                status: got,
                retry_after,
            } => {
                assert_eq!(*got, status.as_u16());
                assert_eq!(*retry_after, Some(Duration::from_secs(secs)));
            }
            other => panic!("{status}: {other}"),
        }
        assert_eq!(
            err.operation(),
            &Operation::Delta {
                drive: drive.clone()
            }
        );
        assert_eq!(sim.requests().len(), 1, "the client must not retry");
        assert_eq!(sim.pending_faults(), 0);
    }

    // Without Retry-After it is still throttling, with no duration; the scheduler decides.
    sim.push_fault(Fault::status(StatusCode::TOO_MANY_REQUESTS));
    let err = client.get_drive(&drive).await.unwrap_err();
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::Throttled {
                status: 429,
                retry_after: None
            }
        ),
        "{err}"
    );
    Ok(())
}

/// Answers the first delta request of a fresh simulator with `fault`. The simulator is
/// returned so it outlives the error.
async fn gone(fault: Fault) -> (mydrive_graph::GraphError, mydrive_graph_sim::GraphSim) {
    let (sim, drive, _) = sim_with(DriveType::Business, DriveOptions::default()).await;
    sim.push_fault(fault.on_path("/root/delta"));
    let err = client(&sim).delta_from_root(&drive).await.unwrap_err();
    (err, sim)
}

/// AC8, ADR-0011 R3 (client side): both documented `410` resync codes map to distinct
/// values carrying the restart link from `Location`; following that link enumerates
/// again; an unknown `410` code maps to a third, conservative value.
#[tokio::test]
async fn ac8_adr_0011_r3_resync_variants() -> TestResult {
    for (code, expected) in [
        ("resyncChangesApplyDifferences", Resync::ApplyDifferences),
        ("resyncChangesUploadDifferences", Resync::UploadDifferences),
    ] {
        let (sim, drive, root) = sim_with(DriveType::Business, DriveOptions::default()).await;
        let file = sim.create_file(&drive, &root, "kept.txt", b"k".to_vec())?;
        let restart = format!("{}/drives/{drive}/root/delta", sim.base_url());
        sim.push_fault(
            Fault::status(StatusCode::GONE)
                .code(code)
                .location(restart.clone())
                .on_path("/root/delta"),
        );
        let client = client(&sim);
        let err = client.delta_from_root(&drive).await.unwrap_err();
        let GraphErrorKind::ResyncRequired {
            resync,
            restart: link,
        } = err.kind()
        else {
            panic!("{code}: {err}");
        };
        assert_eq!(resync, &expected);
        let link = link.clone().expect("the Location restart link is kept");
        assert_eq!(link.as_str(), restart);

        let (items, _, _) = full_delta(&client, &drive, Some(&link)).await;
        assert!(items.iter().any(|i| i.id == file), "{code}");
    }
    assert_ne!(Resync::ApplyDifferences, Resync::UploadDifferences);

    for fault in [
        Fault::status(StatusCode::GONE).code("resyncRequired"),
        Fault::status(StatusCode::GONE),
    ] {
        let (err, _sim) = gone(fault).await;
        match err.kind() {
            GraphErrorKind::ResyncRequired {
                resync: Resync::Unknown { codes },
                restart: None,
            } => assert!(codes.primary().is_some()),
            other => panic!("{other}"),
        }
    }
    Ok(())
}

/// AC8: the resync code is also found as an inner error code, a `410` without `Location`
/// carries no restart link, and a `Location` on another origin is dropped rather than
/// trusted with the bearer token.
#[tokio::test]
async fn resync_location_absent_or_foreign() -> TestResult {
    let (err, _sim) =
        gone(Fault::status(StatusCode::GONE).code("resyncChangesApplyDifferences")).await;
    assert!(matches!(
        err.kind(),
        GraphErrorKind::ResyncRequired {
            resync: Resync::ApplyDifferences,
            restart: None
        }
    ));

    let (err, _sim) = gone(
        Fault::status(StatusCode::GONE)
            .code("resyncChangesUploadDifferences")
            .location("http://192.0.2.1/v1.0/drives/x/root/delta"),
    )
    .await;
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::ResyncRequired {
                resync: Resync::UploadDifferences,
                restart: None
            }
        ),
        "{err}"
    );
    Ok(())
}

/// A relative `Location` is resolved against the request URL.
#[tokio::test]
async fn resync_relative_location_is_resolved() -> TestResult {
    let (sim, drive, _) = sim_with(DriveType::Business, DriveOptions::default()).await;
    sim.push_fault(
        Fault::status(StatusCode::GONE)
            .code("resyncChangesApplyDifferences")
            .location(format!("/v1.0/drives/{drive}/root/delta"))
            .on_path("/root/delta"),
    );
    let err = client(&sim).delta_from_root(&drive).await.unwrap_err();
    let GraphErrorKind::ResyncRequired {
        restart: Some(link),
        ..
    } = err.kind()
    else {
        panic!("{err}");
    };
    assert_eq!(
        link.as_str(),
        format!("{}/drives/{drive}/root/delta", sim.base_url())
    );
    Ok(())
}

/// The other statuses: 403 keeps its code for policy detection, 404, 5xx, and anything
/// unexpected (a bad delta token gets 400). A 410 outside delta is not a resync.
#[tokio::test]
async fn statuses_map_to_typed_errors() -> TestResult {
    let (sim, drive, root) = sim_with(DriveType::DocumentLibrary, DriveOptions::default()).await;
    let client = client(&sim);

    sim.push_fault(
        Fault::status(StatusCode::FORBIDDEN)
            .code("blockedByPolicy")
            .on_method(Method::GET),
    );
    let err = client.get_item(&drive, &root).await.unwrap_err();
    match err.kind() {
        GraphErrorKind::Forbidden { codes } => {
            assert_eq!(codes.primary(), Some("blockedByPolicy"));
            assert!(codes.contains("blockedByPolicy"));
        }
        other => panic!("{other}"),
    }
    assert_eq!(
        err.operation(),
        &Operation::GetItem {
            drive: drive.clone(),
            item: root.clone()
        }
    );
    assert!(
        err.to_string().contains(root.as_str()),
        "errors name the item: {err}"
    );

    let err = client
        .get_item(&drive, &"01MISSING".parse()?)
        .await
        .unwrap_err();
    match err.kind() {
        GraphErrorKind::NotFound { codes } => assert_eq!(codes.primary(), Some("itemNotFound")),
        other => panic!("{other}"),
    }

    sim.push_fault(Fault::status(StatusCode::BAD_GATEWAY));
    let err = client.get_drive(&drive).await.unwrap_err();
    assert!(
        matches!(err.kind(), GraphErrorKind::ServerError { status: 502, .. }),
        "{err}"
    );

    let bad = DeltaLink::new(format!(
        "{}/drives/{drive}/root/delta?token=garbage",
        sim.base_url()
    ));
    let err = client.delta_from_link(&drive, &bad).await.unwrap_err();
    match err.kind() {
        GraphErrorKind::UnexpectedStatus { status: 400, codes } => {
            assert_eq!(codes.primary(), Some("invalidRequest"));
        }
        other => panic!("{other}"),
    }

    sim.push_fault(Fault::status(StatusCode::GONE).code("resyncChangesApplyDifferences"));
    let err = client.get_drive(&drive).await.unwrap_err();
    assert!(
        matches!(
            err.kind(),
            GraphErrorKind::UnexpectedStatus { status: 410, .. }
        ),
        "{err}"
    );

    // The client is still usable after every error.
    let page = client.delta_from_root(&drive).await?;
    assert!(matches!(page.continuation, DeltaContinuation::DeltaLink(_)));
    Ok(())
}

/// A connection failure is a transport error, not a status.
#[tokio::test]
async fn transport_error_when_nothing_listens() -> TestResult {
    // A port that was free a moment ago and has no listener now.
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let client = mydrive_graph::GraphClient::new(
        &format!("http://127.0.0.1:{port}/v1.0"),
        support::FakeTokens::new("t"),
        support::test_user_agent(),
    )?;
    let drive = "b!nowhere".parse()?;
    let err = client.get_drive(&drive).await.unwrap_err();
    assert!(matches!(err.kind(), GraphErrorKind::Transport(_)), "{err}");
    assert!(
        !err.to_string().contains(&port.to_string()),
        "transport errors carry no URL: {err}"
    );
    Ok(())
}
