//! The three-axis item state model and derived display status (ADR-0008).
//!
//! Every item carries three independent axes: [`Hydration`], [`Pin`] and [`Sync`]. They are
//! stored separately and collapsed only for display, by [`display_status`], which is the one
//! place in the workspace that derives a [`DisplayStatus`] (ADR-0008 R2, R3).
//!
//! Every enum here has a stable wire form: the lowercase kebab-case of its name, as listed in
//! `docs/architecture/overview.md` ("Wire forms"). [`Display`](fmt::Display),
//! [`FromStr`] and `serde` all use that one form, generated from a single table per enum, so
//! they cannot drift apart.
//!
//! [`Sync`] shadows the standard `Sync` trait: a module that glob-imports `state::*` gets an
//! ambiguous-name error and must import `Sync` explicitly (or use the path `state::Sync`).

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A string that is not the wire form of any value of a state enum.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot parse {value:?} as a {kind}: expected one of {expected:?}")]
pub struct ParseStateError {
    /// Which enum was being parsed, for example `"sync state"`.
    pub kind: &'static str,
    /// The string that was rejected.
    pub value: String,
    /// Every accepted wire form for that enum.
    pub expected: &'static [&'static str],
}

/// Generates the wire form table, `as_str`, `ALL`, `Display`, `FromStr` and `serde` impls for a
/// fieldless enum, so all string conversions share one table.
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident, kind = $kind:literal {
            $( $(#[$vmeta:meta])* $variant:ident => $wire:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant, )+
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [Self] = &[ $( Self::$variant, )+ ];

            /// Every wire form, in declaration order.
            pub const WIRE_FORMS: &'static [&'static str] = &[ $( $wire, )+ ];

            /// The stable wire form used by the IPC protocol, xattrs, CLI `--json` and the
            /// store's columns.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $wire, )+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = ParseStateError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $( $wire => Ok(Self::$variant), )+
                    other => Err(ParseStateError {
                        kind: $kind,
                        value: other.to_owned(),
                        expected: Self::WIRE_FORMS,
                    }),
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct WireVisitor;

                impl Visitor<'_> for WireVisitor {
                    type Value = $name;

                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        write!(f, "a {} wire form", $kind)
                    }

                    fn visit_str<E: de::Error>(self, v: &str) -> Result<$name, E> {
                        v.parse()
                            .map_err(|_| E::unknown_variant(v, $name::WIRE_FORMS))
                    }
                }

                deserializer.deserialize_str(WireVisitor)
            }
        }
    };
}

wire_enum! {
    /// How much of an item's content is local. Folders are `Full` once listed.
    pub enum Hydration, kind = "hydration state" {
        /// Metadata only; no content is local.
        Placeholder => "placeholder",
        /// Some byte ranges are local.
        Partial => "partial",
        /// All content is local (or, for a folder, it has been listed).
        Full => "full",
    }
}

wire_enum! {
    /// The user's pin policy as set on the item itself.
    pub enum Pin, kind = "pin state" {
        /// Take the nearest ancestor's value; the root default is `Unpinned`.
        Inherit => "inherit",
        /// Always keep on this device: the engine keeps hydration at `Full`.
        Pinned => "pinned",
        /// May be dehydrated. It does not mean "dehydrate now".
        Unpinned => "unpinned",
    }
}

wire_enum! {
    /// The item's relationship to the server.
    pub enum Sync, kind = "sync state" {
        /// Local and server agree; nothing pending.
        InSync => "in-sync",
        /// A download has been scheduled (pin, `mydrive hydrate`, pinned item changed remotely).
        PendingDownload => "pending-download",
        /// Local changes are recorded and not yet on the server.
        PendingUpload => "pending-upload",
        /// An upload request is in flight.
        Uploading => "uploading",
        /// Local and remote changes collided.
        Conflict => "conflict",
        /// A permanent failure needs attention.
        Error => "error",
        /// Organisation policy refuses access.
        Blocked => "blocked",
    }
}

wire_enum! {
    /// A pin after inheritance has been resolved; see [`effective_pin`].
    pub enum EffectivePin, kind = "effective pin" {
        /// The item must stay hydrated.
        Pinned => "pinned",
        /// The item may be dehydrated.
        Unpinned => "unpinned",
    }
}

wire_enum! {
    /// What the user sees for an item. Derived only by [`display_status`]; never stored.
    pub enum DisplayStatus, kind = "display status" {
        /// Sync is `Blocked`.
        Blocked => "blocked",
        /// Sync is `Error` or `Conflict`.
        Error => "error",
        /// Sync is `PendingUpload`, `Uploading` or `PendingDownload`.
        Syncing => "syncing",
        /// Effective pin is `Pinned` and hydration is `Full`.
        Pinned => "pinned",
        /// Hydration is `Full`.
        Local => "local",
        /// Hydration is `Placeholder` or `Partial`.
        OnlineOnly => "online-only",
    }
}

/// The three state axes of one item, held separately (ADR-0008 R1).
///
/// There is deliberately no combined enum: the axes are collapsed only for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemState {
    /// How much content is local.
    pub hydration: Hydration,
    /// The pin set on the item itself (not resolved; see [`effective_pin`]).
    pub pin: Pin,
    /// Relationship to the server.
    pub sync: Sync,
}

/// Indicators that can accompany any [`DisplayStatus`]. They come from item and mount
/// properties, not from the state axes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Modifiers {
    /// The item or its mount is read-only (lock).
    pub read_only: bool,
    /// The item is a shortcut to a folder in another drive (chain link).
    pub shortcut: bool,
}

/// Resolves an item's effective pin (ADR-0008 R4).
///
/// `ancestors` are the pins of the item's ancestors, nearest first (parent, grandparent, ...,
/// up to the mount root). The item's own `Pinned` or `Unpinned` wins; `Inherit` takes the
/// nearest ancestor that is not `Inherit`; if every pin in the chain is `Inherit`, the root
/// default `Unpinned` applies.
#[must_use]
pub fn effective_pin<I>(own: Pin, ancestors: I) -> EffectivePin
where
    I: IntoIterator<Item = Pin>,
{
    std::iter::once(own)
        .chain(ancestors)
        .find_map(|pin| match pin {
            Pin::Inherit => None,
            Pin::Pinned => Some(EffectivePin::Pinned),
            Pin::Unpinned => Some(EffectivePin::Unpinned),
        })
        .unwrap_or(EffectivePin::Unpinned)
}

/// The pin that governs an item, given its own pin and a caller-supplied effective pin.
///
/// An explicit own pin (`Pinned` or `Unpinned`) always wins, as in ADR-0008 R4, so a stale or
/// wrong `supplied` value can never make a pinned item look unpinned. `supplied` is used only
/// when the item's own pin is `Inherit`.
const fn governing_pin(own: Pin, supplied: EffectivePin) -> EffectivePin {
    match own {
        Pin::Pinned => EffectivePin::Pinned,
        Pin::Unpinned => EffectivePin::Unpinned,
        Pin::Inherit => supplied,
    }
}

/// Derives the display status from an item's axes and its resolved pin (ADR-0008 R2).
///
/// This is the only function that derives a display status. The priority order is the
/// overview's table, first match wins: `blocked`, `error`, `syncing`, `pinned`, `local`,
/// `online-only`.
///
/// `effective` is the pin resolved by [`effective_pin`]. It is used only when `state.pin` is
/// `Inherit`; an explicit own pin (`Pinned` or `Unpinned`) overrides it, because the item's
/// own pin always wins (ADR-0008 R4) and a contradictory caller value must not hide a pin.
#[must_use]
pub const fn display_status(state: &ItemState, effective: EffectivePin) -> DisplayStatus {
    let effective = governing_pin(state.pin, effective);
    match (state.sync, effective, state.hydration) {
        (Sync::Blocked, _, _) => DisplayStatus::Blocked,
        (Sync::Error | Sync::Conflict, _, _) => DisplayStatus::Error,
        (Sync::PendingUpload | Sync::Uploading | Sync::PendingDownload, _, _) => {
            DisplayStatus::Syncing
        }
        (Sync::InSync, EffectivePin::Pinned, Hydration::Full) => DisplayStatus::Pinned,
        (Sync::InSync, _, Hydration::Full) => DisplayStatus::Local,
        (Sync::InSync, _, Hydration::Placeholder | Hydration::Partial) => DisplayStatus::OnlineOnly,
    }
}

/// Why a dehydrate request was refused (ADR-0008 R5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DehydrateRefusal {
    /// The item's sync state is not `InSync`. For `PendingUpload`, `Uploading` and `Conflict`
    /// the local content is not on the server and dehydrating would lose it (ADR-0008 R7).
    #[error("dehydrate refused: sync state is {0}, not in-sync")]
    NotInSync(Sync),
    /// The item's effective pin is `Pinned`. The user-facing "Free up space" action unpins
    /// first; the engine itself never dehydrates a pinned item.
    #[error("dehydrate refused: item is pinned")]
    Pinned,
}

/// Decides whether an item may be dehydrated (ADR-0008 R5, R7).
///
/// Refused unless sync is `InSync` and the effective pin is not `Pinned`. When both reasons
/// apply, [`DehydrateRefusal::NotInSync`] is reported: it is the one that protects data, and
/// F06 refuses a non-`InSync` "Free up space" before it changes the pin. This function only
/// decides; it changes no state.
///
/// `effective` is the pin resolved by [`effective_pin`]. It is used only when `state.pin` is
/// `Inherit`; an explicit own pin overrides it (ADR-0008 R4), so an item whose own pin is
/// `Pinned` is refused even if a stale or buggy caller passes `EffectivePin::Unpinned`.
///
/// # Errors
///
/// Returns the [`DehydrateRefusal`] naming why the item must not be dehydrated.
pub const fn check_dehydrate(
    state: &ItemState,
    effective: EffectivePin,
) -> Result<(), DehydrateRefusal> {
    match (state.sync, governing_pin(state.pin, effective)) {
        (Sync::InSync, EffectivePin::Unpinned) => Ok(()),
        (Sync::InSync, EffectivePin::Pinned) => Err(DehydrateRefusal::Pinned),
        (other, _) => Err(DehydrateRefusal::NotInSync(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::Sync;
    use super::*;
    use proptest::prelude::*;
    use serde::de::IntoDeserializer;
    use serde::de::value::{Error as ValueError, StrDeserializer};

    use DisplayStatus as D;
    use EffectivePin as E;
    use Hydration as H;
    use Sync as S;

    fn state(hydration: Hydration, pin: Pin, sync: Sync) -> ItemState {
        ItemState {
            hydration,
            pin,
            sync,
        }
    }

    /// ADR-0008 R2: every one of the 3 x 2 x 7 combinations of hydration, effective pin and
    /// sync, with the expected status written out from the overview's table (first match
    /// wins), not computed.
    #[test]
    fn display_status_matches_table_for_every_combination() {
        let table: [(Hydration, EffectivePin, Sync, DisplayStatus); 42] = [
            // Placeholder, Unpinned
            (H::Placeholder, E::Unpinned, S::InSync, D::OnlineOnly),
            (H::Placeholder, E::Unpinned, S::PendingDownload, D::Syncing),
            (H::Placeholder, E::Unpinned, S::PendingUpload, D::Syncing),
            (H::Placeholder, E::Unpinned, S::Uploading, D::Syncing),
            (H::Placeholder, E::Unpinned, S::Conflict, D::Error),
            (H::Placeholder, E::Unpinned, S::Error, D::Error),
            (H::Placeholder, E::Unpinned, S::Blocked, D::Blocked),
            // Placeholder, Pinned
            (H::Placeholder, E::Pinned, S::InSync, D::OnlineOnly),
            (H::Placeholder, E::Pinned, S::PendingDownload, D::Syncing),
            (H::Placeholder, E::Pinned, S::PendingUpload, D::Syncing),
            (H::Placeholder, E::Pinned, S::Uploading, D::Syncing),
            (H::Placeholder, E::Pinned, S::Conflict, D::Error),
            (H::Placeholder, E::Pinned, S::Error, D::Error),
            (H::Placeholder, E::Pinned, S::Blocked, D::Blocked),
            // Partial, Unpinned
            (H::Partial, E::Unpinned, S::InSync, D::OnlineOnly),
            (H::Partial, E::Unpinned, S::PendingDownload, D::Syncing),
            (H::Partial, E::Unpinned, S::PendingUpload, D::Syncing),
            (H::Partial, E::Unpinned, S::Uploading, D::Syncing),
            (H::Partial, E::Unpinned, S::Conflict, D::Error),
            (H::Partial, E::Unpinned, S::Error, D::Error),
            (H::Partial, E::Unpinned, S::Blocked, D::Blocked),
            // Partial, Pinned
            (H::Partial, E::Pinned, S::InSync, D::OnlineOnly),
            (H::Partial, E::Pinned, S::PendingDownload, D::Syncing),
            (H::Partial, E::Pinned, S::PendingUpload, D::Syncing),
            (H::Partial, E::Pinned, S::Uploading, D::Syncing),
            (H::Partial, E::Pinned, S::Conflict, D::Error),
            (H::Partial, E::Pinned, S::Error, D::Error),
            (H::Partial, E::Pinned, S::Blocked, D::Blocked),
            // Full, Unpinned
            (H::Full, E::Unpinned, S::InSync, D::Local),
            (H::Full, E::Unpinned, S::PendingDownload, D::Syncing),
            (H::Full, E::Unpinned, S::PendingUpload, D::Syncing),
            (H::Full, E::Unpinned, S::Uploading, D::Syncing),
            (H::Full, E::Unpinned, S::Conflict, D::Error),
            (H::Full, E::Unpinned, S::Error, D::Error),
            (H::Full, E::Unpinned, S::Blocked, D::Blocked),
            // Full, Pinned
            (H::Full, E::Pinned, S::InSync, D::Pinned),
            (H::Full, E::Pinned, S::PendingDownload, D::Syncing),
            (H::Full, E::Pinned, S::PendingUpload, D::Syncing),
            (H::Full, E::Pinned, S::Uploading, D::Syncing),
            (H::Full, E::Pinned, S::Conflict, D::Error),
            (H::Full, E::Pinned, S::Error, D::Error),
            (H::Full, E::Pinned, S::Blocked, D::Blocked),
        ];

        // The table covers every combination exactly once.
        let mut seen = std::collections::HashSet::new();
        for (h, e, s, _) in table {
            assert!(seen.insert((h, e, s)), "duplicate row {h} {e} {s}");
        }
        assert_eq!(
            seen.len(),
            Hydration::ALL.len() * EffectivePin::ALL.len() * Sync::ALL.len()
        );

        for (h, e, s, expected) in table {
            // Own pin `Inherit`: the supplied effective pin decides.
            assert_eq!(
                display_status(&state(h, Pin::Inherit, s), e),
                expected,
                "hydration={h} effective={e} sync={s} own-pin=inherit"
            );
            // Own pin explicit and consistent with the effective pin: same result.
            let own = match e {
                E::Pinned => Pin::Pinned,
                E::Unpinned => Pin::Unpinned,
            };
            assert_eq!(
                display_status(&state(h, own, s), e),
                expected,
                "hydration={h} effective={e} sync={s} own-pin={own}"
            );
        }
    }

    /// ADR-0008 R4: an explicit own pin wins over a contradictory supplied effective pin, so a
    /// stale caller cannot make a pinned item display as `local` (or an unpinned one as
    /// `pinned`).
    #[test]
    fn display_status_own_pin_overrides_contradictory_effective_pin() {
        assert_eq!(
            display_status(&state(H::Full, Pin::Pinned, S::InSync), E::Unpinned),
            D::Pinned
        );
        assert_eq!(
            display_status(&state(H::Full, Pin::Unpinned, S::InSync), E::Pinned),
            D::Local
        );
        // Sync still takes priority over the pin.
        assert_eq!(
            display_status(&state(H::Full, Pin::Pinned, S::Uploading), E::Unpinned),
            D::Syncing
        );
    }

    /// The worked examples in F13, as a second, independently written check.
    #[test]
    fn display_status_matches_f13_worked_examples() {
        let examples = [
            (H::Placeholder, E::Unpinned, S::InSync, "online-only"),
            (H::Partial, E::Unpinned, S::InSync, "online-only"),
            (H::Full, E::Unpinned, S::InSync, "local"),
            (H::Full, E::Pinned, S::InSync, "pinned"),
            (H::Placeholder, E::Pinned, S::PendingDownload, "syncing"),
            (H::Full, E::Unpinned, S::PendingUpload, "syncing"),
            (H::Full, E::Pinned, S::Conflict, "error"),
            (H::Full, E::Unpinned, S::Blocked, "blocked"),
        ];
        for (h, e, s, wire) in examples {
            assert_eq!(display_status(&state(h, Pin::Inherit, s), e).as_str(), wire);
        }
    }

    /// ADR-0008 R4: own `Pinned`/`Unpinned` wins; `Inherit` takes the nearest non-`Inherit`
    /// ancestor; with none, `Unpinned`.
    #[test]
    fn effective_pin_examples() {
        use Pin::{Inherit, Pinned, Unpinned};
        assert_eq!(effective_pin(Pinned, []), E::Pinned);
        assert_eq!(effective_pin(Unpinned, []), E::Unpinned);
        assert_eq!(effective_pin(Inherit, []), E::Unpinned);
        assert_eq!(effective_pin(Unpinned, [Pinned]), E::Unpinned);
        assert_eq!(effective_pin(Pinned, [Unpinned]), E::Pinned);
        assert_eq!(
            effective_pin(Inherit, [Inherit, Pinned, Unpinned]),
            E::Pinned
        );
        assert_eq!(effective_pin(Inherit, [Unpinned, Pinned]), E::Unpinned);
        assert_eq!(effective_pin(Inherit, [Inherit, Inherit]), E::Unpinned);
    }

    fn any_pin() -> impl Strategy<Value = Pin> {
        prop_oneof![Just(Pin::Inherit), Just(Pin::Pinned), Just(Pin::Unpinned)]
    }

    proptest! {
        /// ADR-0008 R4, over arbitrary ancestor chains.
        #[test]
        fn effective_pin_over_arbitrary_chains(
            own in any_pin(),
            ancestors in proptest::collection::vec(any_pin(), 0..32),
        ) {
            let got = effective_pin(own, ancestors.iter().copied());

            match own {
                // Own explicit value wins regardless of ancestors.
                Pin::Pinned => prop_assert_eq!(got, E::Pinned),
                Pin::Unpinned => prop_assert_eq!(got, E::Unpinned),
                Pin::Inherit => {
                    // Index of the nearest explicit ancestor, found by scanning by index.
                    let mut nearest = None;
                    for (i, p) in ancestors.iter().enumerate() {
                        if *p != Pin::Inherit {
                            nearest = Some(i);
                            break;
                        }
                    }
                    match nearest {
                        None => prop_assert_eq!(got, E::Unpinned),
                        Some(i) => {
                            let expected = if ancestors[i] == Pin::Pinned {
                                E::Pinned
                            } else {
                                E::Unpinned
                            };
                            prop_assert_eq!(got, expected);
                            // Anything beyond the nearest explicit ancestor is irrelevant.
                            let truncated = effective_pin(own, ancestors[..=i].iter().copied());
                            prop_assert_eq!(got, truncated);
                        }
                    }
                }
            }

            // Inserting `Inherit` anywhere in the chain never changes the result.
            for at in 0..=ancestors.len() {
                let mut padded = ancestors.clone();
                padded.insert(at, Pin::Inherit);
                prop_assert_eq!(effective_pin(own, padded), got);
            }
        }
    }

    /// ADR-0008 R5: dehydration is refused while effective pin is `Pinned`, and refused for
    /// any sync other than `InSync`, each with its reason.
    #[test]
    fn dehydrate_refusals_carry_reason() {
        for &h in Hydration::ALL {
            for &own in Pin::ALL {
                for &s in Sync::ALL {
                    for &e in EffectivePin::ALL {
                        let got = check_dehydrate(&state(h, own, s), e);
                        // Own explicit pin wins; the supplied value counts only for Inherit.
                        let governing = match own {
                            Pin::Pinned => E::Pinned,
                            Pin::Unpinned => E::Unpinned,
                            Pin::Inherit => e,
                        };
                        let expected = match (s, governing) {
                            (S::InSync, E::Unpinned) => Ok(()),
                            (S::InSync, E::Pinned) => Err(DehydrateRefusal::Pinned),
                            (other, _) => Err(DehydrateRefusal::NotInSync(other)),
                        };
                        assert_eq!(got, expected, "hydration={h} own={own} sync={s} eff={e}");
                    }
                }
            }
        }
        assert_eq!(
            check_dehydrate(&state(H::Full, Pin::Pinned, S::InSync), E::Pinned)
                .map_err(|r| r.to_string()),
            Err("dehydrate refused: item is pinned".to_owned())
        );
        assert_eq!(
            check_dehydrate(&state(H::Full, Pin::Inherit, S::Error), E::Unpinned)
                .map_err(|r| r.to_string()),
            Err("dehydrate refused: sync state is error, not in-sync".to_owned())
        );
    }

    /// ADR-0008 R4, R5: an item whose own pin is `Pinned` is refused dehydration even when a
    /// stale or buggy caller passes `EffectivePin::Unpinned`; an own `Unpinned` is allowed
    /// even if `Pinned` is passed, because the own pin wins.
    #[test]
    fn dehydrate_own_pin_overrides_contradictory_effective_pin() {
        for &h in Hydration::ALL {
            assert_eq!(
                check_dehydrate(&state(h, Pin::Pinned, S::InSync), E::Unpinned),
                Err(DehydrateRefusal::Pinned),
                "hydration={h}"
            );
            assert_eq!(
                check_dehydrate(&state(h, Pin::Unpinned, S::InSync), E::Pinned),
                Ok(()),
                "hydration={h}"
            );
        }
        // Not in sync still takes precedence.
        assert_eq!(
            check_dehydrate(&state(H::Full, Pin::Pinned, S::PendingUpload), E::Unpinned),
            Err(DehydrateRefusal::NotInSync(S::PendingUpload))
        );
    }

    /// ADR-0008 R7, data safety (principle 1): a file whose local content is not on the
    /// server (`PendingUpload`, `Uploading`) or is in `Conflict` is never dehydrated, whatever
    /// its hydration, own pin or effective pin. Dehydrating it would destroy the only copy.
    #[test]
    fn dehydrate_never_permitted_with_unsynced_local_changes() {
        for s in [S::PendingUpload, S::Uploading, S::Conflict] {
            for &h in Hydration::ALL {
                for &own in Pin::ALL {
                    for &e in EffectivePin::ALL {
                        assert_eq!(
                            check_dehydrate(&state(h, own, s), e),
                            Err(DehydrateRefusal::NotInSync(s)),
                            "hydration={h} own={own} sync={s} eff={e}"
                        );
                    }
                }
            }
        }
    }

    /// The wire forms listed in the overview ("Wire forms"), written out.
    #[test]
    fn wire_forms_match_overview() {
        assert_eq!(Hydration::WIRE_FORMS, ["placeholder", "partial", "full"]);
        assert_eq!(Pin::WIRE_FORMS, ["inherit", "pinned", "unpinned"]);
        assert_eq!(
            Sync::WIRE_FORMS,
            [
                "in-sync",
                "pending-download",
                "pending-upload",
                "uploading",
                "conflict",
                "error",
                "blocked"
            ]
        );
        assert_eq!(EffectivePin::WIRE_FORMS, ["pinned", "unpinned"]);
        assert_eq!(
            DisplayStatus::WIRE_FORMS,
            [
                "blocked",
                "error",
                "syncing",
                "pinned",
                "local",
                "online-only"
            ]
        );
        assert_eq!(S::PendingUpload.to_string(), "pending-upload");
        assert_eq!(D::OnlineOnly.to_string(), "online-only");
    }

    fn assert_round_trips<T>(values: &[T])
    where
        T: Copy + PartialEq + fmt::Debug + fmt::Display + FromStr<Err = ParseStateError>,
        T: Serialize + for<'de> Deserialize<'de>,
    {
        for &v in values {
            let wire = v.to_string();
            assert_eq!(wire.parse::<T>(), Ok(v), "FromStr of {wire}");
            assert_eq!(
                to_wire_string(&v),
                Ok(wire.clone()),
                "serde serialize {v:?}"
            );
            let de: StrDeserializer<'_, ValueError> = wire.as_str().into_deserializer();
            assert_eq!(T::deserialize(de).ok(), Some(v), "serde deserialize {wire}");
        }
    }

    fn assert_rejects<T>(kind: &str)
    where
        T: fmt::Debug + FromStr<Err = ParseStateError> + for<'de> Deserialize<'de>,
    {
        for bad in [
            "",
            "InSync",
            "in_sync",
            "Full",
            " full",
            "online_only",
            "unknown",
        ] {
            match bad.parse::<T>() {
                Ok(v) => panic!("{bad:?} parsed as {v:?}"),
                Err(err) => {
                    assert_eq!(err.kind, kind);
                    assert_eq!(err.value, bad);
                }
            }
            let de: StrDeserializer<'_, ValueError> = bad.into_deserializer();
            assert!(T::deserialize(de).is_err(), "serde accepted {bad:?}");
        }
    }

    /// AC5: every value round-trips through its string form and through serde, and the
    /// three agree; unknown strings are rejected with a typed error.
    #[test]
    fn every_enum_round_trips_and_rejects_unknown() {
        assert_round_trips(Hydration::ALL);
        assert_round_trips(Pin::ALL);
        assert_round_trips(Sync::ALL);
        assert_round_trips(EffectivePin::ALL);
        assert_round_trips(DisplayStatus::ALL);

        assert_rejects::<Hydration>("hydration state");
        assert_rejects::<Pin>("pin state");
        assert_rejects::<Sync>("sync state");
        assert_rejects::<EffectivePin>("effective pin");
        assert_rejects::<DisplayStatus>("display status");
    }

    /// Serialises a value with a serializer that accepts only a string, so the test checks
    /// that each enum serialises as exactly its wire-form string.
    fn to_wire_string<T: Serialize>(value: &T) -> Result<String, ValueError> {
        value.serialize(StrOnly)
    }

    struct StrOnly;

    fn not_a_str<T>() -> Result<T, ValueError> {
        Err(serde::ser::Error::custom("expected a string"))
    }

    impl Serializer for StrOnly {
        type Ok = String;
        type Error = ValueError;
        type SerializeSeq = serde::ser::Impossible<String, ValueError>;
        type SerializeTuple = serde::ser::Impossible<String, ValueError>;
        type SerializeTupleStruct = serde::ser::Impossible<String, ValueError>;
        type SerializeTupleVariant = serde::ser::Impossible<String, ValueError>;
        type SerializeMap = serde::ser::Impossible<String, ValueError>;
        type SerializeStruct = serde::ser::Impossible<String, ValueError>;
        type SerializeStructVariant = serde::ser::Impossible<String, ValueError>;

        fn serialize_str(self, v: &str) -> Result<String, ValueError> {
            Ok(v.to_owned())
        }
        fn serialize_bool(self, _: bool) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_i8(self, _: i8) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_i16(self, _: i16) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_i32(self, _: i32) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_i64(self, _: i64) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_u8(self, _: u8) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_u16(self, _: u16) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_u32(self, _: u32) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_u64(self, _: u64) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_f32(self, _: f32) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_f64(self, _: f64) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_char(self, _: char) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_bytes(self, _: &[u8]) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_none(self) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_some<T: ?Sized + Serialize>(self, _: &T) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_unit(self) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_unit_struct(self, _: &'static str) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_unit_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
        ) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_newtype_struct<T: ?Sized + Serialize>(
            self,
            _: &'static str,
            _: &T,
        ) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_newtype_variant<T: ?Sized + Serialize>(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: &T,
        ) -> Result<String, ValueError> {
            not_a_str()
        }
        fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, ValueError> {
            not_a_str()
        }
        fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, ValueError> {
            not_a_str()
        }
        fn serialize_tuple_struct(
            self,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeTupleStruct, ValueError> {
            not_a_str()
        }
        fn serialize_tuple_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeTupleVariant, ValueError> {
            not_a_str()
        }
        fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, ValueError> {
            not_a_str()
        }
        fn serialize_struct(
            self,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeStruct, ValueError> {
            not_a_str()
        }
        fn serialize_struct_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeStructVariant, ValueError> {
            not_a_str()
        }
    }
}
