//! Delta processing: a delta run is staged durably, then applied when its delta link
//! arrives (ADR-0022).
//!
//! Declared by WP-014. WP-031 fills [`convert`], [`plan`] and [`run`]; WP-037 fills
//! [`resync`].

pub mod convert;
pub mod plan;
pub mod resync;
pub mod run;
