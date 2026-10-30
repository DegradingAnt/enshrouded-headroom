//! How a climb ends at the top.
//!
//! The game's exit query (`FUN_14024a6f0`) casts ray A from 2.1 m up, 1.05 m into the wall, then
//! ray B 1.35 m down from A's end. It has no walkable test. On a 48 to 76 degree face A misses and
//! B lands on the rock itself, so the mantle plays on a cliff. Where a wall rounds into a slope A
//! hits the slope, so no exit exists at all and the player sticks. This module corrects both, and
//! only for natural surfaces; a lattice keeps the game's answer.

use crate::geom::{Vec3, UP};
use crate::surface::{Caster, Hit};

/// Height of ray A above the feet, the game's own number.
const HEAD_HEIGHT: f32 = 2.1;
/// How far ray A reaches along the wall normal, the game's own number.
const HEAD_REACH: f32 = 1.05;
/// Ray A's mask, the game's own: World, Solid, Walkable, ClimbingWall.
const MASK_HEAD: u32 = 0x129;
/// Ray B's mask, the game's own: World, Walkable.
const MASK_FLOOR: u32 = 0x21;
/// How far down the floor search reaches from ray A's end. The game's 1.35 m misses a shallow
/// slope, which lies farther below that point.
const FLOOR_DROP: f32 = 2.6;
/// How much farther in to look when the first floor hit is the rounded lip of a ledge.
const LIP_STEP: f32 = 0.4;
/// How far above and below a walkable patch to look for the ground under it.
const PATCH_RISE: f32 = 1.0;

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
    /// Is the player moving up? Only then is a new exit worth rays.
    pub moving_up: bool,
    /// The up part of the floor normal the game found, or `None` when it found no exit.
    pub floor_ny: Option<f32>,
    /// The surface in front of the chest this frame, as one hit: its centre and averaged normal.
    pub patch: Option<Hit>,
    /// A floor with at least this much up in its normal can be stood on.
    pub walkable_ny: f32,
}

/// `hit`, if it can be stood on.
fn walkable(hit: Option<Hit>, walkable_ny: f32) -> Option<Hit> {
    hit.filter(|hit| hit.normal.y >= walkable_ny)
}

/// A floor a little past `from`, if the way there is clear. A rounded ledge has a steep lip with
/// a flat top right behind it.
fn past_the_lip<C: Caster>(caster: &C, from: Vec3, query: &Query) -> Option<Hit> {
    let forward = (-query.normal).flat().normalized()?;
    let further = from + forward * LIP_STEP;
    if caster.cast(from, further, MASK_HEAD).is_some() {
        return None;
    }
    let floor = caster.cast(further, further - UP * FLOOR_DROP, MASK_FLOOR);
    walkable(floor, query.walkable_ny)
}

/// The ground under the patch, if the patch has turned walkable. On a long curve the body lies
/// along the rock and the chest reaches flat ground well before anything ahead of the feet does.
/// Waiting for the feet would stop the climb at the hold limit with no exit.
fn over_the_patch<C: Caster>(caster: &C, query: &Query) -> Option<Hit> {
    let patch = query
        .patch
        .filter(|patch| patch.normal.y >= query.walkable_ny)?;
    let above = patch.point + UP * PATCH_RISE;
    let ground = caster.cast(above, above - UP * (PATCH_RISE * 2.0), MASK_FLOOR);
    walkable(ground, query.walkable_ny)
}

/// Correct the game's exit answer for a natural surface.
pub fn decide<C: Caster>(caster: &C, query: &Query) -> Exit {
    let head = query.pos + UP * HEAD_HEIGHT;
    let reach = head - query.normal * HEAD_REACH;
    match query.floor_ny {
        Some(ny) if ny >= query.walkable_ny => return Exit::Keep,
        Some(_) if !query.moving_up => return Exit::Refuse,
        None if !query.moving_up => return Exit::Keep,
        _ => {}
    }
    if let Some(ground) = over_the_patch(caster, query) {
        return Exit::Take(ground);
    }
    match query.floor_ny {
        Some(_) => past_the_lip(caster, reach, query).map_or(Exit::Refuse, Exit::Take),
        None => match caster.cast(head, reach, MASK_HEAD) {
            // Something at head height: a slope to stand on, or the wall going on.
            Some(hit) => walkable(Some(hit), query.walkable_ny).map_or(Exit::Keep, Exit::Take),
            // The space ahead is clear, so a ray down from there starts in the open.
            None => {
                let floor = caster.cast(reach, reach - UP * FLOOR_DROP, MASK_FLOOR);
                walkable(floor, query.walkable_ny).map_or(Exit::Keep, Exit::Take)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testworld::{face, tuning, Scripted};

    /// A floor hit with this much up in its normal.
    fn floor(ny: f32) -> Hit {
        Hit {
            point: Vec3::new(1.0, 1.0, 0.0),
            normal: Vec3::new(-(1.0 - ny * ny).sqrt(), ny, 0.0),
            entity: 7,
        }
    }

    /// A query at the origin on a sheer wall, moving up.
    fn query(floor_ny: Option<f32>) -> Query {
        Query {
            pos: Vec3::default(),
            normal: face(90.0),
            moving_up: true,
            floor_ny,
            patch: None,
            walkable_ny: tuning().walkable_ny,
        }
    }

    /// The chest has reached ground the player can stand on: the long curve that stuck.
    #[test]
    fn a_walkable_patch_is_taken() {
        let ground = floor(0.9);
        let world = Scripted::new(&[Some(ground)]);
        let mut chest = query(None);
        chest.patch = Some(floor(0.8));
        assert_eq!(decide(&world, &chest), Exit::Take(ground));
        assert_eq!(world.casts(), 1);
    }

    /// The patch also beats a steep answer from the game.
    #[test]
    fn a_walkable_patch_replaces_steep_rock() {
        let ground = floor(0.9);
        let world = Scripted::new(&[Some(ground)]);
        let mut chest = query(Some(0.3));
        chest.patch = Some(floor(0.8));
        assert_eq!(decide(&world, &chest), Exit::Take(ground));
    }

    /// A steep patch is the wall, and costs no ray of its own.
    #[test]
    fn a_steep_patch_changes_nothing() {
        let world = Scripted::new(&[Some(floor(0.1))]);
        let mut chest = query(None);
        chest.patch = Some(floor(0.3));
        assert_eq!(decide(&world, &chest), Exit::Keep);
        assert_eq!(world.casts(), 1);
    }

    /// The ground under a walkable patch must be walkable too: one flat facet in a rough face
    /// does not end a climb.
    #[test]
    fn steep_ground_under_a_walkable_patch_is_not_taken() {
        let world = Scripted::new(&[Some(floor(0.2)), Some(floor(0.1))]);
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

    /// A real floor keeps the game's mantle, and costs no rays.
    #[test]
    fn a_walkable_floor_keeps_the_games_answer() {
        let world = Scripted::new(&[]);
        assert_eq!(decide(&world, &query(Some(0.95))), Exit::Keep);
        assert_eq!(world.casts(), 0);
    }

    /// The game's exit landed on the rock itself: this is the mantle onto a cliff face.
    #[test]
    fn steep_rock_is_no_exit() {
        let world = Scripted::new(&[None, Some(floor(0.3))]);
        assert_eq!(decide(&world, &query(Some(0.3))), Exit::Refuse);
    }

    /// The rounded lip of a real ledge must not strand the player under it.
    #[test]
    fn a_flat_top_behind_a_steep_lip_is_taken() {
        let flat = floor(0.98);
        let world = Scripted::new(&[None, Some(flat)]);
        assert_eq!(decide(&world, &query(Some(0.3))), Exit::Take(flat));
    }

    /// With rock in the way there is no looking past the lip.
    #[test]
    fn a_wall_behind_the_lip_is_no_exit() {
        let world = Scripted::new(&[Some(floor(0.1))]);
        assert_eq!(decide(&world, &query(Some(0.3))), Exit::Refuse);
        assert_eq!(world.casts(), 1);
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

    /// A wall that goes on is the everyday case: one ray, and nothing changes.
    #[test]
    fn a_wall_going_on_keeps_the_games_answer() {
        let world = Scripted::new(&[Some(floor(0.1))]);
        assert_eq!(decide(&world, &query(None)), Exit::Keep);
        assert_eq!(world.casts(), 1);
    }

    /// The wall has become a slope at head height. This is the band where the player stuck:
    /// the move check blocked up, and ray A saw the slope, so the game had no exit.
    #[test]
    fn a_slope_at_head_height_is_taken() {
        let slope = floor(0.8);
        let world = Scripted::new(&[Some(slope)]);
        assert_eq!(decide(&world, &query(None)), Exit::Take(slope));
    }

    /// The game's ray B is too short for a shallow slope. The longer ray finds it.
    #[test]
    fn a_shallow_slope_below_ray_a_is_taken() {
        let slope = floor(0.9);
        let world = Scripted::new(&[None, Some(slope)]);
        assert_eq!(decide(&world, &query(None)), Exit::Take(slope));
    }

    /// Clear air above a steep drop is no floor.
    #[test]
    fn steep_rock_below_ray_a_is_no_exit() {
        let world = Scripted::new(&[None, Some(floor(0.2))]);
        assert_eq!(decide(&world, &query(None)), Exit::Keep);
        let world = Scripted::new(&[None, None]);
        assert_eq!(decide(&world, &query(None)), Exit::Keep);
    }

    /// The limit is exact: on it counts as walkable, below it does not.
    #[test]
    fn the_walkable_limit_is_inclusive() {
        let limit = tuning().walkable_ny;
        assert_eq!(decide(&Scripted::new(&[]), &query(Some(limit))), Exit::Keep);
        let below = Scripted::new(&[None, None]);
        assert_eq!(decide(&below, &query(Some(limit - 0.01))), Exit::Refuse);
    }
}
