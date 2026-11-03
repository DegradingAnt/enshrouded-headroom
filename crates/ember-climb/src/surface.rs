//! Find the surface in front of the player and steer along it.
//!
//! The game's own wall probe wants four rays with one exact normal, which only a flat lattice
//! gives (docs/re-notes.md, "Free climbing"). This module fits a patch instead. It casts four
//! rays, averages what they hit, and asks whether the result is steep and flat enough to hold.
//!
//! Everything here is pure and generic over [`Caster`], so the tests can climb simulated rock.
//! The game's raycast is one `Caster` (`game::GameCaster`).

use crate::geom::{Vec3, UP};

/// Heights of the two ray rows along the body, in metres. They are the game's own numbers
/// (`0x1413b5678` and `0x1414faf6c`).
pub const RAY_ROWS: [f32; 2] = [2.0, 1.25];
/// How far each ray sits to the side of the body axis, in metres. The game uses 0.5; a narrower
/// patch stays on one rock face longer.
pub const RAY_SIDE: f32 = 0.35;
/// Rays in one patch.
pub const RAY_COUNT: usize = 4;
/// The game's distance between the player and a wall, in metres (`0x1413b5b3c`).
pub const GAME_STANDOFF: f32 = 0.55;
/// The gap kept between the capsule and an overhang, in metres.
const CAPSULE_GAP: f32 = 0.05;
/// A ray direction with more up in it than this has no sideways axis worth using.
const MAX_RAY_TILT: f32 = 0.95;
/// How far inside [`MAX_RAY_TILT`] the climb normal stays, so that rounding in a later frame
/// cannot push it past the limit and drop the player.
const TILT_MARGIN: f32 = 0.01;
/// How far behind its origin a ray of a running climb starts, in metres. Where a bench tips the
/// climb normal, the ray patch swings up against the rock above the bench, and an origin can lie
/// behind that rock. A ray from there meets no front face, so the move check finds no surface and
/// the hold can fail. In the closed-loop model 0.2 m leaves more such stops, and 0.5 m ends almost
/// none that 0.3 m leaves. HYPOTHESIS: what the game's ray returns from inside rock. No note says.
const RAY_BACK: f32 = 0.3;
/// Extra reach for the hold check, so a move that passed cannot lose the wall a frame later.
const STAY_EXTRA: f32 = 0.4;
/// Extra reach of the entry rays past the farthest face they must touch, in metres.
const ENTRY_RAY_SPARE: f32 = 0.5;
/// How much rougher a surface may be while moving than to start a climb.
const MOVE_ROUGHNESS: f32 = 1.5;
/// Degrees of slope that stay holdable past the walkable limit. The player keeps hold there, so
/// the climb can end with a mantle instead of a drop.
const HOLD_MARGIN_DEGREES: f32 = 15.0;
/// Degrees of overhang a running climb may pass the limit for starting one. A rough patch can read
/// this much steeper than the rock as a whole, and a hard limit would stop the climb there.
const OVERHANG_MARGIN_DEGREES: f32 = 15.0;
/// The steepest development overhang. Native body and traversal acceptance remain pending.
pub const MAX_OVERHANG_DEGREES: f32 = 160.0;
/// The collider flag Solid (bit 3). The default surface mask does not ask for it.
const MASK_SOLID: u32 = 0x08;
/// How far short of its own hit a corner clearance ray stops, in metres. A rock prop can be Solid
/// and Walkable at once, and must not block itself.
const CLEARANCE_GAP: f32 = 0.02;
/// How far in front of the fit plane the chest clearance ray stops, in metres, along the normal.
/// No surface ray ran on that line, so the plane is all that is known there, and rock between the
/// hits stands proud of it: 0.14 m on a ball of radius 1 m, and, head on, up to 0.184 m on rock
/// displaced by 0.15 m either way. At a yaw of 44 degrees that rock reaches 0.221 m, and it refuses
/// 2 of 8963 starts. A Solid rock prop must not block itself on that. `plane_tolerance` is too
/// large for this: at its default 0.45 m a ray from 0.8 m off the rock ends 0.35 m out, inside a
/// capsule of radius 0.4 m. Rock in the existing tests also needs this under 0.3 m.
const CHEST_MARGIN: f32 = 0.2;
/// The widest angle between the stick and the wall that starts a climb, as a cosine. The game's
/// limit is 0.7853982 radians (45 degrees); only the way it is measured differs here.
const ENTRY_CONE_COS: f32 = 0.707_106_77;
/// Time for the climb normal to cover most of the way to a new surface, in seconds.
pub const NORMAL_TIME: f32 = 0.12;
/// How fast a stand-off error closes, per second.
pub const STANDOFF_GAIN: f32 = 6.0;
/// The fastest stand-off correction, in metres per second.
pub const STANDOFF_SPEED: f32 = 1.2;
/// A stand-off error this small is left alone, in metres, so the player does not hunt.
pub const STANDOFF_DEADBAND: f32 = 0.03;

/// What one ray hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    /// Where the ray hit.
    pub point: Vec3,
    /// The surface normal there.
    pub normal: Vec3,
    /// The entity hit. 0 is voxel terrain or a voxel wall.
    pub entity: u32,
}

/// Casts one ray. In the game this is `FUN_1409fff60`; in the tests, an analytic world.
pub trait Caster {
    /// The first surface in `mask` between `from` and `to`, or `None`.
    fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit>;
}

/// The player's upright collision capsule.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Capsule {
    /// Radius of the two end spheres, in metres.
    pub radius: f32,
    /// Full height, in metres.
    pub height: f32,
}

impl Capsule {
    /// Used when the game's numbers make no sense. The real numbers are game data.
    pub const FALLBACK: Capsule = Capsule {
        radius: 0.4,
        height: 2.0,
    };

    /// The capsule from `MovementColliderConfig`, whose `width` is the diameter
    /// (`FUN_14023c0e0`). Numbers that make no sense give [`Capsule::FALLBACK`].
    pub fn from_config(width: f32, height: f32) -> Capsule {
        if (0.1..=3.0).contains(&width) && (0.5..=6.0).contains(&height) {
            Capsule {
                radius: width * 0.5,
                height,
            }
        } else {
            Capsule::FALLBACK
        }
    }

    /// Height of the centre of the upper end sphere above the feet.
    fn top(self) -> f32 {
        (self.height - self.radius).max(self.radius)
    }
}

/// The limits of a climbable surface, from the settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    /// Collider flags the rays may hit.
    pub mask: u32,
    /// The roughest patch that starts a climb: the spread of the hits along the normal, in
    /// metres.
    pub plane_tolerance: f32,
    /// A surface whose normal has more up in it than this is walkable, so it starts no climb.
    pub walkable_ny: f32,
    /// A surface with more up in it than this cannot be moved onto. It is less steep than
    /// walkable, so a climb that reaches it can still end with a mantle.
    pub hold_ny: f32,
    /// A surface whose normal has less up in it than this hangs over too far to start a climb.
    pub overhang_ny: f32,
    /// A surface whose normal has less up in it than this cannot be moved onto.
    pub move_overhang_ny: f32,
    /// The farthest the player may be from a surface and still start a climb, in metres.
    pub entry_reach: f32,
    /// How far the rays reach during a climb, in metres. A wider stand-off under an overhang adds
    /// to it.
    pub hold_reach: f32,
    /// How far the rays reach when a climb starts, in metres.
    pub entry_ray: f32,
    /// The exit's ray for the face going on.
    pub face: crate::exit::FaceRay,
}

impl Tuning {
    /// The limits for these settings. Angles are in degrees from flat ground, so 90 is a sheer
    /// wall.
    pub fn new(
        mask: u32,
        plane_tolerance: f32,
        walkable_angle: f32,
        overhang_angle: f32,
        reach: (f32, f32),
    ) -> Tuning {
        let (entry_reach, hold_reach) = reach;
        let walkable = walkable_angle.to_radians();
        let hold = (walkable_angle - HOLD_MARGIN_DEGREES).to_radians();
        let past_overhang = (overhang_angle + OVERHANG_MARGIN_DEGREES)
            .min(MAX_OVERHANG_DEGREES)
            .to_radians();
        let [top, _] = RAY_ROWS;
        Tuning {
            mask,
            plane_tolerance,
            walkable_ny: walkable.cos(),
            hold_ny: hold.cos().min(MAX_RAY_TILT - TILT_MARGIN),
            overhang_ny: overhang_angle.to_radians().cos(),
            move_overhang_ny: past_overhang.cos(),
            entry_reach,
            hold_reach,
            // A level ray at the top row reaches a leaning face this far past its foot.
            entry_ray: top / walkable.tan() + ENTRY_RAY_SPARE,
            face: crate::exit::FaceRay::new(walkable_angle),
        }
    }
}

/// Which question a probe asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// May a climb start here?
    Entry,
    /// May the player move here?
    Move,
    /// Is the player still on something?
    Stay,
}

impl Mode {
    /// The hits this question needs.
    fn needed(self) -> u8 {
        match self {
            Mode::Entry | Mode::Move => 3,
            Mode::Stay => 2,
        }
    }

    /// How far the rays reach for this question.
    fn reach(self, t: &Tuning) -> f32 {
        match self {
            Mode::Entry => t.entry_ray,
            Mode::Move => t.hold_reach,
            Mode::Stay => t.hold_reach + STAY_EXTRA,
        }
    }

    /// How far behind its origin a ray starts for this question. A start has no tipped frame, so
    /// its origins stand over the feet.
    fn back(self) -> f32 {
        match self {
            Mode::Entry => 0.0,
            Mode::Move | Mode::Stay => RAY_BACK,
        }
    }
}

/// Why a surface was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// The direction has no usable sideways axis.
    Direction,
    /// Too few rays hit anything.
    Hits,
    /// The hits do not lie on one surface.
    Bumpy,
    /// The surface can be walked on.
    Walkable,
    /// The surface hangs over too far.
    Overhang,
    /// The surface is too far away to start a climb.
    TooFar,
    /// Something solid stands between the player and the surface.
    Blocked,
}

/// A surface fitted through the rays' hits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    /// The averaged normal, length 1.
    pub normal: Vec3,
    /// The centre of the hits.
    pub point: Vec3,
    /// The hits used.
    pub hits: u8,
    /// How far apart the hits lie along the normal, in metres.
    pub spread: f32,
    /// The entity of the first hit. 0 is voxel terrain or a voxel wall.
    pub entity: u32,
}

impl Fit {
    /// The signed distance from `pos` to the fitted surface, along `normal`.
    pub fn distance(&self, pos: Vec3, normal: Vec3) -> f32 {
        (pos - self.point).dot(normal)
    }
}

/// The answer to one probe.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scan {
    /// Raw hits in ray-origin order after fitting; early refusals expose no guidance.
    /// These are discovery hints only and never grant a future candidate permission.
    pub samples: [Option<Hit>; RAY_COUNT],
    /// The rays that hit something.
    pub hits: u8,
    /// The fit, when enough rays hit.
    pub fit: Option<Fit>,
    /// `Ok` with where the player stands on this surface, or why it was refused.
    pub verdict: Result<Vec3, Reject>,
    /// Is the fit good enough to steer by? A hold can pass on less than that.
    pub steady: bool,
}

impl Scan {
    /// A probe that found no fit.
    fn refused(hits: u8, why: Reject) -> Scan {
        Scan {
            samples: [None; RAY_COUNT],
            hits,
            fit: None,
            verdict: Err(why),
            steady: false,
        }
    }
}

/// The axes of one ray patch. For a climb they lie along the surface, because the game's target
/// rotation faces it (`FUN_14021c730`, "Climbing"). HYPOTHESIS: the body reaches that target. It
/// can only do so if the rotation mode allows it, which the `climb body:` log line will show.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Where the rays point, length 1.
    pub along: Vec3,
    /// Along the body, feet to head.
    pub up: Vec3,
    /// Sideways.
    pub right: Vec3,
}

impl Frame {
    /// The axes for rays that point along `dir`, or `None` when `dir` is close to straight up or
    /// down.
    pub fn along(dir: Vec3) -> Option<Frame> {
        let along = dir.normalized()?;
        if along.y.abs() > MAX_RAY_TILT {
            return None;
        }
        let right = UP.cross(along).normalized()?;
        Some(Frame {
            along,
            up: along.cross(right),
            right,
        })
    }

    /// The ray origins for a player at `pos`, upper row first. A reach counts from here. The rays
    /// of a running climb start a little behind these points.
    pub fn origins(&self, pos: Vec3) -> [Vec3; RAY_COUNT] {
        let [top, low] = RAY_ROWS;
        let at = |height: f32, side: f32| pos + self.up * height + self.right * side;
        [
            at(top, -RAY_SIDE),
            at(top, RAY_SIDE),
            at(low, -RAY_SIDE),
            at(low, RAY_SIDE),
        ]
    }
}

/// The fit through `hits`, or `None` when there are none or their normals cancel out.
fn fit_of<'a>(hits: impl Iterator<Item = &'a Hit> + Clone) -> Option<Fit> {
    let mut count = 0u8;
    let mut normal = Vec3::default();
    let mut point = Vec3::default();
    let mut entity = 0;
    for hit in hits.clone() {
        if count == 0 {
            entity = hit.entity;
        }
        count = count.saturating_add(1);
        normal = normal + hit.normal;
        point = point + hit.point;
    }
    if count == 0 {
        return None;
    }
    let normal = normal.normalized()?;
    let point = point * (1.0 / f32::from(count));
    let (mut lowest, mut highest) = (f32::MAX, f32::MIN);
    for hit in hits {
        let depth = (hit.point - point).dot(normal);
        lowest = lowest.min(depth);
        highest = highest.max(depth);
    }
    Some(Fit {
        normal,
        point,
        hits: count,
        spread: highest - lowest,
        entity,
    })
}

/// The fit through `hits`. When it is rougher than `tolerance`, one stray hit (a boulder in front
/// of the face) is left out if that gives a smoother fit.
fn best_fit(hits: &[Hit], tolerance: f32) -> Option<Fit> {
    let all = fit_of(hits.iter())?;
    if all.spread <= tolerance || hits.len() < RAY_COUNT {
        return Some(all);
    }
    let without = |skip: usize| {
        fit_of(
            hits.iter()
                .enumerate()
                .filter(move |(index, _)| *index != skip)
                .map(|(_, hit)| hit),
        )
    };
    let smoothest = (0..hits.len())
        .filter_map(without)
        .min_by(|a, b| a.spread.total_cmp(&b.spread));
    smoothest
        .filter(|fit| fit.spread < all.spread)
        .or(Some(all))
}

/// How far the part of the capsule nearest the surface is from it. On a face that leans back
/// that is the feet; under an overhang it is the head.
pub fn near_distance(pos: Vec3, fit: &Fit, capsule: Capsule) -> f32 {
    let low = fit.distance(pos + UP * capsule.radius, fit.normal);
    let high = fit.distance(pos + UP * capsule.top(), fit.normal);
    low.min(high)
}

/// How far the feet stay from a surface whose normal has `ny` up in it.
///
/// The game keeps 0.55 m. HYPOTHESIS until the `climb body:` log line proves it: the body lies
/// along the surface at the same distance. The capsule stays upright, so an overhang tips its top
/// toward the rock; there the feet stay farther out.
pub fn standoff_target(ny: f32, capsule: Capsule) -> f32 {
    GAME_STANDOFF.max(capsule.radius + CAPSULE_GAP - capsule.top() * ny)
}

/// Is `fit` good enough for `mode`?
fn judge(fit: &Fit, mode: Mode, t: &Tuning, pos: Vec3, capsule: Capsule) -> Result<(), Reject> {
    let (tolerance, top, bottom) = match mode {
        Mode::Stay => return Ok(()),
        Mode::Entry => (t.plane_tolerance, t.walkable_ny, t.overhang_ny),
        Mode::Move => (
            t.plane_tolerance * MOVE_ROUGHNESS,
            t.hold_ny,
            t.move_overhang_ny,
        ),
    };
    if fit.spread > tolerance {
        return Err(Reject::Bumpy);
    }
    if fit.normal.y > top {
        return Err(Reject::Walkable);
    }
    if fit.normal.y < bottom {
        return Err(Reject::Overhang);
    }
    if mode == Mode::Entry && near_distance(pos, fit, capsule) > t.entry_reach {
        return Err(Reject::TooFar);
    }
    Ok(())
}

/// Does something solid stand between the rays' `starts` and their `hits`? The surface rays do not
/// ask for Solid, so they pass through a tree, a fence or an enemy. These rays stop just short of
/// each hit. A ray from the chest covers the gap between the side rays, where a thin trunk fits. It
/// has no hit of its own, so it stops [`CHEST_MARGIN`] short of `fit`'s plane.
fn blocked<C: Caster>(
    caster: &C,
    frame: &Frame,
    pos: Vec3,
    fit: &Fit,
    starts: &[Vec3],
    hits: &[Hit],
) -> bool {
    let [top, low] = RAY_ROWS;
    let chest = pos + frame.up * ((top + low) * 0.5);
    let rays = || {
        starts
            .iter()
            .zip(hits)
            .map(|(start, hit)| (*start, (hit.point - *start).dot(frame.along)))
    };
    let in_way = |start: Vec3, length: f32| {
        let end = start + frame.along * length;
        length > 0.0 && caster.cast(start, end, MASK_SOLID).is_some()
    };
    // Each metre of ray closes on the plane by this much. A fit that lies along the ray divides by
    // next to nothing, so the rays that found the surface cap the length. `min` also drops a NaN.
    let closing = -frame.along.dot(fit.normal);
    let to_margin = (fit.distance(chest, fit.normal) - CHEST_MARGIN) / closing;
    let farthest = rays().map(|(_, length)| length).fold(0.0, f32::max);
    in_way(chest, to_margin.min(farthest))
        || rays().any(|(start, length)| in_way(start, length - CLEARANCE_GAP))
}

/// Probe for a surface from `pos` along `dir`, and answer `mode`.
///
/// The upper row is cast first. A probe stops as soon as too many rays have missed, so open air
/// costs two rays.
pub fn scan<C: Caster>(
    caster: &C,
    pos: Vec3,
    dir: Vec3,
    mode: Mode,
    t: &Tuning,
    capsule: Capsule,
) -> Scan {
    let Some(frame) = Frame::along(dir) else {
        return Scan::refused(0, Reject::Direction);
    };
    // An overhang holds the player farther out, and the rays must still get to the rock.
    let wider = standoff_target(-frame.along.y, capsule) - GAME_STANDOFF;
    let reach = mode.reach(t) + wider;
    let needed = usize::from(mode.needed());
    let spare = RAY_COUNT.saturating_sub(needed);
    let mut found = [Hit {
        point: Vec3::default(),
        normal: Vec3::default(),
        entity: 0,
    }; RAY_COUNT];
    let mut starts = [Vec3::default(); RAY_COUNT];
    let mut samples = [None; RAY_COUNT];
    let mut hits = 0usize;
    let mut misses = 0usize;
    for (ray, origin) in frame.origins(pos).into_iter().enumerate() {
        let from = origin - frame.along * mode.back();
        match caster.cast(from, origin + frame.along * reach, t.mask) {
            Some(hit) => {
                if let Some(sample) = samples.get_mut(ray) {
                    *sample = Some(hit);
                }
                if let (Some(slot), Some(start)) = (found.get_mut(hits), starts.get_mut(hits)) {
                    *slot = hit;
                    *start = from;
                }
                hits = hits.saturating_add(1);
            }
            None => {
                misses = misses.saturating_add(1);
                if misses > spare {
                    return Scan::refused(hits as u8, Reject::Hits);
                }
            }
        }
    }
    let tolerance = match mode {
        Mode::Entry => t.plane_tolerance,
        Mode::Move | Mode::Stay => t.plane_tolerance * MOVE_ROUGHNESS,
    };
    let Some(fit) = found
        .get(..hits)
        .and_then(|found| best_fit(found, tolerance))
    else {
        return Scan::refused(hits as u8, Reject::Hits);
    };
    let stand =
        pos + fit.normal * (standoff_target(fit.normal.y, capsule) - fit.distance(pos, fit.normal));
    let mut verdict = judge(&fit, mode, t, pos, capsule).map(|()| stand);
    if mode == Mode::Entry && verdict.is_ok() {
        let starts = starts.get(..hits).unwrap_or_default();
        if blocked(caster, &frame, pos, &fit, starts, &found) {
            verdict = Err(Reject::Blocked);
        }
    }
    Scan {
        samples,
        hits: fit.hits,
        fit: Some(fit),
        verdict,
        steady: judge(&fit, Mode::Move, t, pos, capsule).is_ok() && fit.hits >= Mode::Move.needed(),
    }
}

/// A movement probe supported by the steady natural hold confirmed in this same update.
#[derive(Clone, Copy, Debug)]
pub struct MoveProbe {
    /// The feet before movement.
    pub current: Vec3,
    /// The candidate feet the native move check is testing.
    pub candidate: Vec3,
    /// The native probe direction.
    pub direction: Vec3,
    /// A steady fit from this entity's hold in the same call; never a previous-frame snapshot.
    pub held: Fit,
}

/// Ordinary movement first, with bounded recovery toward a confirmed hold when needed.
pub fn scan_move_from_hold<C: Caster>(
    caster: &C,
    probe: MoveProbe,
    t: &Tuning,
    capsule: Capsule,
) -> Scan {
    let ordinary = scan(
        caster,
        probe.candidate,
        probe.direction,
        Mode::Move,
        t,
        capsule,
    );
    if ordinary.verdict != Err(Reject::Hits)
        || !approaches_hold(probe.current, probe.candidate, &probe.held)
    {
        return ordinary;
    }
    let recovering = Tuning {
        hold_reach: t.hold_reach + STAY_EXTRA,
        ..*t
    };
    let fresh = scan(
        caster,
        probe.candidate,
        probe.direction,
        Mode::Move,
        &recovering,
        capsule,
    );
    if fresh.verdict.is_ok()
        && !fresh
            .fit
            .is_some_and(|fit| approaches_hold(probe.current, probe.candidate, &fit))
    {
        return ordinary;
    }
    fresh
}

/// Both positions remain in front of a finite fit, and the candidate reduces its signed gap.
/// This cannot authorize a parallel step, a step away, or crossing the fitted plane.
fn approaches_hold(current: Vec3, candidate: Vec3, fit: &Fit) -> bool {
    if !current.is_finite() || !candidate.is_finite() || !fit.point.is_finite() {
        return false;
    }
    let Some(normal) = fit.normal.normalized() else {
        return false;
    };
    let before = fit.distance(current, normal);
    let after = fit.distance(candidate, normal);
    before.is_finite() && after.is_finite() && before > after && after > 0.0
}

/// Does the stick point at the wall? The game asks `acos(dot(-normal, input)) <= 45 degrees` with
/// the raw input, so a half-pressed stick or a leaning wall fails it however straight the player
/// runs at the wall. Here both are flat and length 1, which leaves only the heading.
pub fn cone_ok(normal: Vec3, input: Vec3) -> bool {
    match (normal.flat().normalized(), input.flat().normalized()) {
        (Some(normal), Some(input)) => (-normal).dot(input) >= ENTRY_CONE_COS,
        _ => false,
    }
}

/// What one frame of steering works from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SteerInput {
    /// The velocity the game computed. It is world-vertical plus sideways.
    pub velocity: Vec3,
    /// The current climb normal.
    pub normal: Vec3,
    /// The surface fit at the player, if it is steady.
    pub fit: Option<Fit>,
    /// The player's position.
    pub pos: Vec3,
    /// The frame time, in seconds.
    pub dt: f32,
    /// Is the capsule touching something sideways already?
    pub contact: bool,
}

/// What steering decided.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Steered {
    /// The new velocity.
    pub velocity: Vec3,
    /// The new climb normal.
    pub normal: Vec3,
    /// How far the player is past the wanted stand-off, in metres. Positive is too far out.
    pub error: f32,
}

/// `normal` with its up part kept between `low` and `high`, length 1. `None` when it has no
/// heading left to keep.
fn clamp_ny(normal: Vec3, low: f32, high: f32) -> Option<Vec3> {
    let ny = normal.y.clamp(low, high);
    if ny == normal.y {
        return normal.normalized();
    }
    let flat = normal.flat().normalized()?;
    Some(flat * (1.0 - ny * ny).max(0.0).sqrt() + UP * ny)
}

/// How fast to move along the normal to fix a stand-off `error`. Positive moves away from the
/// surface. It never overshoots, however long the frame, and it does not pull into a surface the
/// capsule already touches.
fn standoff_speed(error: f32, dt: f32, contact: bool) -> f32 {
    let past = error.abs() - STANDOFF_DEADBAND;
    if past <= 0.0 {
        return 0.0;
    }
    let speed = (past * STANDOFF_GAIN).min(STANDOFF_SPEED).min(past / dt);
    match (error > 0.0, contact) {
        (true, true) => 0.0,
        (true, false) => -speed,
        (false, _) => speed,
    }
}

/// One frame of steering, or `None` when the input is unusable and the game's velocity stands.
///
/// The game moves a climber along world up. On a surface that leans, that walks straight off it.
/// Movement follows the fresh physical plane with its speed kept, and a small correction holds
/// the stand-off. Body orientation eases separately, so its lag cannot push across that plane.
pub fn steer(input: &SteerInput, t: &Tuning, capsule: Capsule) -> Option<Steered> {
    let SteerInput {
        velocity,
        normal,
        fit,
        pos,
        dt,
        contact,
    } = *input;
    if !(dt > 0.0 && dt.is_finite() && velocity.is_finite() && normal.is_finite()) {
        return None;
    }
    let eased = match &fit {
        Some(fit) => {
            let share = 1.0 - (-dt / NORMAL_TIME).exp();
            (normal + (fit.normal - normal) * share).normalized()?
        }
        None => normal.normalized()?,
    };
    let normal = clamp_ny(eased, t.overhang_ny, t.hold_ny)?;
    // The visible orientation may lag a bend. Movement belongs to the physical plane;
    // projecting onto the eased normal can manufacture outward velocity even at the correct gap.
    let motion_normal = fit.map_or(normal, |fit| fit.normal).normalized()?;
    let speed = velocity.length();
    let along_surface = velocity - motion_normal * velocity.dot(motion_normal);
    let tangent = along_surface
        .normalized()
        .map_or(Vec3::default(), |direction| direction * speed);
    let (pull, error) = match &fit {
        Some(fit) => {
            // The hit point belongs to the fresh plane. The eased normal remains a body/probe
            // direction hint; using it here invents a gap when the hit centroid moves.
            let error = fit.distance(pos, fit.normal) - standoff_target(fit.normal.y, capsule);
            (standoff_speed(error, dt, contact), error)
        }
        None => (0.0, 0.0),
    };
    Some(Steered {
        velocity: tangent + motion_normal * pull,
        normal,
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testworld::{
        face, tuning, Ball, Cylinder, Empty, Faceted, Plane, Profile, Scripted,
    };

    /// A player standing with the wall 0.8 m ahead along +x.
    const AHEAD: Vec3 = Vec3::new(1.0, 0.0, 0.0);
    /// The capsule the tests use.
    const CAPSULE: Capsule = Capsule::FALLBACK;

    /// A hit on a sheer wall at `x`, with this height.
    fn wall_hit(x: f32, y: f32) -> Hit {
        Hit {
            point: Vec3::new(x, y, 0.0),
            normal: Vec3::new(-1.0, 0.0, 0.0),
            entity: 0,
        }
    }

    /// The frame's axes are a right-handed set, and `up` lies in the surface.
    #[test]
    fn the_frame_lies_along_a_leaning_surface() {
        let normal = face(60.0);
        let frame = Frame::along(-normal).unwrap();
        assert!(frame.up.dot(normal).abs() < 1e-6, "{frame:?}");
        assert!(frame.right.dot(normal).abs() < 1e-6);
        assert!(frame.up.y > 0.8, "feet to head still points upward");
        assert!((frame.up.length() - 1.0).abs() < 1e-5);
    }

    /// Straight up and down have no sideways axis.
    #[test]
    fn a_vertical_ray_direction_is_refused() {
        assert_eq!(Frame::along(UP), None);
        assert_eq!(Frame::along(-UP), None);
        assert_eq!(Frame::along(Vec3::default()), None);
        let scan = scan(&Empty, Vec3::default(), UP, Mode::Stay, &tuning(), CAPSULE);
        assert_eq!(scan.verdict, Err(Reject::Direction));
    }

    /// Every ray starts the same distance from the surface, so the reach need not grow with the
    /// slope. This is why the patch lies along the surface.
    #[test]
    fn origins_stay_a_constant_distance_from_a_leaning_surface() {
        let normal = face(60.0);
        let frame = Frame::along(-normal).unwrap();
        let distances = frame
            .origins(Vec3::default())
            .map(|origin| origin.dot(normal));
        for distance in distances {
            assert!(distance.abs() < 1e-5, "{distances:?}");
        }
    }

    /// A sheer wall in reach starts a climb, and the player stands 0.55 m off it at the same
    /// height. This is the game's own result for a flat lattice.
    #[test]
    fn a_sheer_wall_in_reach_starts_a_climb() {
        let wall = Plane::wall(0.8);
        let scan = scan(
            &wall,
            Vec3::default(),
            AHEAD,
            Mode::Entry,
            &tuning(),
            CAPSULE,
        );
        assert_eq!(scan.hits, 4);
        let stand = scan.verdict.unwrap();
        assert!((stand.x - 0.25).abs() < 1e-5, "{stand:?}");
        assert!(stand.y.abs() < 1e-5, "{stand:?}");
        let fit = scan.fit.unwrap();
        assert!((fit.normal.x + 1.0).abs() < 1e-5);
    }

    /// The entry reach is a distance from the capsule, measured to the plane.
    #[test]
    fn a_wall_out_of_reach_starts_nothing() {
        let t = tuning();
        let just_in = Plane::wall(t.entry_reach - 0.01);
        let just_out = Plane::wall(t.entry_reach + 0.01);
        assert!(
            scan(&just_in, Vec3::default(), AHEAD, Mode::Entry, &t, CAPSULE)
                .verdict
                .is_ok()
        );
        assert_eq!(
            scan(&just_out, Vec3::default(), AHEAD, Mode::Entry, &t, CAPSULE).verdict,
            Err(Reject::TooFar)
        );
    }

    /// A slope the player can walk up never latches, however hard they push at it. The player
    /// stands on this one, so the rays are in reach of it.
    #[test]
    fn a_walkable_slope_starts_nothing() {
        let t = tuning();
        let slope = Plane::through(Vec3::new(-1.5, 0.0, 0.0), face(35.0));
        assert_eq!(
            scan(&slope, Vec3::default(), AHEAD, Mode::Entry, &t, CAPSULE).verdict,
            Err(Reject::Walkable)
        );
    }

    /// A face that leans back reaches a level ray far past its foot. It must still latch: the
    /// spread is measured along the normal, not along the ray, and the reach is measured from
    /// the capsule. Faces of 60 degrees are in the test plan and a 1 m ray cannot touch them.
    #[test]
    fn a_leaning_face_starts_a_climb() {
        let t = tuning();
        for angle in [55.0, 60.0, 75.0, 85.0] {
            let rock = Plane::through(Vec3::new(0.3, 0.0, 0.0), face(angle));
            let scan = scan(&rock, Vec3::default(), AHEAD, Mode::Entry, &t, CAPSULE);
            assert!(scan.verdict.is_ok(), "{angle}: {scan:?}");
            assert!(scan.fit.unwrap().spread < 0.01, "{angle}: {scan:?}");
        }
    }

    /// A deep overhang is refused, and a slight one is climbed.
    #[test]
    fn a_deep_overhang_starts_nothing() {
        let t = tuning();
        let deep = Plane::through(Vec3::new(1.0, 2.0, 0.0), face(125.0));
        let slight = Plane::through(Vec3::new(0.8, 2.0, 0.0), face(100.0));
        assert_eq!(
            scan(&deep, Vec3::default(), AHEAD, Mode::Entry, &t, CAPSULE).verdict,
            Err(Reject::Overhang)
        );
        assert!(
            scan(&slight, Vec3::default(), AHEAD, Mode::Entry, &t, CAPSULE)
                .verdict
                .is_ok()
        );
    }

    /// A wall lower than the upper ray row is a ledge to jump, not a wall to climb.
    #[test]
    fn a_low_wall_starts_nothing() {
        let rays = [None, None];
        let scan = scan(
            &Scripted::new(&rays),
            Vec3::default(),
            AHEAD,
            Mode::Entry,
            &tuning(),
            CAPSULE,
        );
        assert_eq!(scan.verdict, Err(Reject::Hits));
    }

    /// Open air must stay cheap: this probe runs every frame the stick is held.
    #[test]
    fn open_air_costs_two_rays() {
        let rays = [None, None, None, None];
        let world = Scripted::new(&rays);
        scan(
            &world,
            Vec3::default(),
            AHEAD,
            Mode::Entry,
            &tuning(),
            CAPSULE,
        );
        assert_eq!(world.casts(), 2);
    }

    /// Three hits are enough for a start, for instance beside a corner.
    #[test]
    fn three_hits_of_four_are_enough() {
        let rays = [
            Some(wall_hit(0.8, 2.0)),
            None,
            Some(wall_hit(0.8, 1.25)),
            Some(wall_hit(0.8, 1.25)),
        ];
        let scan = scan(
            &Scripted::new(&rays),
            Vec3::default(),
            AHEAD,
            Mode::Entry,
            &tuning(),
            CAPSULE,
        );
        assert_eq!(scan.hits, 3);
        assert!(scan.verdict.is_ok(), "{scan:?}");
    }

    /// One boulder in front of a face must not cost the whole patch.
    #[test]
    fn one_stray_hit_is_left_out() {
        let rays = [
            Some(wall_hit(0.8, 2.0)),
            Some(wall_hit(0.1, 2.0)),
            Some(wall_hit(0.8, 1.25)),
            Some(wall_hit(0.8, 1.25)),
        ];
        let scan = scan(
            &Scripted::new(&rays),
            Vec3::default(),
            AHEAD,
            Mode::Entry,
            &tuning(),
            CAPSULE,
        );
        let fit = scan.fit.unwrap();
        assert_eq!(fit.hits, 3);
        assert!(fit.spread < 0.01, "{fit:?}");
        assert!(scan.verdict.is_ok(), "{scan:?}");
    }

    /// Two rays on each of two surfaces is no surface at all.
    #[test]
    fn a_broken_patch_is_refused() {
        let rays = [
            Some(wall_hit(0.3, 2.0)),
            Some(wall_hit(0.3, 2.0)),
            Some(wall_hit(0.9, 1.25)),
            Some(wall_hit(0.9, 1.25)),
        ];
        let scan = scan(
            &Scripted::new(&rays),
            Vec3::default(),
            AHEAD,
            Mode::Entry,
            &tuning(),
            CAPSULE,
        );
        assert_eq!(scan.verdict, Err(Reject::Bumpy));
    }

    /// Moving forgives more roughness than starting, or a climb would stop at every bump.
    #[test]
    fn moving_forgives_more_than_starting() {
        let t = tuning();
        let gap = t.plane_tolerance * 1.3;
        let rays = [
            Some(wall_hit(0.55, 2.0)),
            Some(wall_hit(0.55, 2.0)),
            Some(wall_hit(0.55 + gap, 1.25)),
            Some(wall_hit(0.55 + gap, 1.25)),
        ];
        let probe = |mode| {
            scan(
                &Scripted::new(&rays),
                Vec3::default(),
                AHEAD,
                mode,
                &t,
                CAPSULE,
            )
            .verdict
        };
        assert_eq!(probe(Mode::Entry), Err(Reject::Bumpy));
        assert!(probe(Mode::Move).is_ok());
    }

    /// The hold is the last line before a drop, so it asks only that something is there.
    #[test]
    fn the_hold_takes_two_hits_on_anything() {
        let t = tuning();
        let flat = Hit {
            point: Vec3::new(0.9, 1.25, 0.0),
            normal: UP,
            entity: 0,
        };
        let rays = [None, None, Some(flat), Some(flat)];
        let scan = scan(
            &Scripted::new(&rays),
            Vec3::default(),
            AHEAD,
            Mode::Stay,
            &t,
            CAPSULE,
        );
        assert!(scan.verdict.is_ok(), "{scan:?}");
        assert!(!scan.steady, "too flat and too few hits to steer by");
    }

    /// One hit is not a wall, even for the hold.
    #[test]
    fn the_hold_refuses_one_hit() {
        let rays = [Some(wall_hit(0.8, 2.0)), None, None, None];
        let scan = scan(
            &Scripted::new(&rays),
            Vec3::default(),
            AHEAD,
            Mode::Stay,
            &tuning(),
            CAPSULE,
        );
        assert_eq!(scan.verdict, Err(Reject::Hits));
    }

    /// The hold reaches farther than a move, so a move that passed cannot be a drop next frame.
    #[test]
    fn the_hold_reaches_farther_than_a_move() {
        let t = tuning();
        let wall = Plane::wall(t.hold_reach + 0.2);
        assert_eq!(
            scan(&wall, Vec3::default(), AHEAD, Mode::Move, &t, CAPSULE).verdict,
            Err(Reject::Hits)
        );
        assert!(scan(&wall, Vec3::default(), AHEAD, Mode::Stay, &t, CAPSULE)
            .verdict
            .is_ok());
    }

    /// A rock that keeps every surface ray cast at it.
    struct Kept {
        /// The rock.
        rock: Plane,
        /// Each ray's start and end.
        rays: std::cell::RefCell<Vec<(Vec3, Vec3)>>,
    }

    impl Caster for Kept {
        fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit> {
            if mask != 0x08 {
                self.rays.borrow_mut().push((from, to));
            }
            self.rock.cast(from, to, mask)
        }
    }

    /// The rays of a running climb start 0.3 m behind their origins, and end where they always
    /// did, so an origin behind rock still finds that rock. A start keeps its rays at the origins.
    #[test]
    fn a_running_climb_starts_its_rays_behind_their_origins() {
        let t = tuning();
        let pos = Vec3::new(0.2, 1.0, 0.0);
        let origins = Frame::along(AHEAD).unwrap().origins(pos);
        let modes = [
            (Mode::Entry, 0.0, t.entry_ray),
            (Mode::Move, 0.3, t.hold_reach),
            (Mode::Stay, 0.3, t.hold_reach + STAY_EXTRA),
        ];
        for (mode, back, reach) in modes {
            let world = Kept {
                rock: Plane::wall(0.8),
                rays: Default::default(),
            };
            assert!(scan(&world, pos, AHEAD, mode, &t, CAPSULE).verdict.is_ok());
            let rays = world.rays.borrow();
            assert_eq!(rays.len(), RAY_COUNT, "{mode:?}");
            for ((from, to), origin) in rays.iter().zip(origins) {
                let start = origin - AHEAD * back;
                assert!((*from - start).length() < 1e-5, "{mode:?}: {from:?}");
                let end = origin + AHEAD * reach;
                assert!((*to - end).length() < 1e-5, "{mode:?}: {to:?}");
            }
        }
    }

    /// The first hit's entity is reported, so a prop's id reaches the game.
    #[test]
    fn the_fit_reports_the_first_entity() {
        let mut hit = wall_hit(0.8, 2.0);
        hit.entity = 4242;
        let rays = [Some(hit), Some(wall_hit(0.8, 2.0))];
        let scan = scan(
            &Scripted::new(&rays),
            Vec3::default(),
            AHEAD,
            Mode::Stay,
            &tuning(),
            CAPSULE,
        );
        assert_eq!(scan.fit.unwrap().entity, 4242);
    }

    /// The feet stay at the game's 0.55 m on a wall and on a face that leans back.
    #[test]
    fn the_standoff_is_the_games_unless_an_overhang_needs_more() {
        assert_eq!(standoff_target(0.0, CAPSULE), GAME_STANDOFF);
        assert_eq!(standoff_target(0.5, CAPSULE), GAME_STANDOFF);
        let overhang = standoff_target(-0.3, CAPSULE);
        assert!(overhang > GAME_STANDOFF, "{overhang}");
        // The top of the upright capsule then keeps its gap from the rock.
        let top = overhang + CAPSULE.top() * -0.3;
        assert!((top - (CAPSULE.radius + CAPSULE_GAP)).abs() < 1e-5, "{top}");
    }

    /// A wide capsule is never pushed into a wall by the game's number.
    #[test]
    fn a_wide_capsule_stands_farther_out() {
        let wide = Capsule::from_config(1.4, 2.0);
        assert!(standoff_target(0.0, wide) > wide.radius);
    }

    /// `width` is the diameter, and rubbish from memory gives the fallback.
    #[test]
    fn the_capsule_comes_from_the_config() {
        assert_eq!(
            Capsule::from_config(0.8, 1.9),
            Capsule {
                radius: 0.4,
                height: 1.9
            }
        );
        for (width, height) in [
            (0.0, 2.0),
            (0.8, 0.0),
            (f32::NAN, 2.0),
            (0.8, f32::INFINITY),
        ] {
            assert_eq!(Capsule::from_config(width, height), Capsule::FALLBACK);
        }
    }

    /// The fitted distance is measured along the normal it is given.
    #[test]
    fn near_distance_finds_the_end_of_the_capsule_nearest_the_surface() {
        let hit = Hit {
            point: Vec3::new(0.5, 0.0, 0.0),
            normal: face(60.0),
            entity: 0,
        };
        let fit = fit_of([hit].iter()).unwrap();
        let low = (UP * CAPSULE.radius - hit.point).dot(hit.normal);
        assert!((near_distance(Vec3::default(), &fit, CAPSULE) - low).abs() < 1e-5);
        let under = Hit {
            point: Vec3::new(0.5, 0.0, 0.0),
            normal: face(110.0),
            entity: 0,
        };
        let fit = fit_of([under].iter()).unwrap();
        let high = (UP * CAPSULE.top() - under.point).dot(under.normal);
        assert!((near_distance(Vec3::default(), &fit, CAPSULE) - high).abs() < 1e-5);
    }

    /// The steady four-hit wall used to test movement recovery independently of the new rays.
    fn held_wall_probe(candidate: Vec3) -> MoveProbe {
        MoveProbe {
            current: Vec3::new(-0.2, 0.0, 0.0),
            candidate,
            direction: AHEAD,
            held: Fit {
                normal: Vec3::new(-1.0, 0.0, 0.0),
                point: Vec3::new(1.0, 1.625, 0.0),
                hits: 4,
                spread: 0.0,
                entity: 0,
            },
        }
    }

    /// No spatial recovery can admit a parallel/away/cross-plane candidate or invalid data.
    #[test]
    fn movement_recovery_does_not_extend_other_directions_or_invalid_inputs() {
        for candidate in [
            Vec3::new(-0.2, 0.03, 0.0),
            Vec3::new(-0.22, 0.0, 0.0),
            Vec3::new(1.01, 0.0, 0.0),
            Vec3::new(f32::NAN, 0.0, 0.0),
        ] {
            let world = Scripted::new(&[None, None]);
            let answer =
                scan_move_from_hold(&world, held_wall_probe(candidate), &tuning(), CAPSULE);
            assert_eq!(answer.verdict, Err(Reject::Hits));
            assert_eq!(world.casts(), 2, "no recovery rays for {candidate:?}");
        }
        for change in 0..3 {
            let mut probe = held_wall_probe(Vec3::new(-0.18, 0.0, 0.0));
            match change {
                0 => probe.current.x = f32::INFINITY,
                1 => probe.held.point.y = f32::NAN,
                _ => probe.held.normal = Vec3::default(),
            }
            let world = Scripted::new(&[None, None]);
            assert_eq!(
                scan_move_from_hold(&world, probe, &tuning(), CAPSULE).verdict,
                Err(Reject::Hits)
            );
            assert_eq!(world.casts(), 2);
        }
    }

    /// A previous fit is never substituted for a fresh hit after the surface goes away.
    #[test]
    fn recovery_requires_fresh_geometry_and_stays_inside_the_hold_range() {
        let empty = Scripted::new(&[None, None, None, None]);
        let probe = held_wall_probe(Vec3::new(-0.18, 0.03, 0.0));
        assert_eq!(
            scan_move_from_hold(&empty, probe, &tuning(), CAPSULE).verdict,
            Err(Reject::Hits)
        );
        assert_eq!(empty.casts(), 4, "one ordinary probe and one bounded retry");
        let too_far = Plane::wall(2.0);
        assert_eq!(
            scan_move_from_hold(&too_far, probe, &tuning(), CAPSULE).verdict,
            Err(Reject::Hits)
        );
    }

    /// A wider ray never turns the hold's permissive test into movement admission.
    #[test]
    fn recovery_preserves_move_slope_and_roughness_checks() {
        let probe = held_wall_probe(Vec3::new(-0.18, 0.03, 0.0));
        let walkable = Plane::through(Vec3::new(-0.2, 1.25, 0.0), face(30.0));
        assert_eq!(
            scan_move_from_hold(&walkable, probe, &tuning(), CAPSULE).verdict,
            Err(Reject::Walkable)
        );
        let overhang = Plane::through(Vec3::new(1.13, 1.25, 0.0), face(140.0));
        assert_eq!(
            scan_move_from_hold(&overhang, probe, &tuning(), CAPSULE).verdict,
            Err(Reject::Overhang)
        );
        let bumpy: Vec<_> = [None, None]
            .into_iter()
            .chain([0.0, 0.9, 1.8, 2.7].into_iter().map(|z| {
                Some(Hit {
                    point: Vec3::new(z, 1.5, 0.0),
                    normal: Vec3::new(-1.0, 0.0, 0.0),
                    entity: 0,
                })
            }))
            .collect();
        let world = Scripted::new(&bumpy);
        assert_eq!(
            scan_move_from_hold(&world, probe, &tuning(), CAPSULE).verdict,
            Err(Reject::Bumpy)
        );
    }

    /// A non-hit rejection never triggers a recovery scan.
    #[test]
    fn a_rejected_move_is_not_retried_for_reasons_other_than_missing_hits() {
        let answers: Vec<_> = [0.0, 0.9, 1.8, 2.7]
            .into_iter()
            .map(|x| Some(wall_hit(x, 1.5)))
            .collect();
        let world = Scripted::new(&answers);
        let probe = held_wall_probe(Vec3::new(-0.18, 0.0, 0.0));
        assert_eq!(
            scan_move_from_hold(&world, probe, &tuning(), CAPSULE).verdict,
            Err(Reject::Bumpy)
        );
        assert_eq!(world.casts(), 4);
    }

    /// A confirmed hold must let a fresh movement probe admit a small correction toward it.
    #[test]
    fn a_move_can_recover_toward_its_confirmed_hold() {
        let wall = Plane::wall(1.0);
        let current = Vec3::new(-0.2, 0.0, 0.0);
        let t = tuning();
        let hold = scan(&wall, current, AHEAD, Mode::Stay, &t, CAPSULE);
        assert!(hold.verdict.is_ok() && hold.steady);
        let candidate = current + Vec3::new(0.02, 0.03, 0.0);
        assert_eq!(
            scan(&wall, candidate, AHEAD, Mode::Move, &t, CAPSULE).verdict,
            Err(Reject::Hits)
        );
        let result = scan_move_from_hold(
            &wall,
            MoveProbe {
                current,
                candidate,
                direction: AHEAD,
                held: hold.fit.unwrap(),
            },
            &t,
            CAPSULE,
        );
        assert!(result.verdict.is_ok(), "{result:?}");
    }

    /// A half-pressed stick aimed straight at the wall starts a climb. The game's own test
    /// refuses it, which is one cause of "starting a climb is inconsistent".
    #[test]
    fn a_half_pressed_stick_passes_the_cone() {
        let normal = Vec3::new(-1.0, 0.0, 0.0);
        let half = Vec3::new(0.5, 0.0, 0.0);
        assert!(cone_ok(normal, half));
        assert!(
            (-normal).dot(half) < ENTRY_CONE_COS,
            "the game's test fails"
        );
    }

    /// A leaning wall does not narrow the cone.
    #[test]
    fn a_leaning_wall_keeps_the_full_cone() {
        let normal = face(55.0);
        let heading = 40f32.to_radians();
        let input = Vec3::new(heading.cos(), 0.0, heading.sin());
        assert!(cone_ok(normal, input));
        assert!(
            (-normal).dot(input) < ENTRY_CONE_COS,
            "the game's test fails"
        );
    }

    /// The cone is still 45 degrees: running along a wall starts nothing.
    #[test]
    fn the_cone_refuses_a_stick_along_the_wall() {
        let normal = Vec3::new(-1.0, 0.0, 0.0);
        let at = |degrees: f32| {
            let heading = degrees.to_radians();
            Vec3::new(heading.cos(), 0.0, heading.sin())
        };
        assert!(cone_ok(normal, at(44.0)));
        assert!(!cone_ok(normal, at(46.0)));
        assert!(!cone_ok(normal, at(180.0)));
        assert!(!cone_ok(normal, Vec3::default()));
        assert!(!cone_ok(UP, at(0.0)));
    }

    /// A fit on the plane `normal` with the player at `distance` from it.
    fn fit_at(normal: Vec3, distance: f32) -> Fit {
        Fit {
            normal,
            point: -normal * distance,
            hits: 4,
            spread: 0.0,
            entity: 0,
        }
    }

    /// One steering frame at 60 fps from the origin.
    fn input(velocity: Vec3, normal: Vec3, fit: Option<Fit>) -> SteerInput {
        SteerInput {
            velocity,
            normal,
            fit,
            pos: Vec3::default(),
            dt: 1.0 / 60.0,
            contact: false,
        }
    }

    /// The velocity turns into the surface and keeps its speed. Without this a climber leaves
    /// a 75 degree face after 0.4 m.
    #[test]
    fn steering_keeps_the_speed_and_drops_the_normal_part() {
        let t = tuning();
        for angle in [55.0, 60.0, 75.0, 90.0, 105.0] {
            let normal = face(angle);
            let fit = fit_at(normal, standoff_target(normal.y, CAPSULE));
            let velocity = UP * 1.95;
            let out = steer(&input(velocity, normal, Some(fit)), &t, CAPSULE).unwrap();
            assert!(
                out.velocity.dot(out.normal).abs() < 1e-4,
                "{angle}: {out:?}"
            );
            assert!(
                (out.velocity.length() - 1.95).abs() < 1e-4,
                "{angle}: {out:?}"
            );
            assert!(out.velocity.y > 0.0, "{angle}: {out:?}");
        }
    }

    /// Inside the deadband nothing pulls, so a player does not hunt on rough rock.
    #[test]
    fn a_small_standoff_error_is_left_alone() {
        let t = tuning();
        let normal = face(90.0);
        for error in [-0.02, 0.0, 0.02] {
            let fit = fit_at(normal, GAME_STANDOFF + error);
            let out = steer(&input(UP, normal, Some(fit)), &t, CAPSULE).unwrap();
            assert!(out.velocity.dot(normal).abs() < 1e-5, "{error}: {out:?}");
            assert!((out.error - error).abs() < 1e-5, "{error}: {out:?}");
        }
    }

    /// Too far out pulls in, too close pushes out, and both stop at the speed limit.
    #[test]
    fn the_standoff_pull_has_a_sign_and_a_limit() {
        let t = tuning();
        let normal = face(90.0);
        let toward = |distance: f32| {
            let fit = fit_at(normal, distance);
            let out = steer(&input(UP, normal, Some(fit)), &t, CAPSULE).unwrap();
            out.velocity.dot(normal)
        };
        assert!(toward(GAME_STANDOFF + 0.1) < 0.0);
        assert!(toward(GAME_STANDOFF - 0.1) > 0.0);
        assert!((toward(GAME_STANDOFF + 0.4).abs() - STANDOFF_SPEED).abs() < 1e-4);
    }

    /// The pull never steps past the target, even on a slow server frame.
    #[test]
    fn the_standoff_pull_never_overshoots() {
        let error = 0.1;
        for dt in [1.0 / 120.0, 1.0 / 60.0, 1.0 / 20.0, 0.25, 1.0] {
            let step = standoff_speed(error, dt, false).abs() * dt;
            assert!(step <= error - STANDOFF_DEADBAND + 1e-6, "{dt}: {step}");
        }
    }

    /// The capsule already touches: pulling further in would fight collide-and-slide, which is
    /// the jitter this mod exists to avoid. Pushing out is still allowed.
    #[test]
    fn contact_stops_the_inward_pull_only() {
        assert_eq!(standoff_speed(0.2, 1.0 / 60.0, true), 0.0);
        assert!(standoff_speed(0.2, 1.0 / 60.0, false) < 0.0);
        assert!(standoff_speed(-0.2, 1.0 / 60.0, true) > 0.0);
    }

    /// The normal eases toward the surface. One frame is a small step, so one odd triangle
    /// cannot snap the body round, and it arrives within a fraction of a second.
    #[test]
    fn the_normal_eases_toward_the_surface() {
        let t = tuning();
        let from = face(90.0);
        let to = face(70.0);
        let fit = fit_at(to, GAME_STANDOFF);
        let first = steer(&input(UP, from, Some(fit)), &t, CAPSULE).unwrap();
        assert!(first.normal.dot(from) > 0.998, "{first:?}");
        assert!(first.normal.dot(to) > from.dot(to), "{first:?}");
        let mut normal = from;
        for _ in 0..60 {
            normal = steer(&input(UP, normal, Some(fit)), &t, CAPSULE)
                .unwrap()
                .normal;
        }
        assert!(normal.dot(to) > 0.9999, "{normal:?}");
    }

    /// Without a fit the normal stands and only the direction is corrected.
    #[test]
    fn without_a_fit_only_the_direction_changes() {
        let t = tuning();
        let normal = face(70.0);
        let out = steer(&input(UP, normal, None), &t, CAPSULE).unwrap();
        assert!(out.normal.dot(normal) > 0.999_999);
        assert!(out.velocity.dot(normal).abs() < 1e-5);
        assert_eq!(out.error, 0.0);
    }

    /// The stored normal stays inside what `actor_rotation` can turn the body to. Too much up
    /// logs "view parallel to up" and spins the player.
    #[test]
    fn the_normal_is_kept_off_straight_up_and_down() {
        let t = tuning();
        let high = fit_at(UP, GAME_STANDOFF);
        let mut normal = face(60.0);
        for _ in 0..600 {
            normal = steer(&input(UP, normal, Some(high)), &t, CAPSULE)
                .unwrap()
                .normal;
        }
        assert!(normal.y <= t.hold_ny + 1e-5, "{normal:?}");
        assert!((normal.length() - 1.0).abs() < 1e-5);
    }

    /// Broken input leaves the game's velocity alone, and no NaN reaches an ECS component.
    #[test]
    fn broken_input_changes_nothing() {
        let t = tuning();
        let normal = face(90.0);
        let mut bad = input(UP, normal, None);
        bad.dt = 0.0;
        assert_eq!(steer(&bad, &t, CAPSULE), None);
        bad.dt = f32::NAN;
        assert_eq!(steer(&bad, &t, CAPSULE), None);
        let mut bad = input(Vec3::new(f32::NAN, 0.0, 0.0), normal, None);
        assert_eq!(steer(&bad, &t, CAPSULE), None);
        bad.velocity = UP;
        bad.normal = Vec3::default();
        assert_eq!(steer(&bad, &t, CAPSULE), None);
    }

    /// A collider that only mask bit 3 sees, and the rock.
    struct InFront {
        /// The Solid collider.
        prop: Plane,
        /// The surface.
        rock: Plane,
    }

    impl Caster for InFront {
        fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit> {
            if mask == 0x08 {
                self.prop.cast(from, to, mask)
            } else {
                self.rock.cast(from, to, mask)
            }
        }
    }

    /// The surface rays do not see a Solid-only collider. A tree, a fence or an enemy in front of
    /// the rock must not let the player latch through it.
    #[test]
    fn a_solid_collider_in_the_way_starts_nothing() {
        let t = tuning();
        let world = InFront {
            prop: Plane::wall(0.5),
            rock: Plane::wall(0.8),
        };
        let scan = scan(&world, Vec3::default(), AHEAD, Mode::Entry, &t, CAPSULE);
        assert_eq!(scan.verdict, Err(Reject::Blocked));
    }

    /// A Solid collider across one part of the path, as a range of z, and the rock behind it.
    struct Post(std::ops::Range<f32>);

    impl Caster for Post {
        fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit> {
            let solid = mask == 0x08;
            let wall = if solid { 0.5 } else { 0.8 };
            let hit = Plane::wall(wall).cast(from, to, mask);
            hit.filter(|_| !solid || self.0.contains(&from.z))
        }
    }

    /// A thin trunk straight ahead fits between the side rays, so the ray from the chest must see
    /// it. A trunk at the side must not slip past that one ray.
    #[test]
    fn a_trunk_in_the_path_starts_nothing() {
        let t = tuning();
        for path in [-0.1..0.1, 0.2..0.5, -0.5..-0.2] {
            let world = Post(path);
            let scan = scan(&world, Vec3::default(), AHEAD, Mode::Entry, &t, CAPSULE);
            assert_eq!(scan.verdict, Err(Reject::Blocked), "{:?}", world.0);
        }
    }

    /// A rock prop can be Solid and Walkable at once, and rough rock can stand a little proud of
    /// a hit. The corner rays stop short of each hit, and the chest ray short of the fit plane, so
    /// rock does not block itself.
    #[test]
    fn a_solid_collider_at_the_surface_changes_nothing() {
        let t = tuning();
        let world = InFront {
            prop: Plane::wall(0.79),
            rock: Plane::wall(0.8),
        };
        let scan = scan(&world, Vec3::default(), AHEAD, Mode::Entry, &t, CAPSULE);
        assert!(scan.verdict.is_ok(), "{scan:?}");
    }

    /// Height of the chest ray above the feet: halfway between the ray rows.
    const CHEST: f32 = 1.625;

    /// Round rock stands proud of the plane through the four hits at the chest, 0.06 m on an
    /// upright rock of radius 1 and 0.14 m on a ball of radius 1. These worlds answer every mask,
    /// like a rock prop that is Solid and Walkable at once, and that rock must not block itself.
    #[test]
    fn round_solid_rock_does_not_block_itself() {
        let t = tuning();
        for radius in [1.0, 2.0, 3.0] {
            let rock = Cylinder::new(Vec3::new(radius + 0.6, 0.0, 0.0), radius);
            let scan = scan(&rock, Vec3::default(), AHEAD, Mode::Entry, &t, CAPSULE);
            assert!(scan.verdict.is_ok(), "radius {radius}: {scan:?}");
        }
        for radius in [1.0, 1.5, 2.0, 3.0, 4.0, 6.0] {
            let rock = Ball::new(Vec3::new(radius + 0.6, CHEST, 0.0), radius);
            let scan = scan(&rock, Vec3::default(), AHEAD, Mode::Entry, &t, CAPSULE);
            assert!(scan.verdict.is_ok(), "ball {radius}: {scan:?}");
        }
    }

    /// The same rock with no Solid in it, as voxel terrain has none.
    struct NoSolid<'a>(&'a Faceted);

    impl Caster for NoSolid<'_> {
        fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit> {
            (mask != 0x08)
                .then(|| self.0.cast(from, to, mask))
                .flatten()
        }
    }

    /// Rough rock that answers mask 0x08 too. A start that passes with no Solid in the rock must
    /// pass with it, at every steepness and over the whole reach.
    #[test]
    fn rough_solid_rock_does_not_block_itself() {
        let t = tuning();
        let mut starts = 0;
        for angle in [90.0f32, 75.0, 60.0, 52.0, 100.0] {
            let run = 1.0 / angle.to_radians().tan();
            for depth in [0.05, 0.1, 0.15] {
                for seed in 1..=20 {
                    let rock = Faceted::face(angle, depth, seed);
                    for step in 0..40 {
                        let z = -1.5 + step as f32 * 0.075;
                        let y = 1.0 + step as f32 * 0.05;
                        for far in [0.5, 0.7, 0.9] {
                            let pos = Vec3::new(run * (y + CHEST) - far, y, z);
                            let plain = scan(&NoSolid(&rock), pos, AHEAD, Mode::Entry, &t, CAPSULE);
                            if plain.verdict.is_ok() {
                                starts += 1;
                                let solid = scan(&rock, pos, AHEAD, Mode::Entry, &t, CAPSULE);
                                assert_eq!(
                                    solid.verdict, plain.verdict,
                                    "{angle} degrees, +-{depth} m, seed {seed}, {pos:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
        assert!(
            starts > 34_000,
            "the grid must really start climbs: {starts}"
        );
    }

    /// A thin Solid trunk between the side rays, with its front `out` metres in front of a rock
    /// through `at`. Only the chest ray can meet it.
    struct Trunk {
        /// The rock.
        rock: Plane,
        /// The front of the trunk.
        front: Plane,
    }

    impl Trunk {
        /// The trunk in front of the plane through `at` with `normal`.
        fn out_of(at: Vec3, normal: Vec3, out: f32) -> Trunk {
            Trunk {
                rock: Plane::through(at, normal),
                front: Plane::through(at + normal * out, normal),
            }
        }
    }

    impl Caster for Trunk {
        fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit> {
            if mask == 0x08 {
                let middle = from.z.abs() < 0.1;
                self.front.cast(from, to, mask).filter(|_| middle)
            } else {
                self.rock.cast(from, to, mask)
            }
        }
    }

    /// The chest ray stops 0.2 m off the fit plane, measured along the normal. Rough rock in the
    /// tests stands up to 0.184 m proud of its plane head on, so less than that blocks the rock
    /// itself. A trunk just past the margin still blocks, and the same trunk just inside it does
    /// not. This holds at normal reach on a face that leans either way, so the margin is pinned
    /// both ways.
    #[test]
    fn the_chest_ray_stops_two_tenths_off_the_fit_plane() {
        let t = tuning();
        for angle in [90.0, 75.0, 60.0, 52.0, 105.0] {
            let normal = face(angle);
            let rock = Vec3::new(0.0, CHEST, 0.0) - normal * 0.8;
            let verdict = |out: f32| {
                let world = Trunk::out_of(rock, normal, out);
                scan(&world, Vec3::default(), AHEAD, Mode::Entry, &t, CAPSULE).verdict
            };
            assert_eq!(verdict(0.21), Err(Reject::Blocked), "{angle}");
            assert!(verdict(0.19).is_ok(), "{angle}");
        }
    }

    /// A Solid post in the path of one side ray, `out` metres in front of the rock at 0.8 m.
    struct SidePost(f32);

    impl Caster for SidePost {
        fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit> {
            let solid = mask == 0x08;
            let wall = if solid { 0.8 - self.0 } else { 0.8 };
            let hit = Plane::wall(wall).cast(from, to, mask);
            hit.filter(|_| !solid || from.z > 0.2)
        }
    }

    /// A side ray has a hit of its own, so it looks almost all the way to it. The wide margin is
    /// only for the chest ray. A post right in front of the rock blocks on that ray, and the gap
    /// of 0.02 m is pinned from both sides.
    #[test]
    fn a_side_ray_looks_almost_to_its_hit() {
        let t = tuning();
        let verdict = |out: f32| {
            let world = SidePost(out);
            scan(&world, Vec3::default(), AHEAD, Mode::Entry, &t, CAPSULE).verdict
        };
        assert_eq!(verdict(0.1), Err(Reject::Blocked));
        assert_eq!(verdict(0.03), Err(Reject::Blocked));
        assert!(verdict(0.01).is_ok());
    }

    /// Hits that say the surface lies almost along the ray put the fit plane far down that ray.
    /// The chest ray must stop at the farthest hit, or a bad normal sends a ray of 300 km into the
    /// game.
    #[test]
    fn a_fit_along_the_ray_gives_no_endless_chest_ray() {
        /// Answers every surface ray with one hit, and keeps the longest Solid ray.
        struct AlongTheRay(std::cell::Cell<f32>);

        impl Caster for AlongTheRay {
            fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit> {
                if mask == 0x08 {
                    self.0.set(self.0.get().max((to - from).length()));
                    return None;
                }
                Some(Hit {
                    point: Vec3::new(0.8, CHEST, -0.5),
                    normal: Vec3::new(-1e-6, 0.0, 1.0),
                    entity: 0,
                })
            }
        }

        let t = tuning();
        let world = AlongTheRay(std::cell::Cell::new(0.0));
        let scan = scan(&world, Vec3::default(), AHEAD, Mode::Entry, &t, CAPSULE);
        assert!(scan.verdict.is_ok(), "{scan:?}");
        assert!((world.0.get() - 0.8).abs() < 1e-3, "{}", world.0.get());
    }

    /// Only a start asks. During a climb the capsule itself meets what stands in the way.
    #[test]
    fn a_running_climb_casts_no_solid_ray() {
        let t = tuning();
        let world = InFront {
            prop: Plane::wall(0.5),
            rock: Plane::wall(0.8),
        };
        for mode in [Mode::Move, Mode::Stay] {
            let scan = scan(&world, Vec3::default(), AHEAD, mode, &t, CAPSULE);
            assert!(scan.verdict.is_ok(), "{mode:?}: {scan:?}");
        }
    }

    /// Fresh analytic scans keep the old running allowance, but never admit past 160 degrees.
    #[test]
    fn configured_overhang_caps_move_admission_at_160() {
        for (configured, move_limit) in [
            (90.0_f32, 105.0_f32),
            (110.0, 125.0),
            (125.0, 140.0),
            (135.0, 150.0),
            (145.0, 160.0),
            (155.0, 160.0),
            (160.0, 160.0),
        ] {
            let (settings, warnings) =
                crate::config::parse(&format!("[Tuning]\nOverhangAngle={configured}\n"));
            assert!(warnings.is_empty(), "{configured}: {warnings:?}");
            let t = settings.tuning();
            for (angle, mode, allowed) in [
                (configured - 0.1, Mode::Entry, true),
                (configured + 0.1, Mode::Entry, false),
                (move_limit - 0.1, Mode::Move, true),
                (move_limit + 0.1, Mode::Move, false),
            ] {
                let normal = face(angle);
                let gap = standoff_target(normal.y, CAPSULE);
                let rock = Plane::through(-normal * gap, normal);
                let result = scan(&rock, Vec3::default(), -normal, mode, &t, CAPSULE);
                if allowed {
                    assert!(
                        result.verdict.is_ok(),
                        "{configured}/{angle}/{mode:?}: {result:?}"
                    );
                    assert!(result.steady, "{configured}/{angle}: {result:?}");
                } else {
                    assert_eq!(
                        result.verdict,
                        Err(Reject::Overhang),
                        "{configured}/{angle}/{mode:?}: {result:?}"
                    );
                }
            }
            assert!((t.move_overhang_ny - move_limit.to_radians().cos()).abs() < 1e-6);
        }
    }

    /// Whatever the settings, the climb normal stays a direction a ray patch can be built from.
    /// At `WalkableAngle` 30 the hold limit alone would pass the limit of the rays, and the next
    /// hold would drop the player.
    #[test]
    fn the_normal_limits_stay_inside_the_ray_limit() {
        for walkable_angle in [30.0, 33.0, 40.0, 50.0, 75.0] {
            for overhang_angle in [90.0, 110.0, 135.0, 145.0, 155.0, 160.0] {
                let settings = crate::config::Settings {
                    walkable_angle,
                    overhang_angle,
                    ..Default::default()
                };
                let t = settings.tuning();
                for ny in [t.hold_ny, t.move_overhang_ny] {
                    let normal = Vec3::new(-(1.0 - ny * ny).sqrt(), ny, 0.0);
                    assert!(Frame::along(-normal).is_some(), "{walkable_angle}: {ny}");
                }
                assert!(t.hold_ny > t.walkable_ny, "{walkable_angle}: {t:?}");
            }
        }
    }

    /// At the overhang limit the feet hang about 1 m out. The rays must still reach the surface,
    /// with the same margin as on a sheer wall, whatever the capsule.
    #[test]
    fn the_rays_reach_an_overhang_at_the_limit() {
        for overhang in [100.0, 110.0, 125.0, 135.0, 145.0, 155.0, 160.0] {
            let settings = crate::config::Settings {
                overhang_angle: overhang,
                ..Default::default()
            };
            let t = settings.tuning();
            let normal = face(overhang);
            let wide = Capsule::from_config(1.0, 2.2);
            for capsule in [CAPSULE, wide] {
                let out = standoff_target(normal.y, capsule) + 0.3;
                let rock = Plane::through(Vec3::default() - normal * out, normal);
                let scan = scan(&rock, Vec3::default(), -normal, Mode::Move, &t, capsule);
                assert!(scan.verdict.is_ok(), "{overhang}/{capsule:?}: {scan:?}");
            }
        }
    }

    /// Read the development limit through the real settings boundary.
    fn development_overhang_tuning() -> Tuning {
        let (settings, warnings) = crate::config::parse("[Tuning]\nOverhangAngle=160\n");
        assert!(warnings.is_empty(), "{warnings:?}");
        settings.tuning()
    }

    /// The pure steering model preserves native input speed along the fresh plane, adding only
    /// the bounded normal correction. This does not model native body shape or collision.
    fn assert_overhang_motion(out: Steered, raw: Vec3, physical: Vec3, dt: f32) {
        assert!(out.velocity.is_finite() && out.normal.is_finite() && out.error.is_finite());
        let correction = out.velocity.dot(physical);
        let tangent = out.velocity - physical * correction;
        if out.error.abs() <= STANDOFF_DEADBAND {
            assert!(
                correction.abs() < 2e-5,
                "fresh-plane motion must be tangent: {out:?}"
            );
        }
        assert!((tangent.length() - raw.length()).abs() < 2e-4, "{out:?}");
        assert!(correction.abs() <= STANDOFF_SPEED + 2e-5, "{out:?}");
        assert!(
            correction.abs() * dt <= (out.error.abs() - STANDOFF_DEADBAND).max(0.0) + 2e-5,
            "normal correction must not overshoot: {out:?}"
        );
        assert!(
            out.velocity.length() <= raw.length().hypot(STANDOFF_SPEED) + 2e-4,
            "{out:?}"
        );
        let basis = Frame::along(-out.normal).expect("steered direction keeps a finite ray basis");
        for axis in [basis.along, basis.up, basis.right] {
            assert!(axis.is_finite() && (axis.length() - 1.0).abs() < 1e-5);
        }
        assert!(basis.along.dot(basis.up).abs() < 1e-5);
        assert!(basis.along.dot(basis.right).abs() < 1e-5);
        assert!(basis.up.dot(basis.right).abs() < 1e-5);
    }

    /// Repeated scans and accepted velocity steps retain a high overhang at each frame rate.
    /// Both capsules are configuration models; neither is a stock-body clearance oracle.
    #[test]
    fn overhang_closed_loops_keep_fresh_support() {
        let t = development_overhang_tuning();
        let raw = UP * 1.95;
        for angle in [135.0, 145.0, 155.0, 160.0] {
            let physical = face(angle);
            for dt in [1.0 / 120.0, 1.0 / 60.0, 1.0 / 20.0] {
                for capsule in [CAPSULE, Capsule::from_config(1.0, 2.2)] {
                    let gap = standoff_target(physical.y, capsule);
                    let rock = Plane::through(Vec3::default(), physical);
                    let start = physical * (gap + 0.18);
                    let mut pos = start;
                    let mut normal = physical;
                    for step in 0..(4.0 / dt) as u32 {
                        let hold = scan(&rock, pos, -normal, Mode::Stay, &t, capsule);
                        assert!(
                            hold.verdict.is_ok() && hold.steady,
                            "{angle}/{dt}/{step}: {hold:?}"
                        );
                        let out = steer(
                            &SteerInput {
                                velocity: raw,
                                normal,
                                fit: hold.fit,
                                pos,
                                dt,
                                contact: false,
                            },
                            &t,
                            capsule,
                        )
                        .unwrap();
                        assert_overhang_motion(out, raw, physical, dt);
                        let candidate = pos + out.velocity * dt;
                        let movement = scan(&rock, candidate, -out.normal, Mode::Move, &t, capsule);
                        assert!(
                            movement.verdict.is_ok(),
                            "{angle}/{dt}/{step}: {movement:?}"
                        );
                        pos = candidate;
                        normal = out.normal;
                    }
                    let error = pos.dot(physical) - gap;
                    assert!(
                        error.abs() <= STANDOFF_DEADBAND + 1e-4,
                        "{angle}/{dt}: {error}"
                    );
                    let along = Frame::along(-physical).unwrap().up;
                    assert!((pos - start).dot(along) > 7.0, "{angle}/{dt}: {pos:?}");
                }
            }
        }
    }

    /// A gradually changing static profile must still be rediscovered from the updated pose.
    /// Its ray fit is geometry evidence only, not native volume or mantle-path clearance.
    #[test]
    fn overhang_smooth_inclination_loop_keeps_fresh_support() {
        let t = development_overhang_tuning();
        for target in [135.0_f32, 145.0, 155.0, 160.0] {
            let mut points = vec![(0.0, -100.0), (0.0, 0.0)];
            let (mut x, mut y) = (0.0_f32, 0.0_f32);
            let arc_step = (target - 90.0).to_radians() * 20.0 / 140.0;
            for step in 1..=140 {
                let angle = (90.0 + (target - 90.0) * step as f32 / 140.0).to_radians();
                x += angle.cos() * arc_step;
                y += angle.sin() * arc_step;
                points.push((x, y));
            }
            let end = target.to_radians();
            points.push((x + end.cos() * 100.0, y + end.sin() * 100.0));
            let rock = Profile::new(points);
            for dt in [1.0 / 120.0, 1.0 / 60.0, 1.0 / 20.0] {
                let mut pos = Vec3::new(-GAME_STANDOFF, -3.0, 0.0);
                let mut normal = face(90.0);
                let mut reached = false;
                for step in 0..(20.0 / dt) as u32 {
                    let hold = scan(&rock, pos, -normal, Mode::Stay, &t, CAPSULE);
                    assert!(
                        hold.verdict.is_ok() && hold.steady,
                        "{target}/{dt}/{step}: {hold:?}"
                    );
                    let physical = hold.fit.unwrap().normal;
                    reached |= physical.y < face(target - 0.1).y;
                    let out = steer(
                        &SteerInput {
                            velocity: UP * 1.95,
                            normal,
                            fit: hold.fit,
                            pos,
                            dt,
                            contact: false,
                        },
                        &t,
                        CAPSULE,
                    )
                    .unwrap();
                    assert_overhang_motion(out, UP * 1.95, physical, dt);
                    let candidate = pos + out.velocity * dt;
                    let movement = scan(&rock, candidate, -out.normal, Mode::Move, &t, CAPSULE);
                    assert!(
                        movement.verdict.is_ok(),
                        "{target}/{dt}/{step}: {movement:?}"
                    );
                    pos = candidate;
                    normal = out.normal;
                }
                assert!(
                    reached,
                    "never reached the {target} degree part at dt={dt}: {pos:?}"
                );
            }
        }
    }

    /// High-overhang movement belongs to the physical fit even while the visual normal eases.
    #[test]
    fn overhang_motion_bounds_ignore_facing_lag() {
        let t = development_overhang_tuning();
        let raw = UP * 1.95 + Vec3::new(0.0, 0.0, 0.35);
        for angle in [135.0, 145.0, 155.0, 160.0] {
            let physical = face(angle);
            for error in [-0.18, 0.0, 0.18] {
                let fit = fit_at(physical, standoff_target(physical.y, CAPSULE) + error);
                for dt in [1.0 / 120.0, 1.0 / 60.0, 1.0 / 20.0] {
                    for contact in [false, true] {
                        let mut reference = input(raw, physical, Some(fit));
                        reference.dt = dt;
                        reference.contact = contact;
                        let mut lagged = reference;
                        lagged.normal = face(90.0);
                        let expected = steer(&reference, &t, CAPSULE).unwrap();
                        let actual = steer(&lagged, &t, CAPSULE).unwrap();
                        assert_overhang_motion(actual, raw, physical, dt);
                        assert!((actual.error - error).abs() < 1e-5);
                        assert!((actual.velocity - expected.velocity).length() < 1e-5);
                        assert!(
                            (actual.normal - physical).length() > 0.01,
                            "visual facing still eases"
                        );
                        let correction = actual.velocity.dot(physical);
                        if error > STANDOFF_DEADBAND && !contact {
                            assert!(correction < 0.0, "{actual:?}");
                        } else if error < -STANDOFF_DEADBAND {
                            assert!(correction > 0.0, "{actual:?}");
                        } else {
                            assert!(correction.abs() < 1e-5, "{actual:?}");
                        }
                    }
                }
            }
        }
    }

    /// Extending the development limit does not grant missing support or near-vertical rays.
    #[test]
    fn overhang_support_loss_and_near_vertical_directions_remain_refused() {
        let t = development_overhang_tuning();
        for angle in [135.0, 145.0, 155.0, 160.0] {
            let physical = face(angle);
            let rock = Plane::through(-physical * standoff_target(physical.y, CAPSULE), physical);
            let basis = Frame::along(-physical).unwrap();
            let hits = basis.origins(Vec3::default()).map(|origin| {
                rock.cast(
                    origin + physical * RAY_BACK,
                    origin - physical * 4.0,
                    t.mask,
                )
            });
            assert!(hits.iter().all(Option::is_some));
            let only_two = [hits[0], hits[1], None, None];
            let moving = scan(
                &Scripted::new(&only_two),
                Vec3::default(),
                -physical,
                Mode::Move,
                &t,
                CAPSULE,
            );
            assert_eq!(moving.verdict, Err(Reject::Hits));
            let held = scan(
                &Scripted::new(&only_two),
                Vec3::default(),
                -physical,
                Mode::Stay,
                &t,
                CAPSULE,
            );
            assert!(held.verdict.is_ok() && !held.steady);
            let only_one = [hits[0], None, None, None];
            let lost = scan(
                &Scripted::new(&only_one),
                Vec3::default(),
                -physical,
                Mode::Stay,
                &t,
                CAPSULE,
            );
            assert_eq!(lost.verdict, Err(Reject::Hits));
            assert!(lost.fit.is_none());
        }
        let ceiling = Plane::through(UP * 2.5, -UP);
        for mode in [Mode::Entry, Mode::Move] {
            let result = scan(&ceiling, Vec3::default(), -face(160.0), mode, &t, CAPSULE);
            assert_eq!(result.verdict, Err(Reject::Overhang));
        }
        let held_ceiling = scan(
            &ceiling,
            Vec3::default(),
            -face(160.0),
            Mode::Stay,
            &t,
            CAPSULE,
        );
        assert!(held_ceiling.verdict.is_ok() && !held_ceiling.steady);
        for angle in [162.0, 175.0, 180.0] {
            for mode in [Mode::Entry, Mode::Move, Mode::Stay] {
                let world = Scripted::new(&[]);
                let result = scan(&world, Vec3::default(), -face(angle), mode, &t, CAPSULE);
                assert_eq!(result.verdict, Err(Reject::Direction));
                assert_eq!(world.casts(), 0);
            }
        }
    }

    /// A curved rock gives a usable fit, and a steady one to steer by.
    #[test]
    fn a_curved_rock_fits() {
        let rock = Cylinder::new(Vec3::new(2.55, 0.0, 0.0), 2.0);
        let t = tuning();
        let scan = scan(&rock, Vec3::default(), AHEAD, Mode::Move, &t, CAPSULE);
        assert_eq!(scan.hits, 4);
        assert!(scan.steady, "{scan:?}");
        assert!(scan.verdict.is_ok(), "{scan:?}");
    }

    /// A bend changes the eased heading, not the actual distance to the newly sampled wall.
    #[test]
    fn a_correct_gap_on_a_bend_does_not_pull_when_the_hit_count_changes() {
        let t = tuning();
        let old_normal = Vec3::new(-1.0, 0.0, 0.0);
        let origins = Frame::along(AHEAD).unwrap().origins(Vec3::default());
        for yaw in [20.0f32, 30.0, 45.0] {
            let turn = yaw.to_radians();
            let fresh_normal = Vec3::new(-turn.cos(), 0.0, -turn.sin());
            let rock = Plane::through(-fresh_normal * GAME_STANDOFF, fresh_normal);
            let answers = origins.map(|origin| {
                rock.cast(
                    origin - AHEAD * Mode::Stay.back(),
                    origin + AHEAD * Mode::Stay.reach(&t),
                    t.mask,
                )
            });
            assert!(answers.iter().all(Option::is_some));
            for omit in 0..=RAY_COUNT {
                let mut kept = answers;
                if omit < RAY_COUNT {
                    kept[omit] = None;
                }
                let world = Scripted::new(&kept);
                let fresh = scan(&world, Vec3::default(), AHEAD, Mode::Stay, &t, CAPSULE);
                assert!(fresh.verdict.is_ok() && fresh.steady);
                assert_eq!(world.casts(), RAY_COUNT);
                for dt in [1.0 / 120.0, 1.0 / 60.0, 1.0 / 20.0] {
                    let mut frame = input(UP * 1.95, old_normal, fresh.fit);
                    frame.dt = dt;
                    let out = steer(&frame, &t, CAPSULE).unwrap();
                    assert!(
                        out.error.abs() < 1e-5,
                        "yaw{yaw}, missing{omit},dt{dt}: {out:?}"
                    );
                    assert!(
                        out.velocity.dot(out.normal).abs() < 1e-5,
                        "no invented pull: {out:?}"
                    );
                    assert!((out.velocity.length() - 1.95).abs() < 1e-5);
                    assert!(out.normal.dot(old_normal) > old_normal.dot(fresh_normal));
                }
            }
        }
    }

    /// Any point on one plane must produce the same measured gap during normal smoothing.
    #[test]
    fn changing_the_hit_centroid_on_one_plane_does_not_change_the_pull() {
        let t = tuning();
        for (old, fresh) in [(90.0, 60.0), (60.0, 90.0), (90.0, 105.0), (105.0, 90.0)] {
            let normal = face(fresh);
            let plane = Frame::along(-normal).unwrap();
            for gap_error in [-0.1, 0.0, 0.1] {
                let base = fit_at(normal, standoff_target(normal.y, CAPSULE) + gap_error);
                for dt in [1.0 / 120.0, 1.0 / 60.0, 1.0 / 20.0] {
                    let mut frame = input(UP * 1.95, face(old), Some(base));
                    frame.dt = dt;
                    let reference = steer(&frame, &t, CAPSULE).unwrap();
                    for along in [-0.35, 0.0, 0.35, 1.625] {
                        frame.fit = Some(Fit {
                            point: base.point + plane.up * along + plane.right * 0.35,
                            ..base
                        });
                        let out = steer(&frame, &t, CAPSULE).unwrap();
                        assert!(
                            (out.error - gap_error).abs() < 1e-5,
                            "{old}->{fresh}, shift{along}: {out:?}"
                        );
                        assert!((out.velocity - reference.velocity).length() < 1e-5);
                        assert_eq!(out.normal, reference.normal);
                    }
                }
            }
        }
    }

    /// Upright capsule clearance belongs to the physical overhang while body rotation catches up.
    #[test]
    fn an_overhang_uses_its_fresh_capsule_gap_during_normal_smoothing() {
        let t = tuning();
        for angle in [100.0, 105.0, 110.0, 115.0] {
            let normal = face(angle);
            let gap = standoff_target(normal.y, CAPSULE);
            let fit = fit_at(normal, gap);
            let out = steer(&input(UP * 1.95, face(90.0), Some(fit)), &t, CAPSULE).unwrap();
            assert!(out.error.abs() < 1e-5, "{angle}: {out:?}");
            // Motion follows the fresh wall while the body normal still eases.
            assert!(out.velocity.dot(normal).abs() < 1e-5);
            assert!(
                out.normal.y > normal.y,
                "body still eases rather than snapping"
            );
            if angle > 110.0 {
                let capped = steer(&input(UP * 1.95, face(110.0), Some(fit)), &t, CAPSULE).unwrap();
                assert!(
                    capped.error.abs() < 1e-5,
                    "clamped direction still uses physical clearance"
                );
                assert!((capped.normal.y - t.overhang_ny).abs() < 1e-5);
            }
        }
    }

    /// A changing physical plane must not turn lateral input into an outward push while the
    /// character's visible orientation is still catching up.
    #[test]
    fn motion_on_a_bend_has_no_outward_component_from_eased_facing() {
        let old = Vec3::new(0.0, 0.0, -1.0);
        let velocity = UP.cross(old) * 1.95;
        for degrees in [15.0_f32, 30.0, 45.0, 60.0] {
            let yaw = degrees.to_radians();
            let physical = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
            for dt in [1.0 / 120.0, 1.0 / 60.0, 1.0 / 20.0] {
                for contact in [false, true] {
                    let mut frame = input(velocity, old, Some(fit_at(physical, GAME_STANDOFF)));
                    frame.dt = dt;
                    frame.contact = contact;
                    let out = steer(&frame, &tuning(), CAPSULE).unwrap();
                    assert!(
                        out.velocity.dot(physical).abs() < 1e-5,
                        "orientation lag pushed across the physical wall: yaw={degrees} dt={dt} contact={contact} output={out:?}"
                    );
                    assert!((out.velocity.length() - velocity.length()).abs() < 2e-5);
                    assert!(
                        (out.normal - physical).length() > 0.01,
                        "visible facing must still ease"
                    );
                }
            }
        }
    }

    /// Movement and the gap correction on one physical plane are invariant under a different
    /// visual facing. This also covers real gap errors, contact suppression and overhangs.
    #[test]
    fn motion_on_one_plane_is_independent_of_facing_lag() {
        for degrees in [60.0_f32, 90.0, 110.0, 115.0] {
            let angle = degrees.to_radians();
            let physical = Vec3::new(-angle.sin(), angle.cos(), 0.0);
            for error in [-0.18, 0.0, 0.18] {
                let fit = fit_at(physical, standoff_target(physical.y, CAPSULE) + error);
                for dt in [1.0 / 120.0, 1.0 / 60.0, 1.0 / 20.0] {
                    for contact in [false, true] {
                        let velocity = UP * 1.95 + Vec3::new(0.0, 0.0, 0.35);
                        let mut reference = input(velocity, physical, Some(fit));
                        reference.dt = dt;
                        reference.contact = contact;
                        let mut lagged = reference;
                        lagged.normal = Vec3::new(0.0, 0.0, -1.0);
                        let expected = steer(&reference, &tuning(), CAPSULE).unwrap();
                        let actual = steer(&lagged, &tuning(), CAPSULE).unwrap();
                        assert!(
                            (actual.velocity - expected.velocity).length() < 1e-4,
                            "one plane changed movement with visual facing: slope={degrees} error={error} dt={dt} contact={contact} expected={expected:?} actual={actual:?}"
                        );
                        assert!((actual.normal - expected.normal).length() > 0.01);
                    }
                }
            }
        }
    }

    /// Fresh discovery preserves the actual ray slots, including misses, without substituting
    /// averaged fit points or ray origins. It does not run an extra raycast.
    #[test]
    fn completed_scan_keeps_ordered_fresh_ray_witnesses() {
        let origins = Frame::along(AHEAD).unwrap().origins(Vec3::default());
        let mut rays = origins.map(|origin| {
            Some(Hit {
                point: origin + AHEAD * 0.8,
                normal: -AHEAD,
                entity: 0,
            })
        });
        rays[1] = None;
        let result = scan(
            &Scripted::new(&rays),
            Vec3::default(),
            AHEAD,
            Mode::Stay,
            &tuning(),
            CAPSULE,
        );
        assert!(result.verdict.is_ok());
        assert_eq!(result.samples, rays);
    }
}
