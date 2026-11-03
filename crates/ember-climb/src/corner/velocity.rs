//! Velocity-only corner guidance. Native movement/collision admission remains external.
use super::{
    facing_at, ray_hit, support_at, valid, witnesses, Feature, Guide, Reject, Request, Route,
    ALIGN, CROSS, EPS, SOLID,
};
use crate::geom::{Vec3, UP};
use crate::surface::{
    Capsule, Caster, Fit, Hit, Mode, Scan, Tuning, GAME_STANDOFF, RAY_ROWS, RAY_SIDE,
};
use std::cell::Cell;

/// Actual ray calls shared by one lexical native move, including ordinary fallback scans.
pub const MAX_NATIVE_RAYS: u8 = 36;

/// Owned query usage copied between synchronous callbacks of the same native move.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Usage {
    /// Actual calls already spent by that lexical owner.
    rays: u8,
    /// Sticky whole-candidate refusal.
    denied: bool,
}

/// One native-call query budget, with a sticky denied flag for incomplete patches.
#[derive(Debug, Default)]
pub struct NativeBudget {
    /// Number of actual underlying calls, including misses.
    rays: Cell<u8>,
    /// A request was denied; no later partial scan can grant support.
    denied: Cell<bool>,
}
impl NativeBudget {
    /// A fresh outer-call budget. Reuse it for all inner attempts.
    pub fn new() -> Self {
        Self::default()
    }
    /// Resume this same lexical owner's counts after a synchronous native callback returns.
    pub(crate) fn resume(usage: Usage) -> Self {
        Self {
            rays: Cell::new(usage.rays),
            denied: Cell::new(usage.denied),
        }
    }
    /// Copy owned counts back to the same lexical native owner, including on Rust unwind.
    pub(crate) fn usage(&self) -> Usage {
        Usage {
            rays: self.rays(),
            denied: self.denied(),
        }
    }
    /// Actual calls so far.
    pub fn rays(&self) -> u8 {
        self.rays.get()
    }
    /// Whether an attempted query was denied.
    pub fn denied(&self) -> bool {
        self.denied.get()
    }
}

/// Counted scene boundary borrowed from one outer native movement call.
pub struct BudgetCaster<'a, C> {
    /// Scene caster; never nest counted wrappers.
    caster: &'a C,
    /// Same budget for ordinary scans, discovery and actual-Q support.
    budget: &'a NativeBudget,
}
impl<'a, C: Caster> BudgetCaster<'a, C> {
    /// Wrap the raw caster once for this lexical native call.
    pub fn new(caster: &'a C, budget: &'a NativeBudget) -> Self {
        Self { caster, budget }
    }
    /// Perform an ordinary surface scan under the same complete-scan budget.
    pub fn scan(
        &self,
        p: Vec3,
        n: Vec3,
        mode: Mode,
        t: &Tuning,
        c: Capsule,
    ) -> Result<Scan, Reject> {
        if self.budget.denied() {
            return Err(Reject::Budget);
        }
        let scan = crate::surface::scan(self, p, n, mode, t, c);
        if self.budget.denied() {
            Err(Reject::Budget)
        } else {
            Ok(scan)
        }
    }
}
impl<C: Caster> Caster for BudgetCaster<'_, C> {
    fn cast(&self, from: Vec3, to: Vec3, mask: u32) -> Option<Hit> {
        let used = self.budget.rays.get();
        if self.budget.denied() || used >= MAX_NATIVE_RAYS {
            self.budget.denied.set(true);
            return None;
        }
        self.budget.rays.set(used.saturating_add(1));
        self.caster.cast(from, to, mask)
    }
}

/// Same-update ordered raw witnesses, never future candidate permission.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FreshPatch {
    /// Feet where the native Stay update collected these rays.
    pub position: Vec3,
    /// Raw answers in ray-origin order, including misses.
    pub hits: [Option<Hit>; 4],
}
/// Copyable finite movement request with no whole-body clearance claim.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VelocityPlan {
    /// Proposed feet; the game may select a different actual candidate.
    pub position: Vec3,
    /// Requested velocity within the private original movement budget.
    pub velocity: Vec3,
    /// Eased facing at the proposal only.
    pub facing: Vec3,
    /// Observed corner kind.
    pub route: Route,
    /// Private original movement and guidance; public fields grant no permission.
    pub(super) guide: Guide,
}
/// Three-hit real support at one actual native candidate, before native collision admission.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Supported {
    /// Newly cast one-face fit, never the cached guide geometry.
    pub fit: Fit,
    /// Facing recomputed at this actual candidate.
    pub facing: Vec3,
}
/// Discover a velocity request from same-update witnesses with at most five extra rays.
pub fn prepare_from_patch<C: Caster>(
    caster: &BudgetCaster<'_, C>,
    input: &Request,
    patch: FreshPatch,
    search_outside: bool,
    tuning: &Tuning,
    capsule: Capsule,
) -> Result<VelocityPlan, Reject> {
    valid(input, tuning, capsule)?;
    if patch.position != input.position {
        return Err(Reject::Invalid);
    }
    if caster.budget.denied() {
        return Err(Reject::Budget);
    }
    let hits = patch.hits.map(|hit| {
        hit.and_then(|mut hit| {
            if !hit.point.is_finite() {
                return None;
            }
            hit.normal = hit.normal.normalized()?;
            Some(hit)
        })
    });
    let normal = input.normal.flat().normalized().ok_or(Reject::Invalid)?;
    let (first, second) = witnesses(&hits, normal).ok_or(Reject::NoSupport)?;
    let signed = input.velocity.dot(UP.cross(normal));
    let query = Rays {
        caster,
        tuning,
        capsule,
    };
    let adjacent = match second {
        Some(second) => second,
        None => query.discover(
            input.position,
            first,
            signed.signum(),
            search_outside || hits.iter().flatten().count() < 4,
        )?,
    };
    let feature = Feature::of(first, adjacent, signed.signum()).ok_or(Reject::NoSupport)?;
    let edge = feature.edge(input.position.y).ok_or(Reject::NoSupport)?;
    if (edge - input.position).flat().length() > query.reach() + RAY_SIDE {
        return Err(Reject::NoSupport);
    }
    let position = feature.advance(input, signed.abs() * input.dt, query.radius())?;
    let velocity = (position - input.position) * (1.0 / input.dt);
    if !velocity.is_finite() || velocity.length() > input.velocity.length() + EPS {
        return Err(Reject::Invalid);
    }
    let guide = Guide {
        feature,
        request: *input,
    };
    Ok(VelocityPlan {
        position,
        velocity,
        facing: facing_at(guide, position, query.radius())?,
        route: feature.route,
        guide,
    })
}
/// Obtain fresh support at actual Q within the private original step envelope.
pub fn fresh_support<C: Caster>(
    caster: &BudgetCaster<'_, C>,
    plan: &VelocityPlan,
    actual_q: Vec3,
    tuning: &Tuning,
    capsule: Capsule,
) -> Result<Supported, Reject> {
    let guide = plan.guide;
    valid(&guide.request, tuning, capsule)?;
    let step = (actual_q - guide.request.position).length();
    if !actual_q.is_finite()
        || !step.is_finite()
        || step > RAY_SIDE + EPS
        || step > guide.request.velocity.length() * guide.request.dt + EPS
    {
        return Err(Reject::Invalid);
    }
    if caster.budget.denied() {
        return Err(Reject::Budget);
    }
    let query = Rays {
        caster,
        tuning,
        capsule,
    };
    let fit = support_at(
        guide.feature,
        actual_q,
        tuning,
        query.reach(),
        |from, to, mask| query.ray(from, to, mask),
    )?;
    Ok(Supported {
        fit,
        facing: facing_at(guide, actual_q, query.radius())?,
    })
}

/// Borrowed finite geometry boundary. It has no motion-oracle or native-state write API.
struct Rays<'a, 'b, C> {
    /// Already-counted actual scene boundary.
    caster: &'a BudgetCaster<'b, C>,
    /// Existing roughness, slope, reach and collider filters.
    tuning: &'a Tuning,
    /// Caller-supplied stance dimensions, not a native whole-body clearance promise.
    capsule: Capsule,
}
impl<C: Caster> Rays<'_, '_, C> {
    /// Existing stand-off rule, including wide capsules.
    fn radius(&self) -> f32 {
        crate::surface::standoff_target(0.0, self.capsule)
    }
    /// Ordinary Move reach; scouting does not increase final grip reach.
    fn reach(&self) -> f32 {
        self.tuning.hold_reach + self.radius() - GAME_STANDOFF
    }
    /// One actual finite query with explicit denial distinct from a geometric miss.
    fn ray(&self, from: Vec3, to: Vec3, mask: u32) -> Result<Option<Hit>, Reject> {
        if !from.is_finite() || !to.is_finite() || (to - from).normalized().is_none() {
            return Err(Reject::Invalid);
        }
        if self.caster.budget.denied() {
            return Err(Reject::Budget);
        }
        let hit = self.caster.cast(from, to, mask);
        if self.caster.budget.denied() {
            return Err(Reject::Budget);
        }
        Ok(hit.and_then(|hit| ray_hit(from, to, hit)))
    }
    /// One direct inside query and at most two visibility/return pairs outside.
    /// Adjacent witnesses guide velocity only; actual-Q support is a separate fresh query.
    fn discover(
        &self,
        position: Vec3,
        first: Hit,
        sign: f32,
        outside: bool,
    ) -> Result<Hit, Reject> {
        let normal = first.normal.flat().normalized().ok_or(Reject::NoSupport)?;
        let tangent = UP.cross(normal) * sign;
        let gap = (position - first.point).dot(normal);
        if !(gap > EPS && gap <= self.reach()) {
            return Err(Reject::NoSupport);
        }
        let [top, low] = RAY_ROWS;
        let chest = position + UP * ((top + low) * 0.5);
        if let Some(hit) = self.ray(chest, chest + tangent * self.reach(), self.tuning.mask)? {
            if Feature::of(first, hit, sign).is_some() {
                return Ok(hit);
            }
        }
        if !outside {
            return Err(Reject::NoSupport);
        }
        let mut hits = [None; 2];
        for (slot, height) in hits.iter_mut().zip(RAY_ROWS) {
            let origin = position + UP * height;
            let scout =
                origin - normal * (gap + RAY_SIDE) + tangent * (RAY_SIDE + self.capsule.radius);
            if (scout - origin).length() > self.reach() + RAY_SIDE {
                continue;
            }
            if self.ray(origin, scout, self.tuning.mask | SOLID)?.is_none() {
                let return_reach = (scout - origin).dot(tangent) + self.reach();
                *slot = self.ray(scout, scout - tangent * return_reach, self.tuning.mask)?;
            }
        }
        let [Some(a), Some(b)] = hits else {
            return Err(Reject::NoSupport);
        };
        if a.entity != b.entity
            || a.normal.dot(b.normal) < ALIGN
            || (a.point - b.point).dot(a.normal).abs() > self.tuning.plane_tolerance * 1.5
            || (a.point - b.point).length() <= CROSS
        {
            return Err(Reject::NoSupport);
        }
        let adjacent = Hit {
            point: (a.point + b.point) * 0.5,
            normal: (a.normal + b.normal)
                .normalized()
                .ok_or(Reject::NoSupport)?,
            entity: a.entity,
        };
        Feature::of(first, adjacent, sign)
            .map(|_| adjacent)
            .ok_or(Reject::NoSupport)
    }
}
