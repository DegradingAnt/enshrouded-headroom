//! Pure tick-based seam-forgiveness model for initialized simulation state.
//!
//! Native runtime currently disables this model: initialization of Climb padding is unproven.
//! A future native binding needs a valid state-lifetime and replay contract before enabling it.
//! The initialized test fixtures exercise the previous proposal, including its bounded exit gaps.

/// The ticks in one second. The tick is 0xfe502b ns in both exes.
pub const TICKS_PER_SECOND: f32 = 60.0;
/// The state after a lattice hold, a drop, or a committed native stop.
pub const NOT_NATURAL: u8 = 0;
/// The state after a natural hold that passed. Each tick forgiven adds one to it.
pub const HOLDING: u8 = 1;

/// The state after a hold that passed.
pub fn held(natural: bool) -> u8 {
    if natural {
        HOLDING
    } else {
        NOT_NATURAL
    }
}

/// The state after a hold that failed, and whether to keep holding. `limit` is how many ticks in a
/// row may be forgiven, so the tick after that many drops the player.
pub fn failed(state: u8, limit: u8) -> (u8, bool) {
    if state == NOT_NATURAL || state.saturating_sub(HOLDING) >= limit {
        (NOT_NATURAL, false)
    } else {
        (state.saturating_add(1), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run failed holds from `state` until the drop, and give the ticks forgiven.
    fn forgiven(mut state: u8, limit: u8) -> u32 {
        let mut ticks = 0;
        loop {
            let (next, keep) = failed(state, limit);
            state = next;
            if !keep {
                assert_eq!(state, NOT_NATURAL, "a drop clears the state");
                return ticks;
            }
            ticks += 1;
        }
    }

    /// A failure after a lattice hold is a drop at once, exactly as the game has it.
    #[test]
    fn a_lattice_gets_no_grace() {
        assert_eq!(held(false), NOT_NATURAL);
        assert_eq!(failed(held(false), 9), (NOT_NATURAL, false));
    }

    /// The first failure of a natural hold is forgiven.
    #[test]
    fn the_first_failure_holds() {
        assert_eq!(failed(held(true), 9), (HOLDING + 1, true));
    }

    /// The hold lasts exactly the limit, then the player drops.
    #[test]
    fn the_hold_ends_at_the_limit() {
        for limit in [1, 2, 9, 60] {
            assert_eq!(forgiven(HOLDING, limit), u32::from(limit), "{limit}");
        }
    }

    /// After a drop the state is clear, so the next failure is not forgiven by old state.
    #[test]
    fn a_drop_clears_the_state() {
        assert_eq!(forgiven(HOLDING, 3), 3);
        assert_eq!(forgiven(NOT_NATURAL, 3), 0);
    }

    /// A good tick in between starts the count again, so a long climb of short gaps holds.
    #[test]
    fn a_good_tick_starts_the_count_again() {
        let (state, keep) = failed(held(true), 2);
        assert!(keep);
        let (state, keep) = failed(state, 2);
        assert!(keep);
        assert_eq!(failed(state, 2), (NOT_NATURAL, false));
        assert_eq!(forgiven(held(true), 2), 2);
    }

    /// A limit of 0 turns the grace off.
    #[test]
    fn a_zero_limit_means_no_grace() {
        assert_eq!(failed(HOLDING, 0), (NOT_NATURAL, false));
    }

    /// The decision depends on the byte and the limit alone: the same byte gives the same answer
    /// however often it is asked, which is what a replay asks of it.
    #[test]
    fn the_same_byte_gives_the_same_answer() {
        for state in 0..=u8::MAX {
            assert_eq!(failed(state, 9), failed(state, 9));
        }
    }

    /// Any byte is safe, including one no build of this mod wrote. At the largest limit the
    /// settings allow, it can only shorten the grace.
    #[test]
    fn any_byte_is_safe() {
        for state in 0..=u8::MAX {
            assert!(forgiven(state, 60) <= 60, "{state}");
        }
    }
}
