//! ember-climb: free climbing on any steep rock, cliff or wall, with the game's own climb moves,
//! stamina and collision. There is no button. A climb starts by walking, sprinting, jumping or
//! gliding into a steep surface.
//!
//! The game already climbs, but only on flat iron lattices. Its wall probe wants four rays with
//! one exact normal. This mod lets that probe run first, so a lattice keeps its native geometry answer. When
//! the probe fails it fits a patch of the surface instead ([`surface`]), and steers the climber
//! along it, because the game moves a climber along world up. [`exit`] ends a climb cleanly where a
//! wall turns walkable.
//!
//! The same code runs in the client and in the dedicated server. Every game fact it relies on is in
//! docs/re-notes.md, "Free climbing". It runs on four exact client/server builds. Any other build, a signature that
//! does not match exactly once, or a loaded ClimbAnywhere leaves the game untouched.

pub mod coexist;
pub mod config;
pub mod diag;
pub mod effort;
pub mod exit;
pub mod game;
pub mod geom;
pub mod grace;
pub mod hooks;
pub mod plan;
pub mod runtime;
pub mod sigs;
pub mod surface;

#[cfg(test)]
mod testworld;

use emberkit::guard::{guard, FeatureId};

/// The parts of this mod that can be switched off on their own. A panic in one switches that one
/// off for the session, and the game's own function still runs.
#[derive(Clone, Copy, Debug)]
pub enum Feature {
    /// Start-up. A failure here leaves the game untouched.
    Startup,
    /// Finding a surface, and holding it.
    Surface,
    /// Starting a climb.
    Entry,
    /// Steering along a surface.
    Steer,
    /// Ending a climb at the top.
    Exit,
    /// The glider's CanClimb.
    Glide,
    /// The stamina multiplier.
    Stamina,
    /// The diagnostics.
    Diag,
}

impl FeatureId for Feature {
    fn index(self) -> u32 {
        match self {
            Feature::Startup => 0,
            Feature::Surface => 1,
            Feature::Entry => 2,
            Feature::Steer => 3,
            Feature::Exit => 4,
            Feature::Glide => 5,
            Feature::Stamina => 6,
            Feature::Diag => 7,
        }
    }
}

/// Called by Ultimate ASI Loader once the game process is up, before `WinMain`. It must never
/// unwind into the loader.
#[no_mangle]
pub extern "C" fn InitializeASI() {
    guard(Feature::Startup, || (), runtime::start);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each feature has its own bit: a shared bit would let one failure switch two parts off.
    #[test]
    fn every_feature_has_its_own_bit() {
        let mut bits = [
            Feature::Startup,
            Feature::Surface,
            Feature::Entry,
            Feature::Steer,
            Feature::Exit,
            Feature::Glide,
            Feature::Stamina,
            Feature::Diag,
        ]
        .map(FeatureId::index);
        bits.sort_unstable();
        assert_eq!(bits, [0, 1, 2, 3, 4, 5, 6, 7]);
    }
}
