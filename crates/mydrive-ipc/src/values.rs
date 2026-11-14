//! Open value sets (ADR-0019 "Closed and open value sets").
//!
//! Within a major version these sets may gain values. Each type here has an `Other` variant
//! that keeps a value this build does not know, so a newer daemon never makes an older client
//! fail a whole response (ADR-0019 R4). Code must therefore always have a fallback arm.
//!
//! The closed sets (hydration, pin, effective pin, sync and display status) are the
//! `mydrive-core` types, which reject unknown values.

use std::fmt;

use mydrive_core::names;
use mydrive_core::state::Modifiers as CoreModifiers;
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
