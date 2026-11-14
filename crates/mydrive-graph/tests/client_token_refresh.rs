//! AC5: a `401` causes exactly one forced refresh of the rejected token and one retry; a
//! token provider error is returned as its own typed error and never retried.

#[path = "client_support.rs"]
mod support;

use mydrive_graph::GraphErrorKind;
use mydrive_graph::auth::{TokenError, TokenRequestError};
use mydrive_graph_sim::{DownloadMode, DriveOptions, DriveType, Fault, Origin, StatusCode};
use support::{FakeTokens, TestResult, account, client_with, count, sim_with};

/// AC5: a rejected token is refreshed once and the request retried once with the new
/// token, which succeeds.
#[tokio::test]
async fn ac5_one_refresh_and_one_retry_after_401() -> TestResult {
    let (sim, drive, _) = sim_with(DriveType::Business, DriveOptions::default()).await;
    let tokens = FakeTokens::new("stale-token");
    tokens.refresh_to(sim.token());
    let client = client_with(&sim, tokens.clone());

    client.delta_from_root(&drive).await?;
    assert_eq!(tokens.rejected(), ["stale-token"]);
    let log = sim.requests();
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(log[0].status, 401);
    assert_eq!(log[0].header("authorization"), Some("Bearer stale-token"));
    assert_eq!(log[1].status, 200);
    assert_eq!(
        log[1].header("authorization"),
        Some(format!("Bearer {}", sim.token()).as_str())
    );
    Ok(())
}

/// AC5: a second `401` after the refresh is returned as unauthorised; there is no
/// second refresh and no third request.
#[tokio::test]
async fn ac5_second_401_is_unauthorized() -> TestResult {
    let (sim, drive, _) = sim_with(DriveType::Personal, DriveOptions::default()).await;
    let tokens = FakeTokens::new("stale-token");
    tokens.refresh_to("still-bad");
    let err = client_with(&sim, tokens.clone())
        .get_drive(&drive)
        .await
        .unwrap_err();
    match err.kind() {
        GraphErrorKind::Unauthorized { codes } => {
            assert_eq!(codes.primary(), Some("InvalidAuthenticationToken"));
        }
        other => panic!("{other}"),
    }
    assert_eq!(tokens.rejected(), ["stale-token"]);
    assert_eq!(sim.requests().len(), 2);
    Ok(())
}

/// AC5 through the fault queue: a scripted `401` on a valid token is refreshed once and
/// retried; two scripted `401`s give unauthorised after one refresh.
#[tokio::test]
async fn ac5_scripted_401s() -> TestResult {
    let (sim, drive, root) = sim_with(DriveType::Business, DriveOptions::default()).await;
    let tokens = FakeTokens::new(sim.token());
    let client = client_with(&sim, tokens.clone());

    sim.push_fault(Fault::status(StatusCode::UNAUTHORIZED).on_path("/delta"));
    client.delta_from_root(&drive).await?;
    assert_eq!(tokens.rejected().len(), 1);
    assert_eq!(sim.pending_faults(), 0);

    sim.clear_requests();
    sim.push_fault(Fault::status(StatusCode::UNAUTHORIZED));
    sim.push_fault(Fault::status(StatusCode::UNAUTHORIZED));
    let err = client.get_item(&drive, &root).await.unwrap_err();
    assert!(
        matches!(err.kind(), GraphErrorKind::Unauthorized { .. }),
        "{err}"
    );
    assert_eq!(tokens.rejected().len(), 2, "one more refresh, not two");
    assert_eq!(sim.requests().len(), 2);
    Ok(())
}

/// AC5 on downloads: a `401` on `/content` is refreshed once, then the redirect is
/// followed as usual.
#[tokio::test]
async fn ac5_download_refreshes_once() -> TestResult {
    let (sim, drive, root) = sim_with(DriveType::Personal, DriveOptions::default()).await;
    let file = sim.create_file(&drive, &root, "f", vec![5; 64])?;
    let tokens = FakeTokens::new("stale-token");
    tokens.refresh_to(sim.token());
    let body = client_with(&sim, tokens.clone())
        .download(&drive, &file, None)
        .await?
        .bytes()
        .await?;
    assert_eq!(body, vec![5; 64]);
    assert_eq!(tokens.rejected(), ["stale-token"]);
    assert_eq!(count(&sim, Origin::Graph), 2);
    assert_eq!(count(&sim, Origin::Download), 1);
    Ok(())
}

fn provider_errors() -> Vec<TokenError> {
    vec![
        TokenError::NeedsSignIn { account: account() },
        TokenError::Transient {
            account: account(),
            source: TokenRequestError::Status { status: 503 },
        },
        TokenError::Closed { account: account() },
    ]
}

/// AC5: when the provider cannot give a token (needs sign-in, transient, closed), the
/// error is returned as its own typed error at once: no request, no refresh, no loop.
#[tokio::test]
async fn ac5_provider_error_is_returned_not_retried() -> TestResult {
    let (sim, drive, root) = sim_with(
        DriveType::Business,
        DriveOptions {
            download: DownloadMode::Direct,
            ..DriveOptions::default()
        },
    )
    .await;
    let file = sim.create_file(&drive, &root, "f", vec![1])?;
    for error in provider_errors() {
        let tokens = FakeTokens::new(sim.token());
        tokens.access_fails(error.clone());
        let client = client_with(&sim, tokens.clone());
        let err = client.delta_from_root(&drive).await.unwrap_err();
        match err.kind() {
            GraphErrorKind::Token(got) => assert_eq!(got.to_string(), error.to_string()),
            other => panic!("{other}"),
        }
        assert_eq!(tokens.access_calls(), 1);
        assert!(tokens.rejected().is_empty());

        tokens.access_fails(error.clone());
        let err = client.download(&drive, &file, None).await.unwrap_err();
        assert!(matches!(err.kind(), GraphErrorKind::Token(_)), "{err}");
    }
    assert!(sim.requests().is_empty(), "no request without a token");
    Ok(())
}

/// AC5: a provider error from the forced refresh after a `401` is returned as its own
/// typed error after exactly one request.
#[tokio::test]
async fn ac5_refresh_error_is_returned_not_retried() -> TestResult {
    let (sim, drive, _) = sim_with(DriveType::Business, DriveOptions::default()).await;
    for error in provider_errors() {
        sim.clear_requests();
        let tokens = FakeTokens::new("stale-token");
        tokens.refresh_fails(error.clone());
        let err = client_with(&sim, tokens.clone())
            .get_drive(&drive)
            .await
            .unwrap_err();
        match err.kind() {
            GraphErrorKind::Token(got) => {
                assert_eq!(got.is_needs_sign_in(), error.is_needs_sign_in());
            }
            other => panic!("{other}"),
        }
        assert_eq!(tokens.rejected(), ["stale-token"]);
        assert_eq!(sim.requests().len(), 1);
    }
    Ok(())
}
