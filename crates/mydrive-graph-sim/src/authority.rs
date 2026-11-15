//! The fake sign-in authority: the authorise step, the token endpoint with its PKCE check,
//! refresh-token rotation and revocation.
//!
//! Declared by WP-014; WP-023 fills it, documented in `docs/identity.md`. It is not wired
//! into any listener yet.
