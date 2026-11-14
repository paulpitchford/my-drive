//! Paths as they travel in the protocol (ADR-0009 "Paths", R8).
//!
//! Every path in a request, response or notification is an absolute UTF-8 path under a mount
//! point. [`ProtocolPath`] enforces the parts of that rule that can be checked without knowing
//! the mount table: the path is absolute, contains no NUL byte, and has no `.` or `..`
//! component. The last check matters for R8: the daemon decides "under a mount point" by
//! comparing path components, and must never stat the mount point to answer a status query
//! (F11), so a path such as `/home/u/OneDrive/../.local/share/...` has to be refused
//! lexically rather than resolved.
//!
//! Whether the path is under a mount point, and not under a backing directory, is decided by
//! the daemon, which knows the mounts.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Why a string is not a valid protocol path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PathProblem {
    /// The string is empty.
    #[error("it is empty")]
    Empty,
    /// The path does not start with `/`.
    #[error("it is not absolute")]
    NotAbsolute,
    /// The path contains a NUL byte, which no Linux path can hold.
    #[error("it contains a NUL byte")]
    NulByte,
    /// The path has a `.` or `..` component, so its meaning depends on resolution.
    #[error("it contains a '.' or '..' component")]
    DotComponent,
}

/// A string that cannot be used as a protocol path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("path {path:?} is not a valid protocol path: {problem}")]
pub struct PathError {
    /// The rejected string.
    pub path: String,
    /// The rule it broke.
    pub problem: PathProblem,
}

/// An absolute path as carried by the protocol.
///
/// Built only through [`ProtocolPath::new`], which `serde` also uses, so a relative path can
/// never be deserialised into one (WP-005 AC6). It is kept exactly as given: no
/// normalisation, no case folding, no trailing-slash handling.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProtocolPath(String);

impl ProtocolPath {
    /// Checks and wraps a path.
    ///
    /// # Errors
    ///
    /// [`PathError`] if the path is empty, not absolute, contains a NUL byte or has a `.` or
    /// `..` component.
    pub fn new(path: impl Into<String>) -> Result<Self, PathError> {
        let path = path.into();
        match problem(&path) {
            None => Ok(Self(path)),
            Some(problem) => Err(PathError { path, problem }),
        }
    }

    /// The path as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn problem(path: &str) -> Option<PathProblem> {
    if path.is_empty() {
        return Some(PathProblem::Empty);
    }
    if !path.starts_with('/') {
        return Some(PathProblem::NotAbsolute);
    }
    if path.contains('\0') {
        return Some(PathProblem::NulByte);
    }
    if path.split('/').any(|part| part == "." || part == "..") {
        return Some(PathProblem::DotComponent);
    }
    None
}

impl fmt::Display for ProtocolPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for ProtocolPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for ProtocolPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProtocolPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_absolute_paths() {
        for ok in [
            "/",
            "/home/u/OneDrive",
            "/home/u/OneDrive - Org/a b/ü.txt",
            "/a//b/",
        ] {
            assert!(ProtocolPath::new(ok).is_ok(), "{ok}");
        }
    }

    /// WP-005 AC6: a relative path is rejected.
    #[test]
    fn rejects_relative_paths() {
        for bad in ["OneDrive/a.txt", "./a", "a", "~/OneDrive"] {
            let err = ProtocolPath::new(bad).unwrap_err();
            assert_eq!(err.problem, PathProblem::NotAbsolute, "{bad}");
        }
    }

    #[test]
    fn rejects_empty_nul_and_dot_components() {
        assert_eq!(
            ProtocolPath::new("").unwrap_err().problem,
            PathProblem::Empty
        );
        assert_eq!(
            ProtocolPath::new("/a\0b").unwrap_err().problem,
            PathProblem::NulByte
        );
        for bad in ["/a/../b", "/a/./b", "/a/..", "/.", "/home/u/OneDrive/.."] {
            assert_eq!(
                ProtocolPath::new(bad).unwrap_err().problem,
                PathProblem::DotComponent,
                "{bad}"
            );
        }
        // Names that merely start with dots are ordinary names.
        assert!(ProtocolPath::new("/a/.hidden/...").is_ok());
    }

    #[test]
    fn serde_uses_the_validating_constructor() {
        let ok: ProtocolPath = serde_json::from_str("\"/home/u/OneDrive\"").unwrap();
        assert_eq!(ok.as_str(), "/home/u/OneDrive");
        assert_eq!(serde_json::to_string(&ok).unwrap(), "\"/home/u/OneDrive\"");
        let err = serde_json::from_str::<ProtocolPath>("\"OneDrive\"").unwrap_err();
        assert!(err.to_string().contains("not absolute"), "{err}");
    }
}
