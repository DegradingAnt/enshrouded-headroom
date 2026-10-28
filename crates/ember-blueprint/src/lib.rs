//! Portable interchange for guide geometry and transactional model refresh.
//!
//! This crate does not read game memory or implement camera, rendering or input hooks.
//! An adapter emits a complete model; validation precedes every visible replacement.
//!
//! # Example
//!
//!     use ember_blueprint::{parse, Limits, Session};
//!     let bytes = include_bytes!("../examples/watchtower.blueprint.json");
//!     let model = parse(bytes, Limits::default())?;
//!     let session = Session::new(model);
//!     assert_eq!(session.snapshot().model().revision().sequence, 1);
//!     # Ok::<(), ember_blueprint::Error>(())
mod error;
mod highcrown;
mod model;
mod update;
mod validate;

pub use error::{Error, ErrorKind};
pub use highcrown::convert_highcrown;
pub use model::*;
pub use update::*;
pub use validate::{parse, read_bounded, Limits};

use sha2::{Digest, Sha256};

/// The open JSON Schema, Draft 2020-12. Semantic validation remains mandatory.
pub const JSON_SCHEMA: &str = include_str!("../schema/blueprint-v1.schema.json");

/// Hash the exact input snapshot, including whitespace. A display version is insufficient.
pub fn sha256(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}
