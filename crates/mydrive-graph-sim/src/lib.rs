//! Dev-only fake Microsoft Graph server for tests (ADR-0017).
//!
//! [`GraphSim::start`] binds two listeners on `127.0.0.1` with ephemeral ports: the Graph
//! API origin ([`GraphSim::base_url`], ending `/v1.0`) and a separate origin for
//! pre-authenticated download URLs. The returned handle is also the control API: tests
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

mod conn;
mod error;
mod fault;
mod json;
mod log;
mod model;
mod server;
mod time;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::body::Bytes;
use mydrive_core::ids::{DriveId, ItemId};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

pub use axum::http::{Method, StatusCode};
pub use error::SimError;
pub use fault::{Fault, FaultAction};
pub use log::{LOGGED_HEADERS, LoggedRequest, Origin};
pub use model::{DeltaRepeats, DownloadMode, DriveOptions, DriveType, FolderDeletes, RangeMode};

use conn::{ConnTap, TapListener};
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
    /// It has no drives yet and accepts one bearer token, [`GraphSim::token`].
    ///
    /// # Errors
    ///
    /// [`SimError::Bind`] if a loopback listener cannot be opened.
    pub async fn start() -> Result<Self, SimError> {
        let (graph_listener, graph_addr) = bind_loopback().await?;
        let (download_listener, download_addr) = bind_loopback().await?;
        let token = format!("sim-token-{}", graph_addr.port());
        let mut state = State::new(format!("k{}x", graph_addr.port()));
        state.tokens.insert(token.clone());
        let shared = Arc::new(Shared::new(
            state,
            format!("http://{graph_addr}/v1.0"),
            format!("http://{download_addr}"),
        ));
        let graph = axum::serve(
            TapListener(graph_listener),
            server::graph_router(shared.clone()).into_make_service_with_connect_info::<ConnTap>(),
        );
        let download = axum::serve(
            TapListener(download_listener),
            server::download_router(shared.clone())
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
        ];
        Ok(Self {
            shared,
            token,
            graph_addr,
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

    /// A bearer token this simulator accepts, unique to it.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Makes another bearer token valid.
    pub fn add_token(&self, token: impl Into<String>) {
        self.shared.lock().tokens.insert(token.into());
    }

    /// Makes a bearer token invalid; later requests carrying it get 401.
    pub fn revoke_token(&self, token: &str) {
        self.shared.lock().tokens.remove(token);
    }

    /// Adds a drive with the default options and returns its ID.
    ///
    /// # Errors
    ///
    /// [`SimError::Id`] only if the simulator builds an invalid ID, which is a bug.
    pub fn add_drive(&self, drive_type: DriveType) -> Result<DriveId, SimError> {
        self.add_drive_with(drive_type, DriveOptions::default())
    }

    /// Adds a drive with the given options and returns its ID.
    ///
    /// # Errors
    ///
    /// [`SimError::Id`] only if the simulator builds an invalid ID, which is a bug.
    pub fn add_drive_with(
        &self,
        drive_type: DriveType,
        options: DriveOptions,
    ) -> Result<DriveId, SimError> {
        self.shared.lock().add_drive(drive_type, options)
    }

    /// The drive's current options.
    ///
    /// # Errors
    ///
    /// [`SimError::UnknownDrive`] if the drive does not exist.
    pub fn options(&self, drive: &DriveId) -> Result<DriveOptions, SimError> {
        Ok(self.shared.lock().drive(drive)?.options.clone())
    }

    /// Replaces the drive's options. Takes effect from the next request.
    ///
    /// # Errors
    ///
    /// [`SimError::UnknownDrive`] if the drive does not exist.
    pub fn set_options(&self, drive: &DriveId, options: DriveOptions) -> Result<(), SimError> {
        self.shared.lock().drive_mut(drive)?.options = options;
        Ok(())
    }

    /// The ID of the drive's root folder.
    ///
    /// # Errors
    ///
    /// [`SimError::UnknownDrive`] if the drive does not exist.
    pub fn root_id(&self, drive: &DriveId) -> Result<ItemId, SimError> {
        Ok(self.shared.lock().drive(drive)?.root.clone())
    }

    /// Creates a folder in `parent`, as another device would.
    ///
    /// # Errors
    ///
    /// An unknown drive or parent, a parent that is a file, an empty name or one with `/`,
    /// or a name a sibling already has without regard to case.
    pub fn create_folder(
        &self,
        drive: &DriveId,
        parent: &ItemId,
        name: &str,
    ) -> Result<ItemId, SimError> {
        self.shared.lock().create_folder(drive, parent, name)
    }

    /// Creates a file with `content` in `parent`, as another device would.
    ///
    /// # Errors
    ///
    /// As for [`GraphSim::create_folder`].
    pub fn create_file(
        &self,
        drive: &DriveId,
        parent: &ItemId,
        name: &str,
        content: impl Into<Bytes>,
    ) -> Result<ItemId, SimError> {
        self.shared
            .lock()
            .create_file(drive, parent, name, content.into())
    }

    /// Replaces a file's content: new size, quickXorHash, eTag, cTag and modification
    /// time.
    ///
    /// # Errors
    ///
    /// An unknown drive or item, or an item that is a folder.
    pub fn modify_file(
        &self,
        drive: &DriveId,
        item: &ItemId,
        content: impl Into<Bytes>,
    ) -> Result<(), SimError> {
        self.shared.lock().modify_file(drive, item, content.into())
    }

    /// Renames an item in place. Only the item itself appears in delta.
    ///
    /// # Errors
    ///
    /// An unknown drive or item, the root, or an unusable or clashing name.
    pub fn rename(&self, drive: &DriveId, item: &ItemId, new_name: &str) -> Result<(), SimError> {
        self.shared.lock().rename(drive, item, new_name)
    }

    /// Moves an item to another folder, keeping its name.
    ///
    /// # Errors
    ///
    /// An unknown drive, item or folder, the root, a destination that is a file or lies
    /// inside the item, or a name clash in the destination.
    pub fn move_item(
        &self,
        drive: &DriveId,
        item: &ItemId,
        new_parent: &ItemId,
    ) -> Result<(), SimError> {
        self.shared.lock().move_to(drive, item, new_parent)
    }

    /// Deletes an item and everything below it. The item appears in delta with the
    /// `deleted` facet; its descendants follow it only under
    /// [`FolderDeletes::EveryDescendant`].
    ///
    /// # Errors
    ///
    /// An unknown drive or item, or the root.
    pub fn delete(&self, drive: &DriveId, item: &ItemId) -> Result<(), SimError> {
        self.shared.lock().delete(drive, item)
    }

    /// Moves the simulator's clock forward. Timestamps are whole seconds and the clock
    /// starts at 2026-01-01T00:00:00Z; it never reads the wall clock.
    pub fn advance_clock(&self, secs: u32) {
        self.shared.lock().clock += i64::from(secs);
    }

    /// Queues a fault. It applies to the next request that matches it, then is removed.
    pub fn push_fault(&self, fault: Fault) {
        self.shared.lock().faults.push_back(fault);
    }

    /// How many queued faults have not yet matched a request.
    #[must_use]
    pub fn pending_faults(&self) -> usize {
        self.shared.lock().faults.len()
    }

    /// Removes every queued fault.
    pub fn clear_faults(&self) {
        self.shared.lock().faults.clear();
    }

    /// Every request received so far, in the order the responses were completed.
    #[must_use]
    pub fn requests(&self) -> Vec<LoggedRequest> {
        self.shared.lock().log.clone()
    }

    /// Empties the request log.
    pub fn clear_requests(&self) {
        self.shared.lock().log.clear();
    }
}
