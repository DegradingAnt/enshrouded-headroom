//! The detours. Each runs the game's function exactly once, whatever happens in this mod.
//!
//! The order inside one climbing frame (`FUN_140418c50`) is what ties these together:
//! 1. the wall-lost check runs the probe in [`Mode::Stay`], and the fit is kept for this frame;
//! 2. the move check steers with that fit, then runs the probe in [`Mode::Move`], and its answer
//!    is kept for this frame;
//! 3. the exit query is corrected, using that fit and that answer.
//!
//! All of that happens in one call on one thread, so both live in thread-locals. State that
//! must last between ticks lives in the game's own Climb component: the climb normal and the grace
//! count. The client replays old ticks, and only component bytes come back, so nothing here may
//! carry a decision from one tick to the next any other way. These Rust hooks do not allocate or
//! log. Movement telemetry tries its atomic claim once and drops a busy record without waiting.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;

use emberkit::guard::{guard, FeatureId};

use crate::config::Settings;
use crate::corner::{FreshPatch, Request};
use crate::corner_call::{self, Answer, Call, Key, StayPatch};
use crate::diag::{BodySample, MoveSample, DIAG};
use crate::exit::{self, Exit};
use crate::game::{
    self, encode_result, failed_result, read_vec3, result_code, result_entity, result_hit,
    write_result, write_vec3, Actor, Ctx, GameCaster, RaycastFn, Trigger, MISS, NOT_NOW, OK,
    RESULT_SIZE,
};
use crate::geom::Vec3;
use crate::grace;
use crate::plan::{self, HELD};
use crate::sigs::{self, Sig};
use crate::surface::{
    cone_ok, scan, scan_move_from_hold, steer, Fit, Hit, Mode, MoveProbe, Scan, SteerInput, Tuning,
};
use crate::Feature;

/// The least rise of a predicted position that counts as moving up, in metres.
const MOVING_UP: f32 = 1e-4;
/// The game's stamina cost per metre (`0x1413b6808`), for the moment no trampoline is stored.
const GAME_STAMINA_COST: f32 = 8.0;

/// What the detours need from the settings, fixed at start-up.
pub struct Runtime {
    /// The surface limits.
    pub tuning: Tuning,
    /// The grace time, in game ticks.
    grace_ticks: u8,
    /// Whether native whole-body and path clearance has been proven for natural top-outs.
    natural_mantles: bool,
    /// Does the log get the per-second lines?
    pub diagnostics: bool,
    /// Does walking start a climb?
    pub walk: bool,
    /// Does jumping or falling?
    pub jump: bool,
    /// Does sprinting?
    pub sprint: bool,
    /// Does gliding?
    pub glide: bool,
    /// The stamina multiplier.
    pub stamina: f32,
    /// The game's raycast.
    pub raycast: RaycastFn,
}

impl Runtime {
    /// The runtime for these settings, with the raycast found at `raycast`.
    ///
    /// # Safety
    ///
    /// `raycast` is the address of the game's `FUN_1409fff60`.
    pub unsafe fn new(settings: &Settings, raycast: usize) -> Runtime {
        Runtime {
            tuning: settings.tuning(),
            // Native Climb padding has no proven initialization contract. A setting must not
            // enable its reads. Initialized simulation fixtures may test the pure grace model.
            grace_ticks: 0,
            // The native helper checks only the head sphere. Whole-body placement and the
            // animation's path are unproven, so natural-wall automatic mantles stay unavailable.
            natural_mantles: false,
            diagnostics: settings.diagnostics,
            walk: settings.walk,
            jump: settings.jump,
            sprint: settings.sprint,
            glide: settings.glide,
            stamina: settings.stamina_multiplier,
            // SAFETY: the caller's contract: the address of a function with this type.
            raycast: unsafe { std::mem::transmute::<usize, RaycastFn>(raycast) },
        }
    }

    /// Is this way of starting a climb switched on?
    pub fn allows(&self, trigger: Trigger) -> bool {
        match trigger {
            Trigger::Walk => self.walk,
            Trigger::Jump => self.jump,
            Trigger::Sprint => self.sprint,
            Trigger::Glide => self.glide,
        }
    }
}

/// The settings for the detours. Set once, before any hook is enabled.
static RUNTIME: OnceLock<Runtime> = OnceLock::new();
/// Do the detours do anything? Set only after every hook and write went in, so a start-up that
/// fails half way leaves detours that only forward.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Store the settings for the detours. A second call is ignored: start-up runs once.
pub fn prepare(runtime: Runtime) {
    let _ = RUNTIME.set(runtime);
}

/// Let the detours act.
pub fn activate() {
    ACTIVE.store(true, Ordering::Release);
}

/// Make the detours forward only.
pub fn deactivate() {
    ACTIVE.store(false, Ordering::Release);
}

/// The settings, when the detours may act.
fn runtime() -> Option<&'static Runtime> {
    if ACTIVE.load(Ordering::Acquire) {
        RUNTIME.get()
    } else {
        None
    }
}

/// What the climb update already knows this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Hold {
    /// The climber.
    entity: u32,
    /// Is this a natural surface? A lattice leaves everything to the game.
    natural: bool,
    /// The fit at the player, if the rays found one.
    fit: Option<Fit>,
    /// Is the fit good enough to steer by?
    steady: bool,
    /// Same-update discovery hints keyed to the exact live context/world/feet.
    patch: Option<StayPatch>,
}

/// A natural surface found for the entry running on this thread.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Found {
    /// The normal, for the cone test.
    normal: Vec3,
    /// The probe result, to hand to the game.
    bytes: [u8; RESULT_SIZE],
}

thread_local! {
    /// The question the probe on this thread answers now. `None` leaves it entirely to the game.
    /// These cells are const-initialised and have no `Drop`, so a game thread pays no allocation
    /// and no lazy set-up.
    static MODE: Cell<Option<Mode>> = const { Cell::new(None) };
    /// The code of the last game probe on this thread since [`clear_nested`].
    static NESTED: Cell<Option<u8>> = const { Cell::new(None) };
    /// This frame's hold.
    static HOLD: Cell<Option<Hold>> = const { Cell::new(None) };
    /// Did the move check find no way on this frame?
    static STUCK: Cell<bool> = const { Cell::new(false) };
    /// The trigger of the entry running now, if a natural surface may answer it.
    static TRIGGER: Cell<Option<Trigger>> = const { Cell::new(None) };
    /// The natural surface the entry running now found.
    static FOUND: Cell<Option<Found>> = const { Cell::new(None) };
}

/// Set `cell` on this thread. Thread-local access can fail only while a thread exits.
fn set<T: Copy + 'static>(cell: &'static std::thread::LocalKey<Cell<T>>, value: T) {
    let _ = cell.try_with(|slot| slot.set(value));
}

/// Read `cell` on this thread, or `fallback` while the thread exits.
fn get<T: Copy + 'static>(cell: &'static std::thread::LocalKey<Cell<T>>, fallback: T) -> T {
    cell.try_with(Cell::get).unwrap_or(fallback)
}

/// Forget this thread's last game probe.
fn clear_nested() {
    set(&NESTED, None);
}

/// Run `original` exactly once, then `after` on its result, and return what `after` gives.
///
/// - The feature is off: only `original` runs.
/// - `after` panics: the guard catches it and switches the feature off, and the one result of
///   `original` is returned. `original` does not run again.
fn post_hook<T: Copy>(
    feature: impl FeatureId,
    original: impl Fn() -> T,
    after: impl FnOnce(T) -> T,
) -> T {
    let ran = Cell::new(None);
    guard(
        feature,
        || ran.get().unwrap_or_else(&original),
        || {
            let value = original();
            ran.set(Some(value));
            after(value)
        },
    )
}

/// A trampoline slot. Start-up stores MinHook's trampoline here before the hook is enabled, and
/// the detour reads it.
pub struct Slot(AtomicUsize);

impl Slot {
    /// An empty slot.
    pub(crate) const fn new() -> Slot {
        Slot(AtomicUsize::new(0))
    }

    /// Store a trampoline.
    pub fn set(&self, trampoline: usize) {
        self.0.store(trampoline, Ordering::Release);
    }

    /// The trampoline, or 0 before start-up stored it.
    pub(crate) fn get(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }
}

/// The trampoline of the wall probe.
static PROBE: Slot = Slot::new();
/// The trampoline of the wall-lost check.
static WALL_LOST: Slot = Slot::new();
/// The trampoline of the move check.
static MOVE_CHECK: Slot = Slot::new();
/// The trampoline of the exit query.
static EXIT_QUERY: Slot = Slot::new();
/// The trampoline of the start check.
static START: Slot = Slot::new();
/// The trampoline of the bottom entry.
static LOW_ENTRY: Slot = Slot::new();
/// The trampoline of the top entry.
static TOP_ENTRY: Slot = Slot::new();
/// The trampoline of stop climbing.
static STOP: Slot = Slot::new();
/// The trampoline of the exit-top check.
static EXIT_TOP: Slot = Slot::new();
/// The trampoline of the bottom exit check.
static EXIT_LOW: Slot = Slot::new();
/// The trampoline of the stamina cost.
static STAMINA: Slot = Slot::new();

/// `FUN_14024abc0`, the wall probe. It writes a result to `out` and returns `out`.
type ProbeFn = unsafe extern "system" fn(*mut u8, *mut u8, *const f32, *const f32, f32) -> *mut u8;
/// `FUN_14024b390`, the wall-lost check: an id, [`HELD`], or 0 for "wall lost".
type WallLostFn = unsafe extern "system" fn(*mut u8) -> u32;
/// `FUN_140417d90`, the move check. It may rewrite the velocity it is given. `al` says pass.
type MoveCheckFn = unsafe extern "system" fn(*mut f32, *mut u8, *const f32, f32) -> u64;
/// `FUN_14024a6f0`, the exit query. It writes a result to `out` and returns `out`.
type ExitQueryFn = unsafe extern "system" fn(*mut u8, *mut u8, *const f32) -> *mut u8;
/// `FUN_140419570`, the start check: one argument, the climb program's iterator.
type StartFn = unsafe extern "system" fn(*mut u8);
/// `FUN_14024c0d0`, the bottom entry. The last argument is the game time.
type LowEntryFn = unsafe extern "system" fn(*mut u8, *mut u8, *mut u8, i64) -> *mut u8;
/// `FUN_14024c320`, the top entry. The third and fifth arguments pass through.
type TopEntryFn = unsafe extern "system" fn(*mut u8, *mut u8, u64, *mut u8, u64) -> *mut u8;
/// `FUN_14024a090`, stop climbing.
type StopFn = unsafe extern "system" fn(*mut u8, i64);
/// `FUN_14024c9f0`, the exit-top check. `al` starts the mantle.
type ExitTopFn = unsafe extern "system" fn(*mut u8, *mut u8) -> u8;
/// `FUN_14024c8f0`, the bottom exit check. It writes a result to `out` and returns `out`.
type ExitLowFn = unsafe extern "system" fn(*mut u8, *mut u8, *mut u8) -> *mut u8;
/// `FUN_14024a360`, the stamina cost per metre.
type StaminaFn = unsafe extern "system" fn(*mut u8) -> f32;

/// Copy opaque identities from this live context; only equality uses its addresses.
///
/// # Safety
///
/// The native context is live for this invocation and its entity/world slots are initialized.
unsafe fn corner_key(ctx: Ctx) -> Key {
    // SAFETY: initialized fields of the same native context supplied by the caller.
    unsafe {
        Key {
            context: ctx.0 as usize,
            entity: ctx.entity(),
            world: ctx.world() as usize,
        }
    }
}

/// Ordinary surface policy over either the raw or the same-call counted caster.
fn natural_scan<C: crate::surface::Caster>(
    caster: &C,
    held: Option<MoveProbe>,
    pos: Vec3,
    dir: Vec3,
    mode: Mode,
    tuning: &Tuning,
    capsule: crate::surface::Capsule,
) -> Scan {
    match held {
        Some(probe) => scan_move_from_hold(caster, probe, tuning, capsule),
        None => scan(caster, pos, dir, mode, tuning, capsule),
    }
}

/// A natural surface for `mode` from `pos` along `dir`, if the game's own probe failed. The answer
/// goes to the game as if its probe had found it.
///
/// # Safety
///
/// `out` is the probe's result buffer, and `ctx`, `pos` and `dir` are the game's arguments to
/// the probe that just returned.
unsafe fn natural_probe(
    runtime: &Runtime,
    mode: Mode,
    out: *mut u8,
    ctx: Ctx,
    pos: *const f32,
    dir: *const f32,
) {
    // SAFETY: the function's contract: the game's arguments, alive for this call.
    let (pos, dir, entity, capsule, caster) = unsafe {
        (
            read_vec3(pos),
            read_vec3(dir),
            ctx.entity(),
            ctx.capsule(),
            GameCaster::new(runtime.raycast, ctx),
        )
    };
    let Some(caster) = caster else {
        return;
    };
    let held = (mode == Mode::Move)
        .then(|| get(&HOLD, None))
        .flatten()
        .filter(|hold| hold.natural && hold.steady && hold.entity == entity)
        .and_then(|hold| hold.fit);
    let held = held.map(|held| {
        // SAFETY: this same entity/update owns the initialized CurrentTransform.
        let current = unsafe { ctx.position() };
        MoveProbe {
            current,
            candidate: pos,
            direction: dir,
            held,
        }
    });
    // SAFETY: this context supplies its own initialized identity slots throughout the probe.
    let key = unsafe { corner_key(ctx) };
    let scan = if mode == Mode::Move && corner_call::has_call() {
        match corner_call::answer(&caster, key, pos) {
            Answer::Refused => return,
            Answer::Supported(supported) => Scan {
                samples: [None; crate::surface::RAY_COUNT],
                hits: supported.fit.hits,
                fit: Some(supported.fit),
                verdict: Ok(pos),
                steady: true,
            },
            Answer::Ordinary => {
                let Some(scan) = corner_call::ordinary(&caster, key, |counted| {
                    natural_scan(counted, held, pos, dir, mode, &runtime.tuning, capsule)
                }) else {
                    return;
                };
                scan
            }
        }
    } else {
        natural_scan(&caster, held, pos, dir, mode, &runtime.tuning, capsule)
    };
    DIAG.natural(mode, &scan);
    DIAG.capsule(capsule.radius * 2.0, capsule.height);
    if mode == Mode::Stay {
        remember_hold(
            entity,
            &scan,
            Some(StayPatch {
                key,
                patch: FreshPatch {
                    position: pos,
                    hits: scan.samples,
                },
            }),
        );
    }
    let (Ok(stand), Some(fit)) = (scan.verdict, scan.fit) else {
        return;
    };
    let bytes = encode_result(OK, stand, fit.normal, fit.entity);
    // SAFETY: the function's contract: `out` is the buffer the game gave the probe, and every
    // caller's buffer holds a whole result.
    unsafe { write_result(out, &bytes) };
    // The hooks around the probe read this: as far as the game knows, the probe passed.
    set(&NESTED, Some(OK));
    if mode == Mode::Entry {
        set(
            &FOUND,
            Some(Found {
                normal: fit.normal,
                bytes,
            }),
        );
        if let Some(trigger) = get(&TRIGGER, None) {
            DIAG.found_by(trigger);
        }
    }
}

/// Keep this frame's natural hold for the move check and the exit query.
fn remember_hold(entity: u32, scan: &Scan, patch: Option<StayPatch>) {
    if scan.verdict.is_ok() {
        let hold = Hold {
            entity,
            natural: true,
            fit: scan.fit,
            steady: scan.steady,
            patch,
        };
        set(&HOLD, Some(hold));
    }
}

/// Post-hook on the wall probe. The game's probe runs first and keeps every lattice exactly as it
/// was. Only when it fails does this look for a natural surface, and only for the questions that
/// matter: the top entry leaves [`MODE`] empty and stays the game's.
///
/// # Safety
///
/// The game's contract for `FUN_14024abc0`.
unsafe extern "system" fn probe_detour(
    out: *mut u8,
    ctx: *mut u8,
    pos: *const f32,
    dir: *const f32,
    len: f32,
) -> *mut u8 {
    let trampoline = PROBE.get();
    let original = || {
        if trampoline == 0 {
            return out;
        }
        // SAFETY: non-zero, so MinHook's trampoline for the probe, which has this type.
        let probe = unsafe { std::mem::transmute::<usize, ProbeFn>(trampoline) };
        // SAFETY: the game's own arguments, passed on unchanged.
        unsafe { probe(out, ctx, pos, dir, len) }
    };
    post_hook(Feature::Surface, original, |result| {
        if corner_call::blocked() || corner_call::querying() {
            return result;
        }
        if trampoline == 0 || result.is_null() {
            return result;
        }
        // SAFETY: the probe writes a whole result on every path and returns its buffer.
        let (code, id) = unsafe { (result_code(result), result_entity(result)) };
        DIAG.probe(code, id);
        set(&NESTED, Some(code));
        let ctx = Ctx(ctx);
        let (Some(mode), Some(runtime)) = (get(&MODE, None), runtime()) else {
            return result;
        };
        if code == OK {
            if mode == Mode::Move {
                corner_call::native_success();
            }
            if mode == Mode::Stay {
                // SAFETY: the game's own context, alive for this call.
                let entity = unsafe { ctx.entity() };
                let hold = Hold {
                    entity,
                    natural: false,
                    fit: None,
                    steady: false,
                    patch: None,
                };
                set(&HOLD, Some(hold));
            }
            return result;
        }
        if mode == Mode::Entry && get(&TRIGGER, None).is_none() {
            return result;
        }
        // SAFETY: the game's own arguments to the probe that just returned.
        unsafe { natural_probe(runtime, mode, result, ctx, pos, dir) };
        result
    })
}

/// Run the wall-lost check with the probe in [`Mode::Stay`].
///
/// # Safety
///
/// The game's contract for `FUN_14024b390`.
unsafe fn wall_lost_original(ctx: *mut u8) -> (u32, Option<u8>) {
    let trampoline = WALL_LOST.get();
    if trampoline == 0 {
        return (0, None);
    }
    // SAFETY: non-zero, so MinHook's trampoline for the check, which has this type.
    let lost = unsafe { std::mem::transmute::<usize, WallLostFn>(trampoline) };
    clear_nested();
    set(&HOLD, None);
    set(&STUCK, false);
    set(&MODE, Some(Mode::Stay));
    // SAFETY: the game's own argument, passed on unchanged.
    let id = unsafe { lost(ctx) };
    set(&MODE, None);
    (id, get(&NESTED, None))
}

/// What the wall-lost check's answer means for a natural climb. `state` is the grace count of this
/// tick and `limit` the ticks that may be forgiven. The answer and the next count come back.
fn after_wall_lost(id: u32, probed: bool, state: u8, limit: u8, entity: u32) -> (u32, u8) {
    if id != 0 {
        let natural = get(&HOLD, None).is_some_and(|hold| hold.natural);
        DIAG.held(id == HELD);
        return (id, grace::held(natural));
    }
    if probed {
        let (next, keep) = grace::failed(state, limit);
        if keep {
            DIAG.graced();
            let hold = Hold {
                entity,
                natural: true,
                fit: None,
                steady: false,
                patch: None,
            };
            set(&HOLD, Some(hold));
            return (HELD, next);
        }
    }
    DIAG.dropped(probed);
    (0, grace::NOT_NATURAL)
}

/// Read persistent grace only when a proven backend has enabled it. Zero must not touch padding.
fn grace_state(limit: u8, read: impl FnOnce() -> u8) -> Option<u8> {
    (limit != 0).then(read)
}

/// Hook on the wall-lost check. The patched return already turns a voxel hit into [`HELD`]. This
/// has an initialized-state grace model for inert fixtures. Native runtime gates that model off.
///
/// # Safety
///
/// The game's contract for `FUN_14024b390`.
unsafe extern "system" fn wall_lost_detour(ctx: *mut u8) -> u32 {
    // SAFETY: the game's own argument.
    let original = || unsafe { wall_lost_original(ctx) };
    post_hook(Feature::Surface, original, |(id, nested)| {
        let Some(runtime) = runtime() else {
            return (id, nested);
        };
        let ctx = Ctx(ctx);
        // SAFETY: the game's own context, alive for this call. The check just read this Climb.
        let entity = unsafe { ctx.entity() };
        let limit = runtime.grace_ticks;
        let state = grace_state(limit, || {
            // SAFETY: native runtime construction always sets limit=0, so this read is disabled.
            // Inert hook-flow fixtures explicitly supply initialized Climb bytes and a positive
            // limit. A future native backend must prove initialization before enabling it.
            unsafe { ctx.grace_at().read() }
        });
        let (id, next) = after_wall_lost(id, nested.is_some(), state.unwrap_or(0), limit, entity);
        if state.is_some_and(|state| next != state) {
            // SAFETY: only the initialized fixture/backend above enables a persistent write.
            // The native caller's program also owns Climb for writing.
            unsafe { ctx.grace_at().write(next) };
        }
        (id, nested)
    })
    .0
}

/// Run the bottom entry with the probe in [`Mode::Entry`].
///
/// # Safety
///
/// The game's contract for `FUN_14024c0d0`.
unsafe fn low_entry_original(
    out: *mut u8,
    ctx: *mut u8,
    input: *mut u8,
    time: i64,
) -> (*mut u8, Option<u8>) {
    let trampoline = LOW_ENTRY.get();
    if trampoline == 0 {
        return (out, None);
    }
    // SAFETY: non-zero, so MinHook's trampoline for the entry, which has this type.
    let entry = unsafe { std::mem::transmute::<usize, LowEntryFn>(trampoline) };
    clear_nested();
    set(&MODE, Some(Mode::Entry));
    // SAFETY: the game's own arguments, passed on unchanged.
    let result = unsafe { entry(out, ctx, input, time) };
    set(&MODE, None);
    (result, get(&NESTED, None))
}

/// Which trigger the player's state gives, if natural surfaces may answer it. Off settings count.
///
/// # Safety
///
/// `ctx` and `input` are the game's arguments to the bottom entry.
unsafe fn entry_trigger(ctx: Ctx, input: *const u8) -> Option<Trigger> {
    let runtime = runtime()?;
    // SAFETY: the function's contract.
    let actor = unsafe { ctx.actor() }?;
    // SAFETY: the function's contract.
    let trigger = unsafe { Trigger::of(actor.grounded(), actor.gliding(), game::sprinting(input)) };
    if runtime.allows(trigger) {
        Some(trigger)
    } else {
        DIAG.start_is_off();
        None
    }
}

/// Hook on the bottom entry. Before it, this decides whether a natural surface may answer. After
/// it, this redoes the angle test for a natural surface, because the game's own test depends on
/// how far the stick is pressed and on how far the wall leans.
///
/// # Safety
///
/// The game's contract for `FUN_14024c0d0`.
unsafe extern "system" fn low_entry_detour(
    out: *mut u8,
    ctx: *mut u8,
    input: *mut u8,
    time: i64,
) -> *mut u8 {
    // SAFETY: the game's own arguments.
    let original = || unsafe { low_entry_original(out, ctx, input, time) };
    let ran = Cell::new(None);
    guard(
        Feature::Entry,
        || ran.get().unwrap_or_else(&original).0,
        || {
            // SAFETY: the game's own arguments.
            set(&TRIGGER, unsafe { entry_trigger(Ctx(ctx), input) });
            let (result, nested) = original();
            ran.set(Some((result, nested)));
            // Both go before anything that could panic, so they end with this call on every path
            // out, and a later entry that the guard runs alone finds them empty.
            let found = get(&FOUND, None);
            set(&FOUND, None);
            set(&TRIGGER, None);
            if result.is_null() {
                return result;
            }
            // SAFETY: the entry writes a whole result on every path and returns its buffer.
            let mut code = unsafe { result_code(result) };
            if let (NOT_NOW, Some(found)) = (code, found) {
                // SAFETY: the game's own input.
                let wanted = unsafe { game::world_move_input(input) };
                if cone_ok(found.normal, wanted) {
                    // SAFETY: `result` is the entry's result buffer.
                    unsafe { write_result(result, &found.bytes) };
                    code = OK;
                    DIAG.cone_passed();
                }
            }
            DIAG.low_entry(code, nested);
            result
        },
    )
}

/// Show the log thread how this climber's body lies. It only reads, for the log.
///
/// # Safety
///
/// `ctx` is the game's context to the move check.
unsafe fn note_body(ctx: Ctx) {
    if !runtime().is_some_and(|runtime| runtime.diagnostics) {
        return;
    }
    // SAFETY: the function's contract.
    let Some(actor) = (unsafe { ctx.actor() }) else {
        return;
    };
    // SAFETY: the function's contract: the game's own components, alive for this call.
    let sample = unsafe {
        let (speed, mode) = actor.rotation();
        BodySample {
            entity: ctx.entity(),
            speed,
            mode,
            normal: read_vec3(ctx.normal_at()),
            orientation: ctx.orientation(),
        }
    };
    DIAG.body(&sample);
}

/// Carry one stack-local observation through one native move-check invocation.
///
/// # Safety
///
/// The velocity pointer contains three native floats and remains valid through the original.
/// The before closure preserves the existing native argument and steering contracts.
unsafe fn observed_move(
    feature: impl FeatureId,
    velocity: *const f32,
    before: impl FnOnce() -> Option<MoveSample>,
    original: impl Fn() -> u64,
    publish: impl FnOnce(MoveSample),
) -> u64 {
    let ran = Cell::new(None);
    guard(
        feature,
        || ran.get().unwrap_or_else(&original),
        || {
            let sample = before();
            let value = original();
            // Save the complete native RAX before any observation or publication can fail.
            ran.set(Some(value));
            if let Some(mut sample) = sample {
                // SAFETY: the same native velocity buffer remains alive after the synchronous
                // original. This reads only the three floats the original may have rewritten.
                sample.native_velocity = unsafe { read_vec3(velocity) };
                sample.native_al = value as u8;
                publish(sample);
            }
            value
        },
    )
}

/// One frame of steering, for a natural surface only.
///
/// # Safety
///
/// `velocity`, `ctx` and `pos` are the game's arguments to the move check.
unsafe fn steer_frame(
    velocity: *mut f32,
    ctx: Ctx,
    pos: *const f32,
    dt: f32,
) -> Option<MoveSample> {
    let runtime = runtime()?;
    // SAFETY: the function's contract.
    let entity = unsafe { ctx.entity() };
    let hold = get(&HOLD, None).filter(|hold| hold.natural && hold.entity == entity)?;
    let normal_at = {
        // SAFETY: the function's contract.
        unsafe { ctx.normal_at() }
    };
    // SAFETY: the function's contract. The Velocity pointer comes from the game's accessor, and
    // the climb program has Velocity and Climb in its write list, so this thread owns both.
    let (input, capsule) = unsafe {
        (
            SteerInput {
                velocity: read_vec3(velocity),
                normal: read_vec3(normal_at),
                fit: hold.fit.filter(|_| hold.steady),
                pos: read_vec3(pos),
                dt,
                contact: ctx.in_contact(),
            },
            ctx.capsule(),
        )
    };
    // These values are already needed by steering. Disabled diagnostics makes no sample and
    // adds no native reads. The physical fit normal is distinct from the eased body direction.
    let mut sample = runtime.diagnostics.then(|| MoveSample {
        entity,
        pos: input.pos,
        fit: input.fit.map(|fit| (fit.point, fit.normal)),
        raw_velocity: input.velocity,
        steered_velocity: input.velocity,
        native_velocity: input.velocity,
        dt: input.dt,
        contact: input.contact,
        native_al: 0,
    });
    let Some(out) = steer(&input, &runtime.tuning, capsule) else {
        return sample;
    };
    // SAFETY: as above. Steering gives finite numbers for finite input and checks the input.
    unsafe {
        write_vec3(velocity, out.velocity);
        write_vec3(normal_at, out.normal);
    }
    if let Some(sample) = sample.as_mut() {
        sample.steered_velocity = out.velocity;
    }
    DIAG.steered(out.error);
    sample
}

/// The baseline TLS fields a nested native move must not overwrite in its owner.
struct NestedMove {
    /// Previous mode, including an outer probe question.
    mode: Option<Mode>,
    /// Previous same-update hold.
    hold: Option<Hold>,
    /// Previous probe answer.
    nested: Option<u8>,
    /// Previous native move result.
    stuck: bool,
}
impl NestedMove {
    /// Suspend baseline custom state while the nested call forwards natively.
    fn save() -> Self {
        let previous = Self {
            mode: get(&MODE, None),
            hold: get(&HOLD, None),
            nested: get(&NESTED, None),
            stuck: get(&STUCK, false),
        };
        set(&MODE, None);
        set(&HOLD, None);
        set(&NESTED, None);
        set(&STUCK, false);
        previous
    }
}
impl Drop for NestedMove {
    fn drop(&mut self) {
        set(&MODE, self.mode);
        set(&HOLD, self.hold);
        set(&NESTED, self.nested);
        set(&STUCK, self.stuck);
    }
}
/// Forward a native move while protecting any in-flight outer custom owner.
/// This also runs in the disabled-feature fallback, before entering a native callback.
fn native_forward(original: impl FnOnce() -> u64) -> u64 {
    if corner_call::inside_move() {
        return corner_call::with_move(None, || {
            let _restore = NestedMove::save();
            original()
        });
    }
    original()
}

/// Restore a prior probe mode after the synchronous native move returns or Rust unwinds.
struct MoveMode(Option<Mode>);
impl Drop for MoveMode {
    fn drop(&mut self) {
        set(&MODE, self.0);
    }
}

/// Prepare owned corner state only for matching current wall/actor/collider data.
///
/// # Safety
///
/// The native move owns the initialized context and three-float argument buffers for this call.
unsafe fn corner_before(velocity: *const f32, ctx: Ctx, pos: *const f32, dt: f32) -> Option<Call> {
    let runtime = runtime()?;
    // SAFETY: same live native move and the documented initialized ordinary-wall fields.
    if !unsafe { ctx.ordinary_wall_motion() } {
        return None;
    }
    // SAFETY: this context and every initialized component remain live for this native move.
    let (key, current, input_position, velocity, normal, capsule) = unsafe {
        (
            corner_key(ctx),
            ctx.position(),
            read_vec3(pos),
            read_vec3(velocity),
            read_vec3(ctx.normal_at()),
            ctx.capsule(),
        )
    };
    let hold = get(&HOLD, None).filter(|hold| hold.natural && hold.entity == key.entity)?;
    let patch = hold.patch.filter(|patch| patch.key == key)?.patch;
    if current != input_position {
        return None;
    }
    Call::new(
        key,
        Request {
            position: current,
            velocity,
            normal,
            dt,
        },
        patch,
        hold.fit.filter(|_| hold.steady),
        runtime.tuning,
        capsule,
    )
}

/// Commit only the facing supported at the actual final native candidate and unchanged context.
///
/// # Safety
///
/// The same native context and velocity buffer remain live after its original move check returns.
unsafe fn finish_corner(ctx: Ctx, velocity: *const f32, native: u64) {
    if !corner_call::has_call() || runtime().is_none() {
        return;
    }
    // SAFETY: initialized native state/context slots still belong to this synchronous move.
    if !unsafe { ctx.ordinary_wall_motion() } {
        return;
    }
    // SAFETY: unchanged owner; the original may rewrite the live three-float velocity buffer.
    let (key, position, velocity) =
        unsafe { (corner_key(ctx), ctx.position(), read_vec3(velocity)) };
    if let Some(facing) = corner_call::finish(key, position, velocity, native) {
        // SAFETY: the native climbing program owns Climb for this call; finish proved finite
        // facing from fresh actual-Q support after native AL success. No transform is written.
        unsafe { write_vec3(ctx.normal_at(), facing) };
    }
}

/// Run the move check with the probe in [`Mode::Move`].
///
/// # Safety
///
/// The game's contract for `FUN_140417d90`.
unsafe fn move_check_original(velocity: *mut f32, ctx: *mut u8, pos: *const f32, dt: f32) -> u64 {
    let trampoline = MOVE_CHECK.get();
    if trampoline == 0 {
        return 1;
    }
    // SAFETY: non-zero, so MinHook's trampoline for the check, which has this type.
    let check = unsafe { std::mem::transmute::<usize, MoveCheckFn>(trampoline) };
    let _mode = MoveMode(get(&MODE, None));
    set(&MODE, Some(Mode::Move));
    // SAFETY: the game's own arguments, passed on unchanged.
    let passed = unsafe { check(velocity, ctx, pos, dt) };
    // `al` is the answer. The upper bits are whatever the check left in `rax`.
    set(&STUCK, passed as u8 == 0);
    passed
}

/// Pre-hook on the move check. It steers first, because the check then tests the steered velocity
/// and may rewrite it.
///
/// # Safety
///
/// The game's contract for `FUN_140417d90`.
unsafe extern "system" fn move_check_detour(
    velocity: *mut f32,
    ctx: *mut u8,
    pos: *const f32,
    dt: f32,
) -> u64 {
    let completed = Cell::new(None);
    let original = || {
        if let Some(value) = completed.get() {
            return value;
        }
        // SAFETY: the native caller's unchanged arguments, forwarded once.
        let value = unsafe { move_check_original(velocity, ctx, pos, dt) };
        completed.set(Some(value));
        value
    };
    guard(
        Feature::Steer,
        || native_forward(original),
        || {
            if corner_call::inside_move() {
                return native_forward(original);
            }
            // SAFETY: the complete native move owns these initialized arguments throughout the call.
            let call = unsafe { corner_before(velocity, Ctx(ctx), pos, dt) };
            corner_call::with_move(call, || {
                // SAFETY: original native velocity/context remain live through one move check.
                let value = unsafe {
                    observed_move(
                        Feature::Steer,
                        velocity,
                        || {
                            // SAFETY: the game's own context; diagnostic failure cannot stop steering.
                            guard(Feature::Diag, || (), || note_body(Ctx(ctx)));
                            let mut sample = steer_frame(velocity, Ctx(ctx), pos, dt);
                            if corner_call::has_call() {
                                if let Some(caster) = GameCaster::new(runtime()?.raycast, Ctx(ctx))
                                {
                                    let direction = -read_vec3(Ctx(ctx).normal_at());
                                    let steered = read_vec3(velocity);
                                    if let Some(requested) =
                                        corner_call::prepare(&caster, direction, steered)
                                    {
                                        write_vec3(velocity, requested);
                                        if let Some(sample) = sample.as_mut() {
                                            sample.steered_velocity = requested;
                                        }
                                    }
                                }
                            }
                            sample
                        },
                        original,
                        |sample| {
                            guard(Feature::Diag, || (), || DIAG.movement(&sample));
                        },
                    )
                };
                // completed already holds the entire native RAX. A post failure cannot forward twice.
                // SAFETY: this synchronous native context remains the same owner after the original.
                unsafe { finish_corner(Ctx(ctx), velocity, value) };
                value
            })
        },
    )
}

/// The hit a fit stands for, to hand to the exit decision.
fn patch_of(fit: Fit) -> Hit {
    Hit {
        point: fit.point,
        normal: fit.normal,
        entity: fit.entity,
    }
}

/// Correct the game's exit answer for a natural surface.
///
/// # Safety
///
/// `result` is the exit query's result, and `ctx` and `pos` are its arguments.
unsafe fn correct_exit(result: *mut u8, ctx: Ctx, pos: *const f32) {
    let Some(runtime) = runtime() else {
        return;
    };
    // SAFETY: the caller supplies the native result and borrowed context for this call.
    unsafe { correct_exit_for(runtime, result, ctx, pos) };
}

/// Apply the runtime's exit admission. Initialized inert fixtures may exercise the proposed model;
/// native startup must refuse natural automatic mantles until body/path clearance is proven.
///
/// # Safety
///
/// `result`, `ctx` and `pos` satisfy the same native-call contract as `correct_exit`.
unsafe fn correct_exit_for(runtime: &Runtime, result: *mut u8, ctx: Ctx, pos: *const f32) {
    // SAFETY: the function's contract.
    let entity = unsafe { ctx.entity() };
    let Some(hold) = get(&HOLD, None).filter(|hold| hold.natural && hold.entity == entity) else {
        return;
    };
    if !runtime.natural_mantles {
        // SAFETY: native query initializes the status byte; a successful result may be rewritten
        // in its caller-provided result buffer. Native failures are left byte-for-byte alone.
        if unsafe { result_code(result) } == OK {
            DIAG.exit_refused();
            // SAFETY: the caller supplied a complete writable result, as documented above.
            unsafe { write_result(result, &failed_result(MISS)) };
        }
        return;
    }
    // SAFETY: the function's contract.
    let (code, pos, current, normal, caster) = unsafe {
        (
            result_code(result),
            read_vec3(pos),
            ctx.position(),
            read_vec3(ctx.normal_at()),
            GameCaster::new(runtime.raycast, ctx),
        )
    };
    let Some(caster) = caster else {
        return;
    };
    let query = exit::Query {
        pos,
        normal,
        // The game made `pos` from its velocity before the move check, so this is true for a
        // player the check stopped. The stuck path in `exit::decide` depends on that.
        moving_up: pos.y > current.y + MOVING_UP,
        // SAFETY: the function's contract: the query writes a whole result on every path.
        floor: (code == OK)
            .then(|| unsafe { result_hit(result) })
            .flatten(),
        patch: hold.fit.map(patch_of),
        walkable_ny: runtime.tuning.walkable_ny,
        face: runtime.tuning.face,
        stuck: get(&STUCK, false),
    };
    let bytes = match exit::decide(&caster, &query) {
        Exit::Keep => return,
        Exit::Refuse => {
            DIAG.exit_refused();
            failed_result(MISS)
        }
        Exit::Take(hit) => {
            DIAG.exit_taken();
            encode_result(OK, hit.point, hit.normal, hit.entity)
        }
    };
    // SAFETY: the function's contract: `result` is the buffer the game gave the query.
    unsafe { write_result(result, &bytes) };
}

/// Post-hook on the exit query. A lattice keeps the game's answer.
///
/// # Safety
///
/// The game's contract for `FUN_14024a6f0`.
unsafe extern "system" fn exit_query_detour(
    out: *mut u8,
    ctx: *mut u8,
    pos: *const f32,
) -> *mut u8 {
    let trampoline = EXIT_QUERY.get();
    let original = || {
        if trampoline == 0 {
            return out;
        }
        // SAFETY: non-zero, so MinHook's trampoline for the query, which has this type.
        let query = unsafe { std::mem::transmute::<usize, ExitQueryFn>(trampoline) };
        // SAFETY: the game's own arguments, passed on unchanged.
        unsafe { query(out, ctx, pos) }
    };
    post_hook(Feature::Exit, original, |result| {
        if !result.is_null() {
            // SAFETY: the game's own arguments to the query that just returned.
            unsafe { correct_exit(result, Ctx(ctx), pos) };
        }
        result
    })
}

/// The actor to give CanClimb for this start check, if the glider option asks for it. A glider
/// runs as an action sequence, and its ability mask has no CanClimb, so the check stops there.
///
/// # Safety
///
/// `iterator` is the game's argument to the start check.
unsafe fn glider_actor(iterator: *const u8) -> Option<Actor> {
    if !runtime()?.glide {
        return None;
    }
    // SAFETY: the function's contract.
    let actor = unsafe { Actor::of_iterator(iterator) }?;
    // SAFETY: the actor is the game's, and the climb program holds it for this call.
    (unsafe { actor.gliding() } && unsafe { actor.force_can_climb() }).then_some(actor)
}

/// Hook on the start check: CanClimb for a glider, for the length of this one call, and the
/// diagnostics count.
///
/// # Safety
///
/// The game's contract for `FUN_140419570`.
unsafe extern "system" fn start_detour(iterator: *mut u8) {
    let trampoline = START.get();
    let original = || {
        if trampoline == 0 {
            return;
        }
        // SAFETY: non-zero, so MinHook's trampoline for the check, which has this type.
        let start = unsafe { std::mem::transmute::<usize, StartFn>(trampoline) };
        // SAFETY: the game's own argument, passed on unchanged.
        unsafe { start(iterator) }
    };
    let ran = Cell::new(false);
    guard(
        Feature::Glide,
        || {
            if !ran.get() {
                original();
            }
        },
        || {
            // SAFETY: the game's own argument.
            let forced = unsafe { glider_actor(iterator) };
            ran.set(true);
            original();
            if let Some(actor) = forced {
                // SAFETY: the same actor, and the climb program still holds it.
                unsafe { actor.clear_can_climb() };
            }
            DIAG.start();
        },
    );
}

/// Post-hook on the top entry, for the diagnostics.
///
/// # Safety
///
/// The game's contract for `FUN_14024c320`.
unsafe extern "system" fn top_entry_detour(
    out: *mut u8,
    ctx: *mut u8,
    arg3: u64,
    input: *mut u8,
    arg5: u64,
) -> *mut u8 {
    let trampoline = TOP_ENTRY.get();
    let original = || {
        if trampoline == 0 {
            return out;
        }
        // SAFETY: non-zero, so MinHook's trampoline for the entry, which has this type.
        let entry = unsafe { std::mem::transmute::<usize, TopEntryFn>(trampoline) };
        // SAFETY: the game's own arguments, passed on unchanged.
        unsafe { entry(out, ctx, arg3, input, arg5) }
    };
    post_hook(Feature::Diag, original, |result| {
        if !result.is_null() {
            // SAFETY: the entry writes its code on every path and returns its buffer.
            DIAG.top_entry(unsafe { result_code(result) });
        }
        result
    })
}

/// Clear replayed grace at a committed native stop, then record the diagnostic.
///
/// # Safety
///
/// The game's contract for `FUN_14024a090`.
unsafe extern "system" fn stop_detour(ctx: *mut u8, time: i64) {
    let trampoline = STOP.get();
    let original = || {
        if trampoline == 0 {
            return;
        }
        // SAFETY: non-zero, so MinHook's trampoline for the call, which has this type.
        let stop = unsafe { std::mem::transmute::<usize, StopFn>(trampoline) };
        // SAFETY: the game's own arguments, passed on unchanged.
        unsafe { stop(ctx, time) }
    };
    post_hook(Feature::Surface, original, |()| {
        if runtime().is_some_and(|runtime| runtime.grace_ticks != 0) {
            // SAFETY: the native stop writes Climb in every path and leaves the context alive.
            // All callers already hold writable Climb in their ECS program. The W52 native
            // receipt verifies this on both client and server, including action-sequence exits.
            unsafe { Ctx(ctx).grace_at().write(grace::NOT_NATURAL) };
        }
        DIAG.stop();
    });
}

/// Post-hook on the exit-top check, for the diagnostics.
///
/// # Safety
///
/// The game's contract for `FUN_14024c9f0`.
unsafe extern "system" fn exit_top_detour(ctx: *mut u8, input: *mut u8) -> u8 {
    let trampoline = EXIT_TOP.get();
    let original = || {
        if trampoline == 0 {
            return 0;
        }
        // SAFETY: non-zero, so MinHook's trampoline for the check, which has this type.
        let exit = unsafe { std::mem::transmute::<usize, ExitTopFn>(trampoline) };
        // SAFETY: the game's own arguments, passed on unchanged.
        unsafe { exit(ctx, input) }
    };
    post_hook(Feature::Diag, original, |taken| {
        DIAG.exit_top(taken != 0);
        taken
    })
}

/// Post-hook on the bottom exit check, for the diagnostics.
///
/// # Safety
///
/// The game's contract for `FUN_14024c8f0`.
unsafe extern "system" fn exit_low_detour(out: *mut u8, ctx: *mut u8, input: *mut u8) -> *mut u8 {
    let trampoline = EXIT_LOW.get();
    let original = || {
        if trampoline == 0 {
            return out;
        }
        // SAFETY: non-zero, so MinHook's trampoline for the check, which has this type.
        let exit = unsafe { std::mem::transmute::<usize, ExitLowFn>(trampoline) };
        // SAFETY: the game's own arguments, passed on unchanged.
        unsafe { exit(out, ctx, input) }
    };
    post_hook(Feature::Diag, original, |result| {
        if !result.is_null() {
            // SAFETY: the check writes its code on every path and returns its buffer.
            DIAG.exit_bottom(unsafe { result_code(result) });
        }
        result
    })
}

/// Hook on the stamina cost per metre: the game's cost times the setting.
///
/// # Safety
///
/// The game's contract for `FUN_14024a360`.
unsafe extern "system" fn stamina_detour(data: *mut u8) -> f32 {
    let trampoline = STAMINA.get();
    let original = || {
        if trampoline == 0 {
            return GAME_STAMINA_COST;
        }
        // SAFETY: non-zero, so MinHook's trampoline for the function, which has this type.
        let cost = unsafe { std::mem::transmute::<usize, StaminaFn>(trampoline) };
        // SAFETY: the game's own argument, passed on unchanged.
        unsafe { cost(data) }
    };
    guard(Feature::Stamina, original, || {
        let multiplier = runtime().map_or(1.0, |runtime| runtime.stamina);
        original() * multiplier
    })
}

/// One hook to create.
#[derive(Clone, Copy)]
pub struct Site {
    /// The signature that finds it.
    pub sig: &'static Sig,
    /// Is the function behind the `call` in the match, rather than at the match itself?
    behind_call: bool,
    /// The detour.
    pub detour: usize,
    /// Where its trampoline is stored.
    pub slot: &'static Slot,
}

impl Site {
    /// A hook on the function that `sig` matches the start of.
    fn on(sig: &'static Sig, detour: usize, slot: &'static Slot) -> Site {
        Site {
            sig,
            behind_call: false,
            detour,
            slot,
        }
    }

    /// The address of the function to hook. `read` gives `len` bytes at an address, or `None`.
    pub fn target(
        &self,
        found: &sigs::Found,
        read: impl Fn(usize, usize) -> Option<Vec<u8>>,
    ) -> Result<usize, String> {
        let at = found.address(self.sig)?;
        if !self.behind_call {
            return Ok(at);
        }
        let call = at
            .checked_add(sigs::STAMINA_CALL_AT)
            .ok_or("the call address overflows")?;
        plan::follow_call(call, &sigs::STAMINA_PROLOGUE, read)
            .map_err(|why| format!("{}: {why}", self.sig.name))
    }
}

/// The hooks these settings need. Everything that is not needed stays out of the game.
pub fn wanted(settings: &Settings) -> Vec<Site> {
    let probe: ProbeFn = probe_detour;
    let wall_lost: WallLostFn = wall_lost_detour;
    let low_entry: LowEntryFn = low_entry_detour;
    let move_check: MoveCheckFn = move_check_detour;
    let exit_query: ExitQueryFn = exit_query_detour;
    let mut sites = vec![
        Site::on(&sigs::PROBE_FN, probe as usize, &PROBE),
        Site::on(&sigs::WALL_LOST_FN, wall_lost as usize, &WALL_LOST),
        Site::on(&sigs::LOW_ENTRY_FN, low_entry as usize, &LOW_ENTRY),
        Site::on(&sigs::MOVE_CHECK_FN, move_check as usize, &MOVE_CHECK),
        Site::on(&sigs::EXIT_QUERY_FN, exit_query as usize, &EXIT_QUERY),
    ];
    if settings.diagnostics || settings.glide {
        let start: StartFn = start_detour;
        sites.push(Site::on(&sigs::START_FN, start as usize, &START));
    }
    if settings.diagnostics {
        let top: TopEntryFn = top_entry_detour;
        let exit_top: ExitTopFn = exit_top_detour;
        let exit_low: ExitLowFn = exit_low_detour;
        sites.extend([
            Site::on(&sigs::TOP_ENTRY_FN, top as usize, &TOP_ENTRY),
            Site::on(&sigs::EXIT_TOP_FN, exit_top as usize, &EXIT_TOP),
            Site::on(&sigs::EXIT_LOW_FN, exit_low as usize, &EXIT_LOW),
        ]);
    }
    if settings.diagnostics {
        let stop: StopFn = stop_detour;
        sites.push(Site::on(&sigs::STOP_FN, stop as usize, &STOP));
    }
    if settings.stamina_multiplier != 1.0 {
        let stamina: StaminaFn = stamina_detour;
        sites.push(Site {
            behind_call: true,
            ..Site::on(&sigs::STAMINA_CALL, stamina as usize, &STAMINA)
        });
    }
    sites
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::UP;
    use std::sync::atomic::AtomicU32;

    /// A feature id for the guard tests, far from the mod's own, so a test that switches it off
    /// switches nothing else off.
    #[derive(Clone, Copy, Debug)]
    struct TestFeature(u32);

    impl FeatureId for TestFeature {
        fn index(self) -> u32 {
            self.0
        }
    }

    /// The result of the original comes back through `after`.
    #[test]
    fn post_hook_runs_both_once() {
        let calls = AtomicU32::new(0);
        let result = post_hook(
            TestFeature(20),
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                5
            },
            |value| value + 1,
        );
        assert_eq!(result, 6);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    /// A panic after the original must not run the original a second time: the game function has
    /// already done its work. Its one result is what the game gets.
    #[test]
    fn a_panic_after_the_original_keeps_its_result() {
        let calls = AtomicU32::new(0);
        let result = post_hook(
            TestFeature(21),
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                5
            },
            |_| panic!("the detour failed"),
        );
        assert_eq!(result, 5);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    /// A feature that is switched off still runs the game's function.
    #[test]
    fn a_feature_that_is_off_still_runs_the_original() {
        let calls = AtomicU32::new(0);
        post_hook(TestFeature(22), || 1, |_| panic!("switch it off"));
        let result = post_hook(
            TestFeature(22),
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                7
            },
            |_| panic!("must not run"),
        );
        assert_eq!(result, 7);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    /// The observation preserves the requested velocity and the native fallback's different output.
    #[test]
    fn movement_observation_keeps_native_fallback_rewrite() {
        let mut velocity = [1.0_f32, 2.0, 3.0];
        let ptr = velocity.as_mut_ptr();
        let calls = Cell::new(0_u32);
        let published = Cell::new(None);
        let before = MoveSample {
            entity: 17,
            pos: Vec3::new(10.0, 5.0, -2.0),
            fit: Some((Vec3::new(10.55, 6.0, -2.0), Vec3::new(-1.0, 0.0, 0.0))),
            raw_velocity: Vec3::new(1.0, 2.0, 3.0),
            steered_velocity: Vec3::new(0.5, 2.0, 0.25),
            native_velocity: Vec3::new(1.0, 2.0, 3.0),
            dt: 0.05,
            contact: true,
            native_al: 0,
        };
        let native_result = 0xa5a5_0000_0000_0001;
        // SAFETY: this inert buffer contains three floats and outlives every supplied closure.
        let result = unsafe {
            observed_move(
                TestFeature(23),
                ptr,
                || {
                    // SAFETY: the same live inert buffer; no game function is called.
                    write_vec3(ptr, before.steered_velocity);
                    Some(before)
                },
                || {
                    calls.set(calls.get() + 1);
                    // SAFETY: the fake native fallback writes only the supplied inert buffer.
                    write_vec3(ptr, Vec3::new(0.0, 1.5, 0.0));
                    native_result
                },
                |sample| published.set(Some(sample)),
            )
        };
        assert_eq!(result, native_result);
        assert_eq!(calls.get(), 1);
        assert_eq!(
            published.get(),
            Some(MoveSample {
                native_velocity: Vec3::new(0.0, 1.5, 0.0),
                native_al: 1,
                ..before
            })
        );
        assert_eq!(velocity, [0.0, 1.5, 0.0]);
    }

    /// Failed post-capture must not call native movement twice or replace its original RAX.
    #[test]
    fn movement_observation_post_failure_keeps_one_original_call() {
        let mut velocity = [0.0_f32, 2.0, 0.0];
        let calls = Cell::new(0_u32);
        let published = Cell::new(false);
        let native_result = 0xb6b6_0000_0000_0000;
        // SAFETY: the inert three-float buffer outlives this call and no game code runs.
        let result = unsafe {
            observed_move(
                TestFeature(24),
                velocity.as_mut_ptr(),
                || Some(MoveSample::default()),
                || {
                    calls.set(calls.get() + 1);
                    native_result
                },
                |_| {
                    published.set(true);
                    panic!("post-capture failed");
                },
            )
        };
        assert!(published.get(), "the failing post-capture must have run");
        assert_eq!(result, native_result);
        assert_eq!(calls.get(), 1);
        assert_eq!(velocity, [0.0, 2.0, 0.0]);
    }

    /// Native padding is not proven initialized, so no player setting can enable its reads.
    #[test]
    fn native_grace_is_gated_off_and_zero_never_reads_state() {
        let settings = Settings {
            grace_seconds: 1.0,
            ..Settings::default()
        };
        // SAFETY: this test only checks the runtime and never calls the fake raycast address.
        let runtime = unsafe { Runtime::new(&settings, stamina_detour as *const () as usize) };
        assert_eq!(runtime.grace_ticks, 0);
        assert_eq!(
            grace_state(0, || panic!("disabled grace must not read padding")),
            None
        );
    }

    /// Ray-only floor and head checks cannot admit a natural-wall mantle animation.
    #[test]
    fn native_top_outs_wait_for_body_and_path_clearance() {
        // SAFETY: the fake address is not called by this construction test.
        let runtime =
            unsafe { Runtime::new(&Settings::default(), stamina_detour as *const () as usize) };
        assert!(!runtime.natural_mantles);
    }

    /// The default build has the five hooks that climb, plus the diagnostics, and no stamina hook.
    #[test]
    fn the_defaults_hook_what_they_need() {
        let names: Vec<&str> = wanted(&Settings::default())
            .iter()
            .map(|site| site.sig.name)
            .collect();
        assert_eq!(
            names,
            [
                "PROBE_FN",
                "WALL_LOST_FN",
                "LOW_ENTRY_FN",
                "MOVE_CHECK_FN",
                "EXIT_QUERY_FN",
                "START_FN",
                "TOP_ENTRY_FN",
                "EXIT_TOP_FN",
                "EXIT_LOW_FN",
                "STOP_FN"
            ]
        );
    }

    /// With diagnostics off only what climbing needs goes in.
    #[test]
    fn without_diagnostics_only_climbing_is_hooked() {
        let settings = Settings {
            diagnostics: false,
            ..Settings::default()
        };
        assert_eq!(wanted(&settings).len(), 5);
        assert!(!wanted(&settings)
            .iter()
            .any(|site| site.sig.name == "STOP_FN"));
    }

    /// The glider needs the start check even without diagnostics.
    #[test]
    fn the_glider_hooks_the_start_check() {
        let settings = Settings {
            diagnostics: false,
            glide: true,
            ..Settings::default()
        };
        let sites = wanted(&settings);
        assert!(sites.iter().any(|site| site.sig.name == "START_FN"));
    }

    /// A multiplier of 1 leaves the stamina code alone. Any other adds the hook.
    #[test]
    fn stamina_is_hooked_only_to_change_it() {
        let hooked = |stamina_multiplier| {
            wanted(&Settings {
                stamina_multiplier,
                ..Settings::default()
            })
            .iter()
            .any(|site| site.sig.name == "STAMINA_CALL")
        };
        assert!(!hooked(1.0));
        assert!(hooked(0.0));
        assert!(hooked(2.0));
    }

    /// Every hook goes to a place of its own: two hooks on one signature would fight.
    #[test]
    fn no_two_hooks_share_a_site() {
        let settings = Settings {
            glide: true,
            stamina_multiplier: 2.0,
            ..Settings::default()
        };
        let mut names: Vec<&str> = wanted(&settings).iter().map(|site| site.sig.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
        assert_eq!(count, 11);
    }

    /// A trigger that is off is refused, and each key controls its own trigger.
    #[test]
    fn the_settings_control_their_triggers() {
        let runtime = |settings: &Settings| {
            // SAFETY: the raycast is never called in this test.
            unsafe { Runtime::new(settings, stamina_detour as *const () as usize) }
        };
        let all = runtime(&Settings {
            glide: true,
            ..Settings::default()
        });
        for trigger in [
            Trigger::Walk,
            Trigger::Jump,
            Trigger::Sprint,
            Trigger::Glide,
        ] {
            assert!(all.allows(trigger), "{trigger:?}");
        }
        let walk_off = runtime(&Settings {
            walk: false,
            ..Settings::default()
        });
        assert!(!walk_off.allows(Trigger::Walk));
        assert!(walk_off.allows(Trigger::Jump));
        let defaults = runtime(&Settings::default());
        assert!(!defaults.allows(Trigger::Glide));
    }

    /// A hold on this thread, for the wall-lost tests.
    fn holding(natural: bool) -> Option<Hold> {
        Some(Hold {
            entity: 301,
            natural,
            fit: None,
            steady: false,
            patch: None,
        })
    }

    /// A lattice hold clears the count, and a natural hold starts it.
    #[test]
    fn a_natural_hold_earns_grace_and_a_lattice_does_not() {
        set(&HOLD, holding(true));
        assert_eq!(
            after_wall_lost(HELD, true, grace::NOT_NATURAL, 9, 301),
            (HELD, grace::HOLDING)
        );
        set(&HOLD, holding(false));
        assert_eq!(
            after_wall_lost(55, true, grace::HOLDING, 9, 301),
            (55, grace::NOT_NATURAL)
        );
    }

    /// A hold that came back ends the count, however far it had run.
    #[test]
    fn a_hold_after_a_seam_starts_the_count_again() {
        set(&HOLD, holding(true));
        assert_eq!(
            after_wall_lost(HELD, true, grace::HOLDING + 4, 9, 301),
            (HELD, grace::HOLDING)
        );
    }

    /// A forgiven tick answers as a hold, moves the count, and leaves a hold with no fit, which
    /// tells the steering and the exit that this is a natural climb.
    #[test]
    fn a_forgiven_tick_counts_and_holds() {
        set(&HOLD, None);
        assert_eq!(
            after_wall_lost(0, true, grace::HOLDING, 9, 301),
            (HELD, grace::HOLDING + 1)
        );
        let hold = get(&HOLD, None).unwrap();
        assert!(hold.natural && hold.fit.is_none() && !hold.steady);
        assert_eq!(hold.entity, 301);
    }

    /// Past the limit the player drops, and the count is clear for the next climb.
    #[test]
    fn past_the_limit_the_player_drops() {
        set(&HOLD, None);
        assert_eq!(
            after_wall_lost(0, true, grace::HOLDING + 9, 9, 301),
            (0, grace::NOT_NATURAL)
        );
        assert_eq!(get(&HOLD, None), None);
    }

    /// Without a nested probe nothing is forgiven: the climb normal was 0, which is the game's
    /// own failure. The count ends with the climb.
    #[test]
    fn no_probe_means_no_grace() {
        set(&HOLD, None);
        assert_eq!(
            after_wall_lost(0, false, grace::HOLDING, 9, 302),
            (0, grace::NOT_NATURAL)
        );
    }

    /// The hit that stands for a fit carries its centre and normal.
    #[test]
    fn a_patch_carries_the_fit() {
        let fit = Fit {
            normal: UP,
            point: Vec3::new(1.0, 2.0, 3.0),
            hits: 4,
            spread: 0.1,
            entity: 9,
        };
        let hit = patch_of(fit);
        assert_eq!(hit.point, fit.point);
        assert_eq!(hit.normal, fit.normal);
        assert_eq!(hit.entity, 9);
    }

    /// A feature kill-switch cannot let a reentrant native move consume the outer call scope.
    #[test]
    fn disabled_steering_fallback_still_suspends_nested_custom_work() {
        // Disable an isolated feature bit, leaving the actual shared Steer bit untouched.
        post_hook(
            TestFeature(25),
            || 0_u64,
            |_| panic!("disable fixture feature"),
        );
        let outer_hold = holding(true);
        set(&MODE, Some(Mode::Stay));
        set(&HOLD, outer_hold);
        set(&NESTED, Some(MISS));
        set(&STUCK, true);
        let key = Key {
            context: 1,
            entity: 2,
            world: 3,
        };
        let p = Vec3::default();
        let owned = Call::new(
            key,
            Request {
                position: p,
                velocity: Vec3::new(0.0, 0.0, 1.0),
                normal: Vec3::new(-1.0, 0.0, 0.0),
                dt: 1.0 / 60.0,
            },
            FreshPatch {
                position: p,
                hits: [None; 4],
            },
            None,
            Settings::default().tuning(),
            crate::surface::Capsule::FALLBACK,
        )
        .unwrap();
        let calls = Cell::new(0);
        let native = 0x1234_5678_9abc_de00_u64;
        corner_call::with_move(Some(owned), || {
            let result = guard(
                TestFeature(25),
                || {
                    native_forward(|| {
                        calls.set(calls.get() + 1);
                        assert!(
                            corner_call::blocked(),
                            "disabled nested fallback must suspend custom work"
                        );
                        assert!(!corner_call::has_call());
                        assert_eq!(get(&MODE, None), None);
                        assert_eq!(get(&HOLD, None), None);
                        set(&MODE, Some(Mode::Move));
                        set(&NESTED, Some(OK));
                        set(&STUCK, false);
                        native
                    })
                },
                || panic!("disabled guard body must not run"),
            );
            assert_eq!(result, native);
            assert_eq!(calls.get(), 1);
            assert!(!corner_call::blocked() && corner_call::has_call());
            assert_eq!(get(&MODE, None), Some(Mode::Stay));
            assert_eq!(get(&HOLD, None), outer_hold);
            assert_eq!(get(&NESTED, None), Some(MISS));
            assert!(get(&STUCK, false));
        });
        assert!(!corner_call::inside_move());
        set(&MODE, None);
        set(&HOLD, None);
        set(&NESTED, None);
        set(&STUCK, false);
    }
}

/// The joints between the detours.
#[cfg(test)]
mod flow;
