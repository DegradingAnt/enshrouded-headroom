//! Fresh, bounded proposals for static vertical wall corners.
//!
//! This is a pure policy. A proposal needs new ray hits at its actual destination and an explicit
//! complete capsule-motion oracle. It installs no hooks and changes no native state.

use crate::geom::{Vec3, UP};
use crate::surface::{
    Capsule, Caster, Fit, Frame, Hit, Tuning, GAME_STANDOFF, NORMAL_TIME, RAY_ROWS, RAY_SIDE,
};

mod velocity;
pub(crate) use velocity::Usage;

pub use velocity::{
    fresh_support, prepare_from_patch, BudgetCaster, FreshPatch, NativeBudget, Supported,
    VelocityPlan, MAX_NATIVE_RAYS,
};

/// Maximum ray casts shared by one outer move-check call, including failed attempts and retries.
pub const MAX_RAYS: u8 = 24;
/// Maximum complete motion checks shared by that call.
pub const MAX_MOTIONS: u8 = 2;

/// Native movement and an orientation hint, before a corner proposal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Request {
    /// Actual current feet.
    pub position: Vec3,
    /// The native post-acceleration velocity, including its analog magnitude.
    pub velocity: Vec3,
    /// Orientation hint only. It grants no surface permission.
    pub normal: Vec3,
    /// Positive simulation seconds.
    pub dt: f32,
}

/// A complete motion query's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clearance {
    /// The actual stance capsule can move along the whole segment in a complete coherent scene.
    Clear,
    /// The capsule or its path is blocked.
    Blocked,
    /// Readiness, completeness, ownership or another required contract is unknown.
    Unknown,
}

/// An explicit capsule-motion boundary. The implementation owns filters and scene lifetime.
pub trait MotionOracle {
    /// Check the entire actual capsule translation, including required start/end validity.
    /// `Clear` must not mean only that an endpoint overlap found no contacts.
    fn check(&mut self, from: Vec3, to: Vec3, capsule: Capsule) -> Clearance;
}

/// The corner's proposed route. Neither kind is collision geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Around an outside edge.
    Convex,
    /// Around an inside junction.
    Concave,
}

/// Why this policy supplies no corner move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// Input is not finite or cannot describe a moving vertical-wall corner.
    Invalid,
    /// Fresh support or a usable adjacent face is missing.
    NoSupport,
    /// The shared ray or motion budget is spent.
    Budget,
    /// The complete capsule motion is blocked.
    Blocked,
    /// The complete capsule motion is not known to be clear.
    Unknown,
}

/// Query counts for one outer move-check scope. Reuse this across every attempt and revalidation.
#[derive(Debug, Default)]
pub struct Budget {
    /// Actual caster calls so far.
    rays: u8,
    /// Actual oracle calls so far.
    motions: u8,
}

impl Budget {
    /// A fresh outer-call budget. Never reset it for an inner retry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Actual caster calls consumed, including misses and bad proposals.
    pub fn rays(&self) -> u8 {
        self.rays
    }

    /// Actual complete motion checks consumed.
    pub fn motions(&self) -> u8 {
        self.motions
    }
}

/// A candidate supported by real geometry, with facing kept separate from that geometry.
#[derive(Debug, PartialEq)]
pub struct Step {
    /// Actual proposed feet.
    pub position: Vec3,
    /// Translation velocity from the request's actual feet.
    pub velocity: Vec3,
    /// Eased orientation reference. Never use this as a physical fit normal.
    pub facing: Vec3,
    /// Three or more distinct noncollinear fresh hits on one actual face.
    pub support: Fit,
    /// The guide used for this step.
    pub route: Route,
    /// Per-call guidance only; revalidation obtains new actual hits.
    guide: Guide,
}

/// Propose one bounded move. This does not authorize a different native-selected destination.
pub fn propose<C: Caster, O: MotionOracle>(
    caster: &C,
    oracle: &mut O,
    budget: &mut Budget,
    request: &Request,
    tuning: &Tuning,
    capsule: Capsule,
) -> Result<Step, Reject> {
    valid(request, tuning, capsule)?;
    let mut query = Query {
        caster,
        oracle,
        budget,
        tuning,
        capsule,
    };
    let frame = Frame::along(-request.normal).ok_or(Reject::Invalid)?;
    let mut hits = [None; 4];
    for (slot, origin) in hits.iter_mut().zip(frame.origins(request.position)) {
        *slot = query.ray(origin, origin + frame.along * query.reach(), tuning.mask)?;
    }
    let (first, second) = witnesses(&hits).ok_or(Reject::NoSupport)?;
    let hint = request.normal.flat().normalized().ok_or(Reject::Invalid)?;
    let signed = request.velocity.dot(UP.cross(hint));
    let sign = signed.signum();
    let adjacent = match second {
        Some(hit) => hit,
        None => query.scout(request.position, first, sign)?,
    };
    let feature = Feature::of(first, adjacent, sign).ok_or(Reject::NoSupport)?;
    let edge = feature.edge(request.position.y).ok_or(Reject::NoSupport)?;
    if (edge - request.position).flat().length() > query.reach() + RAY_SIDE {
        return Err(Reject::NoSupport);
    }
    let guide = Guide {
        feature,
        request: *request,
    };
    let destination = feature.advance(request, signed.abs() * request.dt, query.radius())?;
    query.admit(guide, destination)
}

/// Recheck the actual velocity selected by the native move check, using the same outer-call budget.
/// Public proposal fields are not permission: destination support and motion are obtained again.
pub fn revalidate<C: Caster, O: MotionOracle>(
    caster: &C,
    oracle: &mut O,
    budget: &mut Budget,
    step: Step,
    final_velocity: Vec3,
    tuning: &Tuning,
    capsule: Capsule,
) -> Result<Step, Reject> {
    let guide = step.guide;
    valid(&guide.request, tuning, capsule)?;
    let speed = final_velocity.length();
    if !final_velocity.is_finite()
        || !(speed > EPS && speed.is_finite())
        || speed > guide.request.velocity.length() + EPS
    {
        return Err(Reject::Invalid);
    }
    let position = guide.request.position + final_velocity * guide.request.dt;
    if !position.is_finite() || (position - guide.request.position).length() > RAY_SIDE + EPS {
        return Err(Reject::Invalid);
    }
    Query {
        caster,
        oracle,
        budget,
        tuning,
        capsule,
    }
    .admit(guide, position)
}
/// Geometric comparisons are kept away from float degeneracy.
const EPS: f32 = 1e-4;
/// First milestone: vertical faces only, including ordinary normal rounding.
const VERTICAL: f32 = 1e-4;
/// Normals in one sampled face must agree within about six degrees.
const ALIGN: f32 = 0.995;
/// Stop just beyond a targeted surface while remaining inside ordinary Move reach.
const CROSS: f32 = 0.02;
/// Solid is the existing collider bit used by the ordinary entry-clearance rays.
const SOLID: u32 = 0x08;

/// Immutable guidance retained only by a returned proposal from this call.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Guide {
    /// Two actual observed face witnesses, never positive destination permission.
    feature: Feature,
    /// The original movement budget and current feet.
    request: Request,
}

/// Two observed faces ordered in the requested traversal direction.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Feature {
    /// Face approached before the junction.
    first: Hit,
    /// Face left after the junction.
    second: Hit,
    /// Requested wall-relative side.
    sign: f32,
    /// The physical junction kind.
    route: Route,
}

impl Feature {
    /// Order two fresh witnesses and reject ambiguous, flat or reverse-facing junctions.
    fn of(a: Hit, b: Hit, sign: f32) -> Option<Self> {
        if a.entity != b.entity || a.normal.y.abs() > VERTICAL || b.normal.y.abs() > VERTICAL {
            return None;
        }
        let cosine = a.normal.dot(b.normal);
        if !(-EPS..0.95).contains(&cosine) {
            return None;
        }
        let side_a = (a.point - b.point).dot(b.normal);
        let side_b = (b.point - a.point).dot(a.normal);
        let route = if side_a < -EPS && side_b < -EPS {
            Route::Convex
        } else if side_a > EPS && side_b > EPS {
            Route::Concave
        } else {
            return None;
        };
        for (first, second) in [(a, b), (b, a)] {
            let turning = UP.cross(first.normal).dot(second.normal) * sign;
            if (turning > EPS && route == Route::Convex)
                || (turning < -EPS && route == Route::Concave)
            {
                return Some(Self {
                    first,
                    second,
                    sign,
                    route,
                });
            }
        }
        None
    }

    /// Intersection of the observed planes at one feet height. It guides, but never grants a hit.
    fn edge(self, y: f32) -> Option<Vec3> {
        let (a, b) = (self.first.normal, self.second.normal);
        let determinant = a.x * b.z - a.z * b.x;
        if determinant.abs() < EPS {
            return None;
        }
        let ca = self.first.point.dot(a) - a.y * y;
        let cb = self.second.point.dot(b) - b.y * y;
        let edge = Vec3::new(
            (ca * b.z - a.z * cb) / determinant,
            y,
            (a.x * cb - ca * b.x) / determinant,
        );
        edge.is_finite().then_some(edge)
    }

    /// The local guide circle and its first outward direction, separate from physical support.
    fn arc(self, y: f32, radius: f32) -> Option<(Vec3, Vec3, Vec3, f32)> {
        let edge = self.edge(y)?;
        let n = self.first.normal.flat().normalized()?;
        let tangent = UP.cross(n) * self.sign;
        let angle = self
            .first
            .normal
            .dot(self.second.normal)
            .clamp(-1.0, 1.0)
            .acos();
        if self.route == Route::Convex {
            return Some((edge, n, tangent, angle));
        }
        let a = self.first.normal;
        let b = self.second.normal;
        let determinant = a.x * b.z - a.z * b.x;
        if determinant.abs() < EPS {
            return None;
        }
        let junction = edge
            + Vec3::new(
                radius * (b.z - a.z) / determinant,
                0.0,
                radius * (a.x - b.x) / determinant,
            );
        let bisector = (a + b).flat().normalized()?;
        let centre = junction + bisector * (RAY_SIDE / (angle * 0.5).cos());
        centre.is_finite().then_some((centre, -n, tangent, angle))
    }

    /// One finite guide step. The native displacement budget pays for every correction.
    fn advance(self, request: &Request, distance: f32, radius: f32) -> Result<Vec3, Reject> {
        let (centre, start, tangent, angle) = self
            .arc(request.position.y, radius)
            .ok_or(Reject::NoSupport)?;
        let before = (request.position - centre).dot(tangent);
        let flat = if before < 0.0 && distance <= -before {
            request.position + tangent * distance
        } else {
            let at = if before < 0.0 {
                request.position - tangent * before
            } else {
                request.position
            };
            let radial = (at - centre).flat();
            let length = radial.length();
            if !(length > EPS && length.is_finite()) {
                return Err(Reject::NoSupport);
            }
            let theta = radial.dot(tangent).atan2(radial.dot(start));
            if theta < -EPS {
                return Err(Reject::NoSupport);
            }
            if theta >= angle - EPS {
                let exit = UP
                    .cross(self.second.normal)
                    .normalized()
                    .ok_or(Reject::NoSupport)?
                    * self.sign;
                request.position + exit * distance
            } else {
                let travel = (distance + before.min(0.0)).max(0.0);
                let next = (theta + travel / length).min(angle);
                centre + (start * next.cos() + tangent * next.sin()) * length
            }
        };
        let next = Vec3::new(
            flat.x,
            request.position.y + request.velocity.y * request.dt,
            flat.z,
        );
        let step = (next - request.position).length();
        let native = request.velocity.length() * request.dt;
        if !next.is_finite() || step > native + EPS || step > RAY_SIDE + EPS {
            return Err(Reject::Invalid);
        }
        Ok(next)
    }

    /// Facing follows the guide, but its reference is never a fitted collision plane.
    fn facing(self, position: Vec3, radius: f32) -> Option<Vec3> {
        let (centre, start, tangent, angle) = self.arc(position.y, radius)?;
        let radial = (position - centre).flat();
        let theta = radial.dot(tangent).atan2(radial.dot(start));
        if theta <= 0.0 {
            return Some(self.first.normal);
        }
        if theta >= angle {
            return Some(self.second.normal);
        }
        let sign = if self.route == Route::Convex {
            1.0
        } else {
            -1.0
        };
        Some(radial.normalized()? * sign)
    }
}

/// One caller-owned set of query boundaries.
struct Query<'a, C, O> {
    /// Static scene rays.
    caster: &'a C,
    /// Complete capsule motion.
    oracle: &'a mut O,
    /// Counts shared with every retry.
    budget: &'a mut Budget,
    /// Existing surface limits.
    tuning: &'a Tuning,
    /// Actual stance dimensions.
    capsule: Capsule,
}

impl<C: Caster, O: MotionOracle> Query<'_, C, O> {
    /// Stand-off follows the actual capsule, including a wider one.
    fn radius(&self) -> f32 {
        crate::surface::standoff_target(0.0, self.capsule)
    }

    /// Ordinary effective Move reach, without increasing grip range for a scout.
    fn reach(&self) -> f32 {
        self.tuning.hold_reach + self.radius() - GAME_STANDOFF
    }

    /// Charge exactly one actual caster invocation, including misses and malformed answers.
    fn ray(&mut self, from: Vec3, to: Vec3, mask: u32) -> Result<Option<Hit>, Reject> {
        let delta = to - from;
        if !from.is_finite() || !to.is_finite() {
            return Err(Reject::Invalid);
        }
        delta.normalized().ok_or(Reject::Invalid)?;
        if self.budget.rays >= MAX_RAYS {
            return Err(Reject::Budget);
        }
        self.budget.rays = self.budget.rays.saturating_add(1);
        let Some(hit) = self.caster.cast(from, to, mask) else {
            return Ok(None);
        };
        Ok(ray_hit(from, to, hit))
    }

    /// A direct inside witness or finite visibility-checked outside scouts. No scout grants a hold.
    fn scout(&mut self, position: Vec3, first: Hit, sign: f32) -> Result<Hit, Reject> {
        let normal = first.normal.flat().normalized().ok_or(Reject::NoSupport)?;
        let tangent = UP.cross(normal) * sign;
        let gap = (position - first.point).dot(normal);
        if !(gap > EPS && gap <= self.reach()) {
            return Err(Reject::NoSupport);
        }
        let [top, low] = RAY_ROWS;
        let chest = position + UP * ((top + low) * 0.5);
        for direction in [tangent, -tangent] {
            if let Some(hit) =
                self.ray(chest, chest + direction * self.reach(), self.tuning.mask)?
            {
                if hit.entity == first.entity && hit.normal.dot(normal) < 0.95 {
                    return Ok(hit);
                }
            }
        }
        for direction in [sign, -sign] {
            let travel = UP.cross(normal) * direction;
            let mut hits = [None; 4];
            let samples = RAY_ROWS.into_iter().flat_map(|height| {
                [RAY_SIDE * 0.5, RAY_SIDE]
                    .into_iter()
                    .map(move |side| (height, side))
            });
            for (slot, (height, side)) in hits.iter_mut().zip(samples) {
                let origin = position + UP * height;
                let scout =
                    origin - normal * (gap + side) + travel * (RAY_SIDE + self.capsule.radius);
                if (scout - origin).length() > self.reach() + RAY_SIDE {
                    continue;
                }
                if self.ray(origin, scout, self.tuning.mask | SOLID)?.is_none() {
                    // The scout offset must not shorten the search interval around actual feet.
                    let return_reach = (scout - origin).dot(travel) + self.reach();
                    *slot = self.ray(scout, scout - travel * return_reach, self.tuning.mask)?;
                }
            }
            if let Some(fit) = fit(&hits, self.tuning)
                .filter(|fit| fit.entity == first.entity && fit.normal.dot(normal) < 0.95)
            {
                return Ok(Hit {
                    point: fit.point,
                    normal: fit.normal,
                    entity: fit.entity,
                });
            }
        }
        Err(Reject::NoSupport)
    }

    /// A fresh angular patch at the actual destination, on one physical face only.
    fn support(&mut self, feature: Feature, position: Vec3) -> Result<Fit, Reject> {
        let tuning = *self.tuning;
        let reach = self.reach();
        support_at(feature, position, &tuning, reach, |from, to, mask| {
            self.ray(from, to, mask)
        })
    }

    /// Physical support and complete motion are checked before returning any proposal.
    fn admit(&mut self, guide: Guide, position: Vec3) -> Result<Step, Reject> {
        let support = self.support(guide.feature, position)?;
        if self.budget.motions >= MAX_MOTIONS {
            return Err(Reject::Budget);
        }
        self.budget.motions = self.budget.motions.saturating_add(1);
        match self
            .oracle
            .check(guide.request.position, position, self.capsule)
        {
            Clearance::Blocked => return Err(Reject::Blocked),
            Clearance::Unknown => return Err(Reject::Unknown),
            Clearance::Clear => {}
        }
        let facing = facing_at(guide, position, self.radius())?;
        Ok(Step {
            position,
            velocity: (position - guide.request.position) * (1.0 / guide.request.dt),
            facing,
            support,
            route: guide.feature.route,
            guide,
        })
    }
}

/// Validate one real answer against its finite ray segment before using it as geometry.
fn ray_hit(from: Vec3, to: Vec3, mut hit: Hit) -> Option<Hit> {
    let delta = to - from;
    if !from.is_finite() || !to.is_finite() {
        return None;
    }
    let direction = delta.normalized()?;
    let normal = hit.normal.normalized()?;
    let offset = hit.point - from;
    let along = offset.dot(direction);
    if !hit.point.is_finite()
        || along < -EPS
        || along > delta.length() + EPS
        || (offset - direction * along).length() > CROSS
        || normal.dot(direction) >= -EPS
    {
        return None;
    }
    hit.normal = normal;
    Some(hit)
}

/// Obtain three-hit support with the caller's actual ray boundary, never a cached guide fit.
fn support_at(
    feature: Feature,
    position: Vec3,
    tuning: &Tuning,
    reach: f32,
    mut ray: impl FnMut(Vec3, Vec3, u32) -> Result<Option<Hit>, Reject>,
) -> Result<Fit, Reject> {
    let a_gap = (position - feature.first.point).dot(feature.first.normal);
    let b_gap = (position - feature.second.point).dot(feature.second.normal);
    let first = if feature.route == Route::Convex {
        a_gap >= b_gap
    } else {
        a_gap <= b_gap
    };
    let (face, along) = if first {
        (
            feature.first,
            -UP.cross(feature.first.normal) * feature.sign,
        )
    } else {
        (
            feature.second,
            UP.cross(feature.second.normal) * feature.sign,
        )
    };
    let edge = feature.edge(position.y).ok_or(Reject::NoSupport)?;
    let mut hits = [None; 4];
    let samples = RAY_ROWS.into_iter().flat_map(|height| {
        [RAY_SIDE * 0.5, RAY_SIDE]
            .into_iter()
            .map(move |side| (height, side))
    });
    for (slot, (height, side)) in hits.iter_mut().zip(samples) {
        let from = position + UP * height;
        let target = edge + UP * height + along * side;
        let delta = target - from;
        let Some(direction) = delta.normalized() else {
            continue;
        };
        if delta.length() + CROSS > reach {
            continue;
        }
        *slot = ray(from, target + direction * CROSS, tuning.mask)?
            .filter(|hit| hit.entity == face.entity && hit.normal.dot(face.normal) >= ALIGN);
    }
    fit(&hits, tuning).ok_or(Reject::NoSupport)
}

/// Eased facing for an exact feet position; this is never physical support permission.
fn facing_at(guide: Guide, position: Vec3, radius: f32) -> Result<Vec3, Reject> {
    let target = guide
        .feature
        .facing(position, radius)
        .ok_or(Reject::NoSupport)?;
    let old = guide.request.normal.normalized().ok_or(Reject::Invalid)?;
    let share = 1.0 - (-guide.request.dt / NORMAL_TIME).exp();
    (old + (target - old) * share)
        .normalized()
        .ok_or(Reject::Invalid)
}

/// Validate the finite, static vertical-wall movement domain before any scene calls.
pub(crate) fn valid(request: &Request, tuning: &Tuning, capsule: Capsule) -> Result<(), Reject> {
    let length = request.velocity.length();
    if !request.position.is_finite()
        || !request.velocity.is_finite()
        || !request.normal.is_finite()
        || !(request.dt > 0.0 && request.dt.is_finite())
        || !length.is_finite()
        || length * request.dt > RAY_SIDE
        || request.normal.y.abs() > VERTICAL
        || request.normal.flat().normalized().is_none()
        || !(capsule.radius > 0.0 && capsule.radius.is_finite())
        || !(capsule.height >= capsule.radius * 2.0 && capsule.height.is_finite())
        || !(tuning.hold_reach > 0.0 && tuning.hold_reach.is_finite())
        || !(tuning.plane_tolerance >= 0.0 && (tuning.plane_tolerance * 1.5).is_finite())
        || !tuning.hold_ny.is_finite()
        || !tuning.move_overhang_ny.is_finite()
        || tuning.mask == 0
    {
        return Err(Reject::Invalid);
    }
    let hint = request.normal.flat().normalized().ok_or(Reject::Invalid)?;
    (request.velocity.dot(UP.cross(hint)).abs() > EPS)
        .then_some(())
        .ok_or(Reject::Invalid)
}

/// Two plane witnesses inferred from fresh rays. They do not grant movement permission.
fn witnesses(hits: &[Option<Hit>]) -> Option<(Hit, Option<Hit>)> {
    let first = *hits.iter().flatten().next()?;
    let second = hits
        .iter()
        .flatten()
        .find(|hit| hit.normal.dot(first.normal) < ALIGN)
        .copied();
    let mean = |normal: Vec3| {
        let mut sum = Vec3::default();
        let mut count = 0u8;
        for hit in hits
            .iter()
            .flatten()
            .filter(|hit| hit.normal.dot(normal) >= ALIGN)
        {
            if hit.entity != first.entity {
                return None;
            }
            sum = sum + hit.point;
            count = count.saturating_add(1);
        }
        (count > 0).then(|| Hit {
            point: sum * (1.0 / f32::from(count)),
            normal,
            entity: first.entity,
        })
    };
    Some((mean(first.normal)?, second.and_then(|hit| mean(hit.normal))))
}

/// Fit one actual face, requiring three distinct noncollinear hits and unchanged Move limits.
fn fit(hits: &[Option<Hit>], tuning: &Tuning) -> Option<Fit> {
    let first = hits.iter().flatten().next()?;
    let mut normal = Vec3::default();
    let mut point = Vec3::default();
    let mut count = 0u8;
    for hit in hits.iter().flatten() {
        if hit.entity != first.entity || hit.normal.dot(first.normal) < ALIGN {
            return None;
        }
        normal = normal + hit.normal;
        point = point + hit.point;
        count = count.saturating_add(1);
    }
    if count < 3 {
        return None;
    }
    let normal = normal.normalized()?;
    if normal.y.abs() > VERTICAL || normal.y > tuning.hold_ny || normal.y < tuning.move_overhang_ny
    {
        return None;
    }
    let point = point * (1.0 / f32::from(count));
    if !point.is_finite() {
        return None;
    }
    let mut area = 0.0f32;
    let (mut low, mut high) = (f32::MAX, f32::MIN);
    for a in hits.iter().flatten() {
        let depth = (a.point - point).dot(normal);
        low = low.min(depth);
        high = high.max(depth);
        for b in hits.iter().flatten() {
            for c in hits.iter().flatten() {
                area = area.max((b.point - a.point).cross(c.point - a.point).length());
            }
        }
    }
    let spread = high - low;
    if !spread.is_finite() || spread > tuning.plane_tolerance * 1.5 || area <= EPS {
        return None;
    }
    Some(Fit {
        normal,
        point,
        hits: count,
        spread,
        entity: first.entity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::Hit;
    use crate::testworld::tuning;
    use std::cell::Cell;

    /// Static outside walls: x=0,z<=0 and z=0,x>=0. Solid occupies x>=0,z<=0.
    struct Convex {
        /// Actual calls, independently counted by the fixture.
        calls: Cell<u32>,
    }

    impl Caster for Convex {
        fn cast(&self, from: Vec3, to: Vec3, _mask: u32) -> Option<Hit> {
            self.calls.set(self.calls.get() + 1);
            let delta = to - from;
            let mut best: Option<(f32, Hit)> = None;
            for normal in [Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0)] {
                let gap = from.dot(normal);
                let closing = delta.dot(normal);
                if gap <= 1e-6 || closing >= -1e-6 {
                    continue;
                }
                let fraction = -gap / closing;
                let point = from + delta * fraction;
                let on_face = if normal.x < 0.0 {
                    point.z <= 1e-6
                } else {
                    point.x >= -1e-6
                };
                if (0.0..=1.0).contains(&fraction)
                    && on_face
                    && best.is_none_or(|(old, _)| fraction < old)
                {
                    best = Some((
                        fraction,
                        Hit {
                            point,
                            normal,
                            entity: 0,
                        },
                    ));
                }
            }
            best.map(|(_, hit)| hit)
        }
    }

    /// The explicit pure oracle for a fully clear analytic fixture.
    struct Clear;

    impl MotionOracle for Clear {
        fn check(&mut self, _from: Vec3, _to: Vec3, _capsule: Capsule) -> Clearance {
            Clearance::Clear
        }
    }

    /// A symmetric patch stops before this edge. Fresh asymmetric support lets a checked step
    /// approach it, without lowering the three-hit rule or inventing a wall.
    #[test]
    fn a_fresh_convex_edge_can_advance() {
        let world = Convex {
            calls: Cell::new(0),
        };
        let input = Request {
            position: Vec3::new(-0.55, 0.0, -0.35),
            velocity: Vec3::new(0.0, 0.0, 1.95),
            normal: Vec3::new(-1.0, 0.0, 0.0),
            dt: 1.0 / 60.0,
        };
        let mut budget = Budget::new();
        let step = propose(
            &world,
            &mut Clear,
            &mut budget,
            &input,
            &tuning(),
            Capsule::FALLBACK,
        )
        .expect("fresh checked support should allow the approach to the corner");
        assert!(step.position.z > input.position.z);
        assert!((step.position.x + 0.55).abs() < 1e-5);
        assert!(step.support.hits >= 3);
        assert_eq!(step.route, Route::Convex);
        assert_eq!(u32::from(budget.rays()), world.calls.get());
        assert!(budget.rays() <= MAX_RAYS);
        assert_eq!(budget.motions(), 1);
    }

    /// Static inside walls: x=0,z<=0 and z=0,x<=0. Free space has x<0,z<0.
    struct Concave {
        /// Actual casts, counted independently of the policy.
        calls: Cell<u32>,
    }

    impl Caster for Concave {
        fn cast(&self, from: Vec3, to: Vec3, _mask: u32) -> Option<Hit> {
            self.calls.set(self.calls.get() + 1);
            let delta = to - from;
            let mut best: Option<(f32, Hit)> = None;
            for normal in [Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, -1.0)] {
                let gap = from.dot(normal);
                let closing = delta.dot(normal);
                if gap <= 1e-6 || closing >= -1e-6 {
                    continue;
                }
                let fraction = -gap / closing;
                let point = from + delta * fraction;
                let on_face = if normal.x < 0.0 {
                    point.z <= 1e-6
                } else {
                    point.x <= 1e-6
                };
                if (0.0..=1.0).contains(&fraction)
                    && on_face
                    && best.is_none_or(|(old, _)| fraction < old)
                {
                    best = Some((
                        fraction,
                        Hit {
                            point,
                            normal,
                            entity: 0,
                        },
                    ));
                }
            }
            best.map(|(_, hit)| hit)
        }
    }

    /// Turn before entering the inside wall; a complete oracle still decides body clearance.
    #[test]
    fn a_fresh_concave_junction_can_turn() {
        let world = Concave {
            calls: Cell::new(0),
        };
        let input = Request {
            position: Vec3::new(-0.55, 0.0, -0.9),
            velocity: Vec3::new(0.0, 0.0, 1.95),
            normal: Vec3::new(-1.0, 0.0, 0.0),
            dt: 1.0 / 60.0,
        };
        let mut budget = Budget::new();
        let step = propose(
            &world,
            &mut Clear,
            &mut budget,
            &input,
            &tuning(),
            Capsule::FALLBACK,
        )
        .expect("fresh inside-face support should allow a checked turn");
        assert_eq!(step.route, Route::Concave);
        assert!(step.position.x < input.position.x);
        assert!(step.position.z > input.position.z);
        assert!(step.position.x <= -0.55 && step.position.z <= -0.55);
        assert!(step.support.hits >= 3);
        assert_eq!(u32::from(budget.rays()), world.calls.get());
        assert!(budget.rays() <= MAX_RAYS);
    }

    /// An earlier checked Q never authorizes a different velocity selected by the native caller.
    #[test]
    fn a_changed_final_velocity_is_revalidated_with_fresh_support() {
        let world = Convex {
            calls: Cell::new(0),
        };
        let input = Request {
            position: Vec3::new(-0.55, 0.0, -0.35),
            velocity: Vec3::new(0.0, 0.0, 1.95),
            normal: Vec3::new(-1.0, 0.0, 0.0),
            dt: 1.0 / 60.0,
        };
        let mut budget = Budget::new();
        let step = propose(
            &world,
            &mut Clear,
            &mut budget,
            &input,
            &tuning(),
            Capsule::FALLBACK,
        )
        .unwrap();
        let final_velocity = step.velocity * 0.5;
        let before = budget.rays();
        let checked = revalidate(
            &world,
            &mut Clear,
            &mut budget,
            step,
            final_velocity,
            &tuning(),
            Capsule::FALLBACK,
        )
        .expect("changed actual motion needs its own fresh support and oracle");
        let expected = input.position + final_velocity * input.dt;
        assert!((checked.position - expected).length() < 1e-6);
        assert!(budget.rays() >= before + 3);
        assert_eq!(u32::from(budget.rays()), world.calls.get());
        assert_eq!(budget.motions(), 2);
    }
    /// The real oracle calls and final segment are counted independently from Budget.
    struct Recorded {
        answer: Clearance,
        calls: u32,
        last: Option<(Vec3, Vec3, Capsule)>,
    }
    impl Recorded {
        fn new(answer: Clearance) -> Self {
            Self {
                answer,
                calls: 0,
                last: None,
            }
        }
    }
    impl MotionOracle for Recorded {
        fn check(&mut self, from: Vec3, to: Vec3, capsule: Capsule) -> Clearance {
            self.calls += 1;
            self.last = Some((from, to, capsule));
            self.answer
        }
    }
    /// Native state shared by approach and refusal fixtures.
    fn approach() -> Request {
        Request {
            position: Vec3::new(-0.55, 0.0, -0.35),
            velocity: Vec3::new(0.0, 0.0, 1.95),
            normal: Vec3::new(-1.0, 0.0, 0.0),
            dt: 1.0 / 60.0,
        }
    }
    /// Both kinds, both directions and simulation rates keep actual support and native speed.
    #[test]
    fn both_corner_routes_support_reverse_and_forward_motion() {
        let outside = Convex {
            calls: Cell::new(0),
        };
        let inside = Concave {
            calls: Cell::new(0),
        };
        for dt in [1.0 / 120.0, 1.0 / 60.0, 1.0 / 20.0] {
            for degrees in [15.0f32, 30.0, 45.0, 60.0, 75.0] {
                let angle = degrees.to_radians();
                for sign in [-1.0, 1.0] {
                    let normal = Vec3::new(-angle.cos(), 0.0, angle.sin());
                    let input = Request {
                        position: normal * 0.55,
                        velocity: UP.cross(normal) * (1.95 * sign),
                        normal,
                        dt,
                    };
                    let before = outside.calls.get();
                    let mut budget = Budget::new();
                    let step = propose(
                        &outside,
                        &mut Clear,
                        &mut budget,
                        &input,
                        &tuning(),
                        Capsule::FALLBACK,
                    )
                    .unwrap_or_else(|why| panic!("outside {degrees}/{sign}/{dt}: {why:?}"));
                    assert!((step.position.z.atan2(-step.position.x) - angle) * sign > 0.0);
                    assert!(step.support.hits >= 3 && step.route == Route::Convex);
                    assert!((step.position.flat().length() - 0.55).abs() < 1e-4);
                    assert!(step.velocity.length() <= input.velocity.length() + 1e-4);
                    assert_eq!(u32::from(budget.rays()), outside.calls.get() - before);
                    let radial = Vec3::new(angle.cos(), 0.0, angle.sin());
                    let normal = -radial;
                    let centre = Vec3::new(-0.9, 0.0, -0.9);
                    let input = Request {
                        position: centre + radial * 0.35,
                        velocity: UP.cross(normal) * (1.95 * sign),
                        normal,
                        dt,
                    };
                    let before = inside.calls.get();
                    let mut budget = Budget::new();
                    let step = propose(
                        &inside,
                        &mut Clear,
                        &mut budget,
                        &input,
                        &tuning(),
                        Capsule::FALLBACK,
                    )
                    .unwrap_or_else(|why| panic!("inside {degrees}/{sign}/{dt}: {why:?}"));
                    let offset = step.position - centre;
                    assert!((offset.z.atan2(offset.x) - angle) * sign > 0.0);
                    assert!(step.position.x <= -0.55 + 1e-4 && step.position.z <= -0.55 + 1e-4);
                    assert!(step.support.hits >= 3 && step.route == Route::Concave);
                    assert!(step.velocity.length() <= input.velocity.length() + 1e-4);
                    assert_eq!(u32::from(budget.rays()), inside.calls.get() - before);
                    assert!(budget.rays() <= MAX_RAYS);
                }
            }
        }
    }
    /// Slow/glancing input keeps its projected side magnitude and the native vertical component.
    #[test]
    fn analog_glancing_and_vertical_components_do_not_gain_speed() {
        let world = Convex {
            calls: Cell::new(0),
        };
        for velocity in [
            Vec3::new(0.0, 0.0, 0.03),
            Vec3::new(1.5, 0.0, 0.1),
            Vec3::new(0.0, 0.2, 0.5),
        ] {
            let input = Request {
                velocity,
                ..approach()
            };
            let step = propose(
                &world,
                &mut Clear,
                &mut Budget::new(),
                &input,
                &tuning(),
                Capsule::FALLBACK,
            )
            .unwrap();
            assert!((step.velocity.z - velocity.z).abs() < 1e-4);
            assert!((step.velocity.y - velocity.y).abs() < 1e-4);
            assert!(step.velocity.length() <= velocity.length() + 1e-4);
        }
    }
    /// Idle/invalid requests never query the scene; orientation is not surface permission.
    #[test]
    fn invalid_and_idle_requests_make_no_queries() {
        let world = Convex {
            calls: Cell::new(0),
        };
        let mut oracle = Recorded::new(Clearance::Clear);
        let mut budget = Budget::new();
        for input in [
            Request {
                velocity: Vec3::default(),
                ..approach()
            },
            Request {
                dt: 0.0,
                ..approach()
            },
            Request {
                dt: -0.1,
                ..approach()
            },
            Request {
                dt: f32::NAN,
                ..approach()
            },
            Request {
                dt: 1.0,
                ..approach()
            },
            Request {
                normal: UP,
                ..approach()
            },
            Request {
                normal: Vec3::default(),
                ..approach()
            },
            Request {
                position: Vec3::new(f32::NAN, 0.0, 0.0),
                ..approach()
            },
            Request {
                velocity: Vec3::new(f32::INFINITY, 0.0, 0.0),
                ..approach()
            },
        ] {
            assert_eq!(
                propose(
                    &world,
                    &mut oracle,
                    &mut budget,
                    &input,
                    &tuning(),
                    Capsule::FALLBACK
                ),
                Err(Reject::Invalid)
            );
        }
        assert_eq!(
            (
                world.calls.get(),
                oracle.calls,
                budget.rays(),
                budget.motions()
            ),
            (0, 0, 0, 0)
        );
    }
    /// Every explicit oracle failure remains a refusal despite good surface rays.
    #[test]
    fn blocked_and_unknown_motion_do_not_pass() {
        let world = Convex {
            calls: Cell::new(0),
        };
        for (answer, expected) in [
            (Clearance::Blocked, Reject::Blocked),
            (Clearance::Unknown, Reject::Unknown),
        ] {
            let mut oracle = Recorded::new(answer);
            let mut budget = Budget::new();
            assert_eq!(
                propose(
                    &world,
                    &mut oracle,
                    &mut budget,
                    &approach(),
                    &tuning(),
                    Capsule::FALLBACK
                ),
                Err(expected)
            );
            assert_eq!((oracle.calls, budget.motions()), (1, 1));
        }
    }
    /// A changing scene records actual calls even when its geometry is absent.
    struct Switch<'a, C> {
        world: &'a C,
        present: Cell<bool>,
        calls: Cell<u32>,
    }
    impl<C: Caster> Caster for Switch<'_, C> {
        fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit> {
            self.calls.set(self.calls.get() + 1);
            self.present
                .get()
                .then(|| self.world.cast(from, to, mask))
                .flatten()
        }
    }
    /// Old public fields, old support and an earlier oracle answer cannot grant revalidation.
    #[test]
    fn a_disappeared_face_refuses_revalidation() {
        let rock = Convex {
            calls: Cell::new(0),
        };
        let world = Switch {
            world: &rock,
            present: Cell::new(true),
            calls: Cell::new(0),
        };
        let mut oracle = Recorded::new(Clearance::Clear);
        let mut budget = Budget::new();
        let mut step = propose(
            &world,
            &mut oracle,
            &mut budget,
            &approach(),
            &tuning(),
            Capsule::FALLBACK,
        )
        .unwrap();
        let velocity = step.velocity * 0.5;
        world.present.set(false);
        step.position = Vec3::new(100.0, 0.0, 0.0);
        step.facing = UP;
        step.support.normal = UP;
        assert_eq!(
            revalidate(
                &world,
                &mut oracle,
                &mut budget,
                step,
                velocity,
                &tuning(),
                Capsule::FALLBACK
            ),
            Err(Reject::NoSupport)
        );
        assert_eq!(oracle.calls, 1);
        assert_eq!(u32::from(budget.rays()), world.calls.get());
    }
    /// Failed proposals and retries consume the same actual invocation budget.
    #[test]
    fn misses_and_retries_cannot_reset_the_budget() {
        let rock = Convex {
            calls: Cell::new(0),
        };
        let world = Switch {
            world: &rock,
            present: Cell::new(false),
            calls: Cell::new(0),
        };
        let mut budget = Budget::new();
        let mut oracle = Recorded::new(Clearance::Clear);
        for _ in 0..6 {
            assert_eq!(
                propose(
                    &world,
                    &mut oracle,
                    &mut budget,
                    &approach(),
                    &tuning(),
                    Capsule::FALLBACK
                ),
                Err(Reject::NoSupport)
            );
        }
        assert_eq!(
            propose(
                &world,
                &mut oracle,
                &mut budget,
                &approach(),
                &tuning(),
                Capsule::FALLBACK
            ),
            Err(Reject::Budget)
        );
        assert_eq!(world.calls.get(), u32::from(MAX_RAYS));
        assert_eq!(budget.rays(), MAX_RAYS);
        assert_eq!(oracle.calls, 0);
    }
    /// Wider native capsules keep their own measured stand-off and oracle dimensions.
    #[test]
    fn a_wider_capsule_uses_its_own_clearance() {
        let world = Convex {
            calls: Cell::new(0),
        };
        let capsule = Capsule {
            radius: 0.6,
            height: 2.0,
        };
        let radius = crate::surface::standoff_target(0.0, capsule);
        let input = Request {
            position: Vec3::new(-radius, 0.0, -0.35),
            ..approach()
        };
        let mut oracle = Recorded::new(Clearance::Clear);
        let step = propose(
            &world,
            &mut oracle,
            &mut Budget::new(),
            &input,
            &tuning(),
            capsule,
        )
        .unwrap();
        assert!((step.position.x + radius).abs() < 1e-4);
        assert_eq!(oracle.last.unwrap().2, capsule);
    }
    /// Restoring request/scene state requires no hidden plugin corner cache.
    #[test]
    fn replay_is_independent_of_prior_attempts() {
        let world = Convex {
            calls: Cell::new(0),
        };
        let first = propose(
            &world,
            &mut Clear,
            &mut Budget::new(),
            &approach(),
            &tuning(),
            Capsule::FALLBACK,
        )
        .unwrap();
        let other = Request {
            normal: Vec3::new(0.0, 0.0, 1.0),
            position: Vec3::new(0.35, 0.0, 0.55),
            velocity: Vec3::new(-0.8, 0.0, 0.0),
            ..approach()
        };
        let _ = propose(
            &world,
            &mut Clear,
            &mut Budget::new(),
            &other,
            &tuning(),
            Capsule::FALLBACK,
        );
        let restored = propose(
            &world,
            &mut Clear,
            &mut Budget::new(),
            &approach(),
            &tuning(),
            Capsule::FALLBACK,
        )
        .unwrap();
        assert_eq!(first, restored);
    }

    /// A hole in the old face leaves the distant scout visible but removes actual destination hits.
    struct Holes {
        /// Complete outside walls.
        world: Convex,
        /// One or both lower corners of the destination patch are absent.
        count: u8,
    }
    impl Caster for Holes {
        fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit> {
            self.world.cast(from, to, mask).filter(|hit| {
                let in_patch = hit.normal.x < -0.9
                    && (hit.point.y - 1.25).abs() < 0.01
                    && (-0.36..-0.16).contains(&hit.point.z);
                !(in_patch && (self.count >= 2 || hit.point.z > -0.2))
            })
        }
    }
    /// A real three-hit triangle passes; two hits never borrow permission from the scout.
    #[test]
    fn actual_missing_corner_hits_keep_the_three_hit_rule() {
        for missing in [1, 2] {
            let world = Holes {
                world: Convex {
                    calls: Cell::new(0),
                },
                count: missing,
            };
            let mut oracle = Recorded::new(Clearance::Clear);
            let mut budget = Budget::new();
            let result = propose(
                &world,
                &mut oracle,
                &mut budget,
                &approach(),
                &tuning(),
                Capsule::FALLBACK,
            );
            if missing == 1 {
                assert_eq!(result.unwrap().support.hits, 3);
                assert_eq!(oracle.calls, 1);
            } else {
                assert_eq!(result, Err(Reject::NoSupport));
                assert_eq!(oracle.calls, 0);
            }
            assert_eq!(u32::from(budget.rays()), world.world.calls.get());
        }
    }
    /// A thin solid column with an independently computed continuous capsule distance test.
    struct Post {
        /// The column's horizontal centre.
        point: Vec3,
        /// Actual complete oracle calls.
        calls: u32,
    }
    impl MotionOracle for Post {
        fn check(&mut self, from: Vec3, to: Vec3, capsule: Capsule) -> Clearance {
            self.calls += 1;
            let segment = (to - from).flat();
            let along = ((self.point - from).dot(segment) / segment.dot(segment)).clamp(0.0, 1.0);
            let distance = (self.point - (from + segment * along)).flat().length();
            if distance < capsule.radius + 0.005 {
                Clearance::Blocked
            } else {
                Clearance::Clear
            }
        }
    }
    /// Endpoint capsules can both fit while a small obstacle blocks the middle of the actual tick.
    #[test]
    fn an_obstacle_inside_the_actual_segment_refuses_motion() {
        let world = Convex {
            calls: Cell::new(0),
        };
        let input = Request {
            position: Vec3::new(-0.55, 0.0, 0.0),
            dt: 0.15,
            ..approach()
        };
        let proposed = propose(
            &world,
            &mut Clear,
            &mut Budget::new(),
            &input,
            &tuning(),
            Capsule::FALLBACK,
        )
        .unwrap();
        let middle = (input.position + proposed.position) * 0.5;
        let outward = middle.flat().normalized().unwrap();
        let point = middle + outward * 0.399;
        assert!((point - input.position).flat().length() > 0.405);
        assert!((point - proposed.position).flat().length() > 0.405);
        let mut oracle = Post { point, calls: 0 };
        assert_eq!(
            propose(
                &world,
                &mut oracle,
                &mut Budget::new(),
                &input,
                &tuning(),
                Capsule::FALLBACK
            ),
            Err(Reject::Blocked)
        );
        assert_eq!(oracle.calls, 1);
    }
    /// Revalidation checks the exact replacement endpoints and cannot reset the shared limits.
    #[test]
    fn replacement_motion_checks_its_own_segment_and_shared_limits() {
        let world = Convex {
            calls: Cell::new(0),
        };
        let input = approach();
        let mut oracle = Recorded::new(Clearance::Clear);
        let mut budget = Budget::new();
        let step = propose(
            &world,
            &mut oracle,
            &mut budget,
            &input,
            &tuning(),
            Capsule::FALLBACK,
        )
        .unwrap();
        let final_velocity = step.velocity * 0.5;
        let checked = revalidate(
            &world,
            &mut oracle,
            &mut budget,
            step,
            final_velocity,
            &tuning(),
            Capsule::FALLBACK,
        )
        .unwrap();
        let (from, to, _) = oracle.last.unwrap();
        assert_eq!(from, input.position);
        assert!((to - (input.position + final_velocity * input.dt)).length() < 1e-6);
        assert_eq!(oracle.calls, 2);
        let final_velocity = checked.velocity;
        assert_eq!(
            revalidate(
                &world,
                &mut oracle,
                &mut budget,
                checked,
                final_velocity,
                &tuning(),
                Capsule::FALLBACK
            ),
            Err(Reject::Budget)
        );
        assert_eq!(oracle.calls, 2);
        assert_eq!(u32::from(budget.rays()), world.calls.get());
        assert!(budget.rays() <= MAX_RAYS);
    }

    /// Exact horizontal capsule clearance in the two complete, vertically extruded test worlds.
    struct WallMotion {
        route: Route,
    }
    impl MotionOracle for WallMotion {
        fn check(&mut self, from: Vec3, to: Vec3, capsule: Capsule) -> Clearance {
            let clear = if self.route == Route::Concave {
                from.x.max(to.x) <= -capsule.radius + 1e-5
                    && from.z.max(to.z) <= -capsule.radius + 1e-5
            } else {
                let delta = (to - from).flat();
                let mut candidates = vec![0.0, 1.0];
                if delta.x.abs() > 1e-6 {
                    candidates.push((-from.x / delta.x).clamp(0.0, 1.0));
                }
                if delta.z.abs() > 1e-6 {
                    candidates.push((-from.z / delta.z).clamp(0.0, 1.0));
                }
                if delta.dot(delta) > 1e-12 {
                    candidates.push((-from.flat().dot(delta) / delta.dot(delta)).clamp(0.0, 1.0));
                }
                candidates.into_iter().all(|fraction| {
                    let p = from + delta * fraction;
                    let distance = if p.x >= 0.0 && p.z <= 0.0 {
                        0.0
                    } else if p.x < 0.0 && p.z > 0.0 {
                        p.flat().length()
                    } else if p.x < 0.0 {
                        -p.x
                    } else {
                        p.z
                    };
                    distance >= capsule.radius - 1e-5
                })
            };
            if clear {
                Clearance::Clear
            } else {
                Clearance::Blocked
            }
        }
    }

    /// Fresh planning must finish a complete turn with lagged facing, then hand over to a real
    /// ordinary new-face probe. No prior Step/guide is supplied to the next tick.
    #[test]
    fn closed_loop_finishes_both_turns_with_eased_facing_and_real_capsules() {
        use crate::surface::{scan, Mode};
        let capsule = Capsule {
            radius: 0.5,
            height: 2.0,
        };
        for dt in [1.0 / 120.0, 1.0 / 60.0, 1.0 / 20.0] {
            for route in [Route::Convex, Route::Concave] {
                for sign in [-1.0, 1.0] {
                    let outside = Convex {
                        calls: Cell::new(0),
                    };
                    let inside = Concave {
                        calls: Cell::new(0),
                    };
                    let (mut position, mut facing, new_normal) = match (route, sign > 0.0) {
                        (Route::Convex, true) => (
                            Vec3::new(-0.55, 0.0, -0.35),
                            Vec3::new(-1.0, 0.0, 0.0),
                            Vec3::new(0.0, 0.0, 1.0),
                        ),
                        (Route::Convex, false) => (
                            Vec3::new(0.35, 0.0, 0.55),
                            Vec3::new(0.0, 0.0, 1.0),
                            Vec3::new(-1.0, 0.0, 0.0),
                        ),
                        (Route::Concave, true) => (
                            Vec3::new(-0.55, 0.0, -0.99),
                            Vec3::new(-1.0, 0.0, 0.0),
                            Vec3::new(0.0, 0.0, -1.0),
                        ),
                        (Route::Concave, false) => (
                            Vec3::new(-0.99, 0.0, -0.55),
                            Vec3::new(0.0, 0.0, -1.0),
                            Vec3::new(-1.0, 0.0, 0.0),
                        ),
                    };
                    let mut handoff = false;
                    for tick in 0..500 {
                        let input = Request {
                            position,
                            velocity: UP.cross(facing) * (1.95 * sign),
                            normal: facing,
                            dt,
                        };
                        let mut budget = Budget::new();
                        let mut oracle = WallMotion { route };
                        let result = if route == Route::Convex {
                            let before = outside.calls.get();
                            let step = propose(
                                &outside,
                                &mut oracle,
                                &mut budget,
                                &input,
                                &tuning(),
                                capsule,
                            );
                            assert_eq!(u32::from(budget.rays()), outside.calls.get() - before);
                            step
                        } else {
                            let before = inside.calls.get();
                            let step = propose(
                                &inside,
                                &mut oracle,
                                &mut budget,
                                &input,
                                &tuning(),
                                capsule,
                            );
                            assert_eq!(u32::from(budget.rays()), inside.calls.get() - before);
                            step
                        };
                        assert!(budget.rays() <= MAX_RAYS && budget.motions() <= MAX_MOTIONS);
                        let accepted = result.is_ok();
                        let failure = result.as_ref().err().copied();
                        if let Ok(step) = result {
                            assert!(step.position.is_finite() && step.facing.is_finite());
                            assert!(
                                (step.position - position).length() > 1e-6,
                                "stalled {route:?}/{sign}/{dt} tick{tick}: {step:?}"
                            );
                            position = step.position;
                            facing = step.facing;
                        } else if failure != Some(Reject::NoSupport) {
                            panic!("{route:?}/{sign}/{dt} tick{tick}: {failure:?} P={position:?} N={facing:?}");
                        }
                        // This is an independent baseline-handoff check, outside the measured
                        // policy invocation. It supplies no cached geometry to the next tick.
                        let candidate = position + UP.cross(facing) * (1.95 * sign * dt);
                        let ordinary = if route == Route::Convex {
                            scan(&outside, candidate, -facing, Mode::Move, &tuning(), capsule)
                        } else {
                            scan(&inside, candidate, -facing, Mode::Move, &tuning(), capsule)
                        };
                        if ordinary.verdict.is_ok()
                            && ordinary
                                .fit
                                .is_some_and(|fit| fit.normal.dot(new_normal) > 0.995)
                            && oracle.check(position, candidate, capsule) == Clearance::Clear
                        {
                            handoff = true;
                            break;
                        }
                        assert!(accepted, "lost transfer {route:?}/{sign}/{dt} tick{tick} P={position:?} N={facing:?}");
                    }
                    assert!(
                        handoff,
                        "no ordinary handoff: {route:?}/{sign}/{dt} P={position:?} N={facing:?}"
                    );
                }
            }
        }
    }

    /// The same actual-call boundary charges ordinary scans and later retries.
    #[test]
    fn native_corner_budget_is_shared_by_ordinary_scans() {
        let world = Convex {
            calls: Cell::new(0),
        };
        let budget = NativeBudget::new();
        let counted = BudgetCaster::new(&world, &budget);
        for _ in 0..MAX_NATIVE_RAYS / 4 {
            assert!(counted
                .scan(
                    Vec3::new(-0.55, 0.0, -1.0),
                    Vec3::new(1.0, 0.0, 0.0),
                    crate::surface::Mode::Move,
                    &tuning(),
                    Capsule::FALLBACK
                )
                .unwrap()
                .verdict
                .is_ok());
        }
        assert_eq!(budget.rays(), MAX_NATIVE_RAYS);
        assert_eq!(world.calls.get(), u32::from(MAX_NATIVE_RAYS));
        assert_eq!(
            counted.scan(
                Vec3::new(-0.55, 0.0, -1.0),
                Vec3::new(1.0, 0.0, 0.0),
                crate::surface::Mode::Move,
                &tuning(),
                Capsule::FALLBACK
            ),
            Err(Reject::Budget)
        );
        assert!(budget.denied());
        assert_eq!(world.calls.get(), u32::from(MAX_NATIVE_RAYS));
    }

    /// Three real hits do not authorize a patch when the fourth query was denied.
    #[test]
    fn native_corner_denied_fourth_ray_refuses_whole_scan() {
        let world = Convex {
            calls: Cell::new(0),
        };
        let budget = NativeBudget::new();
        let counted = BudgetCaster::new(&world, &budget);
        for _ in 0..MAX_NATIVE_RAYS - 3 {
            let _ = counted.cast(
                Vec3::new(-0.55, 1.0, -1.0),
                Vec3::new(0.45, 1.0, -1.0),
                tuning().mask,
            );
        }
        assert_eq!(
            counted.scan(
                Vec3::new(-0.55, 0.0, -1.0),
                Vec3::new(1.0, 0.0, 0.0),
                crate::surface::Mode::Move,
                &tuning(),
                Capsule::FALLBACK
            ),
            Err(Reject::Budget)
        );
        assert!(budget.denied());
        assert_eq!(budget.rays(), MAX_NATIVE_RAYS);
        assert_eq!(world.calls.get(), u32::from(MAX_NATIVE_RAYS));
    }

    /// A same-update seed can request finite velocity without invoking a motion oracle.
    #[test]
    fn native_corner_convex_seeds_request_only_bounded_velocity() {
        let world = Convex {
            calls: Cell::new(0),
        };
        let input = approach();
        let scan = crate::surface::scan(
            &world,
            input.position,
            -input.normal,
            crate::surface::Mode::Stay,
            &tuning(),
            Capsule::FALLBACK,
        );
        let before = world.calls.get();
        let budget = NativeBudget::new();
        let counted = BudgetCaster::new(&world, &budget);
        let plan = prepare_from_patch(
            &counted,
            &input,
            FreshPatch {
                position: input.position,
                hits: scan.samples,
            },
            true,
            &tuning(),
            Capsule::FALLBACK,
        )
        .unwrap();
        let copy = plan;
        assert_eq!(copy, plan);
        assert_eq!(plan.route, Route::Convex);
        assert!(plan.position.z > input.position.z);
        assert!(plan.position.is_finite() && plan.velocity.is_finite() && plan.facing.is_finite());
        assert!(plan.velocity.length() <= input.velocity.length() + 1e-4);
        assert_eq!(u32::from(budget.rays()), world.calls.get() - before);
        assert!(budget.rays() <= 5);
    }

    /// An inside junction must be discovered before a four-hit ordinary patch loses coverage.
    #[test]
    fn native_corner_concave_direct_witness_works_with_four_stay_hits() {
        let world = Concave {
            calls: Cell::new(0),
        };
        let input = Request {
            position: Vec3::new(-0.55, 0.0, -0.9),
            ..approach()
        };
        let stay = crate::surface::scan(
            &world,
            input.position,
            -input.normal,
            crate::surface::Mode::Stay,
            &tuning(),
            Capsule::FALLBACK,
        );
        assert_eq!(stay.hits, 4);
        assert!(stay.verdict.is_ok());
        let budget = NativeBudget::new();
        let counted = BudgetCaster::new(&world, &budget);
        let plan = prepare_from_patch(
            &counted,
            &input,
            FreshPatch {
                position: input.position,
                hits: stay.samples,
            },
            false,
            &tuning(),
            Capsule::FALLBACK,
        )
        .unwrap();
        assert_eq!(plan.route, Route::Concave);
        assert!(plan.position.x < input.position.x && plan.position.z > input.position.z);
        assert_eq!(budget.rays(), 1);
    }

    /// A native vertical fallback gets fresh real support and ignores public proposal mutations.
    #[test]
    fn native_corner_actual_fallback_is_fresh_and_private_envelope_is_authority() {
        let world = Convex {
            calls: Cell::new(0),
        };
        let input = Request {
            velocity: Vec3::new(0.0, 0.3, 1.0),
            ..approach()
        };
        let strict = propose(
            &world,
            &mut Clear,
            &mut Budget::new(),
            &input,
            &tuning(),
            Capsule::FALLBACK,
        )
        .unwrap();
        let plan = VelocityPlan {
            position: Vec3::new(f32::NAN, 0.0, 0.0),
            velocity: Vec3::new(99.0, 0.0, 0.0),
            facing: Vec3::default(),
            route: Route::Concave,
            guide: strict.guide,
        };
        let budget = NativeBudget::new();
        let counted = BudgetCaster::new(&world, &budget);
        let q = input.position + UP * (input.velocity.y * input.dt);
        let answer = fresh_support(&counted, &plan, q, &tuning(), Capsule::FALLBACK).unwrap();
        assert!(answer.fit.hits >= 3 && answer.facing.is_finite());
        assert_eq!(budget.rays(), 4);
        let used = budget.rays();
        assert_eq!(
            fresh_support(
                &counted,
                &plan,
                input.position + UP,
                &tuning(),
                Capsule::FALLBACK
            ),
            Err(Reject::Invalid)
        );
        assert_eq!(budget.rays(), used);
    }

    /// A previous strict step is not support when geometry disappears or only two rays survive.
    #[test]
    fn native_corner_missing_actual_geometry_never_uses_old_fit() {
        struct Limited<'a> {
            world: &'a Convex,
            left: Cell<u8>,
        }
        impl Caster for Limited<'_> {
            fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit> {
                let left = self.left.get();
                self.left.set(left.saturating_sub(1));
                (left > 0)
                    .then(|| self.world.cast(from, to, mask))
                    .flatten()
            }
        }
        let world = Convex {
            calls: Cell::new(0),
        };
        let input = approach();
        let strict = propose(
            &world,
            &mut Clear,
            &mut Budget::new(),
            &input,
            &tuning(),
            Capsule::FALLBACK,
        )
        .unwrap();
        let plan = VelocityPlan {
            position: strict.position,
            velocity: strict.velocity,
            facing: strict.facing,
            route: strict.route,
            guide: strict.guide,
        };
        for left in [0, 2] {
            let missing = Limited {
                world: &world,
                left: Cell::new(left),
            };
            let budget = NativeBudget::new();
            let counted = BudgetCaster::new(&missing, &budget);
            assert_eq!(
                fresh_support(&counted, &plan, plan.position, &tuning(), Capsule::FALLBACK),
                Err(Reject::NoSupport)
            );
            assert_eq!(budget.rays(), 4);
        }
    }

    /// Matching feet and initialized finite witnesses are required before discovery.
    #[test]
    fn native_corner_stale_or_malformed_seeds_refuse_without_queries() {
        let world = Convex {
            calls: Cell::new(0),
        };
        let input = approach();
        let budget = NativeBudget::new();
        let counted = BudgetCaster::new(&world, &budget);
        assert_eq!(
            prepare_from_patch(
                &counted,
                &input,
                FreshPatch {
                    position: input.position + UP,
                    hits: [None; 4]
                },
                true,
                &tuning(),
                Capsule::FALLBACK
            ),
            Err(Reject::Invalid)
        );
        let broken = Some(Hit {
            point: Vec3::new(f32::NAN, 0.0, 0.0),
            normal: -input.normal,
            entity: 0,
        });
        assert_eq!(
            prepare_from_patch(
                &counted,
                &input,
                FreshPatch {
                    position: input.position,
                    hits: [broken; 4]
                },
                true,
                &tuning(),
                Capsule::FALLBACK
            ),
            Err(Reject::NoSupport)
        );
        assert_eq!(budget.rays(), 0);
    }

    /// Full turns exercise fresh Stay witnesses, independent native motion and real actual-Q rays.
    #[test]
    fn native_corner_full_turns_keep_fresh_stay_and_actual_support() {
        use crate::surface::{scan, Mode};
        let capsule = Capsule {
            radius: 0.5,
            height: 2.0,
        };
        for dt in [1.0 / 120.0, 1.0 / 60.0, 1.0 / 20.0] {
            for route in [Route::Convex, Route::Concave] {
                for sign in [-1.0, 1.0] {
                    let outside = Convex {
                        calls: Cell::new(0),
                    };
                    let inside = Concave {
                        calls: Cell::new(0),
                    };
                    let (mut position, mut facing, new_normal) = match (route, sign > 0.0) {
                        (Route::Convex, true) => (
                            Vec3::new(-0.55, 0.0, -0.35),
                            Vec3::new(-1.0, 0.0, 0.0),
                            Vec3::new(0.0, 0.0, 1.0),
                        ),
                        (Route::Convex, false) => (
                            Vec3::new(0.35, 0.0, 0.55),
                            Vec3::new(0.0, 0.0, 1.0),
                            Vec3::new(-1.0, 0.0, 0.0),
                        ),
                        (Route::Concave, true) => (
                            Vec3::new(-0.55, 0.0, -0.99),
                            Vec3::new(-1.0, 0.0, 0.0),
                            Vec3::new(0.0, 0.0, -1.0),
                        ),
                        (Route::Concave, false) => (
                            Vec3::new(-0.99, 0.0, -0.55),
                            Vec3::new(0.0, 0.0, -1.0),
                            Vec3::new(-1.0, 0.0, 0.0),
                        ),
                    };
                    let mut handoff = false;
                    for tick in 0..500 {
                        let input = Request {
                            position,
                            velocity: UP.cross(facing) * (1.95 * sign),
                            normal: facing,
                            dt,
                        };
                        let budget = NativeBudget::new();
                        let mut oracle = WallMotion { route };
                        let result = if route == Route::Convex {
                            let before = outside.calls.get();
                            let counted = BudgetCaster::new(&outside, &budget);
                            let stay = counted
                                .scan(position, -facing, Mode::Stay, &tuning(), capsule)
                                .unwrap();
                            assert!(stay.verdict.is_ok(), "lost Stay {route:?}/{sign}/{dt} tick{tick} P={position:?} N={facing:?}: {stay:?}");
                            let plan = prepare_from_patch(
                                &counted,
                                &input,
                                FreshPatch {
                                    position,
                                    hits: stay.samples,
                                },
                                true,
                                &tuning(),
                                capsule,
                            );
                            let step = plan.and_then(|plan| {
                                let supported = fresh_support(&counted, &plan, plan.position, &tuning(), capsule)?;
                                assert_eq!(oracle.check(position, plan.position, capsule), Clearance::Clear,
                                    "requested native motion must fit this independent analytic capsule");
                                Ok(Step { position: plan.position, velocity: plan.velocity,
                                    facing: supported.facing, support: supported.fit, route: plan.route, guide: plan.guide })
                            });
                            assert_eq!(u32::from(budget.rays()), outside.calls.get() - before);
                            step
                        } else {
                            let before = inside.calls.get();
                            let counted = BudgetCaster::new(&inside, &budget);
                            let stay = counted
                                .scan(position, -facing, Mode::Stay, &tuning(), capsule)
                                .unwrap();
                            assert!(stay.verdict.is_ok(), "lost Stay {route:?}/{sign}/{dt} tick{tick} P={position:?} N={facing:?}: {stay:?}");
                            let plan = prepare_from_patch(
                                &counted,
                                &input,
                                FreshPatch {
                                    position,
                                    hits: stay.samples,
                                },
                                true,
                                &tuning(),
                                capsule,
                            );
                            let step = plan.and_then(|plan| {
                                let supported = fresh_support(&counted, &plan, plan.position, &tuning(), capsule)?;
                                assert_eq!(oracle.check(position, plan.position, capsule), Clearance::Clear,
                                    "requested native motion must fit this independent analytic capsule");
                                Ok(Step { position: plan.position, velocity: plan.velocity,
                                    facing: supported.facing, support: supported.fit, route: plan.route, guide: plan.guide })
                            });
                            assert_eq!(u32::from(budget.rays()), inside.calls.get() - before);
                            step
                        };
                        assert!(budget.rays() <= MAX_NATIVE_RAYS);
                        let accepted = result.is_ok();
                        let failure = result.as_ref().err().copied();
                        if let Ok(step) = result {
                            assert!(step.position.is_finite() && step.facing.is_finite());
                            assert!(
                                (step.position - position).length() > 1e-6,
                                "stalled {route:?}/{sign}/{dt} tick{tick}: {step:?}"
                            );
                            position = step.position;
                            facing = step.facing;
                        } else if failure != Some(Reject::NoSupport) {
                            panic!("{route:?}/{sign}/{dt} tick{tick}: {failure:?} P={position:?} N={facing:?}");
                        }
                        // This is an independent baseline-handoff check, outside the measured
                        // policy invocation. It supplies no cached geometry to the next tick.
                        let candidate = position + UP.cross(facing) * (1.95 * sign * dt);
                        let ordinary = if route == Route::Convex {
                            scan(&outside, candidate, -facing, Mode::Move, &tuning(), capsule)
                        } else {
                            scan(&inside, candidate, -facing, Mode::Move, &tuning(), capsule)
                        };
                        if ordinary.verdict.is_ok()
                            && ordinary
                                .fit
                                .is_some_and(|fit| fit.normal.dot(new_normal) > 0.995)
                            && oracle.check(position, candidate, capsule) == Clearance::Clear
                        {
                            handoff = true;
                            break;
                        }
                        assert!(accepted, "lost transfer {route:?}/{sign}/{dt} tick{tick} P={position:?} N={facing:?}");
                    }
                    assert!(
                        handoff,
                        "no ordinary handoff: {route:?}/{sign}/{dt} P={position:?} N={facing:?}"
                    );
                }
            }
        }
    }

    /// Per-move state is owned data and all nesting/unwind paths restore the exact outer scope.
    #[test]
    fn native_corner_call_scopes_restore_after_nesting_and_unwind() {
        use crate::corner_call as call;
        assert!(!call::inside_move());
        call::with_move(None, || {
            assert!(call::inside_move() && !call::blocked());
            call::with_move(None, || assert!(call::inside_move() && call::blocked()));
            assert!(!call::blocked());
            let failure =
                std::panic::catch_unwind(|| call::with_move(None, || panic!("nested scope")));
            assert!(failure.is_err());
            assert!(call::inside_move() && !call::blocked());
        });
        assert!(!call::inside_move() && !call::blocked());
    }

    /// Only fresh actual support followed by native success can commit a facing.
    #[test]
    fn native_corner_call_commits_only_matching_actual_q_after_native_success() {
        use crate::corner_call::{self as call, Answer, Call, Key};
        let world = Convex {
            calls: Cell::new(0),
        };
        let input = approach();
        let stay = crate::surface::scan(
            &world,
            input.position,
            -input.normal,
            crate::surface::Mode::Stay,
            &tuning(),
            Capsule::FALLBACK,
        );
        let key = Key {
            context: 1,
            entity: 2,
            world: 3,
        };
        let owned = Call::new(
            key,
            input,
            FreshPatch {
                position: input.position,
                hits: stay.samples,
            },
            stay.fit.filter(|_| stay.steady),
            tuning(),
            Capsule::FALLBACK,
        )
        .unwrap();
        call::with_move(Some(owned), || {
            let velocity = call::prepare(&world, -input.normal, input.velocity).unwrap();
            let q = input.position + velocity * input.dt;
            assert!(matches!(call::answer(&world, key, q), Answer::Supported(_)));
            assert_eq!(
                call::finish(key, input.position, velocity, 0x1234_5678_0000_0000),
                None
            );
            assert!(matches!(call::answer(&world, key, q), Answer::Supported(_)));
            assert!(call::finish(key, input.position, velocity, 0x1234_5678_0000_0001).is_some());
            assert!(matches!(call::answer(&world, key, q), Answer::Supported(_)));
            assert_eq!(call::finish(key, input.position, Vec3::default(), 1), None);
            assert!(matches!(call::answer(&world, key, q), Answer::Supported(_)));
            call::native_success();
            assert_eq!(call::finish(key, input.position, velocity, 1), None);
            assert!(matches!(
                call::answer(&world, Key { world: 4, ..key }, q),
                Answer::Refused
            ));
            assert_eq!(call::finish(key, input.position, velocity, 1), None);
        });
    }

    /// A panic in a scene query restores its flag but cannot refund the actual call already spent.
    #[test]
    fn native_corner_query_unwind_preserves_spent_budget_and_clears_reentry_flag() {
        use crate::corner_call::{self as call, Call, Key};
        struct PanicRay;
        impl Caster for PanicRay {
            fn cast(&self, _a: Vec3, _b: Vec3, _mask: u32) -> Option<Hit> {
                panic!("scene fixture");
            }
        }
        let world = Convex {
            calls: Cell::new(0),
        };
        let input = Request {
            position: Vec3::new(-0.55, 0.0, -1.0),
            ..approach()
        };
        let stay = crate::surface::scan(
            &world,
            input.position,
            -input.normal,
            crate::surface::Mode::Stay,
            &tuning(),
            Capsule::FALLBACK,
        );
        let key = Key {
            context: 1,
            entity: 2,
            world: 3,
        };
        let owner = Call::new(
            key,
            input,
            FreshPatch {
                position: input.position,
                hits: stay.samples,
            },
            stay.fit,
            tuning(),
            Capsule::FALLBACK,
        )
        .unwrap();
        call::with_move(Some(owner), || {
            let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                call::ordinary(&PanicRay, key, |counted| {
                    crate::surface::scan(
                        counted,
                        input.position,
                        -input.normal,
                        crate::surface::Mode::Move,
                        &tuning(),
                        Capsule::FALLBACK,
                    )
                });
            }));
            assert!(failure.is_err());
            assert!(!call::querying() && call::has_call());
            let before = world.calls.get();
            let mut refused = false;
            for _ in 0..12 {
                let scan = call::ordinary(&world, key, |counted| {
                    crate::surface::scan(
                        counted,
                        input.position,
                        -input.normal,
                        crate::surface::Mode::Move,
                        &tuning(),
                        Capsule::FALLBACK,
                    )
                });
                refused |= scan.is_none();
            }
            assert!(refused);
            assert_eq!(
                world.calls.get() - before + 1,
                u32::from(MAX_NATIVE_RAYS),
                "the panicking ray spent one of the shared actual calls"
            );
            assert!(!call::querying());
        });
        assert!(!call::inside_move());
    }
}
