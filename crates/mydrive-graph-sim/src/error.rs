//! Errors from the simulator's control API.

use mydrive_core::ids::{DriveId, IdError, ItemId};

/// Why a control call or the simulator's start-up failed.
#[derive(Debug, thiserror::Error)]
pub enum SimError {
    /// A listening socket could not be opened on the loopback interface.
    #[error("cannot start the Graph simulator: binding a loopback listener failed: {source}")]
    Bind {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The simulator built an identifier that `mydrive-core` refused.
    #[error("the Graph simulator built an invalid identifier: {0}")]
    Id(#[from] IdError),
    /// No drive with this ID exists in this simulator.
    #[error("drive {drive} does not exist in this simulator")]
    UnknownDrive {
        /// The drive asked for.
        drive: DriveId,
    },
    /// No live item with this ID exists in the drive.
    #[error("item {item} does not exist (or is deleted) in drive {drive}")]
    UnknownItem {
        /// The drive searched.
        drive: DriveId,
        /// The item asked for.
        item: ItemId,
    },
    /// The item must be a folder for this operation and is a file.
    #[error("item {item} in drive {drive} is a file, not a folder")]
    NotAFolder {
        /// The drive.
        drive: DriveId,
        /// The item.
        item: ItemId,
    },
    /// The item must be a file for this operation and is a folder.
    #[error("item {item} in drive {drive} is a folder, not a file")]
    NotAFile {
        /// The drive.
        drive: DriveId,
        /// The item.
        item: ItemId,
    },
    /// The drive's root cannot be renamed, moved or deleted.
    #[error("the root of drive {drive} cannot be renamed, moved or deleted")]
    RootItem {
        /// The drive.
        drive: DriveId,
    },
    /// The name is empty or contains `/`.
    #[error("name {name:?} is not usable: it is empty or contains '/'")]
    InvalidName {
        /// The rejected name.
        name: String,
    },
    /// A sibling already has this name, compared without regard to case.
    #[error(
        "folder {parent} in drive {drive} already has an item named {name:?} \
         (names are compared without regard to case)"
    )]
    NameConflict {
        /// The drive.
        drive: DriveId,
        /// The folder holding the clash.
        parent: ItemId,
        /// The clashing name.
        name: String,
    },
    /// A folder cannot be moved into itself or one of its descendants.
    #[error("folder {item} in drive {drive} cannot move into itself or below itself")]
    MoveIntoSelf {
        /// The drive.
        drive: DriveId,
        /// The folder being moved.
        item: ItemId,
    },
}
