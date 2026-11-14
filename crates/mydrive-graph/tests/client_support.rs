//! Shared helpers for the Graph client tests: a scriptable fake token provider and
//! helpers to start the simulator and page through delta.
//!
//! Included by the `client_*` test files with `#[path = "client_support.rs"] mod support;`.
//! Cargo also builds it as a test binary of its own, with no tests; each including file
//! uses only part of it, hence the `dead_code` allowance.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use mydrive_core::ids::{AccountId, DriveId, ItemId};
use mydrive_graph::auth::{AccessToken, BoxFuture, TokenError, TokenProvider};
use mydrive_graph::client::{Body, Download, UserAgent};
use mydrive_graph::model::{DeltaContinuation, DeltaLink, DriveItem};
use mydrive_graph::{GraphClient, GraphError};
use mydrive_graph_sim::{DriveOptions, DriveType, GraphSim, Origin};

pub(crate) type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// The most pages a paging loop follows before the test fails instead of hanging.
pub(crate) const MAX_PAGES: usize = 1000;

/// A User-Agent the tests can recognise in the simulator's log.
pub(crate) fn test_user_agent() -> UserAgent {
    UserAgent::new("Example", "my-drive-test", "0.0.1").unwrap()
}

pub(crate) fn account() -> AccountId {
    AccountId::new("test-account").unwrap()
}

#[derive(Debug, Default)]
struct FakeState {
    current: String,
    /// What each `access_token` call returns instead of `current`, in order.
    access_errors: VecDeque<TokenError>,
    /// What each `force_refresh` returns: a new token, or an error.
    refreshes: VecDeque<Result<String, TokenError>>,
    access_calls: usize,
    /// The token each `force_refresh` was asked to replace.
    rejected: Vec<String>,
}

/// A token provider whose answers the test scripts.
#[derive(Debug, Default)]
pub(crate) struct FakeTokens {
    state: Mutex<FakeState>,
}

impl FakeTokens {
    pub(crate) fn new(token: &str) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState {
                current: token.to_owned(),
                ..FakeState::default()
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The next `force_refresh` hands out `token`.
    pub(crate) fn refresh_to(&self, token: &str) {
        self.lock().refreshes.push_back(Ok(token.to_owned()));
    }

    /// The next `force_refresh` fails with `error`.
    pub(crate) fn refresh_fails(&self, error: TokenError) {
        self.lock().refreshes.push_back(Err(error));
    }

    /// The next `access_token` fails with `error`.
    pub(crate) fn access_fails(&self, error: TokenError) {
        self.lock().access_errors.push_back(error);
    }

    /// The tokens `force_refresh` was asked to replace, in order.
    pub(crate) fn rejected(&self) -> Vec<String> {
        self.lock().rejected.clone()
    }

    pub(crate) fn access_calls(&self) -> usize {
        self.lock().access_calls
    }
}

impl TokenProvider for FakeTokens {
    fn access_token(&self) -> BoxFuture<'_, Result<AccessToken, TokenError>> {
        Box::pin(async move {
            let mut st = self.lock();
            st.access_calls += 1;
            match st.access_errors.pop_front() {
                Some(e) => Err(e),
                None => Ok(AccessToken::new(st.current.clone())),
            }
        })
    }

    fn force_refresh<'a>(
        &'a self,
        rejected: &'a AccessToken,
    ) -> BoxFuture<'a, Result<AccessToken, TokenError>> {
        Box::pin(async move {
            let mut st = self.lock();
            st.rejected.push(rejected.expose_secret().to_owned());
            match st.refreshes.pop_front() {
                Some(Ok(token)) => {
                    st.current = token.clone();
                    Ok(AccessToken::new(token))
                }
                Some(Err(e)) => Err(e),
                // Unscripted: hand back the same token, as a provider does when another
                // caller already refreshed.
                None => Ok(AccessToken::new(st.current.clone())),
            }
        })
    }
}

/// A client for `sim` using `tokens`.
pub(crate) fn client_with(sim: &GraphSim, tokens: Arc<FakeTokens>) -> GraphClient {
    GraphClient::new(sim.base_url(), tokens, test_user_agent()).unwrap()
}

/// A client for `sim` that uses the simulator's own valid token.
pub(crate) fn client(sim: &GraphSim) -> GraphClient {
    client_with(sim, FakeTokens::new(sim.token()))
}

/// A simulator with one drive of `drive_type` and `options`.
pub(crate) async fn sim_with(
    drive_type: DriveType,
    options: DriveOptions,
) -> (GraphSim, DriveId, ItemId) {
    let sim = GraphSim::start().await.unwrap();
    let drive = sim.add_drive_with(drive_type, options).unwrap();
    let root = sim.root_id(&drive).unwrap();
    (sim, drive, root)
}

/// One full delta round: from the root (no link) or from `start`, following next links
/// to the delta link. Returns every item in feed order, the delta link, and the number of
/// pages.
pub(crate) async fn full_delta(
    client: &GraphClient,
    drive: &DriveId,
    start: Option<&DeltaLink>,
) -> (Vec<DriveItem>, DeltaLink, usize) {
    let mut items = Vec::new();
    let mut page = match start {
        None => client.delta_from_root(drive).await.unwrap(),
        Some(link) => client.delta_from_link(drive, link).await.unwrap(),
    };
    for pages in 1..=MAX_PAGES {
        items.append(&mut page.items);
        match page.continuation {
            DeltaContinuation::DeltaLink(link) => return (items, link, pages),
            DeltaContinuation::NextLink(next) => {
                page = client.delta_from_link(drive, &next).await.unwrap();
            }
        }
    }
    panic!("delta did not reach a delta link within {MAX_PAGES} pages");
}

/// The body of a download, whichever coverage it has. Only for tests that do not care;
/// tests about coverage match on [`Download`] themselves.
pub(crate) fn body(download: Download) -> Body {
    match download {
        Download::Range { body, .. } | Download::WholeFile { body } => body,
    }
}

/// The whole body of a download, whichever coverage it has.
pub(crate) async fn read_all(download: Download) -> Result<Vec<u8>, GraphError> {
    body(download).bytes().await
}

/// The IDs of `items`, in order.
pub(crate) fn ids(items: &[DriveItem]) -> Vec<String> {
    items.iter().map(|i| i.id.as_str().to_owned()).collect()
}

/// How many logged requests arrived on `origin`.
pub(crate) fn count(sim: &GraphSim, origin: Origin) -> usize {
    sim.requests().iter().filter(|r| r.origin == origin).count()
}
