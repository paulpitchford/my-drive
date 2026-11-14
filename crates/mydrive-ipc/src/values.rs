//! Open value sets (ADR-0019 "Closed and open value sets").
//!
//! Within a major version these sets may gain values. Each type here has an `Other` variant
//! that keeps a value this build does not know, so a newer daemon never makes an older client
//! fail a whole response (ADR-0019 R4). Code must therefore always have a fallback arm.
//!
//! The state axes are open on the wire only (ADR-0020): [`WireHydration`], [`WirePin`],
//! [`WireEffectivePin`] and [`WireSync`] convert from the closed `mydrive-core` types the
//! daemon uses internally. The display status is the only closed set; it is
//! `mydrive_core::state::DisplayStatus`, which rejects unknown values (ADR-0020 R7).
//!
//! [`VALUE_SETS`] lists every enumerated wire field; a test checks it against the catalogue
//! and against the types in this crate.

use std::fmt;

use mydrive_core::names;
use mydrive_core::state::{
    DisplayStatus, EffectivePin, Hydration, Modifiers as CoreModifiers, Pin, Sync as SyncState,
};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Generates an open string enum: known variants with fixed kebab-case wire forms, plus
/// `Other(String)` for anything else. `Display`, `serde`, `as_str` and `from_wire` share one
/// table.
macro_rules! open_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident, kind = $kind:literal {
            $( $(#[$vmeta:meta])* $variant:ident => $wire:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant, )+
            /// A value this build does not know, kept as received.
            Other(String),
        }

        impl $name {
            /// Every wire form this build knows, in declaration order.
            pub const KNOWN: &'static [&'static str] = &[ $( $wire, )+ ];

            /// The wire form.
            #[must_use]
            pub fn as_str(&self) -> &str {
                match self {
                    $( Self::$variant => $wire, )+
                    Self::Other(other) => other,
                }
            }

            /// Parses a wire form. Never fails: unknown strings become `Other`.
            #[must_use]
            pub fn from_wire(value: &str) -> Self {
                match value {
                    $( $wire => Self::$variant, )+
                    other => Self::Other(other.to_owned()),
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct OpenVisitor;

                impl Visitor<'_> for OpenVisitor {
                    type Value = $name;

                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        write!(f, "a {} string", $kind)
                    }

                    fn visit_str<E: de::Error>(self, v: &str) -> Result<$name, E> {
                        Ok($name::from_wire(v))
                    }
                }

                deserializer.deserialize_str(OpenVisitor)
            }
        }
    };
}

open_enum! {
    /// The kind of account. Open set.
    pub enum AccountKind, kind = "account kind" {
        /// A personal Microsoft account.
        Personal => "personal",
        /// A work or school account.
        Work => "work",
    }
}

impl From<names::AccountKind> for AccountKind {
    fn from(kind: names::AccountKind) -> Self {
        match kind {
            names::AccountKind::Personal => Self::Personal,
            names::AccountKind::Work => Self::Work,
        }
    }
}

open_enum! {
    /// Account state (overview, "Account and mount states"). Open set; clients treat an
    /// unknown state as "not active" and show its word.
    pub enum AccountState, kind = "account state" {
        /// Tokens are valid.
        SignedIn => "signed-in",
        /// The refresh token was rejected; the account's mounts stop syncing.
        NeedsSignIn => "needs-sign-in",
    }
}

open_enum! {
    /// Mount state (overview, "Account and mount states"). Open set; clients treat an unknown
    /// state as "not active" and show its word.
    pub enum MountState, kind = "mount state" {
        /// Syncing normally.
        Active => "active",
        /// Paused by the user.
        Paused => "paused",
        /// Refused by organisation policy; `blocked_reason` says why.
        Blocked => "blocked",
        /// Enumerating; the mount point is not mounted until the first full delta is applied.
        Resyncing => "resyncing",
    }
}

open_enum! {
    /// Why a mount is blocked (F09). Open set.
    pub enum BlockedReason, kind = "blocked reason" {
        /// A policy of the organisation refuses access to this site or library, for example
        /// from unmanaged devices.
        OrganisationPolicy => "organisation-policy",
        /// The tenant refuses the app as a whole (consent refused); every mount of the account
        /// is blocked.
        TenantRefused => "tenant-refused",
    }
}

open_enum! {
    /// Why a request was refused (error code `refused`). Open set.
    pub enum RefusalReason, kind = "refusal reason" {
        /// The item's sync state is not `in-sync`; `data.sync` names it. Dehydrating would risk
        /// content that is not on the server (ADR-0008 R5, R7).
        NotInSync => "not-in-sync",
        /// The item's effective pin is `pinned` (engine-level dehydrate, ADR-0008 R5).
        Pinned => "pinned",
        /// The file has open handles (F06 step 9).
        InUse => "in-use",
        /// The item changed on the server or is gone; delta must apply first (F06).
        RemoteChanged => "remote-changed",
        /// The content does not fit on the backing filesystem (F06).
        InsufficientSpace => "insufficient-space",
    }
}

open_enum! {
    /// Why an item's display status is `error` or `blocked` (F04, F08, F09). Open set.
    pub enum ItemReason, kind = "item reason" {
        /// Local and remote changes collided.
        Conflict => "conflict",
        /// The name cannot exist on the server.
        NameNotAllowed => "name-not-allowed",
        /// The server's storage is full.
        StorageFull => "storage-full",
        /// The server refused the write because the item or library is read-only.
        ReadOnly => "read-only",
        /// A download keeps failing.
        DownloadFailed => "download-failed",
        /// Downloaded content did not match the server's hash.
        HashMismatch => "hash-mismatch",
        /// A local change was lost from the backing directory outside my-drive.
        LocalChangeLost => "local-change-lost",
        /// Organisation policy refuses access.
        OrganisationPolicy => "organisation-policy",
    }
}

open_enum! {
    /// Why an item or mount is read-only (error code `read-only`). Open set.
    pub enum ReadOnlyReason, kind = "read-only reason" {
        /// The whole library is read-only for this account.
        LibraryReadOnly => "library-read-only",
        /// The account may read this item but not change it.
        NoWritePermission => "no-write-permission",
    }
}

open_enum! {
    /// What a client should do about an `invalid-params` error, when it can act on it. Open
    /// set.
    pub enum InvalidParamsReason, kind = "invalid-params reason" {
        /// The cursor was not issued for these parameters, or is in a form or version this
        /// daemon does not issue: start the listing again from the first page, without a
        /// cursor.
        RestartListing => "restart-listing",
    }
}

/// Implements conversion between a closed `mydrive-core` axis and its open wire type.
macro_rules! wire_axis {
    ($wire:ident, $core:ty) => {
        impl From<$core> for $wire {
            fn from(value: $core) -> Self {
                Self::from_wire(value.as_str())
            }
        }

        impl $wire {
            /// The `mydrive-core` value, or `None` for a value this build does not know.
            #[must_use]
            pub fn to_core(&self) -> Option<$core> {
                self.as_str().parse().ok()
            }
        }
    };
}

open_enum! {
    /// Hydration axis on the wire. Open set (ADR-0020).
    pub enum WireHydration, kind = "hydration state" {
        /// Metadata only.
        Placeholder => "placeholder",
        /// Some byte ranges are local.
        Partial => "partial",
        /// All content is local.
        Full => "full",
    }
}
wire_axis!(WireHydration, Hydration);

open_enum! {
    /// Pin axis on the wire. Open set (ADR-0020).
    pub enum WirePin, kind = "pin state" {
        /// Take the nearest ancestor's value.
        Inherit => "inherit",
        /// Always keep on this device.
        Pinned => "pinned",
        /// May be dehydrated.
        Unpinned => "unpinned",
    }
}
wire_axis!(WirePin, Pin);

open_enum! {
    /// Effective pin on the wire. Open set (ADR-0020).
    pub enum WireEffectivePin, kind = "effective pin" {
        /// The item must stay hydrated.
        Pinned => "pinned",
        /// The item may be dehydrated.
        Unpinned => "unpinned",
    }
}
wire_axis!(WireEffectivePin, EffectivePin);

open_enum! {
    /// Sync axis on the wire. Open set (ADR-0020).
    pub enum WireSync, kind = "sync state" {
        /// Nothing pending.
        InSync => "in-sync",
        /// A download is scheduled.
        PendingDownload => "pending-download",
        /// Local changes not yet on the server.
        PendingUpload => "pending-upload",
        /// An upload is in flight.
        Uploading => "uploading",
        /// Local and remote changes collided.
        Conflict => "conflict",
        /// A permanent failure needs attention.
        Error => "error",
        /// Organisation policy refuses access.
        Blocked => "blocked",
    }
}
wire_axis!(WireSync, SyncState);

open_enum! {
    /// One indicator that can accompany any display status. Open set.
    pub enum Modifier, kind = "modifier" {
        /// The item or its mount is read-only (lock).
        ReadOnly => "read-only",
        /// The item is a shortcut to a folder in another drive (chain link).
        Shortcut => "shortcut",
    }
}

/// The modifiers of an item as a list of kebab-case strings, usually empty
/// (ADR-0019 "Modifiers").
///
/// Built from `mydrive-core`'s [`Modifiers`](CoreModifiers), always in the order
/// `read-only`, `shortcut`. Unknown modifiers received from a newer daemon are kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WireModifiers(pub Vec<Modifier>);

impl From<CoreModifiers> for WireModifiers {
    fn from(m: CoreModifiers) -> Self {
        let mut list = Vec::new();
        if m.read_only {
            list.push(Modifier::ReadOnly);
        }
        if m.shortcut {
            list.push(Modifier::Shortcut);
        }
        Self(list)
    }
}

impl WireModifiers {
    /// The modifiers `mydrive-core` knows; unknown ones are ignored.
    #[must_use]
    pub fn to_core(&self) -> CoreModifiers {
        CoreModifiers {
            read_only: self.0.contains(&Modifier::ReadOnly),
            shortcut: self.0.contains(&Modifier::Shortcut),
        }
    }
}

/// One enumerated wire field set, as listed in the catalogue's "Value sets" table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueSet {
    /// The catalogue rows (fields) that use this set.
    pub fields: &'static [&'static str],
    /// The Rust type that carries it on the wire.
    pub type_name: &'static str,
    /// Whether values may be added within a major version.
    pub open: bool,
    /// The values this build knows, when the set is a fixed list of strings.
    pub known: Option<&'static [&'static str]>,
}

/// Every enumerated field in the protocol (ADR-0019 R7). A test checks this list against the
/// catalogue in both directions and against every enumerated wire type in this crate.
pub const VALUE_SETS: &[ValueSet] = &[
    ValueSet {
        fields: &["item.status"],
        type_name: "DisplayStatus",
        open: false,
        known: Some(DisplayStatus::WIRE_FORMS),
    },
    ValueSet {
        fields: &["item.hydration"],
        type_name: "WireHydration",
        open: true,
        known: Some(WireHydration::KNOWN),
    },
    ValueSet {
        fields: &["item.pin"],
        type_name: "WirePin",
        open: true,
        known: Some(WirePin::KNOWN),
    },
    ValueSet {
        fields: &["item.effective_pin"],
        type_name: "WireEffectivePin",
        open: true,
        known: Some(WireEffectivePin::KNOWN),
    },
    ValueSet {
        fields: &["item.sync", "refused.data.sync"],
        type_name: "WireSync",
        open: true,
        known: Some(WireSync::KNOWN),
    },
    ValueSet {
        fields: &["item.modifiers"],
        type_name: "Modifier",
        open: true,
        known: Some(Modifier::KNOWN),
    },
    ValueSet {
        fields: &["item.reason"],
        type_name: "ItemReason",
        open: true,
        known: Some(ItemReason::KNOWN),
    },
    ValueSet {
        fields: &["mount.state"],
        type_name: "MountState",
        open: true,
        known: Some(MountState::KNOWN),
    },
    ValueSet {
        fields: &["mount.blocked_reason", "blocked-by-policy.data.reason"],
        type_name: "BlockedReason",
        open: true,
        known: Some(BlockedReason::KNOWN),
    },
    ValueSet {
        fields: &["account.state"],
        type_name: "AccountState",
        open: true,
        known: Some(AccountState::KNOWN),
    },
    ValueSet {
        fields: &["account.kind"],
        type_name: "AccountKind",
        open: true,
        known: Some(AccountKind::KNOWN),
    },
    ValueSet {
        fields: &["refused.data.reason"],
        type_name: "RefusalReason",
        open: true,
        known: Some(RefusalReason::KNOWN),
    },
    ValueSet {
        fields: &["read-only.data.reason"],
        type_name: "ReadOnlyReason",
        open: true,
        known: Some(ReadOnlyReason::KNOWN),
    },
    ValueSet {
        fields: &["invalid-params.data.reason"],
        type_name: "InvalidParamsReason",
        open: true,
        known: Some(InvalidParamsReason::KNOWN),
    },
    ValueSet {
        fields: &["error.code"],
        type_name: "i64",
        open: true,
        known: None,
    },
    ValueSet {
        fields: &["hello.capabilities"],
        type_name: "String",
        open: true,
        known: None,
    },
];

#[cfg(test)]
mod tests {
    use serde::de::DeserializeOwned;

    use super::*;

    fn open_round_trip<T>()
    where
        T: Serialize + DeserializeOwned + PartialEq + fmt::Debug,
    {
        let v: T = serde_json::from_str("\"from-the-future\"").unwrap();
        assert_eq!(serde_json::to_string(&v).unwrap(), "\"from-the-future\"");
    }

    /// ADR-0020 R7: the wire axes convert from core both ways and keep unknown values; the
    /// display status rejects one.
    #[test]
    fn axes_are_open_on_the_wire_and_display_status_is_closed() {
        for &h in Hydration::ALL {
            assert_eq!(WireHydration::from(h).to_core(), Some(h));
        }
        for &p in Pin::ALL {
            assert_eq!(WirePin::from(p).to_core(), Some(p));
        }
        for &p in EffectivePin::ALL {
            assert_eq!(WireEffectivePin::from(p).to_core(), Some(p));
        }
        for &s in SyncState::ALL {
            assert_eq!(WireSync::from(s).to_core(), Some(s));
        }
        let unknown: WireSync = serde_json::from_str("\"paused-upload\"").unwrap();
        assert_eq!(unknown, WireSync::Other("paused-upload".into()));
        assert_eq!(unknown.to_core(), None);
        assert!(serde_json::from_str::<DisplayStatus>("\"sparkly\"").is_err());
    }

    /// ADR-0019 R4: every open set accepts and preserves an unknown value.
    #[test]
    fn open_sets_preserve_unknown_values() {
        open_round_trip::<AccountKind>();
        open_round_trip::<AccountState>();
        open_round_trip::<MountState>();
        open_round_trip::<BlockedReason>();
        open_round_trip::<RefusalReason>();
        open_round_trip::<ItemReason>();
        open_round_trip::<Modifier>();
        open_round_trip::<ReadOnlyReason>();
        open_round_trip::<InvalidParamsReason>();
        open_round_trip::<WireHydration>();
        open_round_trip::<WirePin>();
        open_round_trip::<WireEffectivePin>();
        open_round_trip::<WireSync>();
        assert_eq!(
            MountState::from_wire("frozen"),
            MountState::Other("frozen".into())
        );
    }

    #[test]
    fn known_values_round_trip() {
        for &w in MountState::KNOWN {
            assert!(!matches!(MountState::from_wire(w), MountState::Other(_)));
            assert_eq!(MountState::from_wire(w).as_str(), w);
        }
        for &w in RefusalReason::KNOWN {
            assert_eq!(RefusalReason::from_wire(w).as_str(), w);
        }
    }

    #[test]
    fn modifiers_convert_from_core_as_a_list() {
        let all = [(false, false), (true, false), (false, true), (true, true)];
        for (read_only, shortcut) in all {
            let core = CoreModifiers {
                read_only,
                shortcut,
            };
            let wire = WireModifiers::from(core);
            assert_eq!(wire.to_core(), core);
            let mut expected = Vec::new();
            if read_only {
                expected.push("read-only");
            }
            if shortcut {
                expected.push("shortcut");
            }
            assert_eq!(
                serde_json::to_value(&wire).unwrap(),
                serde_json::json!(expected)
            );
        }
        let wire: WireModifiers = serde_json::from_str(r#"["shortcut","sparkly"]"#).unwrap();
        assert_eq!(
            wire.0,
            [Modifier::Shortcut, Modifier::Other("sparkly".into())]
        );
        assert_eq!(
            wire.to_core(),
            CoreModifiers {
                read_only: false,
                shortcut: true
            }
        );
    }

    #[test]
    fn account_kind_converts_from_core() {
        assert_eq!(
            AccountKind::from(names::AccountKind::Personal),
            AccountKind::Personal
        );
        assert_eq!(
            AccountKind::from(names::AccountKind::Work),
            AccountKind::Work
        );
    }
}
