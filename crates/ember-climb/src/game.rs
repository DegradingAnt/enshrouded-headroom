//! The game's memory layouts and its raycast. Every offset here has a receipt in
//! docs/re-notes.md, "Free climbing".
//!
//! Decoding is pure, so it is tested on plain bytes. The raw reads and writes are small and sit in
//! one place. They only ever touch memory the game passed to the function being hooked.

use emberkit::scan::u32_at;

use crate::geom::Vec3;
use crate::surface::{Capsule, Caster, Hit};

/// The probe's code for success, and the entries'.
pub const OK: u8 = 0;
/// The probe's code: a corner normal is off the forward ray's plane.
pub const PLANE: u8 = 0x15;
/// The probe's code: the clearance ray hit. The bottom entry's: the re-grab cooldown.
pub const CLEAR: u8 = 0x1e;
/// The probe's code: a ray hit nothing. The bottom entry's: the probe failed.
pub const MISS: u8 = 0x1f;
/// The bottom entry's code for "no move input, the input is too far off the wall, or the
/// building spark is out".
pub const NOT_NOW: u8 = 6;
/// The size of a probe result: code, position, normal, entity.
pub const RESULT_SIZE: usize = 0x24;
/// Where a probe result holds its position.
const RESULT_POSITION: usize = 0x08;
/// Where a probe result holds its normal.
const RESULT_NORMAL: usize = 0x14;
/// Where a probe result holds its entity.
const RESULT_ENTITY: usize = 0x20;

/// Flags for every ray of this mod: 0x20 skips back faces, 0x40 skips ids 1 to 0x3ff, which are
/// players (`FUN_140a00070`).
const RAY_FLAGS: u8 = 0x60;
/// The size of a raycast result.
const RAY_OUT_SIZE: usize = 0x70;
/// Where a raycast result holds the id hit.
const RAY_OUT_ID: usize = 0x0c;
/// Where a raycast result holds the point hit.
const RAY_OUT_POINT: usize = 0x14;
/// Where a raycast result holds the normal.
const RAY_OUT_NORMAL: usize = 0x20;

/// The scale of `CurrentTransform`, whose position is 32.32 fixed point.
const FIXED_TO_METRES: f32 = 2.328_306_4e-10;
/// The climb context's Actor pointer.
const CTX_ACTOR: usize = 0x00;
/// The climb context's Climb pointer.
const CTX_CLIMB: usize = 0x08;
/// The climb context's CurrentTransform pointer.
const CTX_TRANSFORM: usize = 0x10;
/// The climb context's MovementColliderConfig pointer.
const CTX_COLLIDER_CONFIG: usize = 0x18;
/// The climb context's MovementCollider pointer.
const CTX_COLLIDER: usize = 0x20;
/// The climb context's entity id.
const CTX_ENTITY: usize = 0x30;
/// The climb context's ColliderSystem pointer.
const CTX_WORLD: usize = 0x38;
/// `Climb.climbNormal`.
const CLIMB_NORMAL: usize = 0x04;
/// Padding in Climb, between `climbState` and `climbNormal`. This mod keeps its grace count here.
/// +3 stays free.
const CLIMB_GRACE: usize = 0x02;
/// `CurrentTransform.transform.orientation`: x, y, z, w.
const TRANSFORM_ORIENTATION: usize = 0x18;
/// `MovementCollider.results.m_isSet`.
const COLLIDER_RESULTS_SET: usize = 0x50;
/// `MovementCollider.results.m_value.hasSpheroidCollision`.
const COLLIDER_CONTACT: usize = 0x4b;
/// `MovementColliderConfig.height`, after `width` at +0.
const CONFIG_HEIGHT: usize = 0x04;
/// Actor: bit 0 means an action sequence owns the actor's abilities and adds to its state.
const ACTOR_SEQUENCE: usize = 0x1b1;
/// Actor: the ability mask a sequence owns.
const ACTOR_ABILITIES: usize = 0xbc8;
/// Actor: state bits a sequence adds.
const ACTOR_STATE_ADD: usize = 0xbd0;
/// Actor: state bits a sequence removes.
const ACTOR_STATE_REMOVE: usize = 0xbd8;
/// Actor: the state bits.
const ACTOR_STATE: usize = 0xbf8;
/// Actor: the rotation speed that `actor_rotation` stored for this tick's branch.
const ACTOR_ROTATION_SPEED: usize = 0xee0;
/// Actor: the `actor.InterpolationMode` stored with that speed.
const ACTOR_ROTATION_MODE: usize = 0xee4;
/// `AbilityFlag` 16, CanClimb, in the ability mask.
const CAN_CLIMB: u32 = 1 << 16;
/// `StateFlag` 0, Grounded.
const STATE_GROUNDED: u8 = 0;
/// `StateFlag` 37, HangGliding.
const STATE_GLIDING: u8 = 37;
/// `ActorInput.desiredWorldMoveInput`.
const INPUT_WORLD_MOVE: usize = 0x198;
/// `ActorInput.locomotionSpeed`.
const INPUT_SPEED: usize = 0x1c4;
/// `LocomotionSpeedSelector` 4, Sprint.
const SPEED_SPRINT: u8 = 4;
/// Where the climb program's iterator holds the Actor pointer: `param_1[8]`.
const ITERATOR_ACTOR: usize = 0x40;

/// The game's raycast `FUN_1409fff60(out, world, ray, ignore, 0, mask, flags, 0)`.
///
/// `out` is 0x70 bytes and `ray` is two float4s, origin then end. Both are copied with unaligned
/// moves, and `ignore` is an entity id passed in `r9`.
pub type RaycastFn =
    unsafe extern "system" fn(*mut u8, *mut u8, *const f32, u64, u32, u32, u8, u64) -> *mut u8;

/// The float at `offset` in `bytes`.
fn f32_at(bytes: &[u8], offset: usize) -> Option<f32> {
    u32_at(bytes, offset).map(f32::from_bits)
}

/// The three floats at `offset` in `bytes`.
fn vec3_at(bytes: &[u8], offset: usize) -> Option<Vec3> {
    Some(Vec3::new(
        f32_at(bytes, offset)?,
        f32_at(bytes, offset.checked_add(4)?)?,
        f32_at(bytes, offset.checked_add(8)?)?,
    ))
}

/// The hit in a raycast result, or `None` for a miss or a result with broken numbers.
pub fn parse_hit(out: &[u8]) -> Option<Hit> {
    if out.first().copied() != Some(OK) {
        return None;
    }
    let point = vec3_at(out, RAY_OUT_POINT).filter(|point| point.is_finite())?;
    let normal = vec3_at(out, RAY_OUT_NORMAL).and_then(Vec3::normalized)?;
    Some(Hit {
        point,
        normal,
        entity: u32_at(out, RAY_OUT_ID)?,
    })
}

/// A probe result, byte for byte as the wall probe writes it.
pub fn encode_result(code: u8, pos: Vec3, normal: Vec3, entity: u32) -> [u8; RESULT_SIZE] {
    let mut bytes = [0u8; RESULT_SIZE];
    let fields = [
        pos.x.to_bits(),
        pos.y.to_bits(),
        pos.z.to_bits(),
        normal.x.to_bits(),
        normal.y.to_bits(),
        normal.z.to_bits(),
        entity,
    ];
    if let Some(first) = bytes.first_mut() {
        *first = code;
    }
    let (slots, _) = bytes.get_mut(8..).unwrap_or_default().as_chunks_mut::<4>();
    for (slot, field) in slots.iter_mut().zip(fields) {
        *slot = field.to_le_bytes();
    }
    bytes
}

/// A probe result that found nothing, byte for byte as the wall probe writes it.
pub fn failed_result(code: u8) -> [u8; RESULT_SIZE] {
    encode_result(code, Vec3::default(), Vec3::default(), 0)
}

/// `FUN_14021aaa0`: one state bit. While a sequence runs, it adds and removes bits.
pub fn state_bit(state: u64, added: u64, removed: u64, in_sequence: bool, bit: u8) -> bool {
    let state = if in_sequence {
        (state | added) & !removed
    } else {
        state
    };
    state >> (bit & 63) & 1 == 1
}

/// How a climb is being started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// Walking or running into a surface.
    Walk,
    /// Jumping or falling into one.
    Jump,
    /// Sprinting into one.
    Sprint,
    /// Gliding into one.
    Glide,
}

impl Trigger {
    /// The trigger for this actor state. Gliding comes first: a glider is off the ground too.
    pub fn of(grounded: bool, gliding: bool, sprinting: bool) -> Trigger {
        match (gliding, grounded, sprinting) {
            (true, _, _) => Trigger::Glide,
            (false, false, _) => Trigger::Jump,
            (false, true, true) => Trigger::Sprint,
            (false, true, false) => Trigger::Walk,
        }
    }
}

/// `count` bytes at `base + offset`.
///
/// # Safety
///
/// That range is readable.
unsafe fn bytes_at<const N: usize>(base: *const u8, offset: usize) -> [u8; N] {
    // SAFETY: the caller's contract. `[u8; N]` has no alignment or validity requirement.
    unsafe { base.add(offset).cast::<[u8; N]>().read_unaligned() }
}

/// The pointer at `base + offset`.
///
/// # Safety
///
/// Eight bytes there are readable.
unsafe fn pointer_at(base: *const u8, offset: usize) -> *mut u8 {
    // SAFETY: the caller's contract. A raw pointer can hold any bits.
    unsafe { base.add(offset).cast::<*mut u8>().read_unaligned() }
}

/// The floats at `at`, as a vector.
///
/// # Safety
///
/// 12 bytes at `at` are readable.
pub unsafe fn read_vec3(at: *const f32) -> Vec3 {
    // SAFETY: the caller's contract. Any bits are a valid f32.
    unsafe {
        Vec3::new(
            at.read_unaligned(),
            at.add(1).read_unaligned(),
            at.add(2).read_unaligned(),
        )
    }
}

/// Store a vector at `at`.
///
/// # Safety
///
/// 12 bytes at `at` are writable, and the game does not read them at the same time.
pub unsafe fn write_vec3(at: *mut f32, value: Vec3) {
    // SAFETY: the caller's contract.
    unsafe {
        at.write_unaligned(value.x);
        at.add(1).write_unaligned(value.y);
        at.add(2).write_unaligned(value.z);
    }
}

/// The code of the probe result at `result`.
///
/// # Safety
///
/// `result` is a probe result the game just wrote.
pub unsafe fn result_code(result: *const u8) -> u8 {
    // SAFETY: the caller's contract: its first byte is written on every path.
    unsafe { result.read() }
}

/// The entity of the probe result at `result`.
///
/// # Safety
///
/// `result` is a probe result the game just wrote, so all [`RESULT_SIZE`] bytes are readable.
pub unsafe fn result_entity(result: *const u8) -> u32 {
    // SAFETY: the caller's contract. The field is inside the result.
    let bytes = unsafe { bytes_at::<4>(result, RESULT_ENTITY) };
    u32::from_le_bytes(bytes)
}

/// The hit in the bytes of a probe result that passed, or `None` for broken numbers.
pub fn parse_result(bytes: &[u8; RESULT_SIZE]) -> Option<Hit> {
    let point = vec3_at(bytes, RESULT_POSITION).filter(|point| point.is_finite())?;
    let normal = vec3_at(bytes, RESULT_NORMAL).and_then(Vec3::normalized)?;
    Some(Hit {
        point,
        normal,
        entity: u32_at(bytes, RESULT_ENTITY)?,
    })
}

/// The hit in the probe result at `result`, or `None` for broken numbers.
///
/// # Safety
///
/// `result` is a successful native probe or exit result. The position, normal and entity fields
/// are initialized and readable. Padding does not have to be initialized.
pub unsafe fn result_hit(result: *const u8) -> Option<Hit> {
    // SAFETY: the successful native result initializes these fields. Its padding need not be
    // initialized, so reading a whole result as `[u8; RESULT_SIZE]` would be invalid.
    let (point, normal, entity) = unsafe {
        (
            read_vec3(result.add(RESULT_POSITION).cast()),
            read_vec3(result.add(RESULT_NORMAL).cast()),
            result_entity(result),
        )
    };
    Some(Hit {
        point: point.is_finite().then_some(point)?,
        normal: normal.normalized()?,
        entity,
    })
}

/// Replace the probe result at `result`.
///
/// # Safety
///
/// `result` is the caller's result buffer, at least [`RESULT_SIZE`] bytes long and writable.
pub unsafe fn write_result(result: *mut u8, bytes: &[u8; RESULT_SIZE]) {
    // SAFETY: the caller's contract. `bytes` is this function's own, so they cannot overlap.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), result, RESULT_SIZE) };
}

/// The context the climb functions pass around: an array of the entity's components.
#[derive(Clone, Copy, Debug)]
pub struct Ctx(pub *mut u8);

impl Ctx {
    /// The entity climbing.
    ///
    /// # Safety
    ///
    /// `self` is the game's climb context, alive for this call.
    pub unsafe fn entity(self) -> u32 {
        // SAFETY: the caller's contract: the context is 0x40 bytes.
        u32::from_le_bytes(unsafe { bytes_at::<4>(self.0, CTX_ENTITY) })
    }

    /// The world the rays are cast in.
    ///
    /// # Safety
    ///
    /// `self` is the game's climb context, alive for this call.
    pub unsafe fn world(self) -> *mut u8 {
        // SAFETY: the caller's contract: the context is 0x40 bytes.
        unsafe { pointer_at(self.0, CTX_WORLD) }
    }

    /// Where the climb normal is stored.
    ///
    /// # Safety
    ///
    /// `self` is the game's climb context, alive for this call. The game's own code reads this
    /// pointer without a null check.
    pub unsafe fn normal_at(self) -> *mut f32 {
        // SAFETY: the caller's contract: the context is 0x40 bytes, and the Climb component is
        // 0x98 bytes.
        unsafe { pointer_at(self.0, CTX_CLIMB).add(CLIMB_NORMAL).cast() }
    }

    /// Where the grace count is stored: a padding byte of the Climb component. The game copies and
    /// sends the component whole, so the byte goes with it. No store to it was found, and no field
    /// names it (docs/re-notes.md, "Replay and the grace byte").
    ///
    /// # Safety
    ///
    /// `self` is the game's climb context, alive for this call. The game's own code reads this
    /// pointer without a null check.
    pub unsafe fn grace_at(self) -> *mut u8 {
        // SAFETY: the caller's contract: the context is 0x40 bytes, and the Climb component is
        // 0x98 bytes.
        unsafe { pointer_at(self.0, CTX_CLIMB).add(CLIMB_GRACE) }
    }

    /// The body's orientation: x, y, z, w.
    ///
    /// # Safety
    ///
    /// `self` is the game's climb context, alive for this call. The game's own code reads this
    /// pointer without a null check.
    pub unsafe fn orientation(self) -> [f32; 4] {
        // SAFETY: the caller's contract: the CurrentTransform component is 0x38 bytes, and any
        // bits are a valid f32.
        unsafe {
            pointer_at(self.0, CTX_TRANSFORM)
                .add(TRANSFORM_ORIENTATION)
                .cast::<[f32; 4]>()
                .read_unaligned()
        }
    }

    /// Where the player's feet are.
    ///
    /// # Safety
    ///
    /// `self` is the game's climb context, alive for this call. The game's own code reads this
    /// pointer without a null check.
    pub unsafe fn position(self) -> Vec3 {
        // SAFETY: the caller's contract: the position is three i64s at the start of the
        // CurrentTransform component.
        let [x, y, z] = unsafe {
            pointer_at(self.0, CTX_TRANSFORM)
                .cast::<[i64; 3]>()
                .read_unaligned()
        };
        Vec3::new(
            x as f32 * FIXED_TO_METRES,
            y as f32 * FIXED_TO_METRES,
            z as f32 * FIXED_TO_METRES,
        )
    }

    /// The capsule. A missing component gives the fallback.
    ///
    /// # Safety
    ///
    /// `self` is the game's climb context, alive for this call.
    pub unsafe fn capsule(self) -> Capsule {
        // SAFETY: the caller's contract: the context is 0x40 bytes.
        let config = unsafe { pointer_at(self.0, CTX_COLLIDER_CONFIG) };
        if config.is_null() {
            return Capsule::FALLBACK;
        }
        // SAFETY: the component is 0x20 bytes, and the game reads both fields in FUN_14023c0e0.
        let (width, height) = unsafe {
            (
                f32::from_le_bytes(bytes_at::<4>(config, 0)),
                f32::from_le_bytes(bytes_at::<4>(config, CONFIG_HEIGHT)),
            )
        };
        Capsule::from_config(width, height)
    }

    /// Is the capsule touching something sideways? A missing component reads as no.
    ///
    /// # Safety
    ///
    /// `self` is the game's climb context, alive for this call.
    pub unsafe fn in_contact(self) -> bool {
        // SAFETY: the caller's contract: the context is 0x40 bytes.
        let collider = unsafe { pointer_at(self.0, CTX_COLLIDER) };
        if collider.is_null() {
            return false;
        }
        // SAFETY: the component is 0x54 bytes, and both fields are bools inside it.
        unsafe {
            collider.add(COLLIDER_RESULTS_SET).read() != 0
                && collider.add(COLLIDER_CONTACT).read() != 0
        }
    }

    /// The actor, or `None` if the context has none.
    ///
    /// # Safety
    ///
    /// `self` is the game's climb context, alive for this call.
    pub unsafe fn actor(self) -> Option<Actor> {
        // SAFETY: the caller's contract: the context is 0x40 bytes.
        let actor = unsafe { pointer_at(self.0, CTX_ACTOR) };
        (!actor.is_null()).then_some(Actor(actor))
    }
}

/// The `Actor` component.
#[derive(Clone, Copy, Debug)]
pub struct Actor(*mut u8);

impl Actor {
    /// The actor in the climb program's iterator, or `None` if it has none.
    ///
    /// # Safety
    ///
    /// `iterator` is the game's climb program iterator, alive for this call.
    pub unsafe fn of_iterator(iterator: *const u8) -> Option<Actor> {
        // SAFETY: the caller's contract: the iterator is 0x178 bytes.
        let actor = unsafe { pointer_at(iterator, ITERATOR_ACTOR) };
        (!actor.is_null()).then_some(Actor(actor))
    }

    /// `FUN_14021aaa0`: is this state bit set?
    ///
    /// # Safety
    ///
    /// `self` is the game's Actor, alive for this call.
    unsafe fn state(self, bit: u8) -> bool {
        // SAFETY: the caller's contract. The fields are inside the Actor, which runs past +0xc00.
        let (state, sequence) = unsafe {
            (
                u64::from_le_bytes(bytes_at::<8>(self.0, ACTOR_STATE)),
                self.0.add(ACTOR_SEQUENCE).read(),
            )
        };
        if sequence & 1 == 0 {
            return state_bit(state, 0, 0, false, bit);
        }
        // SAFETY: the native predicate reads these masks only during an active sequence.
        let (added, removed) = unsafe {
            (
                u64::from_le_bytes(bytes_at::<8>(self.0, ACTOR_STATE_ADD)),
                u64::from_le_bytes(bytes_at::<8>(self.0, ACTOR_STATE_REMOVE)),
            )
        };
        state_bit(state, added, removed, true, bit)
    }

    /// Is the player on the ground?
    ///
    /// # Safety
    ///
    /// `self` is the game's Actor, alive for this call.
    pub unsafe fn grounded(self) -> bool {
        // SAFETY: the caller's contract.
        unsafe { self.state(STATE_GROUNDED) }
    }

    /// Is the player gliding?
    ///
    /// # Safety
    ///
    /// `self` is the game's Actor, alive for this call.
    pub unsafe fn gliding(self) -> bool {
        // SAFETY: the caller's contract.
        unsafe { self.state(STATE_GLIDING) }
    }

    /// The rotation speed and `actor.InterpolationMode` that `actor_rotation` stored.
    ///
    /// # Safety
    ///
    /// `self` is the game's Actor, alive for this call.
    pub unsafe fn rotation(self) -> (f32, u8) {
        // SAFETY: the caller's contract. Both fields are inside the 0xef8-byte component.
        unsafe {
            (
                f32::from_le_bytes(bytes_at::<4>(self.0, ACTOR_ROTATION_SPEED)),
                self.0.add(ACTOR_ROTATION_MODE).read(),
            )
        }
    }

    /// Set CanClimb in the sequence's ability mask. Returns true if this call set it, which means
    /// the caller must [`Actor::clear_can_climb`] afterward.
    ///
    /// # Safety
    ///
    /// `self` is the game's Actor. The caller holds it for writing: the climb program has Actor
    /// in its write list.
    pub unsafe fn force_can_climb(self) -> bool {
        // SAFETY: the caller's contract. The mask is a u32 inside the Actor.
        let mask = unsafe { self.0.add(ACTOR_ABILITIES).cast::<u32>() };
        // SAFETY: as above.
        let found = unsafe { mask.read_unaligned() };
        if found & CAN_CLIMB != 0 {
            return false;
        }
        // SAFETY: as above.
        unsafe { mask.write_unaligned(found | CAN_CLIMB) };
        true
    }

    /// Clear CanClimb in the sequence's ability mask.
    ///
    /// # Safety
    ///
    /// As [`Actor::force_can_climb`].
    pub unsafe fn clear_can_climb(self) {
        // SAFETY: the caller's contract. The mask is a u32 inside the Actor.
        let mask = unsafe { self.0.add(ACTOR_ABILITIES).cast::<u32>() };
        // SAFETY: as above.
        unsafe { mask.write_unaligned(mask.read_unaligned() & !CAN_CLIMB) };
    }
}

/// The world move input in `ActorInput`.
///
/// # Safety
///
/// `input` is the game's ActorInput, alive for this call.
pub unsafe fn world_move_input(input: *const u8) -> Vec3 {
    // SAFETY: the caller's contract. The field is three floats inside the 0x1c8-byte component.
    unsafe { read_vec3(input.add(INPUT_WORLD_MOVE).cast()) }
}

/// Is the player's locomotion speed selector on Sprint?
///
/// # Safety
///
/// `input` is the game's ActorInput, alive for this call.
pub unsafe fn sprinting(input: *const u8) -> bool {
    // SAFETY: the caller's contract. The field is one byte inside the 0x1c8-byte component.
    unsafe { input.add(INPUT_SPEED).read() == SPEED_SPRINT }
}

/// The game's raycast as a [`Caster`]. It lives for one hook call, on the climbing thread.
pub struct GameCaster {
    /// The raycast.
    raycast: RaycastFn,
    /// The world.
    world: *mut u8,
    /// The entity that rays ignore: the climber.
    ignore: u64,
}

impl GameCaster {
    /// A caster for the climber in `ctx`, or `None` if there is no world.
    ///
    /// # Safety
    ///
    /// `ctx` is the game's climb context, alive for this call, and the caster is used only inside
    /// the hook that got it, on the same thread. The raycast is resolved by signature.
    pub unsafe fn new(raycast: RaycastFn, ctx: Ctx) -> Option<GameCaster> {
        // SAFETY: the caller's contract.
        let (world, ignore) = unsafe { (ctx.world(), ctx.entity()) };
        (!world.is_null()).then_some(GameCaster {
            raycast,
            world,
            ignore: u64::from(ignore),
        })
    }
}

/// One ray for the game: origin, then end, as two float4s.
#[repr(C, align(16))]
struct Ray([f32; 8]);

/// A raycast result buffer.
#[repr(C, align(16))]
struct RayOut([u8; RAY_OUT_SIZE]);

impl Caster for GameCaster {
    fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit> {
        let ray = Ray([from.x, from.y, from.z, 0.0, to.x, to.y, to.z, 0.0]);
        let mut out = RayOut([0; RAY_OUT_SIZE]);
        // SAFETY: `GameCaster::new`'s contract: this runs where the game itself casts rays, with
        // the world the game passed. `out` holds the 0x70 bytes the raycast writes and `ray` the
        // 0x20 bytes it reads, and both outlive the call. The argument order is the game's own
        // (`FUN_14024abc0` at 0x14024acc8; `FUN_1409fff60`'s prologue).
        unsafe {
            (self.raycast)(
                out.0.as_mut_ptr(),
                self.world,
                ray.0.as_ptr(),
                self.ignore,
                0,
                mask,
                RAY_FLAGS,
                0,
            );
        }
        parse_hit(&out.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A raycast result for a hit, laid out as `FUN_140a00070` writes it.
    fn ray_out(point: Vec3, normal: Vec3, id: u32) -> [u8; RAY_OUT_SIZE] {
        let mut out = [0u8; RAY_OUT_SIZE];
        out[RAY_OUT_ID..RAY_OUT_ID + 4].copy_from_slice(&id.to_le_bytes());
        for (index, value) in [point.x, point.y, point.z].into_iter().enumerate() {
            let at = RAY_OUT_POINT + index * 4;
            out[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (index, value) in [normal.x, normal.y, normal.z].into_iter().enumerate() {
            let at = RAY_OUT_NORMAL + index * 4;
            out[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        out
    }

    /// The fields come from the offsets the game writes them to.
    #[test]
    fn a_hit_is_read_from_the_games_layout() {
        let point = Vec3::new(1.0, 2.0, 3.0);
        let normal = Vec3::new(0.0, 0.0, -1.0);
        let hit = parse_hit(&ray_out(point, normal, 77)).unwrap();
        assert_eq!(hit.point, point);
        assert_eq!(hit.normal, normal);
        assert_eq!(hit.entity, 77);
    }

    /// Any status but 0 is a miss, however the rest reads.
    #[test]
    fn a_miss_is_no_hit() {
        let mut out = ray_out(Vec3::default(), Vec3::new(0.0, 0.0, 1.0), 1);
        out[0] = MISS;
        assert_eq!(parse_hit(&out), None);
        assert_eq!(parse_hit(&[]), None);
    }

    /// Broken numbers are no hit: nothing built on a NaN may reach an ECS component.
    #[test]
    fn broken_numbers_are_no_hit() {
        let normal = Vec3::new(0.0, 0.0, 1.0);
        let bad_point = ray_out(Vec3::new(f32::NAN, 0.0, 0.0), normal, 1);
        assert_eq!(parse_hit(&bad_point), None);
        let no_normal = ray_out(Vec3::default(), Vec3::default(), 1);
        assert_eq!(parse_hit(&no_normal), None);
    }

    /// A short buffer is no hit, not a panic.
    #[test]
    fn a_short_buffer_is_no_hit() {
        assert_eq!(parse_hit(&[0u8; 0x10]), None);
    }

    /// The hit's normal is length 1 whatever the game gave.
    #[test]
    fn a_hit_normal_has_length_one() {
        let hit = parse_hit(&ray_out(Vec3::default(), Vec3::new(0.0, 3.0, 4.0), 0)).unwrap();
        assert!((hit.normal.length() - 1.0).abs() < 1e-6);
    }

    /// The probe result is laid out as `FUN_14024abc0` writes it, so the game reads it unchanged.
    #[test]
    fn the_result_has_the_games_layout() {
        let pos = Vec3::new(1.0, 2.0, 3.0);
        let normal = Vec3::new(4.0, 5.0, 6.0);
        let bytes = encode_result(OK, pos, normal, 0x1234);
        assert_eq!(bytes[0], OK);
        assert_eq!(bytes[1..8], [0; 7], "the padding and +4 stay 0");
        assert_eq!(vec3_at(&bytes, 0x08), Some(pos));
        assert_eq!(vec3_at(&bytes, 0x14), Some(normal));
        assert_eq!(u32_at(&bytes, RESULT_ENTITY), Some(0x1234));
    }

    /// `result_*` read what `encode_result` wrote.
    #[test]
    fn result_fields_round_trip() {
        let point = Vec3::new(1.0, 2.0, 3.0);
        let normal = Vec3::new(0.0, 0.6, 0.8);
        let bytes = encode_result(OK, point, normal, 42);
        // SAFETY: `bytes` is a whole probe result.
        unsafe {
            assert_eq!(result_code(bytes.as_ptr()), OK);
            assert_eq!(result_entity(bytes.as_ptr()), 42);
            let hit = result_hit(bytes.as_ptr()).unwrap();
            assert_eq!(hit.point, point);
            assert!((hit.normal - normal).length() < 1e-6, "{hit:?}");
            assert_eq!(hit.entity, 42);
        }
    }

    /// Native padding can be uninitialized. Only the three initialized fields form a hit.
    #[test]
    fn a_native_hit_does_not_require_initialized_padding() {
        let mut storage = std::mem::MaybeUninit::<[u8; RESULT_SIZE]>::uninit();
        let result = storage.as_mut_ptr().cast::<u8>();
        let point = Vec3::new(1.0, 2.0, 3.0);
        let normal = Vec3::new(0.0, 0.0, -2.0);
        // SAFETY: storage is a writable result allocation. Initialize only native hit fields;
        // `result_hit` may read those fields, not the unwritten prefix or padding.
        let hit = unsafe {
            write_vec3(result.add(RESULT_POSITION).cast(), point);
            write_vec3(result.add(RESULT_NORMAL).cast(), normal);
            result.add(RESULT_ENTITY).cast::<u32>().write_unaligned(42);
            result_hit(result).unwrap()
        };
        assert_eq!(hit.point, point);
        assert_eq!(hit.normal, Vec3::new(0.0, 0.0, -1.0));
        assert_eq!(hit.entity, 42);
    }

    /// Broken numbers from memory are no hit.
    #[test]
    fn a_result_with_broken_numbers_is_no_hit() {
        let up = Vec3::new(0.0, 1.0, 0.0);
        let nan = Vec3::new(f32::NAN, 0.0, 0.0);
        assert_eq!(parse_result(&encode_result(OK, nan, up, 1)), None);
        assert_eq!(parse_result(&encode_result(OK, up, nan, 1)), None);
        assert_eq!(
            parse_result(&encode_result(OK, up, Vec3::default(), 1)),
            None
        );
    }

    /// `write_result` fills exactly the result and nothing past it.
    #[test]
    fn write_result_stays_inside_the_result() {
        let mut buffer = [0xaau8; RESULT_SIZE + 8];
        let bytes = encode_result(OK, Vec3::new(1.0, 1.0, 1.0), Vec3::default(), 9);
        // SAFETY: `buffer` is larger than a result.
        unsafe { write_result(buffer.as_mut_ptr(), &bytes) };
        assert_eq!(buffer[..RESULT_SIZE], bytes);
        assert_eq!(buffer[RESULT_SIZE..], [0xaa; 8]);
    }

    /// A failed result is all zeros but its code, as the game leaves it.
    #[test]
    fn a_failed_result_is_zeros_and_a_code() {
        let bytes = failed_result(MISS);
        assert_eq!(bytes[0], MISS);
        assert!(bytes[1..].iter().all(|byte| *byte == 0));
    }

    /// Without a sequence the raw bits count, with one the sequence's masks apply.
    #[test]
    fn state_bits_follow_the_games_rule() {
        let grounded = 1u64;
        assert!(state_bit(grounded, 0, 0, false, 0));
        assert!(!state_bit(0, grounded, 0, false, 0), "no sequence, no add");
        assert!(state_bit(0, grounded, 0, true, 0));
        assert!(!state_bit(grounded, 0, grounded, true, 0));
        assert!(state_bit(1 << 37, 0, 0, false, 37));
    }

    /// Each state gives its trigger. A glider off the ground is a glider, not a jump.
    #[test]
    fn the_trigger_follows_the_state() {
        assert_eq!(Trigger::of(true, false, false), Trigger::Walk);
        assert_eq!(Trigger::of(true, false, true), Trigger::Sprint);
        assert_eq!(Trigger::of(false, false, false), Trigger::Jump);
        assert_eq!(Trigger::of(false, false, true), Trigger::Jump);
        assert_eq!(Trigger::of(false, true, false), Trigger::Glide);
        assert_eq!(Trigger::of(true, true, true), Trigger::Glide);
    }

    /// The vector helpers read and write the three floats in order, at any alignment.
    #[test]
    fn vectors_round_trip_at_any_alignment() {
        let mut buffer = [0u8; 16];
        let value = Vec3::new(1.5, -2.5, 3.5);
        // SAFETY: 12 bytes at offset 1 are inside `buffer`.
        unsafe {
            let at = buffer.as_mut_ptr().add(1).cast::<f32>();
            write_vec3(at, value);
            assert_eq!(read_vec3(at), value);
        }
    }

    /// The context reads each field from its offset. The fake is laid out as `FUN_140419570`
    /// builds it.
    #[test]
    fn the_context_reads_its_fields() {
        let mut climb = [0u8; 0x98];
        climb[4..8].copy_from_slice(&1.0f32.to_le_bytes());
        let transform: [i64; 3] = [1 << 32, 2 << 32, -(3 << 32)];
        let config: [f32; 2] = [0.9, 1.8];
        let mut collider = [0u8; 0x54];
        collider[0x4b] = 1;
        collider[0x50] = 1;
        let world = 0x1234usize;
        let fields: [usize; 8] = [
            0,
            climb.as_mut_ptr() as usize,
            transform.as_ptr() as usize,
            config.as_ptr() as usize,
            collider.as_ptr() as usize,
            0,
            77,
            world,
        ];
        let ctx = Ctx(fields.as_ptr() as *mut u8);
        // SAFETY: `fields` and everything it points to live to the end of this test, and each
        // has the size of the game's component for the fields read here.
        unsafe {
            assert_eq!(ctx.entity(), 77);
            assert_eq!(ctx.world() as usize, world);
            assert_eq!(ctx.position(), Vec3::new(1.0, 2.0, -3.0));
            assert_eq!(read_vec3(ctx.normal_at()).x, 1.0);
            assert!(ctx.in_contact());
            let capsule = ctx.capsule();
            assert_eq!(capsule.height, 1.8);
            assert!((capsule.radius - 0.45).abs() < 1e-6);
            assert!(ctx.actor().is_none());
        }
    }

    /// Without a result to trust, contact reads as no. Stale results must not stop a stand-off.
    #[test]
    fn contact_needs_a_valid_result() {
        let mut collider = [0u8; 0x54];
        collider[0x4b] = 1;
        let fields: [usize; 8] = [0, 0, 0, 0, collider.as_ptr() as usize, 0, 0, 0];
        let ctx = Ctx(fields.as_ptr() as *mut u8);
        // SAFETY: `fields` and `collider` live to the end of this test.
        assert!(!unsafe { ctx.in_contact() });
    }

    /// Optional components can be absent, and the readers cope.
    #[test]
    fn missing_components_read_as_defaults() {
        let fields: [usize; 8] = [0; 8];
        let ctx = Ctx(fields.as_ptr() as *mut u8);
        // SAFETY: `fields` is a whole context, and these readers check their pointers.
        unsafe {
            assert_eq!(ctx.capsule(), Capsule::FALLBACK);
            assert!(!ctx.in_contact());
            assert!(ctx.actor().is_none());
        }
    }

    /// The grace byte is the one after `climbState`. A write there must miss the fields around it,
    /// and +3, which stays free.
    #[test]
    fn the_grace_byte_sits_in_the_padding() {
        let mut climb = [0xaau8; 0x98];
        let fields: [usize; 2] = [0, climb.as_mut_ptr() as usize];
        let ctx = Ctx(fields.as_ptr() as *mut u8);
        // SAFETY: `fields` and `climb` live to the end of this test, and the byte is inside `climb`.
        unsafe { ctx.grace_at().write(7) };
        let mut expected = [0xaau8; 0x98];
        expected[2] = 7;
        assert_eq!(climb, expected);
    }

    /// The orientation comes after the three i64 of the position, x first.
    #[test]
    fn the_orientation_is_read_from_its_offset() {
        let mut transform = [0u8; 0x38];
        for (index, value) in [1.0f32, 2.0, 3.0, 4.0].into_iter().enumerate() {
            let at = TRANSFORM_ORIENTATION + index * 4;
            transform[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        let fields: [usize; 3] = [0, 0, transform.as_ptr() as usize];
        let ctx = Ctx(fields.as_ptr() as *mut u8);
        // SAFETY: `fields` and `transform` live to the end of this test.
        assert_eq!(unsafe { ctx.orientation() }, [1.0, 2.0, 3.0, 4.0]);
    }

    /// An Actor as a plain buffer, the size of the component.
    fn actor_bytes() -> Vec<u8> {
        vec![0u8; 0xef8]
    }

    /// The speed and the mode sit where `actor_rotation` stores them.
    #[test]
    fn the_rotation_is_read_from_its_offsets() {
        let mut bytes = actor_bytes();
        bytes[ACTOR_ROTATION_SPEED..ACTOR_ROTATION_SPEED + 4]
            .copy_from_slice(&7.5f32.to_le_bytes());
        bytes[ACTOR_ROTATION_MODE] = 3;
        let actor = Actor(bytes.as_mut_ptr());
        // SAFETY: `bytes` is big enough and lives to the end of this test.
        assert_eq!(unsafe { actor.rotation() }, (7.5, 3));
    }

    /// The state readers apply the game's rule at the game's offsets.
    #[test]
    fn the_actor_reads_its_state() {
        let mut bytes = actor_bytes();
        bytes[ACTOR_STATE] = 1;
        let glide = (1u64 << 37).to_le_bytes();
        bytes[ACTOR_STATE_ADD..ACTOR_STATE_ADD + 8].copy_from_slice(&glide);
        let actor = Actor(bytes.as_mut_ptr());
        // SAFETY: `bytes` is big enough and lives to the end of this test.
        unsafe {
            assert!(actor.grounded());
            assert!(!actor.gliding(), "no sequence yet");
        }
        bytes[ACTOR_SEQUENCE] = 1;
        let actor = Actor(bytes.as_mut_ptr());
        // SAFETY: as above.
        assert!(unsafe { actor.gliding() });
    }

    /// The force sets only bit 16, says whether it did, and the clear takes back only that bit.
    #[test]
    fn can_climb_is_forced_and_cleared_alone() {
        let mut bytes = actor_bytes();
        let start = 0x023c_4000u32;
        bytes[ACTOR_ABILITIES..ACTOR_ABILITIES + 4].copy_from_slice(&start.to_le_bytes());
        let actor = Actor(bytes.as_mut_ptr());
        let read = |bytes: &[u8]| u32_at(bytes, ACTOR_ABILITIES).unwrap();
        // SAFETY: `bytes` is big enough and lives to the end of this test.
        assert!(unsafe { actor.force_can_climb() });
        assert_eq!(read(&bytes), start | CAN_CLIMB);
        // SAFETY: as above.
        assert!(!unsafe { actor.force_can_climb() }, "already set");
        // SAFETY: as above.
        unsafe { actor.clear_can_climb() };
        assert_eq!(read(&bytes), start);
    }

    /// The iterator holds the actor where the start check reads it.
    #[test]
    fn the_iterator_holds_the_actor() {
        let mut slots = [0usize; 0x10];
        slots[8] = 0x4242;
        // SAFETY: `slots` is larger than the slot read.
        let actor = unsafe { Actor::of_iterator(slots.as_ptr().cast()) };
        assert_eq!(actor.map(|actor| actor.0 as usize), Some(0x4242));
        slots[8] = 0;
        // SAFETY: as above.
        assert!(unsafe { Actor::of_iterator(slots.as_ptr().cast()) }.is_none());
    }

    /// The input fields sit at the game's offsets.
    #[test]
    fn the_input_is_read_from_its_offsets() {
        let mut input = vec![0u8; 0x1c8];
        input[INPUT_WORLD_MOVE..INPUT_WORLD_MOVE + 4].copy_from_slice(&0.5f32.to_le_bytes());
        input[INPUT_SPEED] = SPEED_SPRINT;
        // SAFETY: `input` is the size of the component.
        unsafe {
            assert_eq!(world_move_input(input.as_ptr()).x, 0.5);
            assert!(sprinting(input.as_ptr()));
        }
        input[INPUT_SPEED] = 3;
        // SAFETY: as above.
        assert!(!unsafe { sprinting(input.as_ptr()) });
    }

    /// What a fake raycast was given, and the answer it gives.
    #[derive(Clone, Copy)]
    struct Seen {
        ray: [f32; 8],
        world: usize,
        ignore: u64,
        arg5: u32,
        mask: u32,
        flags: u8,
        arg8: u64,
    }

    thread_local! {
        /// The last call to [`fake_raycast`].
        static SEEN: std::cell::Cell<Option<Seen>> = const { std::cell::Cell::new(None) };
    }

    /// A raycast with the game's signature that records its arguments and always hits.
    ///
    /// # Safety
    ///
    /// `out` is 0x70 writable bytes and `ray` is 0x20 readable bytes.
    #[allow(clippy::too_many_arguments)]
    unsafe extern "system" fn fake_raycast(
        out: *mut u8,
        world: *mut u8,
        ray: *const f32,
        ignore: u64,
        arg5: u32,
        mask: u32,
        flags: u8,
        arg8: u64,
    ) -> *mut u8 {
        // SAFETY: the function's contract.
        let ray = unsafe { ray.cast::<[f32; 8]>().read_unaligned() };
        SEEN.with(|seen| {
            seen.set(Some(Seen {
                ray,
                world: world as usize,
                ignore,
                arg5,
                mask,
                flags,
                arg8,
            }));
        });
        let answer = ray_out(Vec3::new(9.0, 8.0, 7.0), Vec3::new(1.0, 0.0, 0.0), 5);
        // SAFETY: the function's contract.
        unsafe { std::ptr::copy_nonoverlapping(answer.as_ptr(), out, RAY_OUT_SIZE) };
        out
    }

    /// The caster calls the raycast with the game's arguments, in the game's order, and reads
    /// back the answer. A wrong order here would crash the game, so it is pinned.
    #[test]
    fn the_caster_passes_the_games_arguments() {
        let fields: [usize; 8] = [0, 0, 0, 0, 0, 0, 0xABCD, 0x7000];
        let ctx = Ctx(fields.as_ptr() as *mut u8);
        // SAFETY: `fields` is a whole context, and the world pointer is only passed on to the
        // fake, which does not read it.
        let caster = unsafe { GameCaster::new(fake_raycast, ctx) }.unwrap();
        let hit = caster
            .cast(Vec3::new(1.0, 2.0, 3.0), Vec3::new(4.0, 5.0, 6.0), 0x124)
            .unwrap();
        assert_eq!(hit.point, Vec3::new(9.0, 8.0, 7.0));
        assert_eq!(hit.entity, 5);
        let seen = SEEN.with(std::cell::Cell::get).unwrap();
        assert_eq!(seen.ray, [1.0, 2.0, 3.0, 0.0, 4.0, 5.0, 6.0, 0.0]);
        assert_eq!(seen.world, 0x7000);
        assert_eq!(seen.ignore, 0xABCD);
        assert_eq!(seen.arg5, 0);
        assert_eq!(seen.mask, 0x124);
        assert_eq!(seen.flags, 0x60);
        assert_eq!(seen.arg8, 0);
    }

    /// No world, no rays.
    #[test]
    fn no_world_means_no_caster() {
        let fields: [usize; 8] = [0; 8];
        let ctx = Ctx(fields.as_ptr() as *mut u8);
        // SAFETY: `fields` is a whole context.
        assert!(unsafe { GameCaster::new(fake_raycast, ctx) }.is_none());
    }
}
