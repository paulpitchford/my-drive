//! Drives and items: the read routes for drives, items and folder children, and the
//! control API that builds and changes the tree "as if another device did".
//!
//! Documented in `docs/items.md`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State as AxState};
use axum::response::Response;
use axum::routing::get;
use mydrive_core::ids::{DriveId, ItemId};
use serde_json::{Value, json};

use crate::content::download_url;
use crate::json::{View, drive_json, item_json, percent_encode};
use crate::model::{DriveOptions, DriveType, State};
use crate::server::{Shared, bad_request, find_drive, item_id, not_found, ok_json};
use crate::{GraphSim, SimError};

/// The drive, item and children routes of the Graph origin.
pub(crate) fn routes() -> Router<Arc<Shared>> {
    Router::new()
        .route("/v1.0/drives/{drive}", get(get_drive))
        .route("/v1.0/drives/{drive}/root", get(get_root))
        .route(
            "/v1.0/drives/{drive}/root/children",
            get(list_root_children),
        )
        .route("/v1.0/drives/{drive}/items/{item}", get(get_item))
        .route(
            "/v1.0/drives/{drive}/items/{item}/children",
            get(list_children),
        )
}

/// Renders a live item for get-item or list-children.
fn render_item(
    shared: &Shared,
    state: &mut State,
    drive: &DriveId,
    item: &ItemId,
) -> Option<Value> {
    let url = download_url(shared, state, drive, item);
    let d = state.drives.get(drive)?;
    let i = d.live(item).ok()?;
    Some(item_json(d, i, View::Item, url.as_deref()))
}

// ---- handlers --------------------------------------------------------------------------

async fn get_drive(AxState(shared): AxState<Arc<Shared>>, Path(drive): Path<String>) -> Response {
    let state = shared.lock();
    match find_drive(&state, &drive) {
        Some(d) => ok_json(drive_json(d)),
        None => not_found(),
    }
}

async fn get_root(AxState(shared): AxState<Arc<Shared>>, Path(drive): Path<String>) -> Response {
    let mut state = shared.lock();
    let Some((id, root)) = find_drive(&state, &drive).map(|d| (d.id.clone(), d.root.clone()))
    else {
        return not_found();
    };
    render_item(&shared, &mut state, &id, &root).map_or_else(not_found, ok_json)
}

async fn get_item(
    AxState(shared): AxState<Arc<Shared>>,
    Path((drive, item)): Path<(String, String)>,
) -> Response {
    let mut state = shared.lock();
    let (Some(id), Some(item)) = (
        find_drive(&state, &drive).map(|d| d.id.clone()),
        item_id(&item),
    ) else {
        return not_found();
    };
    render_item(&shared, &mut state, &id, &item).map_or_else(not_found, ok_json)
}

async fn list_root_children(
    AxState(shared): AxState<Arc<Shared>>,
    Path(drive): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let mut state = shared.lock();
    let Some((id, root)) = find_drive(&state, &drive).map(|d| (d.id.clone(), d.root.clone()))
    else {
        return not_found();
    };
    children_page(&shared, &mut state, &id, &root, &query)
}

async fn list_children(
    AxState(shared): AxState<Arc<Shared>>,
    Path((drive, item)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let mut state = shared.lock();
    let (Some(id), Some(item)) = (
        find_drive(&state, &drive).map(|d| d.id.clone()),
        item_id(&item),
    ) else {
        return not_found();
    };
    children_page(&shared, &mut state, &id, &item, &query)
}

/// One page of a folder's children. `$top` sets the page size; the nextLink carries
/// `$skiptoken`, the case-folded name of the last child served.
fn children_page(
    shared: &Shared,
    state: &mut State,
    drive: &DriveId,
    folder: &ItemId,
    query: &HashMap<String, String>,
) -> Response {
    let Some(d) = state.drives.get(drive) else {
        return not_found();
    };
    match d.live(folder) {
        Ok(item) if item.is_folder() => {}
        Ok(_) => return bad_request("The item is not a folder."),
        Err(_) => return not_found(),
    }
    let page_size = match query.get("$top") {
        Some(top) => match top.parse::<usize>() {
            Ok(n) if n > 0 => n,
            _ => return bad_request("Invalid $top value."),
        },
        None => d.options.children_page_size.max(1),
    };
    let after = query.get("$skiptoken").map(String::as_str);
    let page = d.children_page(folder, after, page_size);
    let mut value = Vec::with_capacity(page.items.len());
    for id in &page.items {
        if let Some(json) = render_item(shared, state, drive, id) {
            value.push(json);
        }
    }
    let mut body = json!({ "value": value });
    if let Some(next) = page.next_after {
        body["@odata.nextLink"] = json!(format!(
            "{}/drives/{}/items/{}/children?$top={page_size}&$skiptoken={}",
            shared.graph_base,
            percent_encode(drive.as_str()),
            percent_encode(folder.as_str()),
            percent_encode(&next)
        ));
    }
    ok_json(body)
}

// ---- control API -----------------------------------------------------------------------

impl GraphSim {
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
    /// [`FolderDeletes::EveryDescendant`](crate::FolderDeletes::EveryDescendant).
    ///
    /// # Errors
    ///
    /// An unknown drive or item, or the root.
    pub fn delete(&self, drive: &DriveId, item: &ItemId) -> Result<(), SimError> {
        self.shared.lock().delete(drive, item)
    }
}
