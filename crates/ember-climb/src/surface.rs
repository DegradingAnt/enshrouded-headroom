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
/// The collider flag Solid (bit 3). The default surface mask does not ask for it.
const MASK_SOLID: u32 = 0x08;
/// How far short of a hit the clearance rays stop, in metres. A rock prop can be Solid and Walkable
/// at once, and must not block itself.
const CLEARANCE_GAP: f32 = 0.02;
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
        let past_overhang = (overhang_angle + OVERHANG_MARGIN_DEGREES).to_radians();
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
            hits,
            fit: None,
            verdict: Err(why),
            steady: false,
        }
    }
}

/// The axes of one ray patch. For a climb they lie along the surface, because the game turns
/// the whole body to face it (`FUN_14021c730`, "Climbing").
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

    /// Where the rays start for a player at `pos`, upper row first.
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
/// The game keeps 0.55 m, and the body lies along the surface at the same distance. The capsule
/// stays upright, so an overhang tips its top toward the rock; there the feet stay farther out.
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
/// each hit. A ray from the chest covers the gap between the side rays, where a thin trunk fits.
fn blocked<C: Caster>(caster: &C, frame: &Frame, pos: Vec3, starts: &[Vec3], hits: &[Hit]) -> bool {
    let [top, low] = RAY_ROWS;
    let chest = pos + frame.up * ((top + low) * 0.5);
    let rays = || {
        starts
            .iter()
            .zip(hits)
            .map(|(start, hit)| (*start, (hit.point - *start).dot(frame.along)))
    };
    let nearest = rays().map(|(_, length)| length).fold(f32::MAX, f32::min);
    let in_way = |start: Vec3, length: f32| {
        let end = start + frame.along * (length - CLEARANCE_GAP);
        length > CLEARANCE_GAP && caster.cast(start, end, MASK_SOLID).is_some()
    };
    in_way(chest, nearest) || rays().any(|(start, length)| in_way(start, length))
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
    let mut hits = 0usize;
    let mut misses = 0usize;
    for origin in frame.origins(pos) {
        match caster.cast(origin, origin + frame.along * reach, t.mask) {
            Some(hit) => {
                if let (Some(slot), Some(start)) = (found.get_mut(hits), starts.get_mut(hits)) {
                    *slot = hit;
                    *start = origin;
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
        if blocked(caster, &frame, pos, starts, &found) {
            verdict = Err(Reject::Blocked);
        }
    }
    Scan {
        hits: fit.hits,
        fit: Some(fit),
        verdict,
        steady: judge(&fit, Mode::Move, t, pos, capsule).is_ok() && fit.hits >= Mode::Move.needed(),
    }
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
/// So the normal eases toward the surface, the velocity turns into the surface with its speed
/// kept, and a small pull holds the stand-off.
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
    let speed = velocity.length();
    let along_surface = velocity - normal * velocity.dot(normal);
    let tangent = along_surface
        .normalized()
        .map_or(Vec3::default(), |direction| direction * speed);
    let (pull, error) = match &fit {
        Some(fit) => {
            let error = fit.distance(pos, normal) - standoff_target(normal.y, capsule);
            (standoff_speed(error, dt, contact), error)
        }
        None => (0.0, 0.0),
    };
    Some(Steered {
        velocity: tangent + normal * pull,
        normal,
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testworld::{face, tuning, Cylinder, Empty, Plane, Scripted};

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
    /// slope. This is why the patch lies along the body.
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
    /// a hit. The ray stops short of each hit, so rock does not block itself.
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

    /// Whatever the settings, the climb normal stays a direction a ray patch can be built from.
    /// At `WalkableAngle` 30 the hold limit alone would pass the limit of the rays, and the next
    /// hold would drop the player.
    #[test]
    fn the_normal_limits_stay_inside_the_ray_limit() {
        for walkable_angle in [30.0, 33.0, 40.0, 50.0, 75.0] {
            for overhang_angle in [90.0, 110.0, 135.0] {
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
        for overhang in [100.0, 110.0, 125.0, 135.0] {
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
}
