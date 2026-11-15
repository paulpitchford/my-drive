//! Delta runs: staging, apply order, the resync anti-join and finishing a run (ADR-0022).
//!
//! A run's entries live in `delta_staged`, one row per item (the last occurrence,
//! ADR-0022 R2). Nothing here writes an item row (ADR-0022 R3), and only [`finish`] writes
//! the live token (ADR-0022 R4).
//!
//! **Apply order.** When the page with the delta link is staged, [`seal`] numbers every
//! entry in `apply_order`, durably, so that walking the set needs one page in memory at a
//! time however large it is:
//!
//! - An entry that is not a delete gets the depth of its parent chain within the staged
//!   set: 0 when its parent is not a staged entry (the drive root, a parent already in the
//!   item tables, or no known parent), else one more than its parent's. Depths are
//!   assigned level by level with one statement per level, each touching only that level's
//!   children through `delta_staged_by_parent`.
//! - An entry never reached that way has a parent chain that loops inside the staged set
//!   (or hangs below such a loop): it is marked `parent-cycle` and ordered after every other
//!   entry that is not a delete. So the walk always terminates and drops nothing.
//! - A depth-0 entry whose parent is staged as deleted is marked `parent-deleted`; one whose
//!   parent is neither staged nor stored is marked `missing-parent` (the mount's root is
//!   exempt: its parent may lie outside the mount). The engine resolves these by fetching
//!   the item (ADR-0022 R6).
//! - Deletes come last, ordered by item ID.

use std::num::NonZeroUsize;

use mydrive_core::hash::QuickXorHash;
use mydrive_core::ids::{ItemId, ItemKey, MountId};
use rusqlite::{Connection, Row, params};

use crate::error::{SqlContext, StoreError};
use crate::mounts::load_mount;
use crate::ops::{ITEM_COLUMNS, check_name, parse_id, query_items_params, to_i64_checked};
use crate::types::{
    DeltaRunKind, Mount, MountRule, PageLink, Staged, StagedChange, StagedCursor, StagedEntry,
    StagedFacet, StagedItem, StagedPage, StagedProblem, Timestamp, UnstagedPage,
};

/// The `apply_order` of every delete: after any depth a staged set can reach.
const DELETE_ORDER: i64 = 1 << 62;

const STAGED_COLUMNS: &str = "item_id, seq, deleted, problem, problem_detail, is_root, facet, \
     parent_item_id, name, size, created_at, modified_at, etag, ctag, server_hash, apply_order";

fn require_run(mount: &Mount, action: &'static str) -> Result<DeltaRunKind, StoreError> {
    mount.delta_run.ok_or_else(|| StoreError::NoDeltaRun {
        action,
        mount_id: mount.mount_id.clone(),
    })
}

/// The run must be complete: its delta link staged.
fn require_complete(mount: &Mount, action: &'static str) -> Result<(), StoreError> {
    require_run(mount, action)?;
    if mount.delta_staged_link.is_none() {
        return Err(StoreError::DeltaRunNotComplete {
            action,
            mount_id: mount.mount_id.clone(),
        });
    }
    Ok(())
}

fn count_staged(c: &Connection, mount_id: &MountId) -> Result<u64, StoreError> {
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM delta_staged WHERE mount_id = ?1",
            [mount_id.as_str()],
            |row| row.get(0),
        )
        .ctx(|| format!("count the staged entries of mount {mount_id}"))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

fn delete_staged(c: &Connection, mount_id: &MountId) -> Result<u64, StoreError> {
    let n = c
        .execute(
            "DELETE FROM delta_staged WHERE mount_id = ?1",
            [mount_id.as_str()],
        )
        .ctx(|| format!("remove the staged entries of mount {mount_id}"))?;
    Ok(u64::try_from(n).unwrap_or(u64::MAX))
}

pub(crate) fn begin(
    c: &Connection,
    mount_id: &MountId,
    kind: DeltaRunKind,
) -> Result<u64, StoreError> {
    let mount = load_mount(c, mount_id, "begin a delta run")?;
    let invalid = |rule| StoreError::InvalidMount {
        mount_id: mount_id.clone(),
        rule,
    };
    match (kind, mount.delta_token.is_some()) {
        (DeltaRunKind::Incremental, false) => {
            return Err(invalid(MountRule::IncrementalWithoutToken));
        }
        (DeltaRunKind::Initial, true) => return Err(invalid(MountRule::InitialWithToken)),
        _ => {}
    }
    let discarded = delete_staged(c, mount_id)?;
    c.execute(
        "UPDATE mounts SET delta_run = ?2, delta_next_link = NULL, delta_staged_link = NULL \
         WHERE mount_id = ?1",
        params![mount_id.as_str(), kind.as_str()],
    )
    .ctx(|| format!("begin a delta run of mount {mount_id}"))?;
    if discarded > 0 {
        tracing::info!(%mount_id, discarded, run = %kind, "restarted a delta run");
    }
    Ok(discarded)
}

/// Checks an entry against the staging table's rules before it is written.
fn check_entry(mount: &Mount, entry: &StagedEntry) -> Result<(), StoreError> {
    let invalid = |reason| StoreError::InvalidStagedEntry {
        mount_id: mount.mount_id.clone(),
        item_id: entry.item_id.clone(),
        reason,
    };
    let key = ItemKey::new(mount.drive_id.clone(), entry.item_id.clone());
    let check_item = |item: &StagedItem| -> Result<(), StoreError> {
        check_name(&key, &item.name)?;
        to_i64_checked(item.size, &key)?;
        match (entry.is_root, &item.parent_item_id) {
            (true, Some(_)) => Err(invalid("the drive root has no parent")),
            (false, None) => Err(invalid("an item other than the drive root has a parent")),
            _ => Ok(()),
        }
    };
    match &entry.change {
        StagedChange::Present(item) => check_item(item),
        StagedChange::Deleted if entry.is_root => Err(invalid("the drive root cannot be deleted")),
        StagedChange::Deleted => Ok(()),
        StagedChange::Problem {
            problem: StagedProblem::Reported(why),
            item,
        } => {
            if why.is_empty() {
                return Err(invalid("a reported problem must say why"));
            }
            match item {
                // A problem's description may be partial; only what the table holds is
                // checked.
                Some(item) => {
                    check_name(&key, &item.name)?;
                    to_i64_checked(item.size, &key)?;
                    if entry.is_root && item.parent_item_id.is_some() {
                        return Err(invalid("the drive root has no parent"));
                    }
                    Ok(())
                }
                None => Ok(()),
            }
        }
        StagedChange::Problem { .. } => Err(invalid(
            "only the store marks parent problems; stage the entry as it was received",
        )),
    }
}

fn upsert_entry(
    c: &Connection,
    mount: &Mount,
    run: DeltaRunKind,
    entry: &StagedEntry,
    seq: i64,
) -> Result<(), StoreError> {
    let key = ItemKey::new(mount.drive_id.clone(), entry.item_id.clone());
    let (deleted, problem, detail, item) = match &entry.change {
        StagedChange::Present(item) => (false, None, None, Some(item)),
        StagedChange::Deleted => (true, None, None, None),
        StagedChange::Problem { problem, item } => {
            let detail = match problem {
                StagedProblem::Reported(why) => Some(why.as_str()),
                _ => None,
            };
            (false, Some(problem.as_str()), detail, item.as_ref())
        }
    };
    let size = item.map(|i| to_i64_checked(i.size, &key)).transpose()?;
    c.execute(
        "INSERT INTO delta_staged (mount_id, item_id, seq, deleted, problem, problem_detail, \
         is_root, facet, parent_item_id, name, size, created_at, modified_at, etag, ctag, \
         server_hash, apply_order, run) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, NULL, \
         ?17) \
         ON CONFLICT (mount_id, item_id) DO UPDATE SET run = excluded.run, seq = excluded.seq, \
         deleted = excluded.deleted, problem = excluded.problem, \
         problem_detail = excluded.problem_detail, is_root = excluded.is_root, \
         facet = excluded.facet, parent_item_id = excluded.parent_item_id, \
         name = excluded.name, size = excluded.size, created_at = excluded.created_at, \
         modified_at = excluded.modified_at, etag = excluded.etag, ctag = excluded.ctag, \
         server_hash = excluded.server_hash, apply_order = NULL",
        params![
            mount.mount_id.as_str(),
            entry.item_id.as_str(),
            seq,
            deleted,
            problem,
            detail,
            entry.is_root,
            item.map(|i| i.facet.as_str()),
            item.and_then(|i| i.parent_item_id.as_ref().map(ItemId::as_str)),
            item.map(|i| i.name.as_str()),
            size,
            item.map(|i| i.created.as_unix_seconds()),
            item.map(|i| i.modified.as_unix_seconds()),
            item.and_then(|i| i.etag.as_deref()),
            item.and_then(|i| i.ctag.as_deref()),
            item.and_then(|i| i.server_hash.as_ref().map(QuickXorHash::to_base64)),
            run.as_str(),
        ],
    )
    .ctx(|| {
        format!(
            "stage the delta entry for item {} of mount {}",
            entry.item_id, mount.mount_id
        )
    })?;
    Ok(())
}

pub(crate) fn stage_page(
    c: &Connection,
    mount_id: &MountId,
    entries: &[StagedEntry],
    link: &PageLink,
) -> Result<(), StoreError> {
    let mount = load_mount(c, mount_id, "stage a delta page")?;
    let run = require_run(&mount, "stage a delta page")?;
    if mount.delta_staged_link.is_some() {
        return Err(StoreError::DeltaRunComplete {
            mount_id: mount_id.clone(),
        });
    }
    let (next, staged) = match link {
        PageLink::Next(l) => (Some(l.as_str()), None),
        PageLink::Delta(l) => (None, Some(l.as_str())),
    };
    if next.or(staged).is_some_and(str::is_empty) {
        return Err(StoreError::InvalidMount {
            mount_id: mount_id.clone(),
            rule: MountRule::EmptyText,
        });
    }
    for entry in entries {
        check_entry(&mount, entry)?;
    }
    let mut seq: i64 = c
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM delta_staged WHERE mount_id = ?1",
            [mount_id.as_str()],
            |row| row.get(0),
        )
        .ctx(|| format!("read the staging sequence of mount {mount_id}"))?;
    // In page order, so a later entry for an item replaces an earlier one (ADR-0022 R2).
    for entry in entries {
        seq = seq.saturating_add(1);
        upsert_entry(c, &mount, run, entry, seq)?;
    }
    c.execute(
        "UPDATE mounts SET delta_next_link = ?2, delta_staged_link = ?3 WHERE mount_id = ?1",
        params![mount_id.as_str(), next, staged],
    )
    .ctx(|| format!("record the delta link of mount {mount_id}"))?;
    if staged.is_some() {
        seal(c, &mount)?;
    }
    Ok(())
}

/// Puts a complete staged set in apply order and marks the entries it can see are
/// unusable. See the module documentation.
fn seal(c: &Connection, mount: &Mount) -> Result<(), StoreError> {
    let m = mount.mount_id.as_str();
    let what = || format!("order the staged set of mount {m}");
    c.execute(
        "UPDATE delta_staged SET apply_order = CASE WHEN deleted = 1 THEN ?2 ELSE NULL END \
         WHERE mount_id = ?1",
        params![m, DELETE_ORDER],
    )
    .ctx(what)?;
    c.execute(
        "UPDATE delta_staged SET apply_order = 0 WHERE mount_id = ?1 AND deleted = 0 \
         AND (parent_item_id IS NULL OR NOT EXISTS (SELECT 1 FROM delta_staged AS p \
              WHERE p.mount_id = ?1 AND p.item_id = delta_staged.parent_item_id \
              AND p.deleted = 0))",
        [m],
    )
    .ctx(what)?;
    let mut depth: i64 = 0;
    loop {
        let assigned = c
            .execute(
                "UPDATE delta_staged SET apply_order = ?2 + 1 \
                 WHERE mount_id = ?1 AND apply_order IS NULL AND item_id IN ( \
                     SELECT ch.item_id FROM delta_staged AS p JOIN delta_staged AS ch \
                     ON ch.mount_id = p.mount_id AND ch.parent_item_id = p.item_id \
                     WHERE p.mount_id = ?1 AND p.apply_order = ?2 AND p.deleted = 0)",
                params![m, depth],
            )
            .ctx(what)?;
        if assigned == 0 {
            break;
        }
        depth += 1;
    }
    // Never reached from outside the staged set: a cycle, or below one.
    let cycle = c
        .execute(
            "UPDATE delta_staged SET apply_order = ?2 + 1, \
             problem = COALESCE(problem, 'parent-cycle') \
             WHERE mount_id = ?1 AND apply_order IS NULL",
            params![m, depth],
        )
        .ctx(what)?;
    let deleted_parent = c
        .execute(
            "UPDATE delta_staged SET problem = 'parent-deleted' \
             WHERE mount_id = ?1 AND apply_order = 0 AND problem IS NULL AND deleted = 0 \
             AND parent_item_id IS NOT NULL AND EXISTS (SELECT 1 FROM delta_staged AS p \
                 WHERE p.mount_id = ?1 AND p.item_id = delta_staged.parent_item_id \
                 AND p.deleted = 1)",
            [m],
        )
        .ctx(what)?;
    let missing_parent = c
        .execute(
            "UPDATE delta_staged SET problem = 'missing-parent' \
             WHERE mount_id = ?1 AND apply_order = 0 AND problem IS NULL AND deleted = 0 \
             AND parent_item_id IS NOT NULL AND item_id <> ?3 \
             AND NOT EXISTS (SELECT 1 FROM delta_staged AS p \
                 WHERE p.mount_id = ?1 AND p.item_id = delta_staged.parent_item_id) \
             AND NOT EXISTS (SELECT 1 FROM items AS i \
                 WHERE i.drive_id = ?2 AND i.item_id = delta_staged.parent_item_id)",
            params![m, mount.drive_id.as_str(), mount.root_item_id.as_str()],
        )
        .ctx(what)?;
    if cycle + deleted_parent + missing_parent > 0 {
        tracing::warn!(
            mount_id = m,
            cycle,
            deleted_parent,
            missing_parent,
            "staged delta entries marked as problems"
        );
    }
    Ok(())
}

pub(crate) fn count(c: &Connection, mount_id: &MountId) -> Result<u64, StoreError> {
    load_mount(c, mount_id, "count staged entries")?;
    count_staged(c, mount_id)
}

/// The statement behind [`entries`]: `?1` mount, `?2` limit, and with `after`, `?3` and `?4`
/// the cursor's order and item ID.
fn walk_sql(after: bool) -> String {
    let after = if after {
        "AND (apply_order, item_id) > (?3, ?4) "
    } else {
        ""
    };
    format!(
        "SELECT {STAGED_COLUMNS} FROM delta_staged WHERE mount_id = ?1 {after}\
         ORDER BY apply_order, item_id LIMIT ?2"
    )
}

pub(crate) fn entries(
    c: &Connection,
    mount_id: &MountId,
    after: Option<&StagedCursor>,
    limit: NonZeroUsize,
) -> Result<StagedPage, StoreError> {
    let mount = load_mount(c, mount_id, "walk the staged set")?;
    require_complete(&mount, "walk the staged set")?;
    let want = i64::try_from(limit.get())
        .unwrap_or(i64::MAX - 1)
        .saturating_add(1);
    let what = || format!("walk the staged set of mount {mount_id}");
    let mut rows: Vec<RawStaged> = match after {
        None => {
            let mut stmt = c.prepare_cached(&walk_sql(false)).ctx(what)?;
            stmt.query_map(params![mount_id.as_str(), want], RawStaged::from_row)
                .and_then(Iterator::collect)
                .ctx(what)?
        }
        Some(cursor) => {
            let mut stmt = c.prepare_cached(&walk_sql(true)).ctx(what)?;
            stmt.query_map(
                params![
                    mount_id.as_str(),
                    want,
                    cursor.order,
                    cursor.item_id.as_str()
                ],
                RawStaged::from_row,
            )
            .and_then(Iterator::collect)
            .ctx(what)?
        }
    };
    let more = rows.len() > limit.get();
    rows.truncate(limit.get());
    let mut entries = Vec::with_capacity(rows.len());
    let mut next = None;
    for raw in rows {
        let (staged, order) = raw.into_staged(mount_id)?;
        next = Some(StagedCursor {
            order,
            item_id: staged.entry.item_id.clone(),
        });
        entries.push(staged);
    }
    Ok(StagedPage {
        entries,
        next: if more { next } else { None },
    })
}

pub(crate) fn unstaged(
    c: &Connection,
    mount_id: &MountId,
    after: Option<&ItemId>,
    limit: NonZeroUsize,
) -> Result<UnstagedPage, StoreError> {
    let mount = load_mount(c, mount_id, "list items the staged set does not contain")?;
    require_complete(&mount, "list items the staged set does not contain")?;
    let want = i64::try_from(limit.get())
        .unwrap_or(i64::MAX - 1)
        .saturating_add(1);
    let what = || format!("list the items mount {mount_id}'s staged set does not contain");
    let absent = "NOT EXISTS (SELECT 1 FROM delta_staged AS s \
                  WHERE s.mount_id = ?2 AND s.item_id = items.item_id)";
    let mut items = match after {
        None => query_items_params(
            c,
            &format!(
                "SELECT {ITEM_COLUMNS} FROM items WHERE drive_id = ?1 AND {absent} \
                 ORDER BY item_id LIMIT ?3"
            ),
            params![mount.drive_id.as_str(), mount_id.as_str(), want],
            what,
        )?,
        Some(after) => query_items_params(
            c,
            &format!(
                "SELECT {ITEM_COLUMNS} FROM items WHERE drive_id = ?1 AND item_id > ?4 \
                 AND {absent} ORDER BY item_id LIMIT ?3"
            ),
            params![
                mount.drive_id.as_str(),
                mount_id.as_str(),
                want,
                after.as_str()
            ],
            what,
        )?,
    };
    let more = items.len() > limit.get();
    items.truncate(limit.get());
    let next = if more {
        items.last().map(|i| i.key.item_id.clone())
    } else {
        None
    };
    Ok(UnstagedPage { items, next })
}

pub(crate) fn finish(c: &Connection, mount_id: &MountId) -> Result<u64, StoreError> {
    let mount = load_mount(c, mount_id, "finish a delta run")?;
    require_complete(&mount, "finish a delta run")?;
    let removed = delete_staged(c, mount_id)?;
    // One statement: every right-hand side reads the row as it was, so the staged link
    // becomes the token as the run state is cleared.
    c.execute(
        "UPDATE mounts SET delta_token = delta_staged_link, delta_run = NULL, \
         delta_next_link = NULL, delta_staged_link = NULL WHERE mount_id = ?1",
        [mount_id.as_str()],
    )
    .ctx(|| format!("make the delta link of mount {mount_id} live"))?;
    tracing::info!(%mount_id, removed, "finished a delta run");
    Ok(removed)
}

/// A `delta_staged` row as SQLite returns it, before validation.
struct RawStaged {
    item_id: String,
    seq: i64,
    deleted: bool,
    problem: Option<String>,
    problem_detail: Option<String>,
    is_root: bool,
    facet: Option<String>,
    parent_item_id: Option<String>,
    name: Option<String>,
    size: Option<i64>,
    created_at: Option<i64>,
    modified_at: Option<i64>,
    etag: Option<String>,
    ctag: Option<String>,
    server_hash: Option<String>,
    apply_order: Option<i64>,
}

impl RawStaged {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            item_id: row.get(0)?,
            seq: row.get(1)?,
            deleted: row.get(2)?,
            problem: row.get(3)?,
            problem_detail: row.get(4)?,
            is_root: row.get(5)?,
            facet: row.get(6)?,
            parent_item_id: row.get(7)?,
            name: row.get(8)?,
            size: row.get(9)?,
            created_at: row.get(10)?,
            modified_at: row.get(11)?,
            etag: row.get(12)?,
            ctag: row.get(13)?,
            server_hash: row.get(14)?,
            apply_order: row.get(15)?,
        })
    }

    fn into_staged(self, mount_id: &MountId) -> Result<(Staged, i64), StoreError> {
        let what = format!("delta_staged row {} of mount {mount_id}", self.item_id);
        let corrupt = |column: &str, value: String, reason: &str| StoreError::Corrupt {
            what: format!("{what} {column}"),
            value,
            reason: reason.to_owned(),
        };
        let item_id = parse_id(&what, "item_id", self.item_id.clone(), ItemId::new)?;
        let order = self.apply_order.ok_or_else(|| {
            corrupt(
                "apply_order",
                "NULL".to_owned(),
                "a sealed entry is ordered",
            )
        })?;
        let seq = u64::try_from(self.seq)
            .map_err(|_| corrupt("seq", self.seq.to_string(), "negative"))?;
        let server_hash = self
            .server_hash
            .map(|h| {
                QuickXorHash::from_base64(&h)
                    .map_err(|e| corrupt("server_hash", h.clone(), &e.to_string()))
            })
            .transpose()?;
        let item = match (self.name, self.facet) {
            (Some(name), Some(facet)) => {
                let facet: StagedFacet = facet
                    .parse()
                    .map_err(|reason: String| corrupt("facet", facet.clone(), &reason))?;
                let size = self
                    .size
                    .ok_or_else(|| corrupt("size", "NULL".to_owned(), "an item has a size"))?;
                Some(StagedItem {
                    parent_item_id: self
                        .parent_item_id
                        .map(|p| parse_id(&what, "parent_item_id", p, ItemId::new))
                        .transpose()?,
                    name,
                    facet,
                    size: u64::try_from(size)
                        .map_err(|_| corrupt("size", size.to_string(), "negative"))?,
                    created: Timestamp::from_unix_seconds(self.created_at.ok_or_else(|| {
                        corrupt("created_at", "NULL".to_owned(), "an item has a time")
                    })?),
                    modified: Timestamp::from_unix_seconds(self.modified_at.ok_or_else(|| {
                        corrupt("modified_at", "NULL".to_owned(), "an item has a time")
                    })?),
                    etag: self.etag,
                    ctag: self.ctag,
                    server_hash,
                })
            }
            (None, None) => None,
            (name, facet) => {
                return Err(corrupt(
                    "name",
                    format!("{name:?}/{facet:?}"),
                    "name and facet are set together",
                ));
            }
        };
        let change = match (self.deleted, self.problem.as_deref()) {
            (true, _) => StagedChange::Deleted,
            (false, None) => StagedChange::Present(item.ok_or_else(|| {
                corrupt(
                    "name",
                    "NULL".to_owned(),
                    "a usable entry describes its item",
                )
            })?),
            (false, Some(problem)) => StagedChange::Problem {
                problem: match problem {
                    "reported" => StagedProblem::Reported(self.problem_detail.unwrap_or_default()),
                    "parent-cycle" => StagedProblem::ParentCycle,
                    "missing-parent" => StagedProblem::MissingParent,
                    "parent-deleted" => StagedProblem::ParentDeleted,
                    other => {
                        return Err(corrupt("problem", other.to_owned(), "not a problem code"));
                    }
                },
                item,
            },
        };
        Ok((
            Staged {
                seq,
                entry: StagedEntry {
                    item_id,
                    is_root: self.is_root,
                    change,
                },
            },
            order,
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::ops::tests::plan;

    /// WP-017, the lead's bounded-memory point: each page of the walk is a range scan of
    /// `delta_staged_by_order` starting at the cursor, with no sort, so a page reads only
    /// its own rows however large the staged set is.
    #[test]
    fn the_walk_is_an_index_range_scan_without_sorting() {
        let dir = tempfile::tempdir().unwrap();
        let c = crate::schema::open(&dir.path().join("state.db")).unwrap();
        let first = plan(&c, &walk_sql(false), params!["m", 10]);
        let later = plan(&c, &walk_sql(true), params!["m", 10, 3, "i"]);
        for p in [&first, &later] {
            assert!(p.contains("delta_staged_by_order"), "{p}");
            assert!(!p.contains("TEMP B-TREE"), "{p}");
        }
        assert!(later.contains("(apply_order,item_id)>(?,?)"), "{later}");
    }
}
