//! Dev-only fake Microsoft Graph server for tests (ADR-0017).
//!
//! [`GraphSim::start`] binds three listeners on `127.0.0.1` with ephemeral ports: the Graph
//! API origin ([`GraphSim::base_url`], ending `/v1.0`), a separate origin for
//! pre-authenticated download URLs, and a fake sign-in authority
//! ([`GraphSim::authority_url`]). The returned handle is also the control API: tests
//! build a tree, change it "as if another device did", switch per-drive behaviours, script
//! faults and read the request log. Dropping the handle stops the server.
//!
//! What is simulated, where each behaviour comes from, and what is not simulated yet is
//! listed in this crate's `README.md`.
//!
//! See `docs/architecture/overview.md` for what this crate owns and must not do: no
//! non-test code may depend on it (ADR-0017 R1).
//!
//! ```no_run
//! # async fn demo() -> Result<(), mydrive_graph_sim::SimError> {
//! use mydrive_graph_sim::{DriveType, GraphSim};
//!
//! let sim = GraphSim::start().await?;
//! let drive = sim.add_drive(DriveType::Personal)?;
//! let root = sim.root_id(&drive)?;
//! let docs = sim.create_folder(&drive, &root, "Documents")?;
//! sim.create_file(&drive, &docs, "notes.txt", b"hello".to_vec())?;
//! let url = format!("{}/drives/{}/root/delta", sim.base_url(), drive);
//! // GET `url` with `Authorization: Bearer {sim.token()}`.
//! # let _ = url;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

// Each feature lives in its own file: its routes, its handlers and its `impl GraphSim`
// block of the control API. `server` composes the routers (WP-014).
mod auth;
mod authority;
mod conn;
mod content;
mod delta;
mod discovery;
mod error;
mod fault;
mod identity;
mod items;
mod json;
mod log;
mod model;
mod server;
mod session;
mod throttle;
mod time;
mod write;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::task::JoinHandle;

pub use authority::{
    ACCESS_TOKEN_LIFETIME_SECS, CODE_LIFETIME_SECS, IdTokenIssue, OldRefreshToken,
    REFRESH_TOKEN_LIFETIME_SECS, ReusedCode,
};
pub use axum::http::{Method, StatusCode};
pub use discovery::{
    BLOCKED_MESSAGE, DEFAULT_POLICY_CODE, LibraryAccess, ShareWithoutPrefer, SiteId,
    encode_sharing_url,
};
pub use error::SimError;
pub use fault::{Fault, FaultAction};
pub use identity::{
    Account, AccountKind, CONSUMER_TENANT_ID, IdentityError, IdentityOptions, PersonalOrganization,
    TenantId,
};
pub use log::{
    ISSUED_SECRET_PREFIXES, LOGGED_HEADERS, LoggedRequest, MASKED_AUTHORITY_PARAMS, Origin,
    REDACTED,
};
pub use model::{
    DeltaRepeats, DownloadMode, DriveOptions, DriveType, FolderDeletes, MAX_LOGGED_REQUESTS,
    RangeMode,
};
pub use write::{ConflictBehavior, ReplaceStatus, RewriteMode};

use conn::{ConnTap, TapListener};
use identity::Directory;
use model::State;
use server::Shared;

/// A running simulator and its control API.
///
/// Every simulator has its own state, ports and tokens, so several can run in one test
/// process without seeing each other.
#[derive(Debug)]
pub struct GraphSim {
    shared: Arc<Shared>,
    token: String,
    graph_addr: SocketAddr,
    authority_addr: SocketAddr,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for GraphSim {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn bind_loopback() -> Result<(TcpListener, SocketAddr), SimError> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|source| SimError::Bind { source })?;
    let addr = listener
        .local_addr()
        .map_err(|source| SimError::Bind { source })?;
    Ok((listener, addr))
}

impl GraphSim {
    /// Starts a simulator on the current Tokio runtime.
    ///
    /// It has no drives or accounts yet and accepts one bearer token, [`GraphSim::token`],
    /// plus any access token its authority issues later.
    ///
    /// # Errors
    ///
    /// [`SimError::Bind`] if a loopback listener cannot be opened.
    pub async fn start() -> Result<Self, SimError> {
        let (graph_listener, graph_addr) = bind_loopback().await?;
        let (download_listener, download_addr) = bind_loopback().await?;
        let (authority_listener, authority_addr) = bind_loopback().await?;
        let token = format!("sim-token-{}", graph_addr.port());
        let mut state = State::new(format!("k{}x", graph_addr.port()));
        state.tokens.insert(token.clone());
        let authority_base = format!("http://{authority_addr}");
        let shared = Arc::new(
            Shared::new(
                state,
                format!("http://{graph_addr}/v1.0"),
                format!("http://{download_addr}"),
            )
            .with_authority(
                Directory::new(graph_addr.port().to_string(), authority_base.clone()),
                authority_base,
            ),
        );
        let graph = axum::serve(
            TapListener(graph_listener),
            server::graph_router(shared.clone()).into_make_service_with_connect_info::<ConnTap>(),
        );
        let download = axum::serve(
            TapListener(download_listener),
            server::download_router(shared.clone())
                .into_make_service_with_connect_info::<ConnTap>(),
        );
        let authority = axum::serve(
            TapListener(authority_listener),
            server::authority_router(shared.clone())
                .into_make_service_with_connect_info::<ConnTap>(),
        );
        // `serve` only returns on an accept error it cannot recover from; the simulator
        // then stops answering and the test's requests fail, which is the right signal.
        let tasks = vec![
            tokio::spawn(async move {
                let _ = graph.await;
            }),
            tokio::spawn(async move {
                let _ = download.await;
            }),
            tokio::spawn(async move {
                let _ = authority.await;
            }),
        ];
        Ok(Self {
            shared,
            token,
            graph_addr,
            authority_addr,
            tasks,
        })
    }

    /// The Graph base URL, `http://127.0.0.1:<port>/v1.0`, standing in for
    /// `https://graph.microsoft.com/v1.0`.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.shared.graph_base
    }

    /// The origin pre-authenticated download URLs point at, `http://127.0.0.1:<port>`.
    /// It differs from the Graph origin's port, so a client treats it as another host.
    #[must_use]
    pub fn download_origin(&self) -> &str {
        &self.shared.download_base
    }

    /// The socket address of the Graph origin.
    #[must_use]
    pub fn graph_addr(&self) -> SocketAddr {
        self.graph_addr
    }

    /// The socket address of the fake sign-in authority.
    #[must_use]
    pub fn authority_addr(&self) -> SocketAddr {
        self.authority_addr
    }
}
