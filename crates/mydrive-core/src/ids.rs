//! Identifiers: accounts, drives, items and mounts (ADR-0007).
//!
//! Every identifier is a newtype so that one kind can never be passed where another is
//! expected, and none of them holds or derives a filesystem path. Items are named by
//! [`ItemKey`], the pair (`drive_id`, `item_id`), everywhere in the system (ADR-0006).
//!
//! Each newtype validates on construction, and its `serde` form is a plain string that is
//! validated (and, for [`DriveId`], normalised) by the same constructor when deserialised,
//! so an invalid identifier cannot enter the program through any route.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The longest [`MountId`] accepted, in characters.
pub const MOUNT_ID_MAX_LEN: usize = 64;

/// Why an identifier could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// The input was the empty string. No identifier may be empty.
    #[error("cannot build {kind}: the value is empty")]
    Empty {
        /// The identifier kind being built, for example `"drive ID"`.
        kind: &'static str,
    },
    /// The input is not a valid mount ID.
    #[error("cannot build mount ID from {value:?}: {reason}")]
    InvalidMountId {
        /// The rejected input.
        value: String,
        /// Why it was rejected.
        reason: MountIdProblem,
    },
}

/// The specific rule a rejected [`MountId`] broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MountIdProblem {
    /// Longer than [`MOUNT_ID_MAX_LEN`] characters.
    #[error("it is longer than {MOUNT_ID_MAX_LEN} characters")]
    TooLong,
    /// Starts with `-`, which tools would read as an option.
    #[error("it starts with '-'")]
    LeadingHyphen,
    /// Contains a character outside `a-z`, `0-9` and `-`.
    #[error("it contains {0:?}; only a-z, 0-9 and '-' are allowed")]
    InvalidChar(char),
}

/// Shared trait impls for a string newtype whose constructor is `Self::new(String)`.
macro_rules! string_id_impls {
    ($ty:ident) => {
        impl $ty {
            /// The identifier as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $ty {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                Self::new(raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

/// Rejects the empty string, naming the identifier kind in the error.
fn non_empty(value: String, kind: &'static str) -> Result<String, IdError> {
    if value.is_empty() {
        Err(IdError::Empty { kind })
    } else {
        Ok(value)
    }
}

/// A Graph drive ID, always stored lowercase.
///
/// OneDrive Personal has returned the same drive ID in mixed case across response fields
/// since about June 2025, so the ID is lowercased here, once, on construction; two
/// `DriveId`s built from inputs that differ only in case are equal (ADR-0007 R1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DriveId(String);

impl DriveId {
    /// Builds a drive ID, normalising it to lowercase.
    ///
    /// # Errors
    ///
    /// [`IdError::Empty`] if `value` is empty.
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = non_empty(value.into(), "drive ID")?;
        Ok(Self(value.to_lowercase()))
    }
}

string_id_impls!(DriveId);

/// A Graph item ID: opaque, stored exactly as given and compared exactly.
///
/// An item ID is only unique within its drive; use [`ItemKey`] to name an item.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId(String);

impl ItemId {
    /// Builds an item ID without changing it.
    ///
    /// # Errors
    ///
    /// [`IdError::Empty`] if `value` is empty.
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        non_empty(value.into(), "item ID").map(Self)
    }
}

string_id_impls!(ItemId);

/// An account: one signed-in Microsoft identity. Opaque.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountId(String);

impl AccountId {
    /// Builds an account ID without changing it.
    ///
    /// # Errors
    ///
    /// [`IdError::Empty`] if `value` is empty.
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        non_empty(value.into(), "account ID").map(Self)
    }
}

string_id_impls!(AccountId);

/// A tenant: the organisation a work account belongs to. Opaque.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TenantId(String);

impl TenantId {
    /// Builds a tenant ID without changing it.
    ///
    /// # Errors
    ///
    /// [`IdError::Empty`] if `value` is empty.
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        non_empty(value.into(), "tenant ID").map(Self)
    }
}

string_id_impls!(TenantId);

/// A mount: a drive, or a folder within one, attached to a local mount point.
///
/// A mount ID names the mount's backing directory (ADR-0007 R5), so it must be safe as a
/// single directory name: 1 to [`MOUNT_ID_MAX_LEN`] characters from `a-z`, `0-9` and `-`,
/// not starting with `-`. That excludes `/`, `.`, `..`, whitespace and uppercase letters.
/// It is never normalised; an input that breaks a rule is rejected.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MountId(String);

impl MountId {
    /// Builds a mount ID, checking that it is safe as a directory name.
    ///
    /// # Errors
    ///
    /// [`IdError::Empty`] if `value` is empty, [`IdError::InvalidMountId`] if it breaks any
    /// other rule.
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = non_empty(value.into(), "mount ID")?;
        match mount_id_problem(&value) {
            None => Ok(Self(value)),
            Some(reason) => Err(IdError::InvalidMountId { value, reason }),
        }
    }
}

/// The first rule `value` breaks, if any. `value` is known to be non-empty.
fn mount_id_problem(value: &str) -> Option<MountIdProblem> {
    if let Some(c) = value
        .chars()
        .find(|c| !matches!(c, 'a'..='z' | '0'..='9' | '-'))
    {
        return Some(MountIdProblem::InvalidChar(c));
    }
    // Every character is now ASCII, so the byte length is the character count.
    if value.len() > MOUNT_ID_MAX_LEN {
        return Some(MountIdProblem::TooLong);
    }
    if value.starts_with('-') {
        return Some(MountIdProblem::LeadingHyphen);
    }
    None
}

string_id_impls!(MountId);

/// The identity of an item everywhere in the system: (`drive_id`, `item_id`).
///
/// Never a path or a name (ADR-0007 R6). Ordering is by drive, then item.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ItemKey {
    /// The drive that holds the item.
    pub drive_id: DriveId,
    /// The item's ID within that drive.
    pub item_id: ItemId,
}

impl ItemKey {
    /// Builds an item key from its two parts.
    #[must_use]
    pub fn new(drive_id: DriveId, item_id: ItemId) -> Self {
        Self { drive_id, item_id }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use proptest::prelude::*;
    use serde::de::IntoDeserializer;
    use serde::de::value::{Error as ValueError, U32Deserializer};

    use super::*;

    // `serde_json` is not a dependency of this crate, so the serde tests use serde's own
    // string deserializer (the shape a JSON string presents) and a serializer that accepts
    // only a string. A value that serialises as anything other than one string fails.
    mod str_ser {
        use serde::Serialize;
        use serde::ser::{Error as _, Impossible, Serializer};

        #[derive(Debug)]
        pub(super) struct NotAString(String);

        impl std::fmt::Display for NotAString {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::error::Error for NotAString {}

        impl serde::ser::Error for NotAString {
            fn custom<T: std::fmt::Display>(msg: T) -> Self {
                Self(msg.to_string())
            }
        }

        pub(super) fn to_string<T: Serialize>(value: &T) -> Result<String, NotAString> {
            value.serialize(StrOnly)
        }

        struct StrOnly;

        macro_rules! reject {
            ($($name:ident($($arg:ty),*);)*) => {
                $(fn $name(self, $(_: $arg),*) -> Result<String, NotAString> {
                    Err(NotAString::custom(stringify!($name)))
                })*
            };
        }

        impl Serializer for StrOnly {
            type Ok = String;
            type Error = NotAString;
            type SerializeSeq = Impossible<String, NotAString>;
            type SerializeTuple = Impossible<String, NotAString>;
            type SerializeTupleStruct = Impossible<String, NotAString>;
            type SerializeTupleVariant = Impossible<String, NotAString>;
            type SerializeMap = Impossible<String, NotAString>;
            type SerializeStruct = Impossible<String, NotAString>;
            type SerializeStructVariant = Impossible<String, NotAString>;

            fn serialize_str(self, v: &str) -> Result<String, NotAString> {
                Ok(v.to_owned())
            }

            reject! {
                serialize_bool(bool); serialize_i8(i8); serialize_i16(i16);
                serialize_i32(i32); serialize_i64(i64); serialize_u8(u8);
                serialize_u16(u16); serialize_u32(u32); serialize_u64(u64);
                serialize_f32(f32); serialize_f64(f64); serialize_char(char);
                serialize_bytes(&[u8]); serialize_none(); serialize_unit();
                serialize_unit_struct(&'static str);
                serialize_unit_variant(&'static str, u32, &'static str);
            }

            fn serialize_some<T: ?Sized + Serialize>(self, _: &T) -> Result<String, NotAString> {
                Err(NotAString::custom("serialize_some"))
            }
            fn serialize_newtype_struct<T: ?Sized + Serialize>(
                self,
                _: &'static str,
                _: &T,
            ) -> Result<String, NotAString> {
                Err(NotAString::custom("serialize_newtype_struct"))
            }
            fn serialize_newtype_variant<T: ?Sized + Serialize>(
                self,
                _: &'static str,
                _: u32,
                _: &'static str,
                _: &T,
            ) -> Result<String, NotAString> {
                Err(NotAString::custom("serialize_newtype_variant"))
            }
            fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, NotAString> {
                Err(NotAString::custom("serialize_seq"))
            }
            fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, NotAString> {
                Err(NotAString::custom("serialize_tuple"))
            }
            fn serialize_tuple_struct(
                self,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeTupleStruct, NotAString> {
                Err(NotAString::custom("serialize_tuple_struct"))
            }
            fn serialize_tuple_variant(
                self,
                _: &'static str,
                _: u32,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeTupleVariant, NotAString> {
                Err(NotAString::custom("serialize_tuple_variant"))
            }
            fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, NotAString> {
                Err(NotAString::custom("serialize_map"))
            }
            fn serialize_struct(
                self,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeStruct, NotAString> {
                Err(NotAString::custom("serialize_struct"))
            }
            fn serialize_struct_variant(
                self,
                _: &'static str,
                _: u32,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeStructVariant, NotAString> {
                Err(NotAString::custom("serialize_struct_variant"))
            }
        }
    }

    /// Deserialises `T` from a string, as `serde_json` would from a JSON string.
    fn from_str_de<T: for<'de> Deserialize<'de>>(s: &str) -> Result<T, ValueError> {
        T::deserialize(s.into_deserializer())
    }

    /// Asserts that `value` serialises as exactly `expected` and deserialises back to itself.
    fn assert_serde_string<T>(value: &T, expected: &str)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + fmt::Debug,
    {
        let ser = str_ser::to_string(value).unwrap();
        assert_eq!(ser, expected);
        let back: T = from_str_de(&ser).unwrap();
        assert_eq!(&back, value);
    }

    /// ADR-0007 R1: a drive ID is lowercase however it was cased on input, so mixed-case
    /// forms of one ID compare equal and only the lowercase form is ever stored.
    #[test]
    fn adr_0007_r1_drive_id_is_normalised_to_lowercase() {
        let upper = DriveId::new("B!AbC123").unwrap();
        let lower = DriveId::new("b!abc123").unwrap();
        assert_eq!(upper, lower);
        assert_eq!(upper.as_str(), "b!abc123");
        assert_eq!(upper.to_string(), "b!abc123");
        assert_eq!("B!AbC123".parse::<DriveId>().unwrap(), lower);
    }

    /// AC2: item IDs keep their case and compare exactly.
    #[test]
    fn item_id_preserves_case_and_compares_exactly() {
        let id = ItemId::new("01ABcd!23").unwrap();
        assert_eq!(id.as_str(), "01ABcd!23");
        assert_ne!(id, ItemId::new("01abcd!23").unwrap());
        assert_ne!(id, ItemId::new("01ABCD!23").unwrap());
        assert_eq!(id, ItemId::new("01ABcd!23").unwrap());
    }

    /// AC3: every constructor and every `FromStr` rejects the empty string with a typed error.
    #[test]
    fn every_constructor_rejects_empty() {
        let empty = |kind| IdError::Empty { kind };
        assert_eq!(DriveId::new(""), Err(empty("drive ID")));
        assert_eq!(ItemId::new(""), Err(empty("item ID")));
        assert_eq!(AccountId::new(""), Err(empty("account ID")));
        assert_eq!(TenantId::new(""), Err(empty("tenant ID")));
        assert_eq!(MountId::new(""), Err(empty("mount ID")));
        assert_eq!("".parse::<DriveId>(), Err(empty("drive ID")));
        assert_eq!("".parse::<ItemId>(), Err(empty("item ID")));
        assert_eq!("".parse::<AccountId>(), Err(empty("account ID")));
        assert_eq!("".parse::<TenantId>(), Err(empty("tenant ID")));
        assert_eq!("".parse::<MountId>(), Err(empty("mount ID")));
    }

    /// AC4: mount IDs that would be unsafe or ambiguous as a directory name are refused.
    #[test]
    fn mount_id_rejects_unsafe_names() {
        let problem = |s: &str| match MountId::new(s) {
            Err(IdError::InvalidMountId { value, reason }) => {
                assert_eq!(value, s);
                reason
            }
            other => panic!("{s:?} should be rejected as a mount ID, got {other:?}"),
        };
        assert_eq!(problem("/"), MountIdProblem::InvalidChar('/'));
        assert_eq!(problem("a/b"), MountIdProblem::InvalidChar('/'));
        assert_eq!(problem("."), MountIdProblem::InvalidChar('.'));
        assert_eq!(problem(".."), MountIdProblem::InvalidChar('.'));
        assert_eq!(problem("Abc"), MountIdProblem::InvalidChar('A'));
        assert_eq!(problem("ab c"), MountIdProblem::InvalidChar(' '));
        assert_eq!(problem("abc\t"), MountIdProblem::InvalidChar('\t'));
        assert_eq!(problem("abc\n"), MountIdProblem::InvalidChar('\n'));
        assert_eq!(problem("ab_c"), MountIdProblem::InvalidChar('_'));
        assert_eq!(problem("é"), MountIdProblem::InvalidChar('é'));
        assert_eq!(problem("-abc"), MountIdProblem::LeadingHyphen);
        assert_eq!(problem("-"), MountIdProblem::LeadingHyphen);
        assert_eq!(problem(&"a".repeat(65)), MountIdProblem::TooLong);
    }

    /// AC4 boundaries: what the rules allow is accepted unchanged.
    #[test]
    fn mount_id_accepts_valid_names() {
        for s in ["a", "0", "abc-123", "a-", "a--b", "personal-1"] {
            assert_eq!(MountId::new(s).unwrap().as_str(), s);
        }
        let longest = "z".repeat(MOUNT_ID_MAX_LEN);
        assert_eq!(MountId::new(longest.clone()).unwrap().as_str(), longest);
    }

    /// AC5: each type serialises as a plain string and deserialises back to itself.
    #[test]
    fn serde_form_is_a_plain_string() {
        assert_serde_string(&DriveId::new("b!abc").unwrap(), "b!abc");
        assert_serde_string(&ItemId::new("01AbC").unwrap(), "01AbC");
        assert_serde_string(&AccountId::new("Acc-1").unwrap(), "Acc-1");
        assert_serde_string(&TenantId::new("Ten-1").unwrap(), "Ten-1");
        assert_serde_string(&MountId::new("mount-1").unwrap(), "mount-1");
    }

    /// AC5: deserialising validates and normalises exactly as the constructor does.
    #[test]
    fn deserialising_validates_and_normalises() {
        let drive: DriveId = from_str_de("B!AbC123").unwrap();
        assert_eq!(drive.as_str(), "b!abc123");

        let item: ItemId = from_str_de("AbC").unwrap();
        assert_eq!(item.as_str(), "AbC");

        assert!(from_str_de::<DriveId>("").is_err());
        assert!(from_str_de::<ItemId>("").is_err());
        assert!(from_str_de::<AccountId>("").is_err());
        assert!(from_str_de::<TenantId>("").is_err());
        assert!(from_str_de::<MountId>("").is_err());
        assert!(from_str_de::<MountId>("..").is_err());
        assert!(from_str_de::<MountId>("Upper").is_err());
        assert!(from_str_de::<MountId>("-x").is_err());

        let err = from_str_de::<MountId>("a/b").unwrap_err().to_string();
        assert!(
            err.contains("mount ID"),
            "error should name the kind: {err}"
        );
    }

    /// AC5: a non-string is not accepted as an identifier.
    #[test]
    fn deserialising_rejects_non_strings() {
        let de = || -> U32Deserializer<ValueError> { 7u32.into_deserializer() };
        assert!(DriveId::deserialize(de()).is_err());
        assert!(ItemId::deserialize(de()).is_err());
        assert!(AccountId::deserialize(de()).is_err());
        assert!(TenantId::deserialize(de()).is_err());
        assert!(MountId::deserialize(de()).is_err());
    }

    /// AC6: `ItemKey` works as a `HashMap` and `BTreeMap` key, and keys that differ only in
    /// drive-ID case are the same key.
    #[test]
    fn item_key_is_a_map_key() {
        let key =
            |d: &str, i: &str| ItemKey::new(DriveId::new(d).unwrap(), ItemId::new(i).unwrap());

        let mut hash = HashMap::new();
        hash.insert(key("B!Drive", "Item1"), 1);
        hash.insert(key("b!drive", "Item2"), 2);
        hash.insert(key("b!other", "Item1"), 3);
        assert_eq!(hash.get(&key("b!DRIVE", "Item1")), Some(&1));
        assert_eq!(hash.get(&key("b!drive", "item1")), None);
        assert_eq!(hash.len(), 3);

        let mut btree = BTreeMap::new();
        btree.insert(key("b!other", "Item1"), 3);
        btree.insert(key("B!Drive", "Item2"), 2);
        btree.insert(key("b!drive", "Item1"), 1);
        assert_eq!(btree.get(&key("B!DRIVE", "Item2")), Some(&2));
        let order: Vec<_> = btree.values().copied().collect();
        assert_eq!(order, vec![1, 2, 3]);
    }

    /// `ItemKey` is not a string: it serialises as a struct, so the string-only serializer
    /// refuses it. Its fields still go through the identifier validation on deserialisation.
    #[test]
    fn item_key_is_not_a_plain_string() {
        let key = ItemKey::new(DriveId::new("b!d").unwrap(), ItemId::new("i").unwrap());
        assert!(str_ser::to_string(&key).is_err());
    }

    fn non_empty_string() -> impl Strategy<Value = String> {
        any::<String>().prop_filter("identifiers are non-empty", |s| !s.is_empty())
    }

    fn valid_mount_id() -> impl Strategy<Value = String> {
        "[a-z0-9][a-z0-9-]{0,63}"
    }

    proptest! {
        #[test]
        fn display_parse_round_trip(s in non_empty_string(), m in valid_mount_id()) {
            let d = DriveId::new(s.clone()).unwrap();
            prop_assert_eq!(d.to_string().parse::<DriveId>().unwrap(), d);
            let i = ItemId::new(s.clone()).unwrap();
            prop_assert_eq!(i.to_string().parse::<ItemId>().unwrap(), i);
            let a = AccountId::new(s.clone()).unwrap();
            prop_assert_eq!(a.to_string().parse::<AccountId>().unwrap(), a);
            let t = TenantId::new(s).unwrap();
            prop_assert_eq!(t.to_string().parse::<TenantId>().unwrap(), t);
            let mount = MountId::new(m).unwrap();
            prop_assert_eq!(mount.to_string().parse::<MountId>().unwrap(), mount);
        }

        #[test]
        fn serde_round_trip(s in non_empty_string(), m in valid_mount_id()) {
            let d = DriveId::new(s.clone()).unwrap();
            prop_assert_eq!(from_str_de::<DriveId>(&str_ser::to_string(&d).unwrap()).unwrap(), d);
            let i = ItemId::new(s.clone()).unwrap();
            prop_assert_eq!(&str_ser::to_string(&i).unwrap(), &s);
            prop_assert_eq!(from_str_de::<ItemId>(&s).unwrap(), i);
            let a = AccountId::new(s.clone()).unwrap();
            prop_assert_eq!(from_str_de::<AccountId>(&str_ser::to_string(&a).unwrap()).unwrap(), a);
            let t = TenantId::new(s).unwrap();
            prop_assert_eq!(from_str_de::<TenantId>(&str_ser::to_string(&t).unwrap()).unwrap(), t);
            let mount = MountId::new(m).unwrap();
            prop_assert_eq!(from_str_de::<MountId>(&str_ser::to_string(&mount).unwrap()).unwrap(), mount);
        }

        /// ADR-0007 R1: normalising twice is the same as once.
        #[test]
        fn drive_id_is_idempotent(s in non_empty_string()) {
            let once = DriveId::new(s).unwrap();
            let twice = DriveId::new(once.as_str()).unwrap();
            prop_assert_eq!(twice.as_str(), once.as_str());
            prop_assert_eq!(twice, once);
        }

        /// ADR-0007 R1: inputs that differ only in ASCII case give the same drive ID, and
        /// that ID has no uppercase ASCII letters. Graph drive IDs are ASCII; full Unicode
        /// case folding is not a bijection (for example `ß` uppercases to `SS`), so the
        /// property is stated over ASCII case.
        #[test]
        fn drive_id_is_case_insensitive(s in non_empty_string(), flips in any::<Vec<bool>>()) {
            let mixed: String = s
                .chars()
                .zip(flips.iter().copied().chain(std::iter::repeat(false)))
                .map(|(c, up)| if up { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() })
                .collect();
            let a = DriveId::new(s.to_ascii_uppercase()).unwrap();
            let b = DriveId::new(s.to_ascii_lowercase()).unwrap();
            let c = DriveId::new(mixed).unwrap();
            prop_assert_eq!(&a, &b);
            prop_assert_eq!(&a, &c);
            prop_assert!(!a.as_str().chars().any(|ch| ch.is_ascii_uppercase()));
        }

        /// AC4: anything that is not a valid mount ID is rejected, and nothing is normalised.
        #[test]
        fn mount_id_accepts_exactly_the_rule(s in any::<String>()) {
            let valid = !s.is_empty()
                && s.chars().count() <= MOUNT_ID_MAX_LEN
                && !s.starts_with('-')
                && s.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-'));
            match MountId::new(s.clone()) {
                Ok(m) => {
                    prop_assert!(valid);
                    prop_assert_eq!(m.as_str(), s.as_str());
                }
                Err(_) => prop_assert!(!valid),
            }
        }
    }
}
