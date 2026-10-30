//! A short hold when the surface is lost for a moment.
//!
//! Rock has seams. A ray can slip through one for a frame, and dropping the player for that is the
//! "latch, drop, latch" the old mod is known for. So on a natural surface a failed hold is
//! forgiven for a fraction of a second. A lattice keeps the game's exact behaviour.
//!
//! Climb programs can run on more than one thread and one server has many players, so the state
//! lives in a table of atomics, one slot per entity.

use std::sync::atomic::{AtomicU64, Ordering};

/// Slots in the table. Player ids are below 0x400 (`FUN_140a00070`, flag 0x40).
const SLOTS: usize = 0x400;
/// The state for "this player is on a lattice, or has no climb".
const NOT_NATURAL: u64 = 0;
/// The state for "this player holds a natural surface".
const HOLDING: u64 = 1;
/// A larger state is the time of the first failure, moved up by this much so it cannot look like
/// either state above.
const TIME_BASE: u64 = 2;

/// One step of the state of a slot: the next state, and whether to keep holding.
pub fn fail_step(state: u64, now_ms: u64, limit_ms: u64) -> (u64, bool) {
    match state {
        NOT_NATURAL => (NOT_NATURAL, false),
        HOLDING if limit_ms == 0 => (NOT_NATURAL, false),
        HOLDING => (now_ms.saturating_add(TIME_BASE), true),
        since => {
            let waited = now_ms.saturating_sub(since.saturating_sub(TIME_BASE));
            if waited <= limit_ms {
                (since, true)
            } else {
                (NOT_NATURAL, false)
            }
        }
    }
}

/// The grace state of every player.
pub struct Grace {
    /// One state per entity id.
    slots: [AtomicU64; SLOTS],
}

impl Grace {
    /// Nobody holds anything.
    pub const fn new() -> Grace {
        Grace {
            slots: [const { AtomicU64::new(NOT_NATURAL) }; SLOTS],
        }
    }

    /// The slot of `entity`.
    fn slot(&self, entity: u32) -> Option<&AtomicU64> {
        self.slots.get(entity as usize % SLOTS)
    }

    /// `entity` holds a natural surface right now.
    pub fn held(&self, entity: u32) {
        if let Some(slot) = self.slot(entity) {
            slot.store(HOLDING, Ordering::Relaxed);
        }
    }

    /// `entity` holds a lattice right now, so a failure there is the game's to judge.
    pub fn lattice(&self, entity: u32) {
        if let Some(slot) = self.slot(entity) {
            slot.store(NOT_NATURAL, Ordering::Relaxed);
        }
    }

    /// `entity` failed its hold at `now_ms`. Should it keep holding?
    pub fn failed(&self, entity: u32, now_ms: u64, limit_ms: u64) -> bool {
        let Some(slot) = self.slot(entity) else {
            return false;
        };
        let (next, hold) = fail_step(slot.load(Ordering::Relaxed), now_ms, limit_ms);
        slot.store(next, Ordering::Relaxed);
        hold
    }
}

impl Default for Grace {
    fn default() -> Grace {
        Grace::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failure on a lattice is a drop at once, exactly as the game has it.
    #[test]
    fn a_lattice_gets_no_grace() {
        assert_eq!(fail_step(NOT_NATURAL, 1000, 150), (NOT_NATURAL, false));
        let grace = Grace::new();
        grace.lattice(9);
        assert!(!grace.failed(9, 1000, 150));
    }

    /// The first failure starts the clock and holds.
    #[test]
    fn the_first_failure_holds() {
        let (state, hold) = fail_step(HOLDING, 1000, 150);
        assert!(hold);
        assert_eq!(state, 1000 + TIME_BASE);
    }

    /// The hold lasts exactly as long as the limit, then the player drops.
    #[test]
    fn the_hold_ends_at_the_limit() {
        let grace = Grace::new();
        grace.held(5);
        assert!(grace.failed(5, 1000, 150));
        assert!(grace.failed(5, 1100, 150));
        assert!(grace.failed(5, 1150, 150));
        assert!(!grace.failed(5, 1151, 150));
    }

    /// After a drop the slot is clear, so the next failure is not forgiven by old state.
    #[test]
    fn a_drop_clears_the_state() {
        let grace = Grace::new();
        grace.held(5);
        assert!(grace.failed(5, 1000, 150));
        assert!(!grace.failed(5, 2000, 150));
        assert!(!grace.failed(5, 2001, 150));
    }

    /// A good frame in between starts the clock again, so a long climb of short gaps holds.
    #[test]
    fn a_good_frame_starts_the_clock_again() {
        let grace = Grace::new();
        grace.held(5);
        assert!(grace.failed(5, 1000, 150));
        grace.held(5);
        assert!(grace.failed(5, 1140, 150));
        assert!(grace.failed(5, 1290, 150));
    }

    /// A limit of 0 turns the grace off.
    #[test]
    fn a_zero_limit_means_no_grace() {
        let grace = Grace::new();
        grace.held(5);
        assert!(!grace.failed(5, 1000, 0));
    }

    /// Players do not share state.
    #[test]
    fn players_do_not_share_state() {
        let grace = Grace::new();
        grace.held(5);
        assert!(!grace.failed(6, 1000, 150));
        assert!(grace.failed(5, 1000, 150));
    }

    /// Any id is safe, including ids past the table.
    #[test]
    fn any_id_is_safe() {
        let grace = Grace::new();
        grace.held(u32::MAX);
        assert!(grace.failed(u32::MAX, 1000, 150));
    }

    /// A clock that jumps back cannot underflow.
    #[test]
    fn a_clock_going_backward_is_safe() {
        let (state, hold) = fail_step(5000 + TIME_BASE, 1000, 150);
        assert!(hold);
        assert_eq!(state, 5000 + TIME_BASE);
    }
}
