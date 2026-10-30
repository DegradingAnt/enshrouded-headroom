//! Pure client measurement and input-planning core.
//!
//! This crate has no native sample producer, loader export, renderer or input hook.
//! A new session is unavailable until a caller supplies a current world contract.
//! Supplying data is a caller assertion, not proof of a native binding.
//!
//! # Offline geometry
//!
//!     use ember_blueprint::Frame;
//!     use ember_measurement::{measure, Point};
//!     let frame = Frame {
//!         units: "metres".to_owned(),
//!         axes: "right_handed_x_east_y_up_z_south".to_owned(),
//!         source_mapping: "Offline canonical-coordinate calculation.".to_owned(),
//!     };
//!     let a = Point::new([0.0; 3])?;
//!     let b = Point::new([3.0, 4.0, 12.0])?;
//!     assert_eq!(measure(&frame, a, b)?.diagonal().metres, 13.0);
//!     # Ok::<(), ember_measurement::Error>(())
mod context;
mod error;
mod geometry;
mod panel;
mod state;

pub use context::*;
pub use error::Error;
pub use geometry::*;
pub use panel::*;
pub use state::*;
