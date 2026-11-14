//! PKCE (RFC 7636) and the OAuth `state` value.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng as _;
use sha2::{Digest as _, Sha256};

use super::secret::{PkceVerifier, REDACTED, constant_time_eq};

/// The only PKCE method we send (ADR-0010 R1).
pub const CODE_CHALLENGE_METHOD: &str = "S256";

/// Random bytes behind a verifier: 32 bytes encode to 43 characters, the RFC 7636 minimum.
const VERIFIER_BYTES: usize = 32;

/// Random bytes behind a `state` value.
const STATE_BYTES: usize = 32;

/// Base64url without padding of `n` bytes from the thread-local CSPRNG, which `rand`
/// seeds from the operating system.
fn random_token(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(&bytes)
}

impl PkceVerifier {
    /// Generates a fresh verifier: 43 characters from the unreserved set.
    #[must_use]
    pub fn generate() -> Self {
        Self::new(random_token(VERIFIER_BYTES))
    }

    /// The `S256` code challenge for this verifier: base64url(SHA-256(verifier)).
    #[must_use]
    pub fn challenge(&self) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(self.expose_secret().as_bytes()))
    }
}

/// The OAuth `state` value that ties a redirect to the sign-in attempt that started it.
#[derive(Clone)]
pub struct State(String);

impl State {
    /// Generates a fresh, unguessable state value.
    #[must_use]
    pub fn generate() -> Self {
        Self(random_token(STATE_BYTES))
    }

    /// The value, as it goes into the authorisation URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `candidate` is this state, compared in constant time.
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        constant_time_eq(self.0.as_bytes(), candidate.as_bytes())
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "State({REDACTED})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 appendix B test vector.
    #[test]
    fn s256_challenge_matches_rfc_7636_vector() {
        let v = PkceVerifier::new("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(v.challenge(), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn verifier_has_valid_length_and_alphabet() {
        let v = PkceVerifier::generate();
        let s = v.expose_secret();
        assert!((43..=128).contains(&s.len()), "{}", s.len());
        assert!(
            s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
        );
    }

    #[test]
    fn generated_values_differ() {
        assert_ne!(
            PkceVerifier::generate().expose_secret(),
            PkceVerifier::generate().expose_secret()
        );
        assert_ne!(State::generate().as_str(), State::generate().as_str());
    }

    #[test]
    fn state_matches_only_itself() {
        let s = State::generate();
        assert!(s.matches(s.as_str()));
        assert!(!s.matches(""));
        assert!(!s.matches(&format!("{}x", s.as_str())));
        assert!(!format!("{s:?}").contains(s.as_str()));
    }
}
