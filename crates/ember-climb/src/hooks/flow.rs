//! The real detours, called through their real signatures, with the game's functions and raycast
//! replaced by small fakes that behave like them.
//!
//! This tests the joints between the hooks: which mode each probe runs in, what the wall-lost check
//! leaves for the move check, and that a lattice is left alone. It runs as one test, because the
//! detours share process-wide state.

use super::*;
use crate::geom::UP;
use std::sync::atomic::AtomicU32;

/// The wall the fake raycast answers: a sheer plane at this x, facing -x.
const WALL_X: f32 = 1.0;
/// The entity climbing. Far from the entity ids the other tests use.
const PLAYER: usize = 77;
/// A frame at 60 fps.
const DT: f32 = 1.0 / 60.0;

/// Does the fake raycast hit anything?
static WALL_THERE: AtomicBool = AtomicBool::new(true);
/// Does the fake game probe pass, as on a lattice?
static LATTICE: AtomicBool = AtomicBool::new(false);
/// Rays cast so far.
static RAYS: AtomicU32 = AtomicU32::new(0);
/// Calls of the game's wall-lost check so far.
static LOST_CALLS: AtomicU32 = AtomicU32::new(0);
/// What the fake exit query answers: 0 is no exit, otherwise the floor's `ny` in hundredths.
static EXIT_NY: AtomicU32 = AtomicU32::new(0);

/// A raycast with the game's signature, against the wall.
///
/// # Safety
///
/// `out` is 0x70 writable bytes and `ray` is 0x20 readable bytes.
#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn plane_raycast(
    out: *mut u8,
    _world: *mut u8,
    ray: *const f32,
    _ignore: u64,
    _arg5: u32,
    _mask: u32,
    _flags: u8,
    _arg8: u64,
) -> *mut u8 {
    RAYS.fetch_add(1, Ordering::Relaxed);
    // SAFETY: the function's contract.
    let [fx, fy, fz, _, tx, ty, tz, _] = unsafe { ray.cast::<[f32; 8]>().read_unaligned() };
    let mut bytes = [0u8; 0x70];
    bytes[0] = MISS;
    if WALL_THERE.load(Ordering::Relaxed) && fx < WALL_X && tx >= WALL_X {
        let t = (WALL_X - fx) / (tx - fx);
        bytes[0] = OK;
        let fields = [
            WALL_X,
            fy + (ty - fy) * t,
            fz + (tz - fz) * t,
            -1.0,
            0.0,
            0.0,
        ];
        let (slots, _) = bytes[0x14..0x2c].as_chunks_mut::<4>();
        for (slot, field) in slots.iter_mut().zip(fields) {
            *slot = field.to_le_bytes();
        }
    }
    // SAFETY: the function's contract.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    out
}

/// The game's probe: on rock it fails, on a lattice it passes with a real id.
///
/// # Safety
///
/// `out` is a whole probe result.
unsafe extern "system" fn game_probe(
    out: *mut u8,
    _ctx: *mut u8,
    _pos: *const f32,
    _dir: *const f32,
    _len: f32,
) -> *mut u8 {
    let bytes = if LATTICE.load(Ordering::Relaxed) {
        encode_result(OK, Vec3::default(), Vec3::new(-1.0, 0.0, 0.0), 4242)
    } else {
        failed_result(MISS)
    };
    // SAFETY: the function's contract.
    unsafe { write_result(out, &bytes) };
    out
}

/// Call the hooked probe as the game does.
///
/// # Safety
///
/// `ctx` is a fake context.
unsafe fn call_probe(ctx: *mut u8, pos: Vec3, dir: Vec3, len: f32) -> [u8; RESULT_SIZE] {
    let mut out = [0u8; RESULT_SIZE];
    let pos = [pos.x, pos.y, pos.z];
    let dir = [dir.x, dir.y, dir.z];
    // SAFETY: all four buffers live through the call, and `out` holds a whole result.
    unsafe { probe_detour(out.as_mut_ptr(), ctx, pos.as_ptr(), dir.as_ptr(), len) };
    out
}

/// The game's wall-lost check, with the return patch: `FUN_14024b390` and `WALL_LOST_HOLD`.
///
/// # Safety
///
/// `ctx` is a fake context.
unsafe extern "system" fn game_wall_lost(ctx: *mut u8) -> u32 {
    LOST_CALLS.fetch_add(1, Ordering::Relaxed);
    // SAFETY: the function's contract.
    let (pos, normal) = unsafe { (Ctx(ctx).position(), read_vec3(Ctx(ctx).normal_at())) };
    // SAFETY: the function's contract.
    let out = unsafe { call_probe(ctx, pos, -normal, 0.0) };
    if out[0] != OK {
        return 0;
    }
    // SAFETY: `out` is a whole result.
    match unsafe { result_entity(out.as_ptr()) } {
        0 => HELD,
        id => id,
    }
}

/// The game's move check, cut down to its first probe.
///
/// # Safety
///
/// `velocity` and `pos` hold three floats, and `ctx` is a fake context.
unsafe extern "system" fn game_move_check(
    velocity: *mut f32,
    ctx: *mut u8,
    pos: *const f32,
    dt: f32,
) -> u64 {
    // SAFETY: the function's contract.
    let (velocity, pos, normal) = unsafe {
        (
            read_vec3(velocity),
            read_vec3(pos),
            read_vec3(Ctx(ctx).normal_at()),
        )
    };
    // SAFETY: the function's contract.
    let out = unsafe { call_probe(ctx, pos + velocity * dt, -normal, 0.0) };
    u64::from(out[0] == OK)
}

/// The game's exit query, with a scripted answer.
///
/// # Safety
///
/// `out` is a whole probe result.
unsafe extern "system" fn game_exit_query(
    out: *mut u8,
    _ctx: *mut u8,
    _pos: *const f32,
) -> *mut u8 {
    let ny = EXIT_NY.load(Ordering::Relaxed);
    let bytes = if ny == 0 {
        failed_result(MISS)
    } else {
        encode_result(OK, Vec3::default(), UP * (ny as f32 / 100.0), 0)
    };
    // SAFETY: the function's contract.
    unsafe { write_result(out, &bytes) };
    out
}

/// The game's bottom entry, with its raw 45 degree test.
///
/// # Safety
///
/// `out` is a whole result, `ctx` is a fake context and `input` a fake ActorInput.
unsafe extern "system" fn game_low_entry(
    out: *mut u8,
    ctx: *mut u8,
    input: *mut u8,
    _time: i64,
) -> *mut u8 {
    // SAFETY: the function's contract.
    let (pos, raw) = unsafe { (Ctx(ctx).position(), game::world_move_input(input)) };
    let bytes = match raw.normalized() {
        None => failed_result(NOT_NOW),
        Some(direction) => {
            // SAFETY: the function's contract.
            let probe = unsafe { call_probe(ctx, pos, direction, 0.75) };
            if probe[0] != OK {
                failed_result(MISS)
            } else if Vec3::new(1.0, 0.0, 0.0).dot(raw) >= 0.707 {
                probe
            } else {
                failed_result(NOT_NOW)
            }
        }
    };
    // SAFETY: the function's contract.
    unsafe { write_result(out, &bytes) };
    out
}

/// The game's stamina cost.
///
/// # Safety
///
/// None: it reads nothing.
unsafe extern "system" fn game_stamina(_data: *mut u8) -> f32 {
    8.0
}

/// One player's components, in memory that lasts as long as the test process.
struct Player {
    /// The Actor, large enough for the fields read.
    actor: &'static mut [u8],
    /// The Climb component.
    climb: &'static mut [u8],
    /// The position, in 32.32 fixed point.
    transform: &'static mut [i64; 3],
    /// The ActorInput component.
    input: &'static mut [u8],
    /// The context, as `FUN_140419570` builds it.
    ctx: &'static mut [usize; 8],
}

impl Player {
    /// A player on the ground in front of the wall, with the normal on the wall.
    fn new() -> Player {
        let actor = vec![0u8; 0xc10].leak();
        actor[0xbf8] = 1;
        let climb = vec![0u8; 0x98].leak();
        let transform = Box::leak(Box::new([0i64; 3]));
        let config = Box::leak(Box::new([0.8f32, 2.0]));
        let collider = vec![0u8; 0x54].leak();
        let input = vec![0u8; 0x1c8].leak();
        let ctx = Box::leak(Box::new([
            actor.as_mut_ptr() as usize,
            climb.as_mut_ptr() as usize,
            transform.as_ptr() as usize,
            config.as_ptr() as usize,
            collider.as_ptr() as usize,
            0,
            PLAYER,
            0x7000,
        ]));
        let mut player = Player {
            actor,
            climb,
            transform,
            input,
            ctx,
        };
        player.set_normal(Vec3::new(-1.0, 0.0, 0.0));
        player
    }

    /// The context pointer the game would pass.
    fn ctx(&mut self) -> *mut u8 {
        self.ctx.as_mut_ptr().cast()
    }

    /// Put the feet at this x.
    fn set_x(&mut self, x: f32) {
        self.transform[0] = (f64::from(x) * 4_294_967_296.0) as i64;
    }

    /// Store a climb normal.
    fn set_normal(&mut self, normal: Vec3) {
        let at = self.climb[4..16].as_mut_ptr().cast::<f32>();
        // SAFETY: 12 bytes inside the component.
        unsafe { write_vec3(at, normal) };
    }

    /// The stored climb normal.
    fn normal(&self) -> Vec3 {
        // SAFETY: 12 bytes inside the component.
        unsafe { read_vec3(self.climb[4..16].as_ptr().cast()) }
    }

    /// Push the stick along x, this far.
    fn set_stick(&mut self, x: f32) {
        self.input[0x198..0x19c].copy_from_slice(&x.to_le_bytes());
    }

    /// Push the stick along z too, which runs along the wall.
    fn set_stick_along(&mut self, z: f32) {
        self.input[0x1a0..0x1a4].copy_from_slice(&z.to_le_bytes());
    }

    /// Set whether the player is on the ground, gliding or sprinting.
    fn set_state(&mut self, grounded: bool, gliding: bool, sprinting: bool) {
        let state = u64::from(grounded) | u64::from(gliding) << 37;
        self.actor[0xbf8..0xc00].copy_from_slice(&state.to_le_bytes());
        self.input[0x1c4] = if sprinting { 4 } else { 3 };
    }

    /// Run the hooked bottom entry and give its result.
    fn low_entry(&mut self) -> [u8; RESULT_SIZE] {
        let mut out = [0xffu8; RESULT_SIZE];
        let (ctx, input) = (self.ctx(), self.input.as_mut_ptr());
        // SAFETY: all buffers live through the call and are as large as the game's.
        unsafe { low_entry_detour(out.as_mut_ptr(), ctx, input, 0) };
        out
    }

    /// Run the hooked wall-lost check.
    fn wall_lost(&mut self) -> u32 {
        let ctx = self.ctx();
        // SAFETY: the context lives through the call.
        unsafe { wall_lost_detour(ctx) }
    }

    /// Run the hooked move check on this velocity, and give back the velocity and the answer.
    fn move_check(&mut self, velocity: Vec3) -> (Vec3, u64) {
        let mut v = [velocity.x, velocity.y, velocity.z];
        let ctx = self.ctx();
        // SAFETY: the context lives through the call.
        let pos = unsafe { Ctx(ctx).position() };
        let pos = [pos.x, pos.y, pos.z];
        // SAFETY: all buffers live through the call.
        let passed = unsafe { move_check_detour(v.as_mut_ptr(), ctx, pos.as_ptr(), DT) };
        (Vec3::new(v[0], v[1], v[2]), passed)
    }

    /// Run the hooked exit query for a predicted position this far above the feet.
    fn exit_query(&mut self, rise: f32) -> [u8; RESULT_SIZE] {
        let mut out = [0u8; RESULT_SIZE];
        let ctx = self.ctx();
        // SAFETY: the context lives through the call.
        let pos = unsafe { Ctx(ctx).position() } + UP * rise;
        let pos = [pos.x, pos.y, pos.z];
        // SAFETY: all buffers live through the call.
        unsafe { exit_query_detour(out.as_mut_ptr(), ctx, pos.as_ptr()) };
        out
    }
}

/// Point every slot at its fake, and turn the mod on.
fn install() {
    let probe: ProbeFn = game_probe;
    let lost: WallLostFn = game_wall_lost;
    let moved: MoveCheckFn = game_move_check;
    let query: ExitQueryFn = game_exit_query;
    let entry: LowEntryFn = game_low_entry;
    let stamina: StaminaFn = game_stamina;
    PROBE.set(probe as usize);
    WALL_LOST.set(lost as usize);
    MOVE_CHECK.set(moved as usize);
    EXIT_QUERY.set(query as usize);
    LOW_ENTRY.set(entry as usize);
    STAMINA.set(stamina as usize);
    let raycast: RaycastFn = plane_raycast;
    let settings = Settings {
        grace_seconds: 0.05,
        stamina_multiplier: 2.0,
        ..Settings::default()
    };
    // SAFETY: `raycast` has the type `Runtime` needs.
    prepare(unsafe { Runtime::new(&settings, raycast as usize) });
    activate();
}

/// The floats of a probe result: position, then normal.
fn fields(result: &[u8; RESULT_SIZE]) -> (Vec3, Vec3) {
    let at = |offset: usize| {
        // SAFETY: `offset + 12` is inside the result.
        unsafe { read_vec3(result.as_ptr().add(offset).cast()) }
    };
    (at(0x08), at(0x14))
}

/// A climb on rock, and a climb on a lattice, through every detour.
#[test]
fn a_climb_runs_through_the_detours() {
    install();
    let mut player = Player::new();

    // The stick is half pressed at the wall. The game's raw test fails and the mod's passes. The
    // result it hands over puts the feet 0.55 m from the wall, at the same height.
    player.set_x(0.2);
    player.set_stick(0.5);
    let entry = player.low_entry();
    assert_eq!(entry[0], OK);
    let (stand, normal) = fields(&entry);
    assert!((stand.x - 0.45).abs() < 1e-4, "{stand:?}");
    assert!(stand.y.abs() < 1e-4, "{stand:?}");
    assert!((normal.x + 1.0).abs() < 1e-4, "{normal:?}");

    // Each start can be off. A glider is off by default, so the answer stays the game's.
    player.set_state(false, true, false);
    assert_eq!(player.low_entry()[0], MISS);
    player.set_state(true, false, true);
    assert_eq!(player.low_entry()[0], OK);
    player.set_state(true, false, false);

    // A stick well along the wall finds the surface, and both angle tests refuse it. The log must
    // call that an angle, not a failed probe, or it blames the wrong step.
    player.set_x(0.45);
    player.set_stick(0.5);
    player.set_stick_along(1.0);
    let angles = DIAG.snapshot().low_angle;
    assert_eq!(player.low_entry()[0], NOT_NOW);
    assert_eq!(DIAG.snapshot().low_angle, angles + 1);
    player.set_stick_along(0.0);

    // No stick, no probe, nothing to answer.
    player.set_stick(0.0);
    assert_eq!(player.low_entry()[0], NOT_NOW);

    // On the wall: a voxel surface (id 0) holds, by the stand-in value.
    player.set_x(0.45);
    assert_eq!(player.wall_lost(), HELD);

    // The hold's fit reaches the move check, which steers. Here the player is 0.15 m too far out,
    // so the velocity gets a pull toward the wall and keeps moving up.
    player.set_x(0.3);
    assert_eq!(player.wall_lost(), HELD);
    let (steered, passed) = player.move_check(UP * 1.95);
    assert_eq!(passed, 1);
    assert!(steered.x > 0.0, "{steered:?}");
    assert!(steered.y > 1.9, "{steered:?}");

    // The stored climb normal eases toward the surface, in the game's own component. This is what
    // turns the body with the rock, and what points the next frame's rays.
    let leaning = Vec3::new(-0.98, 0.2, 0.0).normalized().unwrap();
    player.set_normal(leaning);
    assert_eq!(player.wall_lost(), HELD);
    player.move_check(UP * 1.95);
    let eased = player.normal();
    assert!(eased.y < leaning.y, "{eased:?}");
    assert!(eased.y > 0.0, "one frame is a small step: {eased:?}");
    player.set_normal(Vec3::new(-1.0, 0.0, 0.0));

    // A move onto a surface that is gone is refused, and the game gets that answer.
    player.set_x(0.45);
    assert_eq!(player.wall_lost(), HELD);
    WALL_THERE.store(false, Ordering::Relaxed);
    let (_, passed) = player.move_check(UP * 1.95);
    assert_eq!(passed, 0);

    // The surface goes for a moment: the grace time holds, then the player drops.
    assert_eq!(player.wall_lost(), HELD, "the first miss is forgiven");
    assert_eq!(player.wall_lost(), HELD, "and the next, inside the limit");
    std::thread::sleep(std::time::Duration::from_millis(120));
    assert_eq!(player.wall_lost(), 0, "past the limit the player drops");
    WALL_THERE.store(true, Ordering::Relaxed);

    // Steep rock is no exit for a climber, and a real floor stays the game's exit.
    assert_eq!(player.wall_lost(), HELD);
    EXIT_NY.store(30, Ordering::Relaxed);
    assert_eq!(player.exit_query(0.03)[0], MISS);
    EXIT_NY.store(95, Ordering::Relaxed);
    assert_eq!(player.exit_query(0.03)[0], OK);
    EXIT_NY.store(0, Ordering::Relaxed);
    assert_eq!(player.exit_query(0.03)[0], MISS);

    // A lattice is the game's alone. No ray is cast, the velocity stands, the normal stands, and
    // the game's exit stands even on a steep floor.
    LATTICE.store(true, Ordering::Relaxed);
    player.set_x(0.3);
    player.set_normal(Vec3::new(-1.0, 0.0, 0.0));
    let rays = RAYS.load(Ordering::Relaxed);
    assert_eq!(player.wall_lost(), 4242);
    let (velocity, _) = player.move_check(UP * 1.95);
    assert_eq!(velocity, UP * 1.95);
    assert_eq!(player.normal(), Vec3::new(-1.0, 0.0, 0.0));
    EXIT_NY.store(30, Ordering::Relaxed);
    assert_eq!(player.exit_query(0.03)[0], OK);
    assert_eq!(RAYS.load(Ordering::Relaxed), rays, "a lattice costs no ray");
    EXIT_NY.store(0, Ordering::Relaxed);
    LATTICE.store(false, Ordering::Relaxed);

    // The game's function runs once per call, with or without the mod's work.
    let calls = LOST_CALLS.load(Ordering::Relaxed);
    player.wall_lost();
    assert_eq!(LOST_CALLS.load(Ordering::Relaxed), calls + 1);

    // The stamina cost is the game's times the setting.
    // SAFETY: the fake reads nothing.
    assert_eq!(unsafe { stamina_detour(std::ptr::null_mut()) }, 16.0);

    // With the mod off the detours only forward: rock is the game's "no" again.
    deactivate();
    player.set_stick(1.0);
    assert_eq!(player.low_entry()[0], MISS);
    assert_eq!(player.wall_lost(), 0);
    // SAFETY: the fake reads nothing.
    assert_eq!(unsafe { stamina_detour(std::ptr::null_mut()) }, 8.0);
}
