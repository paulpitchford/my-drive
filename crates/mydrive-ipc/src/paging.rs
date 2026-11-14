//! Splitting `item.status` results into pages that fit one line (ADR-0019 R1 to R3).
//!
//! The daemon builds the full, stably ordered list of entries for a request (children by
//! name, or paths in request order) and calls [`item_status_page`] with the request's cursor.
//! The cursor is an opaque string to clients; here it is the index of the first entry of the
//! page, so with the same list every entry is delivered exactly once.

use crate::codec::MAX_LINE_BYTES;
use crate::envelope::{Id, SuccessResponse};
use crate::error::{ErrorCode, RpcError};
use crate::methods::{ItemStatusEntry, ItemStatusResult, method};

/// The longest cursor this module produces: the decimal digits of `u64::MAX`.
const CURSOR_MAX: &str = "18446744073709551615";

/// Builds the page of an `item.status` result that starts at `cursor`.
///
/// The page is as large as fits: the whole success response for request `id`, encoded as one
/// line, is at most [`MAX_LINE_BYTES`] (ADR-0019 R2). `next_cursor` is set when entries
/// remain and absent on the last page (R3). `directory` is repeated on every page.
///
/// # Errors
///
/// `invalid-params` if `cursor` is not one this function produced for a list of this length;
/// `internal` if a single entry cannot fit in a line on its own, or serialisation fails.
pub fn item_status_page(
    directory: Option<&ItemStatusEntry>,
    entries: &[ItemStatusEntry],
    cursor: Option<&str>,
    id: &Id,
) -> Result<ItemStatusResult, RpcError> {
    let start = match cursor {
        None => 0,
        Some(text) => text
            .parse::<usize>()
            .ok()
            .filter(|&i| i <= entries.len())
            .ok_or_else(|| {
                RpcError::invalid_params(
                    method::ITEM_STATUS,
                    format!("cursor {text:?} does not belong to this listing"),
                )
            })?,
    };

    let mut page = ItemStatusResult {
        directory: directory.cloned(),
        entries: Vec::new(),
        next_cursor: Some(CURSOR_MAX.to_owned()),
    };
    // Every byte except the entries themselves, with the longest possible cursor.
    let overhead = encoded_len(id, &page)?;
    let budget = MAX_LINE_BYTES.saturating_sub(overhead);

    let mut used = 0_usize;
    let mut end = start;
    for entry in &entries[start..] {
        let separator = usize::from(end > start);
        let len = serde_json::to_vec(entry).map_err(internal)?.len() + separator;
        if used + len > budget {
            break;
        }
        used += len;
        end += 1;
    }
    if end == start && start < entries.len() {
        return Err(RpcError::new(
            ErrorCode::Internal,
            format!(
                "the status entry for {} is too large to send in one {MAX_LINE_BYTES}-byte line",
                entries[start].path
            ),
        ));
    }

    page.entries = entries[start..end].to_vec();
    page.next_cursor = (end < entries.len()).then(|| end.to_string());

    // The sum above is exact; this check guards the guarantee against future changes.
    let total = encoded_len(id, &page)?;
    if total > MAX_LINE_BYTES {
        return Err(RpcError::new(
            ErrorCode::Internal,
            format!("item.status page is {total} bytes, over the {MAX_LINE_BYTES}-byte limit"),
        ));
    }
    Ok(page)
}

fn encoded_len(id: &Id, result: &ItemStatusResult) -> Result<usize, RpcError> {
    let response = SuccessResponse::new(id.clone(), result).map_err(internal)?;
    Ok(serde_json::to_vec(&response).map_err(internal)?.len())
}

fn internal(e: serde_json::Error) -> RpcError {
    RpcError::new(
        ErrorCode::Internal,
        format!("cannot serialise item.status page: {e}"),
    )
}

#[cfg(test)]
mod tests {
    use mydrive_core::state::{EffectivePin, Hydration, ItemState, Modifiers, Pin, Sync};

    use super::*;
    use crate::codec::encode_line;
    use crate::error::ErrorCode;
    use crate::methods::ItemInfo;
    use crate::path::ProtocolPath;

    fn entry(path: String) -> ItemStatusEntry {
        let state = ItemState {
            hydration: Hydration::Placeholder,
            pin: Pin::Inherit,
            sync: Sync::InSync,
        };
        ItemStatusEntry::known(
            ProtocolPath::new(path).unwrap(),
            ItemInfo::new(state, EffectivePin::Unpinned, Modifiers::default()),
        )
    }

    /// Fetches every page the way a client's cursor loop does, checking each page's line.
    fn fetch_all(
        directory: Option<&ItemStatusEntry>,
        entries: &[ItemStatusEntry],
        id: &Id,
    ) -> (Vec<ItemStatusEntry>, usize) {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let page = item_status_page(directory, entries, cursor.as_deref(), id).unwrap();
            pages += 1;
            let line = encode_line(&SuccessResponse::new(id.clone(), &page).unwrap())
                .expect("page encodes within the line limit");
            assert!(line.len() - 1 <= MAX_LINE_BYTES);
            assert_eq!(page.directory.as_ref(), directory);
            out.extend(page.entries);
            match page.next_cursor {
                // ADR-0019 R3: present on every page but the last.
                Some(next) => {
                    assert!(out.len() < entries.len(), "cursor on the last page");
                    cursor = Some(next);
                }
                None => return (out, pages),
            }
        }
    }

    /// ADR-0019 R1, R2, R3: 50,000 entries are delivered in full, in order, each exactly
    /// once, with no line over 1 MiB and `next_cursor` absent only on the last page.
    #[test]
    fn fifty_thousand_entries_page_under_the_line_limit() {
        let dir = "/home/user/OneDrive/Pictures/2026";
        let entries: Vec<_> = (0..50_000)
            .map(|i| entry(format!("{dir}/IMG_{i:05}.jpg")))
            .collect();
        let per_entry = serde_json::to_vec(&entries[0]).unwrap().len();
        assert!(
            per_entry > 150,
            "entries are realistically sized ({per_entry} bytes)"
        );
        let directory = entry(dir.to_owned());
        let id = Id::String("a-long-request-identifier-0123456789".into());
        let (got, pages) = fetch_all(Some(&directory), &entries, &id);
        assert!(pages > 1, "50,000 entries must not fit in one line");
        assert_eq!(got, entries);
    }

    /// Pages pack tightly even with entries of very different sizes.
    #[test]
    fn mixed_entry_sizes_fill_pages_exactly() {
        let entries: Vec<_> = (0..3_000)
            .map(|i| entry(format!("/m/{}{i}", "x".repeat((i * 37) % 3_000))))
            .collect();
        let (got, pages) = fetch_all(None, &entries, &Id::Number(i64::MIN));
        assert!(pages > 1);
        assert_eq!(got, entries);
    }

    #[test]
    fn small_and_empty_listings_are_one_page() {
        let entries = vec![entry("/m/a".into()), entry("/m/b".into())];
        let page = item_status_page(None, &entries, None, &Id::Number(1)).unwrap();
        assert_eq!(page.entries, entries);
        assert_eq!(page.next_cursor, None);
        let page = item_status_page(None, &[], None, &Id::Number(1)).unwrap();
        assert!(page.entries.is_empty());
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn foreign_cursors_are_invalid_params() {
        let entries = vec![entry("/m/a".into())];
        for bad in ["x", "-1", "2", ""] {
            let err = item_status_page(None, &entries, Some(bad), &Id::Number(1)).unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{bad}");
        }
    }
}
