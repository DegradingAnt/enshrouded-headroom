//! How a climb ends at the top.
//!
//! The game's exit query (`FUN_14024a6f0`) casts ray A from 2.1 m up, 1.05 m into the wall, then
//! ray B 1.35 m down from A's end. It has no walkable test. On a 48 to 76 degree face A misses and
//! B lands on the rock itself, so the mantle plays on a cliff. Where a wall rounds into a slope A
//! hits the slope, so no exit exists at all and the player sticks. This module corrects both, and
//! only for natural surfaces; a lattice keeps the game's answer.
//!
//! The normal of one hit proves nothing. Displaced terrain has a flat normal per triangle, and one
//! triangle can lie 20 degrees flatter than the rock around it. So this module asks two questions
//! that cover more ground than one triangle. Does the face go on ahead of the head? Is the ground
//! around a candidate walkable as an area?

use crate::geom::{Vec3, UP};
use crate::surface::{Caster, Hit, GAME_STANDOFF};

/// Height of ray A above the feet, the game's own number.
const HEAD_HEIGHT: f32 = 2.1;
/// How far ray A reaches along the wall normal, the game's own number.
const HEAD_REACH: f32 = 1.05;
/// Ray A's mask, the game's own: World, Solid, Walkable, ClimbingWall.
const MASK_HEAD: u32 = 0x129;
/// The mask of this module's own ground rays: WorldNormalsNotDisplaced and Walkable. Its terrain
/// body has the positions of the game's World body, and normals from before displacement.
const MASK_GROUND: u32 = 0x24;
/// How far down the floor search reaches from ray A's end. The game's 1.35 m misses a shallow
/// slope, which lies farther below that point.
const FLOOR_DROP: f32 = 2.6;
/// How far above and below a walkable patch to look for the ground under it.
const PATCH_RISE: f32 = 1.0;
/// How far above ray A's end the ray for the face ahead starts. Near a flat top that end is only a
/// hand above the ground, and the ray must clear the bumps there.
const FACE_RISE: f32 = 0.5;
/// How many degrees flatter than the walkable limit a plane may be and still meet the ray for the
/// face ahead. Rock just past the limit reads walkable as often as not.
const FACE_REACH_MARGIN_DEGREES: f32 = 3.0;
/// How many degrees flatter than the walkable limit the rise to the rock ahead may be and still
/// count as a face. One metre of rough rock can read this much flatter than it is.
const FACE_SLOPE_MARGIN_DEGREES: f32 = 8.0;
/// A rock this far off the plane of the climb, in metres, is another rock: the wall behind a shelf.
const FACE_PLANE: f32 = 0.7;
/// How far past ray A's end to look for a floor, in metres. On a face that leans back the upper
/// rays reach the top well before ray A's end does, and a rounded lip has its flat part behind it.
const FLOOR_STEPS: [f32; 4] = [0.0, 0.4, 0.8, 1.2];
/// How far each ring ray sits from a candidate floor, in metres.
const RING_SIDE: f32 = 0.35;
/// How far above and below a candidate floor the ring rays reach, in metres.
const RING_RISE: f32 = 1.0;
/// The ring rays that must hit. A ledge has one side that falls away.
const RING_HITS: u8 = 3;

/// The level ray that looks for the face going on ahead of the head. With the ground under its
/// start it measures the slope of the rock over a metre or more, from positions alone. No normal
/// of one triangle comes into it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceRay {
    /// How far it reaches, in metres: far enough to touch any plane too steep to stand on, with
    /// the player climbing that plane.
    pub length: f32,
    /// The rise per metre of run from the ground to the hit that makes a face.
    pub slope: f32,
}

impl FaceRay {
    /// The ray for this walkable limit, in degrees.
    pub fn new(walkable_angle: f32) -> FaceRay {
        let reached = (walkable_angle - FACE_REACH_MARGIN_DEGREES).to_radians();
        let above_plane = HEAD_HEIGHT - (HEAD_REACH - GAME_STANDOFF) / reached.cos();
        FaceRay {
            length: (above_plane + FACE_RISE) / reached.tan(),
            slope: (walkable_angle - FACE_SLOPE_MARGIN_DEGREES)
                .to_radians()
                .tan(),
        }
    }
}

/// What to do with the game's answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Exit {
    /// The game's answer stands.
    Keep,
    /// The game found steep rock, not a floor: no exit here.
    Refuse,
    /// There is a floor to mantle onto that the game did not find.
    Take(Hit),
}

/// What the decision works from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Query {
    /// The position the game asked about.
    pub pos: Vec3,
    /// The current climb normal.
    pub normal: Vec3,
    /// Is the player rising? Only then can a climb end at the top.
    pub moving_up: bool,
    /// The floor the game found, or `None` when it found no exit.
    pub floor: Option<Hit>,
    /// The surface in front of the chest this frame, as one hit: its centre and averaged normal.
    pub patch: Option<Hit>,
    /// A floor with at least this much up in its normal can be stood on.
    pub walkable_ny: f32,
    /// The ray that looks for the face going on.
    pub face: FaceRay,
    /// Did the move check find no way on this frame?
    pub stuck: bool,
}

/// One exit decision: the rays it casts, and what they are judged by.
struct Search<'a, C> {
    /// The rays.
    caster: &'a C,
    /// The question.
    query: &'a Query,
    /// Into the rock, flat.
    forward: Vec3,
}

impl<C: Caster> Search<'_, C> {
    /// Is the ground around `at` walkable as an area? Four rays around it must mostly land, and
    /// their averaged normal decides.
    fn walkable_around(&self, at: Vec3) -> bool {
        let right = UP.cross(self.forward);
        let mut normals = Vec3::default();
        let mut hits = 0u8;
        for side in [self.forward, -self.forward, right, -right] {
            let top = at + side * RING_SIDE + UP * RING_RISE;
            let ground = top - UP * (RING_RISE * 2.0);
            if let Some(hit) = self.caster.cast(top, ground, MASK_GROUND) {
                normals = normals + hit.normal;
                hits = hits.saturating_add(1);
            }
        }
        hits >= RING_HITS
            && normals
                .normalized()
                .is_some_and(|normal| normal.y >= self.query.walkable_ny)
    }

    /// `hit`, if it can be stood on: its own triangle first, because that costs no ray. A player
    /// who cannot move on gets no second look at this spot, so there the area alone decides.
    fn floor(&self, hit: Option<Hit>) -> Option<Hit> {
        let query = self.query;
        hit.filter(|hit| {
            (query.stuck || hit.normal.y >= query.walkable_ny) && self.walkable_around(hit.point)
        })
    }

    /// The patch, if it has turned walkable.
    fn walkable_patch(&self) -> Option<Hit> {
        let query = self.query;
        query
            .patch
            .filter(|patch| patch.normal.y >= query.walkable_ny)
    }

    /// The ground under the patch, if the patch has turned walkable. On a long curve the body lies
    /// along the rock and the chest reaches flat ground well before anything ahead of the feet
    /// does. Waiting for the feet would stop the climb at the hold limit with no exit.
    fn over_the_patch(&self) -> Option<Hit> {
        let above = self.walkable_patch()?.point + UP * PATCH_RISE;
        let below = above - UP * (PATCH_RISE * 2.0);
        self.floor(self.caster.cast(above, below, MASK_GROUND))
    }

    /// The first ground straight under `above`.
    fn ground_under(&self, above: Vec3) -> Option<Hit> {
        let below = above - UP * (FLOOR_DROP + FACE_RISE);
        self.caster.cast(above, below, MASK_GROUND)
    }

    /// How far ahead of `start` the air is clear, or `None` when the face goes on there. Rock
    /// ahead that rises gently from the ground under `start` is a slope to stand on, and rock far
    /// off the plane of the climb is another rock, such as the wall behind a shelf.
    fn clear_ahead(&self, start: Vec3) -> Option<f32> {
        let query = self.query;
        let end = start + self.forward * query.face.length;
        let Some(hit) = self.caster.cast(start, end, MASK_GROUND) else {
            return Some(query.face.length);
        };
        let run = (hit.point - start).dot(self.forward);
        let steep = self
            .ground_under(start)
            .is_none_or(|ground| hit.point.y - ground.point.y >= run * query.face.slope);
        let on_plane = |patch: Hit| (hit.point - patch.point).dot(query.normal).abs() < FACE_PLANE;
        (!steep || !query.patch.is_none_or(on_plane)).then_some(run)
    }

    /// A floor past the lip, under the air that [`Search::clear_ahead`] found clear.
    fn past_the_lip(&self, start: Vec3, clear: f32) -> Option<Hit> {
        FLOOR_STEPS
            .into_iter()
            .take_while(|step| *step < clear)
            .find_map(|step| self.floor(self.ground_under(start + self.forward * step)))
    }
}

/// Correct the game's exit answer for a natural surface.
pub fn decide<C: Caster>(caster: &C, query: &Query) -> Exit {
    let no_exit = query.floor.map_or(Exit::Keep, |_| Exit::Refuse);
    // The game's exit also needs the stick up, and that alone is not enough: for a few frames
    // after going down the stick is up and the player still moves down.
    let Some(forward) = (-query.normal)
        .flat()
        .normalized()
        .filter(|_| query.moving_up)
    else {
        return no_exit;
    };
    let search = Search {
        caster,
        query,
        forward,
    };
    let head = query.pos + UP * HEAD_HEIGHT;
    let reach = head - query.normal * HEAD_REACH;
    let start = reach + UP * FACE_RISE;
    // A player stopped over a walkable patch has come to the end of the climb, whatever the rock
    // ahead looks like. Nothing changes here from frame to frame, so a "no" would stand for good.
    let clear = if query.stuck && search.walkable_patch().is_some() {
        Some(query.face.length)
    } else {
        search.clear_ahead(start)
    };
    let Some(clear) = clear else {
        return no_exit;
    };
    if search.floor(query.floor).is_some() {
        return Exit::Keep;
    }
    if let Some(ground) = search.over_the_patch() {
        return Exit::Take(ground);
    }
    if query.floor.is_none() {
        // Something at head height: a slope to stand on, or the wall going on.
        if let Some(hit) = caster.cast(head, reach, MASK_HEAD) {
            return search.floor(Some(hit)).map_or(Exit::Keep, Exit::Take);
        }
    }
    search
        .past_the_lip(start, clear)
        .map_or(no_exit, Exit::Take)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testworld::{face, tuning, Plane, Profile, Scripted};

    /// A floor hit with this much up in its normal.
    fn floor(ny: f32) -> Hit {
        Hit {
            point: Vec3::new(1.0, 1.0, 0.0),
            normal: Vec3::new(-(1.0 - ny * ny).sqrt(), ny, 0.0),
            entity: 7,
        }
    }

    /// A hit at this place.
    fn hit_at(x: f32, y: f32) -> Hit {
        Hit {
            point: Vec3::new(x, y, 0.0),
            normal: face(90.0),
            entity: 0,
        }
    }

    /// A query at the origin on a sheer wall, moving up. The level ray starts at (1.05, 2.6).
    fn query(game: Option<f32>) -> Query {
        Query {
            pos: Vec3::default(),
            normal: face(90.0),
            moving_up: true,
            floor: game.map(floor),
            patch: None,
            walkable_ny: tuning().walkable_ny,
            face: tuning().face,
            stuck: false,
        }
    }

    /// The patch of a climber on the sheer wall of [`query`].
    fn wall_patch() -> Option<Hit> {
        Some(Hit {
            point: Vec3::new(1.0, 1.6, 0.0),
            normal: face(90.0),
            entity: 0,
        })
    }

    /// The answers of a clear level ray, then of a ring on this ground.
    fn clear_then_ring(ground: Hit) -> [Option<Hit>; 5] {
        [None, Some(ground), Some(ground), Some(ground), Some(ground)]
    }

    /// The default level ray reaches a plane past the walkable limit, and stops short of one the
    /// player can stand on. Each is the plane the player climbs.
    #[test]
    fn the_face_ray_reaches_steep_planes_only() {
        let t = tuning();
        let reaches = |angle: f32| {
            let normal = face(angle);
            let feet = normal * GAME_STANDOFF;
            let start = feet + UP * (HEAD_HEIGHT + FACE_RISE) - normal * HEAD_REACH;
            let end = start + Vec3::new(t.face.length, 0.0, 0.0);
            Plane::through(Vec3::default(), normal)
                .cast(start, end, 0)
                .is_some()
        };
        for angle in [48.0, 50.0, 52.0, 55.0, 60.0, 70.0, 75.0] {
            assert!(reaches(angle), "{angle} goes on");
        }
        for angle in [20.0, 35.0, 45.0, 46.0] {
            assert!(!reaches(angle), "{angle} is ground");
        }
    }

    /// A lower walkable limit needs a longer look, and a lower slope makes a face.
    #[test]
    fn the_face_ray_follows_the_limit() {
        let (low, normal, high) = (FaceRay::new(30.0), FaceRay::new(50.0), FaceRay::new(70.0));
        assert!(low.length > normal.length && normal.length > high.length);
        assert!(high.length > 0.0);
        assert!(low.slope < normal.slope && normal.slope < high.slope);
        assert!((normal.slope - 42f32.to_radians().tan()).abs() < 1e-5);
    }

    /// The chest has reached ground the player can stand on: the long curve that stuck.
    #[test]
    fn a_walkable_patch_is_taken() {
        let ground = floor(0.9);
        let world = Scripted::new(&[
            None,
            Some(ground),
            Some(ground),
            Some(ground),
            Some(ground),
            Some(ground),
        ]);
        let mut chest = query(None);
        chest.patch = Some(floor(0.8));
        assert_eq!(decide(&world, &chest), Exit::Take(ground));
        assert_eq!(world.casts(), 6);
    }

    /// The patch also beats a steep answer from the game.
    #[test]
    fn a_walkable_patch_replaces_steep_rock() {
        let ground = floor(0.9);
        let world = Scripted::new(&[
            None,
            Some(ground),
            Some(ground),
            Some(ground),
            Some(ground),
            Some(ground),
        ]);
        let mut chest = query(Some(0.3));
        chest.patch = Some(floor(0.8));
        assert_eq!(decide(&world, &chest), Exit::Take(ground));
    }

    /// A steep patch is the wall, and costs no ray of its own.
    #[test]
    fn a_steep_patch_changes_nothing() {
        let world = Scripted::new(&[None, Some(floor(0.1))]);
        let mut chest = query(None);
        chest.patch = Some(floor(0.3));
        assert_eq!(decide(&world, &chest), Exit::Keep);
        assert_eq!(world.casts(), 2);
    }

    /// The ground under a walkable patch must be walkable too: one flat facet in a rough face
    /// does not end a climb.
    #[test]
    fn steep_ground_under_a_walkable_patch_is_not_taken() {
        let world = Scripted::new(&[None, Some(floor(0.2)), Some(floor(0.1))]);
        let mut chest = query(None);
        chest.patch = Some(floor(0.8));
        assert_eq!(decide(&world, &chest), Exit::Keep);
    }

    /// Hanging still on a walkable patch starts nothing: the mantle is the player's choice.
    #[test]
    fn a_walkable_patch_waits_for_up() {
        let world = Scripted::new(&[]);
        let mut still = query(None);
        still.moving_up = false;
        still.patch = Some(floor(0.8));
        assert_eq!(decide(&world, &still), Exit::Keep);
        assert_eq!(world.casts(), 0);
    }

    /// Steep rock stays no exit while the player is still, and costs no rays.
    #[test]
    fn steep_rock_is_refused_without_rays_while_still() {
        let world = Scripted::new(&[]);
        let mut still = query(Some(0.3));
        still.moving_up = false;
        assert_eq!(decide(&world, &still), Exit::Refuse);
        assert_eq!(world.casts(), 0);
    }

    /// After going down the stick is up for a few frames while the player still moves down, and
    /// the game's exit goes by the stick. One flat triangle must not mantle in those frames.
    #[test]
    fn a_game_floor_is_refused_without_rays_unless_moving_up() {
        let world = Scripted::new(&[]);
        let mut turning = query(Some(0.95));
        turning.moving_up = false;
        assert_eq!(decide(&world, &turning), Exit::Refuse);
        assert_eq!(world.casts(), 0);
    }

    /// A real floor keeps the game's mantle: clear air ahead, and ground all round.
    #[test]
    fn a_walkable_floor_keeps_the_games_answer() {
        let world = Scripted::new(&clear_then_ring(floor(0.95)));
        assert_eq!(decide(&world, &query(Some(0.95))), Exit::Keep);
        assert_eq!(world.casts(), 5);
    }

    /// One flat triangle in a steep face: the game's floor reads walkable, the air ahead is clear,
    /// and the ground around it is steep.
    #[test]
    fn one_flat_triangle_in_a_steep_ring_is_no_floor() {
        let world = Scripted::new(&clear_then_ring(floor(0.45)));
        assert_eq!(decide(&world, &query(Some(0.95))), Exit::Refuse);
    }

    /// The face going on ends the question at two rays, however flat the game's floor reads.
    #[test]
    fn a_face_ahead_refuses_a_flat_triangle_with_two_rays() {
        let answers = [Some(hit_at(1.6, 2.6)), Some(hit_at(1.05, 1.2))];
        let world = Scripted::new(&answers);
        assert_eq!(decide(&world, &query(Some(0.95))), Exit::Refuse);
        assert_eq!(world.casts(), 2);
    }

    /// With no answer from the game a face ahead is simply no exit.
    #[test]
    fn a_face_ahead_keeps_no_exit() {
        let answers = [Some(hit_at(1.6, 2.6)), Some(hit_at(1.05, 1.2))];
        let world = Scripted::new(&answers);
        assert_eq!(decide(&world, &query(None)), Exit::Keep);
        assert_eq!(world.casts(), 2);
    }

    /// With nothing under the level ray there is no slope to measure, and rock ahead is a face.
    #[test]
    fn rock_ahead_of_nothing_is_a_face() {
        let world = Scripted::new(&[Some(hit_at(1.6, 2.6))]);
        assert_eq!(decide(&world, &query(Some(0.95))), Exit::Refuse);
        assert_eq!(world.casts(), 2);
    }

    /// Rock ahead that rises gently from the ground is a slope: the wall has bent, and the body
    /// has only turned part of the way. This stuck a climber under a 35 degree slope.
    #[test]
    fn a_gentle_rise_ahead_is_no_face() {
        let ground = floor(0.95);
        let answers = [
            Some(hit_at(2.2, 2.6)),
            Some(hit_at(1.05, 2.0)),
            Some(ground),
            Some(ground),
            Some(ground),
            Some(ground),
        ];
        let mut chest = query(Some(0.95));
        chest.patch = wall_patch();
        assert_eq!(decide(&Scripted::new(&answers), &chest), Exit::Keep);
    }

    /// The slope is exact: on the limit is a face, just under it is not.
    #[test]
    fn the_face_slope_is_inclusive() {
        let slope = tuning().face.slope;
        let run = 1.0;
        let on = 2.6 - run * slope;
        let answers = [Some(hit_at(1.05 + run, 2.6)), Some(hit_at(1.05, on))];
        let world = Scripted::new(&answers);
        assert_eq!(decide(&world, &query(Some(0.95))), Exit::Refuse);
        assert_eq!(world.casts(), 2);
        let under = on + 0.01;
        let answers = [Some(hit_at(1.05 + run, 2.6)), Some(hit_at(1.05, under))];
        let world = Scripted::new(&answers);
        decide(&world, &query(Some(0.95)));
        assert!(world.casts() > 2, "the search goes on");
    }

    /// A shelf: the rock ahead stands well behind the plane of the climb, so it is not this face.
    /// The floor in front of it still counts.
    #[test]
    fn rock_far_behind_the_plane_is_another_wall() {
        let ground = floor(0.95);
        let answers = [
            Some(hit_at(1.75, 2.6)),
            Some(hit_at(1.05, 1.0)),
            Some(ground),
            Some(ground),
            Some(ground),
            Some(ground),
        ];
        let mut chest = query(Some(0.95));
        chest.patch = wall_patch();
        assert_eq!(decide(&Scripted::new(&answers), &chest), Exit::Keep);
    }

    /// The same rock closer to the plane of the climb is the face going on.
    #[test]
    fn rock_near_the_plane_is_the_face() {
        let answers = [Some(hit_at(1.65, 2.6)), Some(hit_at(1.05, 1.0))];
        let mut chest = query(Some(0.95));
        chest.patch = wall_patch();
        let world = Scripted::new(&answers);
        assert_eq!(decide(&world, &chest), Exit::Refuse);
        assert_eq!(world.casts(), 2);
    }

    /// The look past the lip stops at the rock it found.
    #[test]
    fn the_floor_search_stops_at_another_wall() {
        let mut chest = query(None);
        chest.pos = Vec3::new(-0.45, 0.0, 0.0);
        chest.patch = Some(Hit {
            point: Vec3::new(0.0, 1.6, 0.0),
            normal: face(90.0),
            entity: 0,
        });
        // The hit is 0.45 m ahead of the ray's start, so only the first two columns are open.
        let answers = [Some(hit_at(1.05, 2.6)), Some(hit_at(0.6, 1.0))];
        let world = Scripted::new(&answers);
        assert_eq!(decide(&world, &chest), Exit::Keep);
        assert_eq!(world.casts(), 5);
    }

    /// The rounded lip of a real ledge must not strand the player under it.
    #[test]
    fn a_flat_top_behind_a_steep_lip_is_taken() {
        let flat = floor(0.98);
        let world = Scripted::new(&[
            None,
            Some(floor(0.3)),
            Some(flat),
            Some(flat),
            Some(flat),
            Some(flat),
            Some(flat),
        ]);
        assert_eq!(decide(&world, &query(Some(0.3))), Exit::Take(flat));
        assert_eq!(world.casts(), 7);
    }

    /// A smooth 60 degree face under a flat top, at the place where the move check stops the
    /// climber. The game's ray lands on the face, and the top is 0.87 m on from the feet.
    #[test]
    fn the_search_reaches_past_the_top_of_a_leaning_face() {
        let corner = (7.0 / 60f32.to_radians().tan(), 7.0);
        let rock = Profile::new(vec![(0.0, 0.0), corner, (corner.0 + 50.0, 7.0)]);
        let normal = face(60.0);
        let foot = 5.27;
        let mut chest = query(None);
        chest.normal = normal;
        chest.pos = Vec3::new(foot / 60f32.to_radians().tan(), foot, 0.0) + normal * 0.55;
        let Exit::Take(hit) = decide(&rock, &chest) else {
            panic!("no exit under a flat top");
        };
        assert!((hit.point.y - 7.0).abs() < 1e-3, "{hit:?}");
    }

    /// A ledge has one side that falls away: three rays of the ring are enough.
    #[test]
    fn three_ring_rays_make_a_floor() {
        let ground = floor(0.95);
        let world = Scripted::new(&[None, Some(ground), None, Some(ground), Some(ground)]);
        assert_eq!(decide(&world, &query(Some(0.95))), Exit::Keep);
    }

    /// Two rays of the ring are no area.
    #[test]
    fn two_ring_rays_make_no_floor() {
        let ground = floor(0.95);
        let world = Scripted::new(&[None, Some(ground), None, None, Some(ground)]);
        assert_eq!(decide(&world, &query(Some(0.95))), Exit::Refuse);
    }

    /// A player the move check stopped gets no second look at this spot, so one steep triangle
    /// must not keep them there. The ring still decides.
    #[test]
    fn a_stuck_player_is_judged_by_the_area_alone() {
        let ground = floor(0.95);
        let mut stuck = query(Some(0.3));
        stuck.stuck = true;
        let world = Scripted::new(&clear_then_ring(ground));
        assert_eq!(decide(&world, &stuck), Exit::Keep);
        let world = Scripted::new(&clear_then_ring(floor(0.3)));
        assert_eq!(decide(&world, &stuck), Exit::Refuse);
    }

    /// Stopped over a walkable patch is the end of the climb. The rock ahead has no say, and a
    /// player who was moving past a flat reading keeps the face test.
    #[test]
    fn a_stuck_player_over_a_walkable_patch_skips_the_face_test() {
        let ground = floor(0.95);
        let mut chest = query(None);
        chest.patch = Some(floor(0.8));
        chest.stuck = true;
        let world = Scripted::new(&[
            Some(ground),
            Some(ground),
            Some(ground),
            Some(ground),
            Some(ground),
        ]);
        assert_eq!(decide(&world, &chest), Exit::Take(ground));
        assert_eq!(world.casts(), 5);
        chest.stuck = false;
        let answers = [Some(hit_at(1.6, 2.6)), Some(hit_at(1.05, 1.2))];
        assert_eq!(decide(&Scripted::new(&answers), &chest), Exit::Keep);
    }

    /// Stopped under steep rock is no exit: the face test still stands.
    #[test]
    fn a_stuck_player_on_steep_rock_keeps_the_face_test() {
        let answers = [Some(hit_at(1.6, 2.6)), Some(hit_at(1.05, 1.2))];
        let mut stuck = query(Some(0.95));
        stuck.patch = wall_patch();
        stuck.stuck = true;
        let world = Scripted::new(&answers);
        assert_eq!(decide(&world, &stuck), Exit::Refuse);
        assert_eq!(world.casts(), 2);
    }

    /// Hanging still on a wall costs no extra rays, and no exit appears from nowhere.
    #[test]
    fn no_search_without_up_movement() {
        let world = Scripted::new(&[]);
        let mut still = query(None);
        still.moving_up = false;
        assert_eq!(decide(&world, &still), Exit::Keep);
        assert_eq!(world.casts(), 0);
    }

    /// A wall that goes on is the everyday case: two rays, and nothing changes.
    #[test]
    fn a_wall_going_on_keeps_the_games_answer() {
        let world = Scripted::new(&[None, Some(floor(0.1))]);
        assert_eq!(decide(&world, &query(None)), Exit::Keep);
        assert_eq!(world.casts(), 2);
    }

    /// The wall has become a slope at head height. This is the band where the player stuck:
    /// the move check blocked up, and ray A saw the slope, so the game had no exit.
    #[test]
    fn a_slope_at_head_height_is_taken() {
        let slope = floor(0.8);
        let world = Scripted::new(&[
            None,
            Some(slope),
            Some(slope),
            Some(slope),
            Some(slope),
            Some(slope),
        ]);
        assert_eq!(decide(&world, &query(None)), Exit::Take(slope));
    }

    /// The game's ray B is too short for a shallow slope. The longer ray finds it.
    #[test]
    fn a_shallow_slope_below_ray_a_is_taken() {
        let slope = floor(0.9);
        let world = Scripted::new(&[
            None,
            None,
            Some(slope),
            Some(slope),
            Some(slope),
            Some(slope),
            Some(slope),
        ]);
        assert_eq!(decide(&world, &query(None)), Exit::Take(slope));
    }

    /// Clear air above a steep drop is no floor.
    #[test]
    fn steep_rock_below_ray_a_is_no_exit() {
        let steep = Some(floor(0.2));
        let world = Scripted::new(&[None, None, steep, steep, steep, steep]);
        assert_eq!(decide(&world, &query(None)), Exit::Keep);
        let world = Scripted::new(&[]);
        assert_eq!(decide(&world, &query(None)), Exit::Keep);
    }

    /// The limit is exact for the game's own floor: on it counts as walkable, below it does not.
    #[test]
    fn the_walkable_limit_is_inclusive() {
        let limit = tuning().walkable_ny;
        let world = Scripted::new(&clear_then_ring(floor(0.95)));
        assert_eq!(decide(&world, &query(Some(limit))), Exit::Keep);
        let below = Scripted::new(&[]);
        assert_eq!(decide(&below, &query(Some(limit - 0.01))), Exit::Refuse);
    }

    /// Ground that reads flat to the displaced body and steep to the undisplaced one.
    struct TwoBodies;

    impl Caster for TwoBodies {
        fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit> {
            let ny = if mask & 1 != 0 { 0.95 } else { 0.3 };
            (to.y < from.y).then(|| Hit {
                point: from + (to - from) * 0.5,
                normal: floor(ny).normal,
                entity: 0,
            })
        }
    }

    /// The ring and the search ask the body with undisplaced normals. With the game's own mask they
    /// would read the one tilted triangle this module exists to look past.
    #[test]
    fn the_ground_rays_ask_for_the_undisplaced_body() {
        assert_eq!(decide(&TwoBodies, &query(Some(0.95))), Exit::Refuse);
        assert_eq!(decide(&TwoBodies, &query(None)), Exit::Keep);
    }

    /// A 75 degree face under a flat top with a bump on it, at the place where the move check
    /// stops the climber. Ray A's end is only 0.15 m above the top there, so a level ray from that
    /// height reads the bump as the face going on.
    #[test]
    fn a_bump_on_the_top_is_not_the_face() {
        let lean = 75f32.to_radians().tan();
        let corner = 7.0 / lean;
        let rock = Profile::new(vec![
            (0.0, 0.0),
            (corner, 7.0),
            (corner + 0.05, 7.0),
            (corner + 0.1, 7.4),
            (corner + 0.4, 7.0),
            (corner + 50.0, 7.0),
        ]);
        let normal = face(75.0);
        let foot = 5.18;
        let mut chest = query(None);
        chest.normal = normal;
        chest.pos = Vec3::new(foot / lean, foot, 0.0) + normal * 0.55;
        assert!(matches!(decide(&rock, &chest), Exit::Take(_)));
    }

    /// The whole decision on a sharp top: the search walks past the steep lip and lands on it.
    #[test]
    fn a_flat_top_past_the_reach_is_found_by_position() {
        let rock = Profile::ledge(0.0, 2.0);
        let mut chest = query(None);
        chest.pos = Vec3::new(-1.2, 0.0, 0.0);
        let Exit::Take(hit) = decide(&rock, &chest) else {
            panic!("no exit over a ledge");
        };
        assert!((hit.point.y - 2.0).abs() < 1e-4, "{hit:?}");
    }
}
