//! Owned scalar state for one native move. No borrowed frame pointer enters TLS.
use crate::corner::{
    self, BudgetCaster, FreshPatch, NativeBudget, Request, Supported, Usage, VelocityPlan,
};
use crate::geom::Vec3;
use crate::surface::{
    scan_move_from_hold, Capsule, Caster, Fit, Mode, MoveProbe, Reject, Scan, Tuning,
};
use std::cell::Cell;

/// Opaque identities copied from the currently live native context; never dereferenced here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    /// Native context address, used only for equality.
    pub(crate) context: usize,
    /// Climber entity identity.
    pub(crate) entity: u32,
    /// World address, used only for equality.
    pub(crate) world: usize,
}
/// Same-update raw Stay witnesses with their exact opaque native identities.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StayPatch {
    /// Identities at the completed Stay query.
    pub(crate) key: Key,
    /// Fresh raw rays at current feet.
    pub(crate) patch: FreshPatch,
}
/// Newly cast support at one actual native candidate.
#[derive(Clone, Copy, Debug)]
struct Proof {
    /// Actual feet queried by the original move-check callback.
    position: Vec3,
    /// Real support and eased facing at those feet.
    supported: Supported,
}
/// All retained values are owned scalars. This is never a cross-frame gameplay cache.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Call {
    /// Exact currently live context identity.
    key: Key,
    /// Native movement budget, before custom steering.
    request: Request,
    /// Current-update discovery hints only.
    patch: FreshPatch,
    /// Existing same-update recovery fit, when baseline policy admitted it.
    held: Option<Fit>,
    /// Existing native-config surface settings.
    tuning: Tuning,
    /// Existing stance configuration.
    capsule: Capsule,
    /// Counts shared by every query of this same lexical call.
    usage: Usage,
    /// Discovery result; never a native collision-clearance claim.
    plan: Option<VelocityPlan>,
    /// Last real custom support, invalidated by a native successful answer.
    proof: Option<Proof>,
    /// Reentrant scene callbacks cannot start another custom query.
    querying: bool,
}
impl Call {
    /// Require matching current feet and finite native inputs before retaining owned values.
    pub(crate) fn new(
        key: Key,
        request: Request,
        patch: FreshPatch,
        held: Option<Fit>,
        tuning: Tuning,
        capsule: Capsule,
    ) -> Option<Self> {
        if key.context == 0
            || key.world == 0
            || key.entity == 0
            || patch.position != request.position
            || !request.position.is_finite()
            || !request.velocity.is_finite()
            || !request.normal.is_finite()
            || !(request.dt > 0.0 && request.dt.is_finite())
        {
            return None;
        }
        Some(Self {
            key,
            request,
            patch,
            held,
            tuning,
            capsule,
            usage: Usage::default(),
            plan: None,
            proof: None,
            querying: false,
        })
    }
}
/// Copyable lexical state. It contains no borrowed storage or native-memory access capability.
#[derive(Clone, Copy)]
struct State {
    /// Zero outside a move; nesting is bounded for custom work, not native calls.
    depth: u8,
    /// Nested moves forward natively with every custom query suspended.
    blocked: bool,
    /// Optional corner owner in this one native move.
    call: Option<Call>,
}
thread_local! {
    /// Const-initialized owned storage needs no heap allocation or destructor.
    static CURRENT: Cell<State> = const { Cell::new(State { depth: 0, blocked: false, call: None }) };
}
/// Whether a native move already owns this lexical scope.
pub(crate) fn inside_move() -> bool {
    CURRENT
        .try_with(|slot| slot.get().depth != 0)
        .unwrap_or(false)
}
/// Whether nested/custom scene callbacks must forward natively.
pub(crate) fn blocked() -> bool {
    CURRENT.try_with(|slot| slot.get().blocked).unwrap_or(true)
}
/// Whether this move owns an optional counted corner scope.
pub(crate) fn has_call() -> bool {
    current().is_some()
}
/// Whether an actual custom scene query is currently on the stack.
pub(crate) fn querying() -> bool {
    CURRENT
        .try_with(|slot| slot.get().call.is_some_and(|call| call.querying))
        .unwrap_or(true)
}
/// Run one native move under owned lexical state, restoring the exact previous state on unwind.
pub(crate) fn with_move<R>(call: Option<Call>, run: impl FnOnce() -> R) -> R {
    let previous = CURRENT
        .try_with(|slot| {
            let previous = slot.get();
            let blocked = previous.depth != 0;
            slot.set(State {
                depth: previous.depth.saturating_add(1),
                blocked,
                call: if blocked { None } else { call },
            });
            previous
        })
        .ok();
    let _restore = Restore(previous);
    run()
}
/// Calculate a finite velocity request under the shared precheck/discovery budget.
pub(crate) fn prepare<C: Caster>(caster: &C, direction: Vec3, velocity: Vec3) -> Option<Vec3> {
    let result = query(caster, |counted, call| {
        corner::valid(&call.request, &call.tuning, call.capsule)?;
        let q = call.request.position + velocity * call.request.dt;
        let ordinary = match call.held {
            Some(held) => scan_move_from_hold(
                counted,
                MoveProbe {
                    current: call.request.position,
                    candidate: q,
                    direction,
                    held,
                },
                &call.tuning,
                call.capsule,
            ),
            None => counted.scan(q, direction, Mode::Move, &call.tuning, call.capsule)?,
        };
        corner::prepare_from_patch(
            counted,
            &call.request,
            call.patch,
            ordinary.verdict == Err(Reject::Hits),
            &call.tuning,
            call.capsule,
        )
    })?
    .ok()?;
    update(|call| {
        call.plan = Some(result);
        call.proof = None;
    });
    Some(result.velocity)
}
/// The native original succeeded, so no earlier custom candidate grants a facing commit.
pub(crate) fn native_success() {
    update(|call| call.proof = None);
}
/// Which custom answer is permitted after the original native probe failed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Answer {
    /// No corner plan exists; retain the ordinary natural-surface policy.
    Ordinary,
    /// Missing/stale scope, geometry or budget refuses custom support.
    Refused,
    /// Newly cast one-face support at this actual native Q.
    Supported(Supported),
}
/// Answer only from current actual geometry, never the public proposal or historical fit.
pub(crate) fn answer<C: Caster>(caster: &C, key: Key, q: Vec3) -> Answer {
    update(|call| call.proof = None);
    let Some(call) = current() else {
        return Answer::Refused;
    };
    if blocked() || call.querying || call.key != key {
        return Answer::Refused;
    }
    let Some(plan) = call.plan else {
        return Answer::Ordinary;
    };
    let Some(Ok(supported)) = query(caster, |counted, call| {
        corner::fresh_support(counted, &plan, q, &call.tuning, call.capsule)
    }) else {
        return Answer::Refused;
    };
    update(|call| {
        call.proof = Some(Proof {
            position: q,
            supported,
        })
    });
    Answer::Supported(supported)
}
/// Apply ordinary surface policy with this same lexical call's counted boundary.
pub(crate) fn ordinary<C: Caster>(
    caster: &C,
    key: Key,
    scan: impl FnOnce(&BudgetCaster<'_, C>) -> Scan,
) -> Option<Scan> {
    let call = current()?;
    if call.key != key {
        return None;
    }
    query(caster, |counted, _| scan(counted))
}
/// Take a facing only after native AL success and matching final velocity/current context.
pub(crate) fn finish(key: Key, current: Vec3, velocity: Vec3, native: u64) -> Option<Vec3> {
    let call = self::current()?;
    let proof = call.proof;
    update(|call| call.proof = None);
    if native as u8 == 0
        || call.key != key
        || call.request.position != current
        || !velocity.is_finite()
        || call.plan.is_none()
        || blocked()
        || call.querying
    {
        return None;
    }
    let proof = proof?;
    let expected = current + velocity * call.request.dt;
    let error = (expected - proof.position).length();
    if !expected.is_finite()
        || !error.is_finite()
        || error > 1e-4
        || velocity.length() > call.request.velocity.length() + 1e-4
    {
        return None;
    }
    Some(proof.supported.facing)
}

/// Restore an owned previous scope even when a Rust callback unwinds.
struct Restore(Option<State>);
impl Drop for Restore {
    fn drop(&mut self) {
        if let Some(previous) = self.0 {
            let _ = CURRENT.try_with(|slot| slot.set(previous));
        }
    }
}
/// Current owned optional call; no reference or native pointer escapes TLS.
fn current() -> Option<Call> {
    CURRENT.try_with(|slot| slot.get().call).ok().flatten()
}
/// Change only owned current-call values, with no native memory access.
fn update(change: impl FnOnce(&mut Call)) {
    let _ = CURRENT.try_with(|slot| {
        let mut state = slot.get();
        if let Some(mut call) = state.call {
            change(&mut call);
            state.call = Some(call);
            slot.set(state);
        }
    });
}
/// Copy the same owner's actual query counts back on normal return and Rust unwind.
struct QueryUsage<'a> {
    /// Original lexical owner; nested moves cannot replace it while custom queries run.
    key: Key,
    /// Local counted boundary; its mutations survive callback unwinding through this guard.
    budget: &'a NativeBudget,
}
impl Drop for QueryUsage<'_> {
    fn drop(&mut self) {
        update(|call| {
            if call.key == self.key {
                call.usage = self.budget.usage();
                call.querying = false;
            }
        });
    }
}
/// Borrow a counted scene boundary synchronously; only owned usage returns to TLS.
fn query<C: Caster, R>(caster: &C, run: impl FnOnce(&BudgetCaster<'_, C>, Call) -> R) -> Option<R> {
    let owner = CURRENT
        .try_with(|slot| {
            let mut state = slot.get();
            let mut call = state.call?;
            if state.blocked || call.querying {
                return None;
            }
            call.querying = true;
            state.call = Some(call);
            slot.set(state);
            Some(call)
        })
        .ok()
        .flatten()?;
    let budget = NativeBudget::resume(owner.usage);
    let _usage = QueryUsage {
        key: owner.key,
        budget: &budget,
    };
    if budget.denied() {
        return None;
    }
    let counted = BudgetCaster::new(caster, &budget);
    let answer = run(&counted, owner);
    if budget.denied() {
        None
    } else {
        Some(answer)
    }
}
