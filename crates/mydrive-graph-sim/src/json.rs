//! Graph v1.0 JSON shapes for drives, items and errors.
//!
//! Shapes follow the `drive`, `driveItem`, `itemReference` and `deleted` resource pages and
//! the get-drive, get-item, list-children and delta examples (see the crate README).

use serde_json::{Map, Value, json};

use crate::model::{Drive, Item, ItemKind};
use crate::time::format_timestamp;

/// Which response an item is rendered for; delta differs from the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum View {
    /// Get item and list children.
    Item,
    /// A delta page.
    Delta,
}

/// A drive resource, as `GET /drives/{id}` returns it.
pub(crate) fn drive_json(drive: &Drive) -> Value {
    let used: u64 = drive
        .items
        .get(&drive.root)
        .map_or(0, |root| drive.size(root));
    let total: u64 = 1 << 40;
    json!({
        "id": drive.id.as_str(),
        "driveType": drive.drive_type.as_wire(),
        "name": if drive.drive_type.is_personal() { "OneDrive" } else { "Documents" },
        "owner": {
            "user": {
                "id": format!("00000000-0000-4000-8000-{:012x}", drive.number),
                "displayName": "Simulated User",
            }
        },
        "quota": {
            "deleted": 0,
            "remaining": total.saturating_sub(used),
            "state": "normal",
            "total": total,
            "used": used,
        },
    })
}

/// An item. `download_url` is the `@microsoft.graph.downloadUrl` annotation, if any.
pub(crate) fn item_json(
    drive: &Drive,
    item: &Item,
    view: View,
    download_url: Option<&str>,
) -> Value {
    if item.deleted {
        return deleted_item_json(drive, item);
    }
    let personal = drive.drive_type.is_personal();
    let mut out = Map::new();
    out.insert("id".into(), json!(item.id.as_str()));
    out.insert("name".into(), json!(item.name));
    out.insert("eTag".into(), json!(item.etag()));
    // Delta on OneDrive for Business omits cTag on create and modify (delta, "properties
    // omitted"); business folders never carry one (driveItem resource).
    let ctag = personal || (view == View::Item && !item.is_folder());
    if ctag {
        out.insert("cTag".into(), json!(item.ctag()));
    }
    out.insert("size".into(), json!(drive.size(item)));
    let created = format_timestamp(item.created);
    let modified = format_timestamp(item.modified);
    out.insert("createdDateTime".into(), json!(created));
    out.insert("lastModifiedDateTime".into(), json!(modified));
    out.insert(
        "fileSystemInfo".into(),
        json!({ "createdDateTime": created, "lastModifiedDateTime": modified }),
    );
    out.insert(
        "parentReference".into(),
        parent_reference(
            drive,
            item,
            view == View::Item && drive.options.parent_path_outside_delta,
        ),
    );
    match &item.kind {
        ItemKind::Folder => {
            let child_count = drive.children(&item.id).len();
            out.insert("folder".into(), json!({ "childCount": child_count }));
        }
        ItemKind::File { content, quick_xor } => {
            let mut file = Map::new();
            file.insert("mimeType".into(), json!("application/octet-stream"));
            if !content.is_empty() || drive.options.hash_empty_files {
                file.insert("hashes".into(), json!({ "quickXorHash": quick_xor }));
            }
            out.insert("file".into(), Value::Object(file));
        }
    }
    if item.parent.is_none() {
        out.insert("root".into(), json!({}));
    }
    if let Some(url) = download_url {
        out.insert("@microsoft.graph.downloadUrl".into(), json!(url));
    }
    Value::Object(out)
}

/// A deleted item in delta: the `deleted` facet, with business omitting `cTag` and `name`
/// and consumer omitting `cTag` and `size` (delta, "properties omitted").
fn deleted_item_json(drive: &Drive, item: &Item) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), json!(item.id.as_str()));
    if drive.drive_type.is_personal() {
        out.insert("name".into(), json!(item.name));
    } else {
        let size = match &item.kind {
            ItemKind::File { content, .. } => content.len() as u64,
            ItemKind::Folder => 0,
        };
        out.insert("size".into(), json!(size));
    }
    out.insert(
        "parentReference".into(),
        parent_reference(drive, item, false),
    );
    out.insert("deleted".into(), json!({}));
    if item.is_folder() {
        out.insert("folder".into(), json!({}));
    } else {
        out.insert("file".into(), json!({}));
    }
    Value::Object(out)
}

/// `parentReference`: drive ID and type, the parent's ID, and `path` only if asked for.
/// The root has no parent, so its reference names only the drive.
fn parent_reference(drive: &Drive, item: &Item, with_path: bool) -> Value {
    let mut out = Map::new();
    out.insert("driveId".into(), json!(drive.id.as_str()));
    out.insert("driveType".into(), json!(drive.drive_type.as_wire()));
    if let Some(parent) = &item.parent {
        out.insert("id".into(), json!(parent.as_str()));
        if with_path {
            out.insert("path".into(), json!(parent_path(drive, item)));
        }
    }
    Value::Object(out)
}

/// The percent-encoded path of an item's parent, for example `/drive/root:/Documents`.
fn parent_path(drive: &Drive, item: &Item) -> String {
    let mut path = if drive.drive_type.is_personal() {
        "/drive/root:".to_owned()
    } else {
        format!("/drives/{}/root:", drive.id.as_str())
    };
    for name in drive.ancestor_names(item) {
        path.push('/');
        path.push_str(&percent_encode(name));
    }
    path
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub(crate) fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A Graph error body: `{"error": {"code": ..., "message": ...}}`.
pub(crate) fn error_json(code: &str, message: &str) -> Value {
    json!({ "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encoding_matches_itemreference_example() {
        assert_eq!(percent_encode("my file.docx"), "my%20file.docx");
        assert_eq!(percent_encode("é"), "%C3%A9");
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
    }
}
