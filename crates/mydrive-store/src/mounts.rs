//! Accounts, mounts and mount removal.

use std::path::PathBuf;
use std::str::FromStr;

use mydrive_core::ids::{AccountId, DriveId, ItemId, ItemKey, MountId, TenantId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{SqlContext, StoreError};
use crate::ops::parse_id;
use crate::types::{
    Account, BlockedReason, DIRTY_SAMPLE_LIMIT, DeltaRunKind, DriveType, LocalOnlyContent, Mount,
    MountAccess, MountBlock, MountPause, MountRemoved, MountRule, ReadOnlyReason, Timestamp,
    account_kind_str, parse_account_kind,
};

const ACCOUNT_COLUMNS: &str = "account_id, tenant_id, kind, username, display_name, org_name, \
     needs_sign_in, hash_percent_refused";

pub(crate) const MOUNT_COLUMNS: &str = "mount_id, account_id, drive_id, root_item_id, \
     mount_point, drive_type, display_name, read_only, read_only_reason, paused, paused_until, \
     blocked_reason, blocked_message, delta_token, delta_run, delta_next_link, \
     delta_staged_link";

// ------------------------------------------------------------------ accounts

pub(crate) fn upsert_account(c: &Connection, account: &Account) -> Result<(), StoreError> {
    for (value, reason) in [
        (&account.username, "the user name is empty"),
        (&account.display_name, "the display name is empty"),
        (&account.org_name, "the organisation name is empty"),
    ] {
        if value.as_deref() == Some("") {
            return Err(StoreError::InvalidAccount {
                account_id: account.account_id.clone(),
                reason,
            });
        }
    }
    c.execute(
        &format!(
            "INSERT INTO accounts ({ACCOUNT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT (account_id) DO UPDATE SET tenant_id = excluded.tenant_id, \
             kind = excluded.kind, username = excluded.username, \
             display_name = excluded.display_name, org_name = excluded.org_name, \
             needs_sign_in = excluded.needs_sign_in, \
             hash_percent_refused = excluded.hash_percent_refused"
        ),
        params![
            account.account_id.as_str(),
            account.tenant_id.as_ref().map(TenantId::as_str),
            account_kind_str(account.kind),
            account.username.as_deref(),
            account.display_name.as_deref(),
            account.org_name.as_deref(),
            account.needs_sign_in,
            account.refuses_hash_and_percent,
        ],
    )
    .ctx(|| format!("store account {}", account.account_id))?;
    Ok(())
}

pub(crate) fn account(
    c: &Connection,
    account_id: &AccountId,
) -> Result<Option<Account>, StoreError> {
    c.query_row(
        &format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE account_id = ?1"),
        [account_id.as_str()],
        RawAccount::from_row,
    )
    .optional()
    .ctx(|| format!("read account {account_id}"))?
    .map(RawAccount::into_account)
    .transpose()
}

pub(crate) fn accounts(c: &Connection) -> Result<Vec<Account>, StoreError> {
    let mut stmt = c
        .prepare_cached(&format!(
            "SELECT {ACCOUNT_COLUMNS} FROM accounts ORDER BY account_id"
        ))
        .ctx(|| "list accounts")?;
    let raws: Vec<RawAccount> = stmt
        .query_map([], RawAccount::from_row)
        .and_then(Iterator::collect)
        .ctx(|| "list accounts")?;
    raws.into_iter().map(RawAccount::into_account).collect()
}

pub(crate) fn update_account_flag(
    c: &Connection,
    account_id: &AccountId,
    column: &str,
    value: bool,
) -> Result<(), StoreError> {
    let changed = c
        .execute(
            &format!("UPDATE accounts SET {column} = ?2 WHERE account_id = ?1"),
            params![account_id.as_str(), value],
        )
        .ctx(|| format!("set {column} of account {account_id}"))?;
    if changed == 0 {
        return Err(StoreError::AccountNotFound {
            action: "set an account flag",
            account_id: account_id.clone(),
        });
    }
    Ok(())
}

struct RawAccount {
    account_id: String,
    tenant_id: Option<String>,
    kind: String,
    username: Option<String>,
    display_name: Option<String>,
    org_name: Option<String>,
    needs_sign_in: bool,
    hash_percent_refused: bool,
}

impl RawAccount {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            account_id: row.get(0)?,
            tenant_id: row.get(1)?,
            kind: row.get(2)?,
            username: row.get(3)?,
            display_name: row.get(4)?,
            org_name: row.get(5)?,
            needs_sign_in: row.get(6)?,
            hash_percent_refused: row.get(7)?,
        })
    }

    fn into_account(self) -> Result<Account, StoreError> {
        let what = format!("accounts row {}", self.account_id);
        let kind = parse_account_kind(&self.kind).map_err(|reason| StoreError::Corrupt {
            what: format!("{what} kind"),
            value: self.kind.clone(),
            reason,
        })?;
        Ok(Account {
            account_id: parse_id(&what, "account_id", self.account_id.clone(), AccountId::new)?,
            tenant_id: self
                .tenant_id
                .map(|t| parse_id(&what, "tenant_id", t, TenantId::new))
                .transpose()?,
            kind,
            username: self.username,
            display_name: self.display_name,
            org_name: self.org_name,
            needs_sign_in: self.needs_sign_in,
            refuses_hash_and_percent: self.hash_percent_refused,
        })
    }
}

// -------------------------------------------------------------------- mounts

/// Checks every cross-column rule of the `mounts` table on a whole row, so a violation is
/// a typed error rather than a SQL constraint failure. Order matters only for which rule
/// is reported when several are broken.
fn check_mount_rules(mount: &Mount) -> Result<(), MountRule> {
    let links = mount.delta_next_link.is_some() || mount.delta_staged_link.is_some();
    if mount.delta_run.is_none() && links {
        return Err(MountRule::LinkWithoutRun);
    }
    if mount.delta_next_link.is_some() && mount.delta_staged_link.is_some() {
        return Err(MountRule::BothLinks);
    }
    if mount.blocked_message.is_some() && mount.blocked_reason.is_none() {
        return Err(MountRule::BlockedMessageWithoutReason);
    }
    if mount.paused_until.is_some() && !mount.paused {
        return Err(MountRule::PausedUntilWithoutPause);
    }
    if mount.read_only_reason.is_some() && !mount.read_only {
        return Err(MountRule::ReadOnlyReasonWithoutReadOnly);
    }
    match (mount.delta_run, mount.delta_token.is_some()) {
        (Some(DeltaRunKind::Incremental), false) => return Err(MountRule::IncrementalWithoutToken),
        (Some(DeltaRunKind::Initial), true) => return Err(MountRule::InitialWithToken),
        _ => {}
    }
    let empty = |v: &Option<String>| v.as_deref() == Some("");
    if empty(&mount.display_name)
        || empty(&mount.blocked_message)
        || empty(&mount.delta_next_link)
        || empty(&mount.delta_staged_link)
    {
        return Err(MountRule::EmptyText);
    }
    Ok(())
}

pub(crate) fn insert_mount(c: &Connection, mount: &Mount) -> Result<(), StoreError> {
    let invalid = |rule| StoreError::InvalidMount {
        mount_id: mount.mount_id.clone(),
        rule,
    };
    check_mount_rules(mount).map_err(invalid)?;
    if mount.drive_type.is_none() {
        return Err(invalid(MountRule::DriveTypeRequired));
    }
    if mount.delta_token.is_some()
        || mount.delta_next_link.is_some()
        || mount.delta_staged_link.is_some()
    {
        return Err(invalid(MountRule::NewMountHasDeltaState));
    }
    if mount
        .delta_run
        .is_some_and(|run| run != DeltaRunKind::Initial)
    {
        return Err(invalid(MountRule::NewMountRunNotInitial));
    }
    let mount_point = mount
        .mount_point
        .to_str()
        .ok_or_else(|| StoreError::NonUtf8Path {
            what: format!("mount point of mount {}", mount.mount_id),
            path: mount.mount_point.clone(),
        })?;
    let account_known: bool = c
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM accounts WHERE account_id = ?1)",
            [mount.account_id.as_str()],
            |row| row.get(0),
        )
        .ctx(|| format!("look up account {}", mount.account_id))?;
    if !account_known {
        return Err(StoreError::AccountNotFound {
            action: "add a mount",
            account_id: mount.account_id.clone(),
        });
    }
    let taken: bool = c
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM mounts WHERE mount_id = ?1)",
            [mount.mount_id.as_str()],
            |row| row.get(0),
        )
        .ctx(|| format!("look up mount {}", mount.mount_id))?;
    if taken {
        return Err(StoreError::MountExists {
            mount_id: mount.mount_id.clone(),
        });
    }
    let same_root: Option<String> = c
        .query_row(
            "SELECT mount_id FROM mounts WHERE drive_id = ?1 AND root_item_id = ?2",
            [mount.drive_id.as_str(), mount.root_item_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .ctx(|| {
            format!(
                "check whether the root of mount {} is mounted",
                mount.mount_id
            )
        })?;
    if let Some(existing) = same_root {
        return Err(StoreError::MountRootTaken {
            mount_id: mount.mount_id.clone(),
            root: ItemKey::new(mount.drive_id.clone(), mount.root_item_id.clone()),
            existing: parse_id("mount row", "mount_id", existing, MountId::new)?,
        });
    }
    c.execute(
        &format!(
            "INSERT INTO mounts ({MOUNT_COLUMNS}) VALUES \
             (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)"
        ),
        params![
            mount.mount_id.as_str(),
            mount.account_id.as_str(),
            mount.drive_id.as_str(),
            mount.root_item_id.as_str(),
            mount_point,
            mount.drive_type.map(DriveType::as_str),
            mount.display_name.as_deref(),
            mount.read_only,
            mount.read_only_reason.map(ReadOnlyReason::as_str),
            mount.paused,
            mount.paused_until.map(Timestamp::as_unix_seconds),
            mount.blocked_reason.map(BlockedReason::as_str),
            mount.blocked_message.as_deref(),
            mount.delta_token.as_deref(),
            mount.delta_run.map(DeltaRunKind::as_str),
            mount.delta_next_link.as_deref(),
            mount.delta_staged_link.as_deref(),
        ],
    )
    .ctx(|| format!("insert mount {}", mount.mount_id))?;
    Ok(())
}

pub(crate) fn mount(c: &Connection, mount_id: &MountId) -> Result<Option<Mount>, StoreError> {
    c.query_row(
        &format!("SELECT {MOUNT_COLUMNS} FROM mounts WHERE mount_id = ?1"),
        [mount_id.as_str()],
        RawMount::from_row,
    )
    .optional()
    .ctx(|| format!("read mount {mount_id}"))?
    .map(RawMount::into_mount)
    .transpose()
}

/// The mount, or [`StoreError::MountNotFound`].
pub(crate) fn load_mount(
    c: &Connection,
    mount_id: &MountId,
    action: &'static str,
) -> Result<Mount, StoreError> {
    mount(c, mount_id)?.ok_or_else(|| StoreError::MountNotFound {
        action,
        mount_id: mount_id.clone(),
    })
}

pub(crate) fn mounts(c: &Connection) -> Result<Vec<Mount>, StoreError> {
    let mut stmt = c
        .prepare_cached(&format!(
            "SELECT {MOUNT_COLUMNS} FROM mounts ORDER BY mount_id"
        ))
        .ctx(|| "list mounts")?;
    let raws = stmt
        .query_map([], RawMount::from_row)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .ctx(|| "list mounts")?;
    raws.into_iter().map(RawMount::into_mount).collect()
}

pub(crate) fn delta_token(
    c: &Connection,
    mount_id: &MountId,
) -> Result<Option<String>, StoreError> {
    c.query_row(
        "SELECT delta_token FROM mounts WHERE mount_id = ?1",
        [mount_id.as_str()],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .ctx(|| format!("read the delta token of mount {mount_id}"))?
    .ok_or_else(|| StoreError::MountNotFound {
        action: "read the delta token",
        mount_id: mount_id.clone(),
    })
}

fn changed_mount(
    changed: usize,
    mount_id: &MountId,
    action: &'static str,
) -> Result<(), StoreError> {
    if changed == 0 {
        return Err(StoreError::MountNotFound {
            action,
            mount_id: mount_id.clone(),
        });
    }
    Ok(())
}

pub(crate) fn set_paused(
    c: &Connection,
    mount_id: &MountId,
    pause: MountPause,
) -> Result<(), StoreError> {
    let (paused, until) = match pause {
        MountPause::Running => (false, None),
        MountPause::Paused => (true, None),
        MountPause::PausedUntil(at) => (true, Some(at.as_unix_seconds())),
    };
    let changed = c
        .execute(
            "UPDATE mounts SET paused = ?2, paused_until = ?3 WHERE mount_id = ?1",
            params![mount_id.as_str(), paused, until],
        )
        .ctx(|| format!("set the pause of mount {mount_id}"))?;
    changed_mount(changed, mount_id, "set the pause")
}

pub(crate) fn set_blocked(
    c: &Connection,
    mount_id: &MountId,
    block: Option<&MountBlock>,
) -> Result<(), StoreError> {
    if block.is_some_and(|b| b.message.as_deref() == Some("")) {
        return Err(StoreError::InvalidMount {
            mount_id: mount_id.clone(),
            rule: MountRule::EmptyText,
        });
    }
    let changed = c
        .execute(
            "UPDATE mounts SET blocked_reason = ?2, blocked_message = ?3 WHERE mount_id = ?1",
            params![
                mount_id.as_str(),
                block.map(|b| b.reason.as_str()),
                block.and_then(|b| b.message.as_deref()),
            ],
        )
        .ctx(|| format!("set the block of mount {mount_id}"))?;
    changed_mount(changed, mount_id, "set the block")
}

pub(crate) fn set_read_only(
    c: &Connection,
    mount_id: &MountId,
    access: MountAccess,
) -> Result<(), StoreError> {
    let (read_only, reason) = match access {
        MountAccess::ReadWrite => (false, None),
        MountAccess::ReadOnly(reason) => (true, reason.map(ReadOnlyReason::as_str)),
    };
    let changed = c
        .execute(
            "UPDATE mounts SET read_only = ?2, read_only_reason = ?3 WHERE mount_id = ?1",
            params![mount_id.as_str(), read_only, reason],
        )
        .ctx(|| format!("set the read-only flag of mount {mount_id}"))?;
    changed_mount(changed, mount_id, "set the read-only flag")
}

pub(crate) fn set_drive_info(
    c: &Connection,
    mount_id: &MountId,
    drive_type: DriveType,
    display_name: Option<&str>,
) -> Result<(), StoreError> {
    if display_name == Some("") {
        return Err(StoreError::InvalidMount {
            mount_id: mount_id.clone(),
            rule: MountRule::EmptyText,
        });
    }
    let changed = c
        .execute(
            "UPDATE mounts SET drive_type = ?2, display_name = ?3 WHERE mount_id = ?1",
            params![mount_id.as_str(), drive_type.as_str(), display_name],
        )
        .ctx(|| format!("set the drive information of mount {mount_id}"))?;
    changed_mount(changed, mount_id, "set the drive information")
}

pub(crate) fn remove_mount(
    c: &Connection,
    mount_id: &MountId,
    local_only: LocalOnlyContent,
) -> Result<MountRemoved, StoreError> {
    let mount = load_mount(c, mount_id, "remove a mount")?;
    let drive = mount.drive_id.as_str();

    // The mount's item rows are its drive's (store-schema, "Mounts own their drive's
    // rows"): removing them under another mount of the same drive would empty that one.
    let (others, other): (i64, Option<String>) = c
        .query_row(
            "SELECT COUNT(*), MIN(mount_id) FROM mounts WHERE drive_id = ?1 AND mount_id <> ?2",
            [drive, mount_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ctx(|| format!("look for other mounts of the drive of mount {mount_id}"))?;
    if let Some(other) = other {
        return Err(StoreError::DriveSharedByMounts {
            mount_id: mount_id.clone(),
            drive_id: mount.drive_id.clone(),
            other: parse_id("mounts row", "mount_id", other, MountId::new)?,
            count: u64::try_from(others).unwrap_or(0),
        });
    }

    let dirty: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM items WHERE local_dirty = 1 AND drive_id = ?1",
            [drive],
            |row| row.get(0),
        )
        .ctx(|| format!("count the dirty items of mount {mount_id}"))?;
    let dirty = u64::try_from(dirty).unwrap_or(0);
    match local_only {
        LocalOnlyContent::Refuse if dirty > 0 => {
            return Err(StoreError::MountHasDirtyItems {
                mount_id: mount_id.clone(),
                count: dirty,
                sample: dirty_sample(c, &mount.drive_id)?,
            });
        }
        LocalOnlyContent::KeptElsewhere { dirty_items } if dirty_items != dirty => {
            return Err(StoreError::DirtyCountChanged {
                mount_id: mount_id.clone(),
                confirmed: dirty_items,
                actual: dirty,
            });
        }
        LocalOnlyContent::Refuse | LocalOnlyContent::KeptElsewhere { .. } => {}
    }

    let staged = c
        .execute(
            "DELETE FROM delta_staged WHERE mount_id = ?1",
            [mount_id.as_str()],
        )
        .ctx(|| format!("remove the staged entries of mount {mount_id}"))?;
    let ranges = c
        .execute("DELETE FROM hydrated_ranges WHERE drive_id = ?1", [drive])
        .ctx(|| format!("remove the hydrated ranges of mount {mount_id}"))?;
    let items = c
        .execute("DELETE FROM items WHERE drive_id = ?1", [drive])
        .ctx(|| format!("remove the items of mount {mount_id}"))?;
    let removed = c
        .execute(
            "DELETE FROM mounts WHERE mount_id = ?1",
            [mount_id.as_str()],
        )
        .ctx(|| format!("remove mount {mount_id}"))?;
    changed_mount(removed, mount_id, "remove a mount")?;
    tracing::info!(%mount_id, items, dirty, "removed mount rows");
    let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    Ok(MountRemoved {
        items: count(items),
        ranges: count(ranges),
        staged: count(staged),
    })
}

/// At most [`DIRTY_SAMPLE_LIMIT`] dirty items of the drive, in key order.
fn dirty_sample(c: &Connection, drive_id: &DriveId) -> Result<Vec<ItemKey>, StoreError> {
    let mut stmt = c
        .prepare_cached(
            "SELECT item_id FROM items WHERE local_dirty = 1 AND drive_id = ?1 \
             ORDER BY item_id LIMIT ?2",
        )
        .ctx(|| format!("list dirty items of drive {drive_id}"))?;
    let limit = i64::try_from(DIRTY_SAMPLE_LIMIT).unwrap_or(i64::MAX);
    let ids: Vec<String> = stmt
        .query_map(params![drive_id.as_str(), limit], |row| row.get(0))
        .and_then(Iterator::collect)
        .ctx(|| format!("list dirty items of drive {drive_id}"))?;
    ids.into_iter()
        .map(|id| {
            parse_id("items row", "item_id", id, ItemId::new)
                .map(|item_id| ItemKey::new(drive_id.clone(), item_id))
        })
        .collect()
}

/// A `mounts` row as SQLite returns it, before validation.
struct RawMount {
    mount_id: String,
    account_id: String,
    drive_id: String,
    root_item_id: String,
    mount_point: String,
    drive_type: Option<String>,
    display_name: Option<String>,
    read_only: bool,
    read_only_reason: Option<String>,
    paused: bool,
    paused_until: Option<i64>,
    blocked_reason: Option<String>,
    blocked_message: Option<String>,
    delta_token: Option<String>,
    delta_run: Option<String>,
    delta_next_link: Option<String>,
    delta_staged_link: Option<String>,
}

impl RawMount {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            mount_id: row.get(0)?,
            account_id: row.get(1)?,
            drive_id: row.get(2)?,
            root_item_id: row.get(3)?,
            mount_point: row.get(4)?,
            drive_type: row.get(5)?,
            display_name: row.get(6)?,
            read_only: row.get(7)?,
            read_only_reason: row.get(8)?,
            paused: row.get(9)?,
            paused_until: row.get(10)?,
            blocked_reason: row.get(11)?,
            blocked_message: row.get(12)?,
            delta_token: row.get(13)?,
            delta_run: row.get(14)?,
            delta_next_link: row.get(15)?,
            delta_staged_link: row.get(16)?,
        })
    }

    fn into_mount(self) -> Result<Mount, StoreError> {
        let what = format!("mounts row {}", self.mount_id);
        Ok(Mount {
            mount_id: parse_id(&what, "mount_id", self.mount_id.clone(), MountId::new)?,
            account_id: parse_id(&what, "account_id", self.account_id, AccountId::new)?,
            drive_id: parse_id(&what, "drive_id", self.drive_id, DriveId::new)?,
            root_item_id: parse_id(&what, "root_item_id", self.root_item_id, ItemId::new)?,
            mount_point: PathBuf::from(self.mount_point),
            drive_type: parse_opt(&what, "drive_type", self.drive_type)?,
            display_name: self.display_name,
            read_only: self.read_only,
            read_only_reason: parse_opt(&what, "read_only_reason", self.read_only_reason)?,
            paused: self.paused,
            paused_until: self.paused_until.map(Timestamp::from_unix_seconds),
            blocked_reason: parse_opt(&what, "blocked_reason", self.blocked_reason)?,
            blocked_message: self.blocked_message,
            delta_token: self.delta_token,
            delta_run: parse_opt(&what, "delta_run", self.delta_run)?,
            delta_next_link: self.delta_next_link,
            delta_staged_link: self.delta_staged_link,
        })
    }
}

fn parse_opt<T: FromStr<Err = String>>(
    what: &str,
    column: &str,
    value: Option<String>,
) -> Result<Option<T>, StoreError> {
    value
        .map(|v| {
            v.parse().map_err(|reason| StoreError::Corrupt {
                what: format!("{what} {column}"),
                value: v.clone(),
                reason,
            })
        })
        .transpose()
}
