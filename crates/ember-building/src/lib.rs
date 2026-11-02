//! Bounded pure commands for one two-corner, full-cuboid area-build mode.
//!
//! Grid indices have no declared game units, origin, axis mapping or native meaning.
//! The host must authenticate callers independently of every command payload.
//! Native client/server adapters, transport, UI, PlayStation input and holograms
//! remain unavailable. The only shipped adapter rejects before payment or mutation.
//!
//! # Example
//!
//! This example assumes that a trusted host has already authenticated its caller.
//! Calling `connect` does not itself perform authentication.
//!
//!     use ember_building::{Action, Building, Command, CommandOutcome, GridPoint,
//!                          InputContext, Limits, State};
//!     let mut building = Building::new(7, Limits::default())?;
//!     let caller = building.connect(42, 1)?;
//!     let intent = Command::new(1, 7, Action::Select(GridPoint::new(7, [0, 0, 0])));
//!     assert_eq!(building.handle(caller, InputContext::Menu, intent)?,
//!                CommandOutcome::Ignored);
//!     assert_eq!(building.handle(caller, InputContext::Building, intent)?,
//!                CommandOutcome::Accepted);
//!     assert!(matches!(building.snapshot(caller)?.state, State::FirstCorner { .. }));
//!     # Ok::<(), ember_building::Error>(())
//!
//! A payload cannot construct a trusted lookup handle through its fields:
//!
//! ```compile_fail
//! use ember_building::Session;
//! let forged = Session { key: 42, generation: 1 };
//! ```
mod model;
mod native;
mod session;

pub use model::*;
pub use native::*;
pub use session::*;
