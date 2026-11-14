//! Secret value types whose `Debug` output is redacted (ADR-0010 R6).
//!
//! None of these types implements `Display`, `Serialize` or `Deserialize`, so a secret can
//! reach a log line, an error message or an IPC response only through an explicit call to
//! `expose_secret`, which is easy to find in review.

use std::fmt;

/// What every redacted `Debug` prints in place of the value.
pub(crate) const REDACTED: &str = "<redacted>";

macro_rules! secret_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq)]
        pub struct $name(String);

        impl $name {
            /// Wraps a secret value.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// The secret value. Never log, print or persist what this returns, except into
            /// the Secret Service or the request that needs it.
            #[must_use]
            pub fn expose_secret(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({REDACTED})", stringify!($name))
            }
        }
    };
}

secret_type!(
    /// An OAuth access token: the bearer credential sent to Graph. Lives in memory only.
    AccessToken
);
secret_type!(
    /// An OAuth refresh token. Stored only in the Secret Service (ADR-0010 R6).
    RefreshToken
);
secret_type!(
    /// An OpenID Connect ID token, as returned by the token endpoint.
    IdToken
);
secret_type!(
    /// An authorisation code taken from a redirect, redeemable once for tokens.
    AuthorizationCode
);
secret_type!(
    /// A PKCE `code_verifier` (RFC 7636).
    PkceVerifier
);

/// Compares two byte strings in time that depends only on their lengths, so a `state`
/// check does not reveal how much of a guess was right.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0010 R6: no secret type prints its value through `Debug`.
    #[test]
    fn debug_is_redacted() {
        let value = "very-secret-value";
        let printed = [
            format!("{:?}", AccessToken::new(value)),
            format!("{:?}", RefreshToken::new(value)),
            format!("{:?}", IdToken::new(value)),
            format!("{:?}", AuthorizationCode::new(value)),
            format!("{:?}", PkceVerifier::new(value)),
            format!("{:#?}", Some(AccessToken::new(value))),
        ];
        for p in printed {
            assert!(!p.contains(value), "{p}");
            assert!(p.contains(REDACTED), "{p}");
        }
    }

    #[test]
    fn constant_time_eq_compares_whole_value() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
