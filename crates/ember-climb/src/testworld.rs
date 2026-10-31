//! Worlds for the tests: analytic surfaces that answer rays, and a climber that runs the game's
//! climb update (`FUN_140418c50`) over them, step by step, with the mod's decisions in place.
//!
//! This proves the geometry and the control loop. It proves nothing about feel, or about the
//! game: the collision capsule and the animation are not in here.

use std::cell::Cell;

use crate::exit::{self, Exit};
use crate::geom::{Vec3, UP};
use crate::surface::{scan, steer, Capsule, Caster, Hit, Mode, SteerInput, Tuning};

/// The settings' defaults as limits.
pub fn tuning() -> Tuning {
    crate::config::Settings::default().tuning()
}

/// The normal of a face `angle` degrees from flat ground, facing the player, who looks along +x.
/// 90 is a sheer wall; more hangs over.
pub fn face(angle: f32) -> Vec3 {
    let radians = angle.to_radians();
    Vec3::new(-radians.sin(), radians.cos(), 0.0)
}

/// A world with nothing in it.
pub struct Empty;

impl Caster for Empty {
    fn cast(&self, _from: Vec3, _to: Vec3, _mask: u32) -> Option<Hit> {
        None
    }
}

/// A world that gives scripted answers in order, and `None` once they run out.
pub struct Scripted {
    /// The answers.
    answers: Vec<Option<Hit>>,
    /// The rays cast so far.
    casts: Cell<usize>,
}

impl Scripted {
    /// A world with these answers.
    pub fn new(answers: &[Option<Hit>]) -> Scripted {
        Scripted {
            answers: answers.to_vec(),
            casts: Cell::new(0),
        }
    }

    /// The rays cast so far.
    pub fn casts(&self) -> usize {
        self.casts.get()
    }
}

impl Caster for Scripted {
    fn cast(&self, _from: Vec3, _to: Vec3, _mask: u32) -> Option<Hit> {
        let index = self.casts.get();
        self.casts.set(index + 1);
        self.answers.get(index).copied().flatten()
    }
}

/// One endless flat surface.
pub struct Plane {
    /// A point on it.
    point: Vec3,
    /// Its normal, facing the open side.
    normal: Vec3,
}

impl Plane {
    /// The plane through `point` with `normal`.
    pub fn through(point: Vec3, normal: Vec3) -> Plane {
        Plane { point, normal }
    }

    /// A sheer wall at `x`, facing the player at the origin.
    pub fn wall(x: f32) -> Plane {
        Plane::through(Vec3::new(x, 0.0, 0.0), face(90.0))
    }
}

impl Caster for Plane {
    fn cast(&self, from: Vec3, to: Vec3, _mask: u32) -> Option<Hit> {
        let start = (from - self.point).dot(self.normal);
        let end = (to - self.point).dot(self.normal);
        if start < 0.0 || end >= 0.0 {
            return None;
        }
        Some(Hit {
            point: from + (to - from) * (start / (start - end)),
            normal: self.normal,
            entity: 0,
        })
    }
}

/// A round upright rock, for going sideways around a curve.
pub struct Cylinder {
    /// A point on its axis.
    centre: Vec3,
    /// Its radius.
    radius: f32,
}

impl Cylinder {
    /// The rock at `centre` with `radius`.
    pub fn new(centre: Vec3, radius: f32) -> Cylinder {
        Cylinder { centre, radius }
    }
}

impl Caster for Cylinder {
    fn cast(&self, from: Vec3, to: Vec3, _mask: u32) -> Option<Hit> {
        let start = (from - self.centre).flat();
        let delta = (to - from).flat();
        let a = delta.dot(delta);
        let b = 2.0 * start.dot(delta);
        let c = start.dot(start) - self.radius * self.radius;
        let root = b * b - 4.0 * a * c;
        if a <= 0.0 || root < 0.0 || c < 0.0 {
            return None;
        }
        let t = (-b - root.sqrt()) / (2.0 * a);
        if !(0.0..=1.0).contains(&t) {
            return None;
        }
        let point = from + (to - from) * t;
        Some(Hit {
            point,
            normal: (point - self.centre).flat().normalized()?,
            entity: 0,
        })
    }
}

/// A rock face drawn as a line in the x-y plane and stretched along z. The line runs from the foot
/// up to the top, with the rock on its right, like a voxel mesh of flat triangles. The player is on
/// the left. Only front faces answer.
pub struct Profile {
    /// The corners, bottom first.
    points: Vec<(f32, f32)>,
}

impl Profile {
    /// The face through these corners.
    pub fn new(points: Vec<(f32, f32)>) -> Profile {
        Profile { points }
    }

    /// A sheer wall at `x` that rounds into flat ground at `top`, along a quarter circle of
    /// `radius`. The ground then runs on.
    pub fn rounded(x: f32, top: f32, radius: f32) -> Profile {
        let mut points = vec![(x, -50.0), (x, top - radius)];
        for step in 1..=24 {
            let turn = f32::to_radians(90.0 * step as f32 / 24.0);
            points.push((
                x + radius * (1.0 - turn.cos()),
                top - radius + radius * turn.sin(),
            ));
        }
        points.push((x + 50.0, top));
        Profile::new(points)
    }

    /// A sheer wall at `x` with a sharp flat top at `top`.
    pub fn ledge(x: f32, top: f32) -> Profile {
        Profile::new(vec![(x, -50.0), (x, top), (x + 50.0, top)])
    }

    /// A face at `angle` degrees through (`x`, 0) with no end.
    pub fn slope(x: f32, angle: f32) -> Profile {
        let radians = angle.to_radians();
        let (dx, dy) = (radians.cos(), radians.sin());
        Profile::new(vec![
            (x - dx * 100.0, -dy * 100.0),
            (x + dx * 100.0, dy * 100.0),
        ])
    }

    /// A sheer rough wall around `x`: flat facets of `step` metres, set in and out by `depth`.
    pub fn rough(x: f32, depth: f32, step: f32, height: f32) -> Profile {
        let mut points = Vec::new();
        let mut y = -10.0;
        let mut index = 0u32;
        while y < height {
            // A fixed, uneven pattern, so the rays cross facets at uneven times.
            let offset = match index % 5 {
                0 => 0.0,
                1 => 1.0,
                2 => -0.6,
                3 => 0.4,
                _ => -1.0,
            };
            points.push((x + offset * depth, y));
            y += step;
            index += 1;
        }
        Profile::new(points)
    }
}

impl Caster for Profile {
    fn cast(&self, from: Vec3, to: Vec3, _mask: u32) -> Option<Hit> {
        let ray = (to.x - from.x, to.y - from.y);
        let mut best: Option<(f32, Hit)> = None;
        for corners in self.points.windows(2) {
            let (a, b) = (corners[0], corners[1]);
            let edge = (b.0 - a.0, b.1 - a.1);
            let Some(normal) = Vec3::new(-edge.1, edge.0, 0.0).normalized() else {
                continue;
            };
            if normal.x * ray.0 + normal.y * ray.1 >= 0.0 {
                continue;
            }
            let cross = ray.0 * edge.1 - ray.1 * edge.0;
            if cross == 0.0 {
                continue;
            }
            let gap = (a.0 - from.x, a.1 - from.y);
            let t = (gap.0 * edge.1 - gap.1 * edge.0) / cross;
            let u = (gap.0 * ray.1 - gap.1 * ray.0) / cross;
            if !(0.0..=1.0).contains(&t) || !(0.0..=1.0).contains(&u) {
                continue;
            }
            if best.is_none_or(|(nearest, _)| t < nearest) {
                let point = from + (to - from) * t;
                let hit = Hit {
                    point,
                    normal,
                    entity: 0,
                };
                best = Some((t, hit));
            }
        }
        best.map(|(_, hit)| hit)
    }
}

/// A rock line in the x-y plane, stretched along z and cut into flat triangles, like the game's
/// voxel mesh. Every corner is set in or out by a fixed pattern of up to `depth` metres, which is
/// the displacement. As with the game's voxel bodies, a ray always stops at the displaced rock.
/// Mask bit 0 gets the flat normal of the displaced triangle. Any other mask gets the flat normal
/// of the same triangle before displacement. Only front faces answer.
pub struct Faceted {
    /// The grid size, in metres.
    cell: f32,
    /// Corners in one row.
    width: usize,
    /// The corners before displacement, row by row.
    plain: Vec<Vec3>,
    /// The corners after displacement.
    rough: Vec<Vec3>,
    /// Each row of cells as lowest x, highest x, lowest y and highest y, so a ray skips most rows.
    bounds: Vec<[f32; 4]>,
}

/// How far a [`Faceted`] rock runs to each side of the climber, in metres.
const FACETED_SIDE: f32 = 4.0;
/// How far a [`Faceted`] rock runs below the feet of a climber at height 0, in metres.
const FACETED_FOOT: f32 = 4.0;

impl Faceted {
    /// The rock along `line`, bottom first, with the rock on its right.
    pub fn new(line: &[(f32, f32)], depth: f32, cell: f32, seed: u32) -> Faceted {
        let mut samples = Vec::new();
        for pair in line.windows(2) {
            let (from, to) = (pair[0], pair[1]);
            let steps = ((to.0 - from.0).hypot(to.1 - from.1) / cell)
                .round()
                .max(1.0) as u32;
            for step in 0..steps {
                let share = step as f32 / steps as f32;
                samples.push((
                    from.0 + (to.0 - from.0) * share,
                    from.1 + (to.1 - from.1) * share,
                ));
            }
        }
        samples.extend(line.last());
        let width = (FACETED_SIDE * 2.0 / cell) as usize + 1;
        // xorshift: the same seed gives the same rock on every machine.
        let mut state = seed.wrapping_mul(2_654_435_761) | 1;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as f32 / u32::MAX as f32 * 2.0 - 1.0
        };
        let (mut plain, mut rough) = (Vec::new(), Vec::new());
        for (index, &(x, y)) in samples.iter().enumerate() {
            let before = samples[index.saturating_sub(1)];
            let after = samples[(index + 1).min(samples.len() - 1)];
            let normal = Vec3::new(before.1 - after.1, after.0 - before.0, 0.0)
                .normalized()
                .expect("the corners differ");
            for column in 0..width {
                let corner = Vec3::new(x, y, column as f32 * cell - FACETED_SIDE);
                plain.push(corner);
                rough.push(corner + normal * (next() * depth));
            }
        }
        let bounds = rough
            .chunks(width)
            .collect::<Vec<_>>()
            .windows(2)
            .map(|rows| {
                let (mut low_x, mut high_x) = (f32::MAX, f32::MIN);
                let (mut low_y, mut high_y) = (f32::MAX, f32::MIN);
                for corner in rows[0].iter().chain(rows[1]) {
                    low_x = low_x.min(corner.x);
                    high_x = high_x.max(corner.x);
                    low_y = low_y.min(corner.y);
                    high_y = high_y.max(corner.y);
                }
                [low_x, high_x, low_y, high_y]
            })
            .collect();
        Faceted {
            cell,
            width,
            plain,
            rough,
            bounds,
        }
    }

    /// The same rock with its roughness in the mesh itself, so the undisplaced normals are just as
    /// rough as the displaced ones. Nobody knows yet how rough the game's undisplaced mesh is.
    pub fn rough_normals(mut self) -> Faceted {
        self.plain.clone_from(&self.rough);
        self
    }

    /// A face at `angle` degrees through the origin, as far up as anyone climbs in a test.
    pub fn face(angle: f32, depth: f32, seed: u32) -> Faceted {
        Faceted::bent(angle, angle, 14.0, depth, seed)
    }

    /// A face at `lower` degrees through the origin up to height `corner`, then 6 m at `upper`.
    pub fn bent(lower: f32, upper: f32, corner: f32, depth: f32, seed: u32) -> Faceted {
        let lean = lower.to_radians();
        let foot = (-FACETED_FOOT / lean.tan(), -FACETED_FOOT);
        let knee = (corner / lean.tan(), corner);
        let turn = upper.to_radians();
        let end = (knee.0 + turn.cos() * 6.0, knee.1 + turn.sin() * 6.0);
        Faceted::new(&[foot, knee, end], depth, 0.5, seed)
    }

    /// The three corners of one triangle of a cell.
    fn triangle(&self, row: usize, column: usize, second: bool) -> [usize; 3] {
        let at = row * self.width + column;
        let (up, side) = (at + self.width, at + 1);
        if second {
            [up + 1, side, up]
        } else {
            [at, up, side]
        }
    }

    /// The flat normal of a triangle, facing the player.
    fn normal_of(corners: &[Vec3], [a, b, c]: [usize; 3]) -> Option<Vec3> {
        (corners[c] - corners[a])
            .cross(corners[b] - corners[a])
            .normalized()
    }
}

impl Caster for Faceted {
    fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit> {
        let ray = to - from;
        let (low_x, high_x) = (from.x.min(to.x), from.x.max(to.x));
        let (low_y, high_y) = (from.y.min(to.y), from.y.max(to.y));
        let column_of = |z: f32| (z + FACETED_SIDE) / self.cell;
        let first = column_of(from.z.min(to.z)).floor().max(0.0) as usize;
        let last = (column_of(from.z.max(to.z)).floor() as usize + 1).min(self.width - 1);
        let mut best: Option<(f32, [usize; 3])> = None;
        for (row, row_box) in self.bounds.iter().enumerate() {
            let [left, right, bottom, top] = *row_box;
            if right < low_x || left > high_x || top < low_y || bottom > high_y {
                continue;
            }
            for column in first..last {
                for second in [false, true] {
                    let triangle = self.triangle(row, column, second);
                    let Some(normal) = Faceted::normal_of(&self.rough, triangle) else {
                        continue;
                    };
                    if normal.dot(ray) >= 0.0 {
                        continue;
                    }
                    let [a, b, c] = triangle.map(|corner| self.rough[corner]);
                    let (edge_b, edge_c) = (b - a, c - a);
                    let across = ray.cross(edge_c);
                    let scale = 1.0 / edge_b.dot(across);
                    let gap = from - a;
                    let u = gap.dot(across) * scale;
                    let turned = gap.cross(edge_b);
                    let v = ray.dot(turned) * scale;
                    let t = edge_c.dot(turned) * scale;
                    if u >= 0.0
                        && v >= 0.0
                        && u + v <= 1.0
                        && (0.0..=1.0).contains(&t)
                        && best.is_none_or(|(nearest, _)| t < nearest)
                    {
                        best = Some((t, triangle));
                    }
                }
            }
        }
        let (t, triangle) = best?;
        let corners = if mask & 1 == 0 {
            &self.plain
        } else {
            &self.rough
        };
        Some(Hit {
            point: from + ray * t,
            normal: Faceted::normal_of(corners, triangle)?,
            entity: 0,
        })
    }
}

/// What one climbing frame did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Frame {
    /// The player moved.
    Moved,
    /// The move check refused every direction.
    Blocked,
    /// The hold failed, so the climb ended in a fall.
    Dropped,
    /// The exit fired: the mantle starts, onto this point.
    Mantle(Vec3),
}

/// A player on a wall, running the game's climb update. All speeds are the game's.
#[derive(Clone, Copy, Debug)]
pub struct Climber {
    /// Feet.
    pub pos: Vec3,
    /// Velocity.
    pub velocity: Vec3,
    /// The climb normal.
    pub normal: Vec3,
    /// The capsule.
    pub capsule: Capsule,
}

/// The game's top climbing speed (`0x14150af74`).
const SPEED: f32 = 1.95;
/// How fast the game moves velocity toward its target (`FUN_140419b60`, its fourth argument).
const ACCELERATION: f32 = 50.0;

impl Climber {
    /// A climber standing at the game's distance from `surface`, which is under the point
    /// straight ahead at the height of `start`.
    pub fn on<C: Caster>(surface: &C, start: Vec3, normal: Vec3) -> Climber {
        let hit = surface
            .cast(start + UP * 1.625, start + UP * 1.625 - normal * 6.0, 0)
            .expect("the surface is in front of the climber");
        Climber {
            pos: start + normal * (0.55 - (start - hit.point).dot(hit.normal)),
            velocity: Vec3::default(),
            normal: hit.normal,
            capsule: Capsule::FALLBACK,
        }
    }

    /// The signed distance from the feet to `surface`, along the climb normal.
    pub fn gap<C: Caster>(&self, surface: &C) -> Option<f32> {
        let hit = surface.cast(self.pos, self.pos - self.normal * 6.0, 0)?;
        Some((self.pos - hit.point).dot(hit.normal))
    }

    /// The game's velocity for this stick: `up * z - right * x`, normalised, at the speed the
    /// stick asks for.
    fn wanted(&self, stick_x: f32, stick_z: f32) -> Vec3 {
        let right = Vec3::new(self.normal.z, 0.0, -self.normal.x);
        let Some(direction) = (UP * stick_z - right * stick_x).normalized() else {
            return Vec3::default();
        };
        let press = (stick_x * stick_x + stick_z * stick_z).sqrt();
        direction * (press * (SPEED - 0.75) + 0.75)
    }

    /// The game's exit query: ray A, then ray B.
    fn native_exit<C: Caster>(&self, world: &C, pos: Vec3) -> Option<Hit> {
        let head = pos + UP * 2.1;
        let reach = head - self.normal * 1.05;
        if world.cast(head, reach, 0x129).is_some() {
            return None;
        }
        world.cast(reach, reach - UP * 1.35, 0x21)
    }

    /// May the player stand at `pos`? The native probe never passes here, so this is the mod's
    /// answer.
    fn may_stand<C: Caster>(&self, world: &C, pos: Vec3, t: &Tuning) -> bool {
        scan(world, pos, -self.normal, Mode::Move, t, self.capsule)
            .verdict
            .is_ok()
    }

    /// The game's move check: the whole velocity, else its vertical part, else its sideways part,
    /// else nothing.
    fn check_move<C: Caster>(&mut self, world: &C, dt: f32, t: &Tuning) -> bool {
        let v = self.velocity;
        if self.may_stand(world, self.pos + v * dt, t) {
            return true;
        }
        let up = UP * v.y;
        if self.may_stand(world, self.pos + up * dt, t) {
            self.velocity = up;
            return true;
        }
        let side = v.flat();
        if side.length() > 1e-5 && self.may_stand(world, self.pos + side * dt, t) {
            self.velocity = side;
            return true;
        }
        self.velocity = Vec3::default();
        false
    }

    /// One frame of the climb update, in the game's order.
    pub fn step<C: Caster>(&mut self, world: &C, stick: (f32, f32), dt: f32, t: &Tuning) -> Frame {
        let hold = scan(world, self.pos, -self.normal, Mode::Stay, t, self.capsule);
        if hold.verdict.is_err() {
            return Frame::Dropped;
        }
        let wanted = self.wanted(stick.0, stick.1);
        let change = wanted - self.velocity;
        let most = ACCELERATION * dt;
        self.velocity = if change.length() <= most {
            wanted
        } else {
            self.velocity + change.normalized().unwrap_or_default() * most
        };
        // The game takes this prediction before the move check changes the velocity.
        let predicted = self.pos + self.velocity * dt;
        let mut frame = Frame::Blocked;
        let mut stuck = false;
        if self.velocity.length() > 1e-5 {
            let input = SteerInput {
                velocity: self.velocity,
                normal: self.normal,
                fit: hold.fit.filter(|_| hold.steady),
                pos: self.pos,
                dt,
                contact: false,
            };
            if let Some(out) = steer(&input, t, self.capsule) {
                self.velocity = out.velocity;
                self.normal = out.normal;
            }
            if self.check_move(world, dt, t) {
                frame = Frame::Moved;
            } else {
                stuck = true;
            }
        }
        let native = self.native_exit(world, predicted);
        let query = exit::Query {
            pos: predicted,
            normal: self.normal,
            moving_up: predicted.y > self.pos.y,
            floor: native,
            patch: hold.fit.map(|fit| Hit {
                point: fit.point,
                normal: fit.normal,
                entity: fit.entity,
            }),
            walkable_ny: t.walkable_ny,
            face: t.face,
            stuck,
        };
        let target = match exit::decide(world, &query) {
            Exit::Keep => native,
            Exit::Refuse => None,
            Exit::Take(hit) => Some(hit),
        };
        if let Some(hit) = target.filter(|_| stick.1 > 0.0) {
            return Frame::Mantle(hit.point);
        }
        self.pos = self.pos + self.velocity * dt;
        frame
    }
}

/// Frame times to test: a fast client, a normal one, and a slow server tick.
pub const FRAME_TIMES: [f32; 3] = [1.0 / 120.0, 1.0 / 60.0, 1.0 / 20.0];

#[cfg(test)]
mod climbs {
    use super::*;
    use crate::surface::{Capsule, GAME_STANDOFF};

    /// Climb straight up until `height` metres are gained. Panics on a drop, a stall or a mantle.
    fn climb_up<C: Caster>(world: &C, mut climber: Climber, height: f32, dt: f32) -> Climber {
        let t = tuning();
        let start = climber.pos.y;
        let mut stuck = 0;
        for frame in 0..((height / SPEED / dt) as u32 + 400) {
            let before = climber.pos;
            match climber.step(world, (0.0, 1.0), dt, &t) {
                Frame::Moved => {}
                other => panic!("frame {frame}: {other:?} at {climber:?}"),
            }
            if (climber.pos - before).length() < 1e-4 {
                stuck += 1;
                assert!(stuck < 10, "stalled at {climber:?}");
            }
            if climber.pos.y - start >= height {
                return climber;
            }
        }
        panic!("too slow: {climber:?}");
    }

    /// The design's test plan: 10 m of plain face at every steepness, at every frame rate, with
    /// no stall, no mantle and no drop. The fixed entry normal stalls after 0.4 m here.
    #[test]
    fn climbs_ten_metres_of_any_steep_face() {
        for dt in FRAME_TIMES {
            for angle in [52.0, 60.0, 68.0, 75.0, 83.0, 90.0, 97.0] {
                let rock = Profile::slope(0.0, angle);
                let start = Vec3::new(-1.0, 0.0, 0.0);
                let climber = Climber::on(&rock, start, face(angle));
                let end = climb_up(&rock, climber, 10.0, dt);
                let gap = end.gap(&rock).unwrap();
                assert!((gap - GAME_STANDOFF).abs() < 0.1, "{angle}/{dt}: {gap}");
            }
        }
    }

    /// A start must lead straight into a climb. If the place an entry gives were out of the hold's
    /// reach, the player would latch and drop at once, which is the old complaint.
    #[test]
    fn an_entry_leads_straight_into_a_climb() {
        let t = tuning();
        let dt = 1.0 / 60.0;
        for angle in [52.0, 60.0, 75.0, 90.0, 100.0] {
            let rock = Profile::slope(0.0, angle);
            // Where a walking player arrives: the capsule is about to touch the rock.
            let walking = Vec3::new(-0.45, 0.0, 0.0);
            let forward = Vec3::new(1.0, 0.0, 0.0);
            let entry = scan(&rock, walking, forward, Mode::Entry, &t, Capsule::FALLBACK);
            let pos = entry
                .verdict
                .unwrap_or_else(|why| panic!("{angle}: {why:?}"));
            let mut climber = Climber {
                pos,
                velocity: Vec3::default(),
                normal: entry.fit.unwrap().normal,
                capsule: Capsule::FALLBACK,
            };
            for frame in 0..120 {
                let step = climber.step(&rock, (0.0, 1.0), dt, &t);
                assert_eq!(step, Frame::Moved, "{angle}, frame {frame}");
            }
        }
    }

    /// The climbing speed along the surface is the game's, on a leaning face too.
    #[test]
    fn the_climbing_speed_stays_the_games() {
        let dt = 1.0 / 60.0;
        let t = tuning();
        let rock = Profile::slope(0.0, 60.0);
        let mut climber = Climber::on(&rock, Vec3::new(-1.0, 0.0, 0.0), face(60.0));
        for _ in 0..30 {
            climber.step(&rock, (0.0, 1.0), dt, &t);
        }
        let speed = climber.velocity.length();
        assert!((speed - SPEED).abs() < 0.05, "{speed}");
    }

    /// With the stick centred nothing moves and nothing is lost.
    #[test]
    fn a_still_climber_stays_put() {
        let t = tuning();
        let rock = Profile::slope(0.0, 75.0);
        let mut climber = Climber::on(&rock, Vec3::new(-1.0, 0.0, 0.0), face(75.0));
        let pos = climber.pos;
        for _ in 0..600 {
            assert_eq!(
                climber.step(&rock, (0.0, 0.0), 1.0 / 60.0, &t),
                Frame::Blocked
            );
        }
        assert_eq!(climber.pos, pos);
    }

    /// Up and back down the same face returns to the start: no drift off or into the rock.
    #[test]
    fn up_and_down_returns_to_the_start() {
        let t = tuning();
        let dt = 1.0 / 60.0;
        let rock = Profile::slope(0.0, 70.0);
        let mut climber = Climber::on(&rock, Vec3::new(-1.0, 0.0, 0.0), face(70.0));
        for _ in 0..240 {
            climber.step(&rock, (0.0, 1.0), dt, &t);
        }
        for _ in 0..240 {
            assert_ne!(climber.step(&rock, (0.0, -1.0), dt, &t), Frame::Dropped);
        }
        let gap = climber.gap(&rock).unwrap();
        assert!((gap - GAME_STANDOFF).abs() < 0.05, "{gap}");
    }

    /// Going sideways round a boulder: the normal follows the curve and the gap holds. This is
    /// the case a fixed normal cannot do at all.
    #[test]
    fn goes_sideways_round_a_boulder() {
        for dt in FRAME_TIMES {
            for radius in [1.5, 2.5, 6.0] {
                let t = tuning();
                let rock = Cylinder::new(Vec3::new(radius, 0.0, 0.0), radius);
                let start = Vec3::new(-1.0, 0.0, 0.0);
                let mut climber = Climber::on(&rock, start, face(90.0));
                let first = climber.normal;
                let mut turned = 0.0f32;
                for frame in 0..((6.0 / 1.2 / dt) as u32) {
                    match climber.step(&rock, (1.0, 0.0), dt, &t) {
                        Frame::Moved => {}
                        other => panic!("{radius}/{dt} frame {frame}: {other:?} {climber:?}"),
                    }
                    let gap = (climber.pos - Vec3::new(radius, 0.0, 0.0)).flat().length() - radius;
                    assert!((gap - GAME_STANDOFF).abs() < 0.2, "{radius}/{dt}: {gap}");
                    turned = turned.max(1.0 - first.dot(climber.normal));
                }
                assert!(
                    turned > 0.2,
                    "{radius}/{dt}: the normal never followed: {turned}"
                );
            }
        }
    }

    /// Rough rock: facets 0.5 m tall, set in and out by 0.12 m. The climb must not stop or drop,
    /// and the gap must not hunt.
    #[test]
    fn climbs_rough_rock_without_stopping() {
        for dt in FRAME_TIMES {
            let rock = Profile::rough(0.0, 0.12, 0.5, 20.0);
            let start = Vec3::new(-1.0, 0.0, 0.0);
            let climber = Climber::on(&rock, start, face(90.0));
            let end = climb_up(&rock, climber, 8.0, dt);
            let gap = end.gap(&rock).unwrap();
            assert!((gap - GAME_STANDOFF).abs() < 0.35, "{dt}: {gap}");
        }
    }

    /// The sideways speed of the normal is limited: rough rock must not whip the body round.
    #[test]
    fn rough_rock_turns_the_body_gently() {
        let t = tuning();
        let dt = 1.0 / 60.0;
        let rock = Profile::rough(0.0, 0.12, 0.5, 20.0);
        let mut climber = Climber::on(&rock, Vec3::new(-1.0, 0.0, 0.0), face(90.0));
        let mut last = climber.normal;
        for _ in 0..300 {
            climber.step(&rock, (0.0, 1.0), dt, &t);
            let turn = last
                .dot(climber.normal)
                .clamp(-1.0, 1.0)
                .acos()
                .to_degrees();
            assert!(turn < 6.0, "{turn} degrees in one frame");
            last = climber.normal;
        }
    }

    /// Climb until the mantle fires, and return where it lands. Panics on a drop or a long stall.
    fn climb_to_exit<C: Caster>(world: &C, mut climber: Climber, dt: f32) -> Vec3 {
        let t = tuning();
        let mut stuck = 0;
        for frame in 0..((30.0 / SPEED / dt) as u32) {
            let before = climber.pos;
            match climber.step(world, (0.0, 1.0), dt, &t) {
                Frame::Moved | Frame::Blocked => {}
                Frame::Mantle(target) => return target,
                Frame::Dropped => panic!("dropped at frame {frame}: {climber:?}"),
            }
            if (climber.pos - before).length() < 1e-4 {
                stuck += 1;
                assert!(stuck < 30, "stuck on the wall at {climber:?}");
            } else {
                stuck = 0;
            }
        }
        panic!("no exit: {climber:?}");
    }

    /// The author's complaint: a wall that curves into something walkable. The climb must end in
    /// a mantle onto ground, whatever the curve, and never in a drop or a stall.
    #[test]
    fn a_wall_that_rounds_into_walkable_ground_ends_in_a_mantle() {
        for dt in FRAME_TIMES {
            for radius in [0.5, 1.5, 3.0, 6.0] {
                let rock = Profile::rounded(0.0, 12.0, radius);
                let climber = Climber::on(&rock, Vec3::new(-1.0, 0.0, 0.0), face(90.0));
                let target = climb_to_exit(&rock, climber, dt);
                let slope = rock
                    .cast(target + UP * 0.1, target - UP * 0.1, 0)
                    .expect("the target is on the rock");
                assert!(
                    slope.normal.y >= tuning().walkable_ny,
                    "{radius}/{dt}: a mantle onto {slope:?}"
                );
            }
        }
    }

    /// A sharp ledge ends in the game's own mantle.
    #[test]
    fn a_sharp_ledge_ends_in_a_mantle() {
        for dt in FRAME_TIMES {
            let rock = Profile::ledge(0.0, 8.0);
            let climber = Climber::on(&rock, Vec3::new(-1.0, 0.0, 0.0), face(90.0));
            let target = climb_to_exit(&rock, climber, dt);
            assert!((target.y - 8.0).abs() < 1e-3, "{dt}: {target:?}");
        }
    }

    /// The other half of the exit: a steep face has no misfire. The game's own query would play
    /// the mantle here on every face from 48 to 76 degrees.
    #[test]
    fn a_steep_face_never_mantles() {
        for angle in [50.5, 55.0, 60.0, 65.0, 70.0, 75.0] {
            let rock = Profile::slope(0.0, angle);
            let climber = Climber::on(&rock, Vec3::new(-1.0, 0.0, 0.0), face(angle));
            climb_up(&rock, climber, 6.0, 1.0 / 60.0);
        }
    }

    /// The game's query really does misfire there, so the test above tests something.
    #[test]
    fn the_games_own_exit_misfires_on_a_steep_face() {
        let rock = Profile::slope(0.0, 60.0);
        let climber = Climber::on(&rock, Vec3::new(-1.0, 0.0, 0.0), face(60.0));
        assert!(climber.native_exit(&rock, climber.pos).is_some());
    }

    /// A wall that ends on the way up with nothing to stand on (a thin fin) stops the player.
    /// It must not throw them off.
    #[test]
    fn a_wall_with_nothing_above_stops_the_player() {
        let t = tuning();
        let rock = Profile::new(vec![(0.0, -50.0), (0.0, 9.0)]);
        let mut climber = Climber::on(&rock, Vec3::new(-1.0, 0.0, 0.0), face(90.0));
        for _ in 0..1200 {
            assert_ne!(
                climber.step(&rock, (0.0, 1.0), 1.0 / 60.0, &t),
                Frame::Dropped
            );
        }
        assert!(climber.pos.y < 9.0, "{climber:?}");
    }

    /// Climb `height` metres up rough rock. Panics on a mantle, a drop or a long stall.
    fn climb_rough(rock: &Faceted, angle: f32, height: f32, dt: f32, label: &str) {
        let t = tuning();
        let mut climber = Climber::on(rock, Vec3::new(-1.0, 0.0, 0.0), face(angle));
        let start = climber.pos.y;
        let mut stuck = 0;
        for frame in 0..((height * 3.0 / SPEED / dt) as u32) {
            let before = climber.pos;
            match climber.step(rock, (0.0, 1.0), dt, &t) {
                Frame::Moved | Frame::Blocked => {}
                other => panic!(
                    "{label}: {other:?} at frame {frame}, {} m up: {climber:?}",
                    climber.pos.y - start
                ),
            }
            if (climber.pos - before).length() < 1e-4 {
                stuck += 1;
                assert!(stuck < 30, "{label}: stalled at {climber:?}");
            } else {
                stuck = 0;
            }
            if climber.pos.y - start >= height {
                return;
            }
        }
        panic!("{label}: too slow: {climber:?}");
    }

    /// One flat triangle in a steep face must not play the mantle. A triangle of the displaced
    /// body can lie 20 degrees flatter than the rock around it, and the game's floor ray lands on
    /// the rock every frame in this band.
    #[test]
    fn rough_leaning_rock_never_mantles() {
        for angle in [55.0, 60.0, 65.0, 70.0] {
            for seed in 1..=6 {
                for depth in [0.08, 0.15] {
                    let rock = Faceted::face(angle, depth, seed);
                    let label = format!("{angle} degrees, +-{depth} m, seed {seed}");
                    climb_rough(&rock, angle, 6.0, 1.0 / 60.0, &label);
                    let label = format!("{label}, rough normals");
                    climb_rough(&rock.rough_normals(), angle, 6.0, 1.0 / 60.0, &label);
                }
            }
        }
    }

    /// The floor test must not cost the real exits: rough rock that turns walkable still ends in
    /// a mantle on the ground above the bend, and never in a stall or a drop.
    #[test]
    fn rough_rock_that_turns_walkable_ends_in_a_mantle() {
        let bends = [
            (90.0, 35.0),
            (90.0, 20.0),
            (90.0, 0.0),
            (75.0, 30.0),
            (75.0, 0.0),
            (60.0, 20.0),
            (60.0, 0.0),
            (70.0, 45.0),
        ];
        for (lower, upper) in bends {
            for seed in 1..=4 {
                let rock = Faceted::bent(lower, upper, 6.0, 0.15, seed);
                let climber = Climber::on(&rock, Vec3::new(-1.0, 0.0, 0.0), face(lower));
                let target = climb_to_exit(&rock, climber, 1.0 / 60.0);
                assert!(
                    target.y > 5.3,
                    "{lower}->{upper}, seed {seed}: a mantle at {target:?}, under the bend"
                );
                let rock = rock.rough_normals();
                let target = climb_to_exit(&rock, climber, 1.0 / 60.0);
                assert!(target.y > 5.3, "{lower}->{upper}, seed {seed}: {target:?}");
            }
        }
    }

    /// A smooth face that leans back and has a flat top. The upper rays reach the top long before
    /// any ray of the exit does, so the move check says "walkable" while the exit still sees the
    /// face. Without a look past the lip the player stops under the top for good.
    #[test]
    fn a_leaning_face_with_a_flat_top_ends_in_a_mantle() {
        for dt in FRAME_TIMES {
            for angle in [52.0, 55.0, 60.0, 65.0, 70.0, 75.0] {
                let rise = 7.0 / f32::to_radians(angle).tan();
                let rock =
                    Profile::new(vec![(-rise * 3.0, -21.0), (rise, 7.0), (rise + 50.0, 7.0)]);
                let climber = Climber::on(&rock, Vec3::new(-1.0, 0.0, 0.0), face(angle));
                let target = climb_to_exit(&rock, climber, dt);
                assert!((target.y - 7.0).abs() < 1e-3, "{angle}/{dt}: {target:?}");
            }
        }
    }

    /// One rough patch can read 15 degrees past the rock as a whole, and more where a wall bends
    /// into an overhang. A climb must not stop at the limit for starting one.
    #[test]
    fn a_rough_overhang_does_not_stop_a_climb() {
        for (angle, depth) in [(100.0, 0.15), (105.0, 0.08)] {
            for seed in 1..=6 {
                let rock = Faceted::bent(90.0, angle, 3.0, depth, seed).rough_normals();
                let label = format!("90 to {angle} degrees, +-{depth} m, seed {seed}");
                climb_rough(&rock, 90.0, 5.0, 1.0 / 60.0, &label);
            }
        }
    }

    /// A low `WalkableAngle` lets the climb normal tip far back. If it passes what a ray patch can
    /// be built from, the next hold is refused and the player drops. A slope of 17 degrees is
    /// past the hold limit there, so the normal runs into that limit.
    #[test]
    fn a_low_walkable_angle_keeps_the_ray_direction() {
        let settings = crate::config::Settings {
            walkable_angle: 30.0,
            ..Default::default()
        };
        let t = settings.tuning();
        let rock = Plane::through(Vec3::default(), face(17.0));
        let mut climber = Climber {
            pos: face(17.0) * GAME_STANDOFF,
            velocity: Vec3::default(),
            normal: face(30.0),
            capsule: Capsule::FALLBACK,
        };
        for frame in 0..300 {
            let step = climber.step(&rock, (0.0, 1.0), 1.0 / 60.0, &t);
            assert_ne!(step, Frame::Dropped, "frame {frame}: {climber:?}");
        }
    }

    /// The fixed normal the game uses cannot climb a leaning face. This pins the numbers the
    /// design gives, and shows the climb tests above would fail without steering.
    #[test]
    fn without_steering_a_leaning_face_stalls() {
        let t = tuning();
        let rock = Profile::slope(0.0, 75.0);
        let climber = Climber::on(&rock, Vec3::new(-1.0, 0.0, 0.0), face(75.0));
        let normal = climber.normal;
        let mut pos = climber.pos;
        let capsule = Capsule::FALLBACK;
        let mut climbed = 0.0;
        for _ in 0..600 {
            let next = pos + UP * (SPEED / 60.0);
            if scan(&rock, next, -normal, Mode::Move, &t, capsule)
                .verdict
                .is_err()
            {
                break;
            }
            pos = next;
            climbed += SPEED / 60.0;
        }
        assert!(climbed < 2.0, "{climbed}");
    }
}
