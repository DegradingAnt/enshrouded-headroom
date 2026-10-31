//! The per-second diagnostics line.
//!
//! The first in-game test of climbing failed and its log was lost, so nobody knows which step
//! failed. This keeps that from happening again. The hooks only bump atomic counters: they do not
//! allocate, take a lock or log on a game thread. A `climb-diag` thread wakes once a second and
//! writes one `climb diag:` line when a climb moved a count. A surface that was near with no climb
//! writes one much less often, because a start that failed must leave a trace too.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use emberkit::guard::guard;

use crate::game::{CLEAR, MISS, NOT_NOW, OK, PLANE};
use crate::surface::{Mode, Reject, Scan};
use crate::Feature;

/// How often the log thread looks at the counts.
const LOG_INTERVAL: Duration = Duration::from_secs(1);
/// The fewest ticks between two lines when no climb ran.
const NEAR_TICKS: u32 = 30;
/// The last probe code before any probe ran. It is above every `u8` code.
const NO_CODE: u32 = 0x100;

/// Declare the counts: the atomics the hooks bump, and the plain copy the log thread reads.
macro_rules! counters {
    ($($(#[$doc:meta])* $name:ident,)*) => {
        /// Every count the hooks keep. Each one stands alone, so `Relaxed` is enough; a log line
        /// may mix counts from two moments a few nanoseconds apart.
        struct Counts {
            $($(#[$doc])* $name: AtomicU64,)*
        }

        /// The counts at one moment.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct Snapshot {
            $($(#[$doc])* pub $name: u64,)*
        }

        impl Counts {
            /// Every count 0.
            const fn new() -> Counts {
                Counts {
                    $($name: AtomicU64::new(0),)*
                }
            }

            /// Every count now.
            fn snapshot(&self) -> Snapshot {
                Snapshot {
                    $($name: self.$name.load(Ordering::Relaxed),)*
                }
            }
        }
    };
}

counters! {
    /// Game probes that passed: a lattice.
    probe_ok,
    /// Game probes that failed with 0x15, a corner off the plane.
    probe_plane,
    /// Game probes that failed with 0x1e, the clearance ray.
    probe_clear,
    /// Game probes that failed with 0x1f, a ray that missed.
    probe_miss,
    /// Game probes with any other code.
    probe_other,
    /// Natural probes to start a climb.
    entry_tried,
    /// Natural probes that could start a climb.
    entry_ok,
    /// Natural probes for a move.
    move_tried,
    /// Natural probes that allowed the move.
    move_ok,
    /// Natural probes for the hold.
    stay_tried,
    /// Natural probes that kept the hold.
    stay_ok,
    /// Refused: the ray direction has no sideways axis.
    reject_direction,
    /// Refused: too few rays hit.
    reject_hits,
    /// Refused: the hits do not lie on one surface.
    reject_bumpy,
    /// Refused: the surface is walkable.
    reject_walkable,
    /// Refused: the surface hangs over too far.
    reject_overhang,
    /// Refused: the surface is too far away.
    reject_far,
    /// Refused: something solid stands in the way.
    reject_blocked,
    /// Natural entries found while walking.
    by_walk,
    /// Natural entries found while jumping or falling.
    by_jump,
    /// Natural entries found while sprinting.
    by_sprint,
    /// Natural entries found while gliding.
    by_glide,
    /// Entries skipped because the setting for this start is off.
    start_off,
    /// Start-check calls: one a frame while the player does not climb.
    start_calls,
    /// Top-entry calls.
    top_calls,
    /// Top-entry calls that passed.
    top_ok,
    /// Bottom-entry calls.
    low_calls,
    /// Bottom-entry calls that passed.
    low_ok,
    /// Bottom entry: code 6 with no probe: no move input, or the building spark is out.
    low_no_input,
    /// Bottom entry: code 6 after a probe passed: the stick is too far off the wall.
    low_angle,
    /// Bottom entry: the game's angle test failed and this mod's cone test passed.
    low_cone,
    /// Bottom entry: code 0x1e: the re-grab cooldown after a drop.
    low_cooldown,
    /// Bottom entry: code 0x1f: no surface.
    low_probe,
    /// Bottom entry: anything else.
    low_other,
    /// Wall-lost calls: one a frame while the player climbs.
    lost_calls,
    /// Wall-lost calls that held on a voxel surface.
    lost_held,
    /// Wall-lost calls forgiven by the grace time.
    lost_grace,
    /// Wall-lost calls that dropped the player.
    lost_drops,
    /// Drops with no probe at all: the climb normal was 0.
    lost_no_probe,
    /// Frames of steering.
    steered,
    /// Stop-climbing calls.
    stop,
    /// Exit-top checks that started the mantle.
    exit_top,
    /// Bottom exit checks that stepped off.
    exit_bottom,
    /// Game exits refused because the floor was steep rock.
    exit_refused,
    /// Exits this mod found that the game did not.
    exit_taken,
}

/// The latest values, for reading a line by itself.
struct Lasts {
    /// The code of the last game probe, or [`NO_CODE`].
    code: AtomicU32,
    /// The entity of the last game probe that passed.
    entity: AtomicU32,
    /// The hits of the last natural probe.
    hits: AtomicU32,
    /// The spread of the last natural probe, as float bits.
    spread: AtomicU32,
    /// The up part of the last natural normal, as float bits.
    ny: AtomicU32,
    /// The last stand-off error, as float bits.
    gap: AtomicU32,
    /// The capsule width, as float bits.
    width: AtomicU32,
    /// The capsule height, as float bits.
    height: AtomicU32,
}

/// The latest values at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LastValues {
    /// The code of the last game probe, or 0x100 before the first one.
    pub code: u32,
    /// The entity of the last game probe that passed.
    pub entity: u32,
    /// The hits of the last natural probe.
    pub hits: u32,
    /// The spread of the last natural probe, in metres.
    pub spread: f32,
    /// The up part of the last natural normal.
    pub ny: f32,
    /// The last stand-off error, in metres.
    pub gap: f32,
    /// The capsule width, in metres.
    pub width: f32,
    /// The capsule height, in metres.
    pub height: f32,
}

/// All the diagnostics of this process.
pub struct Diag {
    /// The counts.
    counts: Counts,
    /// The latest values.
    lasts: Lasts,
}

/// Add one to `counter`. It wraps at `u64::MAX`, which a session never reaches.
fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

impl Diag {
    /// Every count 0, and no probe yet.
    pub const fn new() -> Diag {
        Diag {
            counts: Counts::new(),
            lasts: Lasts {
                code: AtomicU32::new(NO_CODE),
                entity: AtomicU32::new(0),
                hits: AtomicU32::new(0),
                spread: AtomicU32::new(0),
                ny: AtomicU32::new(0),
                gap: AtomicU32::new(0),
                width: AtomicU32::new(0),
                height: AtomicU32::new(0),
            },
        }
    }

    /// One game probe with this `code`. `entity` is kept only when it passed.
    pub fn probe(&self, code: u8, entity: u32) {
        let c = &self.counts;
        match code {
            OK => {
                self.lasts.entity.store(entity, Ordering::Relaxed);
                bump(&c.probe_ok);
            }
            PLANE => bump(&c.probe_plane),
            CLEAR => bump(&c.probe_clear),
            MISS => bump(&c.probe_miss),
            _ => bump(&c.probe_other),
        }
        self.lasts.code.store(u32::from(code), Ordering::Relaxed);
    }

    /// One natural probe for `mode` with this answer.
    pub fn natural(&self, mode: Mode, scan: &Scan) {
        let c = &self.counts;
        let (tried, passed) = match mode {
            Mode::Entry => (&c.entry_tried, &c.entry_ok),
            Mode::Move => (&c.move_tried, &c.move_ok),
            Mode::Stay => (&c.stay_tried, &c.stay_ok),
        };
        bump(tried);
        match scan.verdict {
            Ok(_) => bump(passed),
            Err(Reject::Direction) => bump(&c.reject_direction),
            Err(Reject::Hits) => bump(&c.reject_hits),
            Err(Reject::Bumpy) => bump(&c.reject_bumpy),
            Err(Reject::Walkable) => bump(&c.reject_walkable),
            Err(Reject::Overhang) => bump(&c.reject_overhang),
            Err(Reject::TooFar) => bump(&c.reject_far),
            Err(Reject::Blocked) => bump(&c.reject_blocked),
        }
        self.lasts
            .hits
            .store(u32::from(scan.hits), Ordering::Relaxed);
        if let Some(fit) = &scan.fit {
            let bits = (fit.spread.to_bits(), fit.normal.y.to_bits());
            self.lasts.spread.store(bits.0, Ordering::Relaxed);
            self.lasts.ny.store(bits.1, Ordering::Relaxed);
        }
    }

    /// A natural entry was found with this trigger.
    pub fn found_by(&self, trigger: crate::game::Trigger) {
        use crate::game::Trigger;
        let c = &self.counts;
        bump(match trigger {
            Trigger::Walk => &c.by_walk,
            Trigger::Jump => &c.by_jump,
            Trigger::Sprint => &c.by_sprint,
            Trigger::Glide => &c.by_glide,
        });
    }

    /// An entry was skipped because its setting is off.
    pub fn start_is_off(&self) {
        bump(&self.counts.start_off);
    }

    /// One start-check call.
    pub fn start(&self) {
        bump(&self.counts.start_calls);
    }

    /// One top-entry call with result `code`.
    pub fn top_entry(&self, code: u8) {
        bump(&self.counts.top_calls);
        if code == OK {
            bump(&self.counts.top_ok);
        }
    }

    /// One bottom-entry call with result `code`. `nested` is the code of the probe that ran inside
    /// it, or `None` if none ran.
    pub fn low_entry(&self, code: u8, nested: Option<u8>) {
        let c = &self.counts;
        bump(&c.low_calls);
        bump(match (code, nested) {
            (OK, _) => &c.low_ok,
            (NOT_NOW, None) => &c.low_no_input,
            (NOT_NOW, Some(OK)) => &c.low_angle,
            (CLEAR, _) => &c.low_cooldown,
            (MISS, _) => &c.low_probe,
            _ => &c.low_other,
        });
    }

    /// The game's angle test failed and this mod's cone test passed.
    pub fn cone_passed(&self) {
        bump(&self.counts.low_cone);
    }

    /// One wall-lost call that held: it returned an id or the voxel value.
    pub fn held(&self, voxel: bool) {
        bump(&self.counts.lost_calls);
        if voxel {
            bump(&self.counts.lost_held);
        }
    }

    /// One wall-lost call forgiven by the grace time.
    pub fn graced(&self) {
        bump(&self.counts.lost_calls);
        bump(&self.counts.lost_grace);
    }

    /// One wall-lost call that dropped the player. `probed` is false when no probe ran.
    pub fn dropped(&self, probed: bool) {
        bump(&self.counts.lost_calls);
        bump(&self.counts.lost_drops);
        if !probed {
            bump(&self.counts.lost_no_probe);
        }
    }

    /// One frame of steering, with this stand-off error.
    pub fn steered(&self, gap: f32) {
        bump(&self.counts.steered);
        self.lasts.gap.store(gap.to_bits(), Ordering::Relaxed);
    }

    /// The capsule the game gave.
    pub fn capsule(&self, width: f32, height: f32) {
        self.lasts.width.store(width.to_bits(), Ordering::Relaxed);
        self.lasts.height.store(height.to_bits(), Ordering::Relaxed);
    }

    /// One stop-climbing call.
    pub fn stop(&self) {
        bump(&self.counts.stop);
    }

    /// One exit-top check. `taken` starts the mantle.
    pub fn exit_top(&self, taken: bool) {
        if taken {
            bump(&self.counts.exit_top);
        }
    }

    /// One bottom exit check with result `code`.
    pub fn exit_bottom(&self, code: u8) {
        if code == OK {
            bump(&self.counts.exit_bottom);
        }
    }

    /// The game's exit landed on steep rock and was refused.
    pub fn exit_refused(&self) {
        bump(&self.counts.exit_refused);
    }

    /// This mod found an exit the game did not.
    pub fn exit_taken(&self) {
        bump(&self.counts.exit_taken);
    }

    /// Every count now.
    pub fn snapshot(&self) -> Snapshot {
        self.counts.snapshot()
    }

    /// The latest values now.
    pub fn lasts(&self) -> LastValues {
        let bits = |value: &AtomicU32| value.load(Ordering::Relaxed);
        let float = |value: &AtomicU32| f32::from_bits(bits(value));
        LastValues {
            code: bits(&self.lasts.code),
            entity: bits(&self.lasts.entity),
            hits: bits(&self.lasts.hits),
            spread: float(&self.lasts.spread),
            ny: float(&self.lasts.ny),
            gap: float(&self.lasts.gap),
            width: float(&self.lasts.width),
            height: float(&self.lasts.height),
        }
    }
}

impl Default for Diag {
    fn default() -> Diag {
        Diag::new()
    }
}

impl Snapshot {
    /// The counts that move only while a climb starts, runs or ends. Anything else is left out, so
    /// a count added later stays quiet until someone decides otherwise.
    fn climbing(&self) -> Snapshot {
        Snapshot {
            top_ok: self.top_ok,
            low_ok: self.low_ok,
            move_tried: self.move_tried,
            stay_tried: self.stay_tried,
            lost_calls: self.lost_calls,
            steered: self.steered,
            stop: self.stop,
            exit_top: self.exit_top,
            exit_bottom: self.exit_bottom,
            ..Snapshot::default()
        }
    }

    /// The counts that also move when a surface is near but no climb runs. What runs every frame
    /// anywhere is zeroed: the start check's calls, and the open-air probes of a held stick. A
    /// player crossing a field must not fill the log.
    fn near(&self) -> Snapshot {
        Snapshot {
            start_calls: 0,
            top_calls: 0,
            low_calls: 0,
            low_no_input: 0,
            low_probe: 0,
            probe_miss: 0,
            entry_tried: 0,
            reject_hits: 0,
            start_off: 0,
            ..*self
        }
    }
}

/// A probe code as text: `0x1f`, or `none` before the first probe.
fn code_text(code: u32) -> String {
    if code == NO_CODE {
        "none".to_owned()
    } else {
        format!("{code:#04x}")
    }
}

/// The `climb diag:` log line.
pub fn line(s: &Snapshot, l: &LastValues) -> String {
    let entity = if s.probe_ok == 0 {
        "none".to_owned()
    } else {
        l.entity.to_string()
    };
    format!(
        "climb diag: probe ok {}, 0x15 {}, 0x1e {}, 0x1f {}, other {} (last code {}, last entity \
         {entity}) | natural entry {}/{}, move {}/{}, stay {}/{} (refused dir {}, hits {}, bumpy \
         {}, walkable {}, overhang {}, far {}, blocked {}; last hits {} spread {:.2} ny {:.2} gap \
         {:+.2}) | found walk {} jump {} sprint {} glide {} off {} | start {}/{} (top {}/{}, low \
         {}/{}: no input {}, angle {}, cone {}, cooldown {}, probe {}, other {}) | lost {}/{} \
         (voxel {}, grace {}, no probe {}) | steer {} | stop {} | exit top {} bottom {} (refused \
         {}, taken {}) | capsule {:.2} x {:.2}",
        s.probe_ok,
        s.probe_plane,
        s.probe_clear,
        s.probe_miss,
        s.probe_other,
        code_text(l.code),
        s.entry_ok,
        s.entry_tried,
        s.move_ok,
        s.move_tried,
        s.stay_ok,
        s.stay_tried,
        s.reject_direction,
        s.reject_hits,
        s.reject_bumpy,
        s.reject_walkable,
        s.reject_overhang,
        s.reject_far,
        s.reject_blocked,
        l.hits,
        l.spread,
        l.ny,
        l.gap,
        s.by_walk,
        s.by_jump,
        s.by_sprint,
        s.by_glide,
        s.start_off,
        s.top_ok.saturating_add(s.low_ok),
        s.start_calls,
        s.top_ok,
        s.top_calls,
        s.low_ok,
        s.low_calls,
        s.low_no_input,
        s.low_angle,
        s.low_cone,
        s.low_cooldown,
        s.low_probe,
        s.low_other,
        s.lost_drops,
        s.lost_calls,
        s.lost_held,
        s.lost_grace,
        s.lost_no_probe,
        s.steered,
        s.stop,
        s.exit_top,
        s.exit_bottom,
        s.exit_refused,
        s.exit_taken,
        l.width,
        l.height,
    )
}

/// What the log thread remembers from tick to tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pace {
    /// The counts at the last tick.
    last: Snapshot,
    /// The counts at the last line.
    logged: Snapshot,
    /// Ticks since the last line.
    quiet: u32,
}

impl Pace {
    /// No counts yet, and free to write a line at once.
    pub fn new() -> Pace {
        Pace {
            last: Snapshot::default(),
            logged: Snapshot::default(),
            quiet: NEAR_TICKS,
        }
    }
}

impl Default for Pace {
    fn default() -> Pace {
        Pace::new()
    }
}

/// The line for this tick, if one is due. A climb writes one on every tick it moved a count. A
/// surface that was near writes one at most every `NEAR_TICKS`, so a start that failed leaves a
/// trace and a walk along a cliff does not fill the log.
pub fn changed_line(pace: &mut Pace, now: Snapshot, lasts: &LastValues) -> Option<String> {
    let climbing = now.climbing() != pace.last.climbing();
    pace.last = now;
    pace.quiet = pace.quiet.saturating_add(1);
    let near = now.near() != pace.logged.near() && pace.quiet >= NEAR_TICKS;
    (climbing || near).then(|| {
        pace.logged = now;
        pace.quiet = 0;
        line(&now, lasts)
    })
}

/// The diagnostics of this process.
pub static DIAG: Diag = Diag::new();

/// One tick of the log thread.
fn tick(pace: &mut Pace) {
    if let Some(text) = changed_line(pace, DIAG.snapshot(), &DIAG.lasts()) {
        log::info!("{text}");
    }
}

/// Start the log thread. It runs until the process ends.
pub fn spawn() -> Result<(), String> {
    std::thread::Builder::new()
        .name("climb-diag".to_owned())
        .spawn(|| {
            let mut pace = Pace::new();
            loop {
                std::thread::sleep(LOG_INTERVAL);
                guard(Feature::Diag, || (), || tick(&mut pace));
            }
        })
        .map(drop)
        .map_err(|error| format!("cannot start the diagnostics thread: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::Trigger;
    use crate::geom::Vec3;
    use crate::surface::Fit;

    /// A scan that passed, with these numbers.
    fn passed(hits: u8, spread: f32, ny: f32) -> Scan {
        Scan {
            hits,
            fit: Some(Fit {
                normal: Vec3::new(0.0, ny, -1.0),
                point: Vec3::default(),
                hits,
                spread,
                entity: 0,
            }),
            verdict: Ok(Vec3::default()),
            steady: true,
        }
    }

    /// A scan refused for `why`.
    fn refused(why: Reject) -> Scan {
        Scan {
            hits: 1,
            fit: None,
            verdict: Err(why),
            steady: false,
        }
    }

    /// Before any activity the line says so plainly.
    #[test]
    fn the_empty_line_reads_none() {
        let diag = Diag::new();
        assert_eq!(
            line(&diag.snapshot(), &diag.lasts()),
            "climb diag: probe ok 0, 0x15 0, 0x1e 0, 0x1f 0, other 0 (last code none, last \
             entity none) | natural entry 0/0, move 0/0, stay 0/0 (refused dir 0, hits 0, bumpy \
             0, walkable 0, overhang 0, far 0, blocked 0; last hits 0 spread 0.00 ny 0.00 gap \
             +0.00) | found walk 0 jump 0 sprint 0 glide 0 off 0 | start 0/0 (top 0/0, low 0/0: \
             no input 0, angle 0, cone 0, cooldown 0, probe 0, other 0) | lost 0/0 (voxel 0, \
             grace 0, no probe 0) | steer 0 | stop 0 | exit top 0 bottom 0 (refused 0, taken 0) \
             | capsule 0.00 x 0.00"
        );
    }

    /// Each hook call lands in its place in the line. A swapped argument would misreport which
    /// step failed, which is the whole point of the line.
    #[test]
    fn every_count_lands_in_its_place() {
        let diag = Diag::new();
        for code in [
            OK, PLANE, PLANE, CLEAR, CLEAR, CLEAR, MISS, MISS, MISS, MISS, 9,
        ] {
            diag.probe(code, 4242);
        }
        diag.natural(Mode::Entry, &passed(4, 0.12, 0.25));
        diag.natural(Mode::Entry, &refused(Reject::Walkable));
        diag.natural(Mode::Move, &refused(Reject::Bumpy));
        diag.natural(Mode::Stay, &refused(Reject::Hits));
        diag.natural(Mode::Stay, &refused(Reject::Overhang));
        diag.natural(Mode::Stay, &refused(Reject::TooFar));
        diag.natural(Mode::Stay, &refused(Reject::TooFar));
        diag.natural(Mode::Stay, &refused(Reject::Direction));
        diag.natural(Mode::Entry, &refused(Reject::Blocked));
        diag.found_by(Trigger::Walk);
        diag.found_by(Trigger::Jump);
        diag.found_by(Trigger::Jump);
        diag.found_by(Trigger::Sprint);
        diag.found_by(Trigger::Glide);
        diag.start_is_off();
        diag.start();
        diag.top_entry(OK);
        diag.top_entry(MISS);
        diag.low_entry(OK, Some(OK));
        diag.low_entry(NOT_NOW, None);
        diag.low_entry(NOT_NOW, Some(OK));
        diag.low_entry(CLEAR, None);
        diag.low_entry(MISS, Some(MISS));
        diag.low_entry(NOT_NOW, Some(MISS));
        diag.cone_passed();
        diag.held(true);
        diag.held(false);
        diag.graced();
        diag.dropped(true);
        diag.dropped(false);
        diag.steered(-0.04);
        diag.capsule(0.8, 1.9);
        diag.stop();
        diag.exit_top(true);
        diag.exit_top(false);
        diag.exit_bottom(OK);
        diag.exit_bottom(NOT_NOW);
        diag.exit_refused();
        diag.exit_taken();
        assert_eq!(
            line(&diag.snapshot(), &diag.lasts()),
            "climb diag: probe ok 1, 0x15 2, 0x1e 3, 0x1f 4, other 1 (last code 0x09, last \
             entity 4242) | natural entry 1/3, move 0/1, stay 0/5 (refused dir 1, hits 1, bumpy \
             1, walkable 1, overhang 1, far 2, blocked 1; last hits 1 spread 0.12 ny 0.25 gap \
             -0.04) | found walk 1 jump 2 sprint 1 glide 1 off 1 | start 2/1 (top 1/2, low 1/6: \
             no input 1, angle 1, cone 1, cooldown 1, probe 1, other 1) | lost 2/5 (voxel 1, \
             grace 1, no probe 1) | steer 1 | stop 1 | exit top 1 bottom 1 (refused 1, taken 1) \
             | capsule 0.80 x 1.90"
        );
    }

    /// A failed probe has no entity to keep.
    #[test]
    fn only_a_passed_probe_keeps_its_entity() {
        let diag = Diag::new();
        diag.probe(OK, 7);
        diag.probe(MISS, 0);
        assert_eq!(diag.lasts().entity, 7);
        assert_eq!(diag.lasts().code, u32::from(MISS));
    }

    /// Walking across a field with the stick held logs nothing.
    #[test]
    fn open_ground_stays_quiet() {
        let diag = Diag::new();
        let mut pace = Pace::new();
        for _ in 0..1000 {
            diag.start();
            diag.top_entry(MISS);
            diag.low_entry(MISS, Some(MISS));
            diag.probe(MISS, 0);
            diag.natural(Mode::Entry, &refused(Reject::Hits));
            diag.start_is_off();
        }
        assert_eq!(
            changed_line(&mut pace, diag.snapshot(), &diag.lasts()),
            None
        );
    }

    /// One second of walking beside rock: the probes find a surface, and no climb starts.
    fn walk_beside_rock(diag: &Diag) {
        for _ in 0..60 {
            diag.start();
            diag.low_entry(NOT_NOW, Some(OK));
            diag.probe(MISS, 0);
            diag.natural(Mode::Entry, &refused(Reject::Walkable));
            diag.natural(Mode::Entry, &refused(Reject::TooFar));
            diag.natural(Mode::Entry, &refused(Reject::Bumpy));
            diag.natural(Mode::Entry, &refused(Reject::Overhang));
            diag.natural(Mode::Entry, &passed(4, 0.1, 0.1));
            diag.found_by(Trigger::Walk);
        }
    }

    /// Walking beside a slope or a wall for ten minutes is no climb, and must not write a line a
    /// second. A few lines stay, so a start that failed still leaves a trace in the log.
    #[test]
    fn walking_beside_rock_does_not_fill_the_log() {
        let diag = Diag::new();
        let mut pace = Pace::new();
        let mut lines = 0;
        for _ in 0..600 {
            walk_beside_rock(&diag);
            if changed_line(&mut pace, diag.snapshot(), &diag.lasts()).is_some() {
                lines += 1;
            }
        }
        assert!((20..=21).contains(&lines), "{lines} lines in 600 seconds");
    }

    /// The first failed start shows at once, or a session with no climb at all leaves no line,
    /// which is how the first test of climbing lost its evidence.
    #[test]
    fn the_first_failed_start_logs_a_line() {
        let diag = Diag::new();
        let mut pace = Pace::new();
        walk_beside_rock(&diag);
        assert!(changed_line(&mut pace, diag.snapshot(), &diag.lasts()).is_some());
    }

    /// A failed start right after a climb waits its turn, and then still shows.
    #[test]
    fn a_failed_start_after_a_climb_waits_its_turn() {
        let diag = Diag::new();
        let mut pace = Pace::new();
        diag.steered(0.0);
        assert!(changed_line(&mut pace, diag.snapshot(), &diag.lasts()).is_some());
        walk_beside_rock(&diag);
        for _ in 1..NEAR_TICKS {
            assert_eq!(
                changed_line(&mut pace, diag.snapshot(), &diag.lasts()),
                None
            );
        }
        assert!(changed_line(&mut pace, diag.snapshot(), &diag.lasts()).is_some());
    }

    /// Standing still logs nothing either.
    #[test]
    fn no_change_logs_nothing() {
        let diag = Diag::new();
        let mut pace = Pace::new();
        diag.steered(0.0);
        assert!(changed_line(&mut pace, diag.snapshot(), &diag.lasts()).is_some());
        assert_eq!(
            changed_line(&mut pace, diag.snapshot(), &diag.lasts()),
            None
        );
    }

    /// Everything that means a climb is happening logs a line, on every tick.
    #[test]
    fn climb_activity_logs_a_line_every_tick() {
        let events: [fn(&Diag); 11] = [
            |diag| diag.dropped(true),
            |diag| diag.held(true),
            |diag| diag.graced(),
            |diag| diag.stop(),
            |diag| diag.exit_top(true),
            |diag| diag.exit_bottom(OK),
            |diag| diag.top_entry(OK),
            |diag| diag.low_entry(OK, Some(OK)),
            |diag| diag.steered(0.0),
            |diag| diag.natural(Mode::Move, &refused(Reject::Overhang)),
            |diag| diag.natural(Mode::Stay, &refused(Reject::Hits)),
        ];
        for event in events {
            let diag = Diag::new();
            let mut pace = Pace::new();
            for _ in 0..5 {
                event(&diag);
                let text = changed_line(&mut pace, diag.snapshot(), &diag.lasts());
                assert!(text.is_some());
            }
        }
    }

    /// The thread's tick reads the shared counts without trouble.
    #[test]
    fn a_tick_runs_on_the_shared_counts() {
        let mut pace = Pace::new();
        tick(&mut pace);
        assert_eq!(pace.last, DIAG.snapshot());
    }
}
