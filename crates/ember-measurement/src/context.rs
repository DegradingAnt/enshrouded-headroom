//! Caller-observed world contexts, bounded freshness and explicit grid calibration.
use crate::{Error, Point};
use ember_blueprint::Frame;

/// An opaque world identity and coordinate/origin lifetime, supplied by the caller.
///
/// A numeric zero epoch is allowed only when explicitly known. Unknown epochs use
/// Unavailable::Epoch. This type does not prove a native address or lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldEpoch {
    /// Stable opaque ID, never a pointer to dereference or a filename.
    world: String,
    /// Caller-observed lifetime of the coordinate/origin frame.
    epoch: u64,
}

impl WorldEpoch {
    /// Name one explicitly known world and epoch. No identity is guessed by default.
    pub fn new(world: &str, epoch: u64) -> Result<Self, Error> {
        if world.is_empty()
            || world.len() > 128
            || !world
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.:/@-".contains(&b))
        {
            return Err(Error::InvalidWorld);
        }
        Ok(Self {
            world: world.to_owned(),
            epoch,
        })
    }
    /// Read the opaque world identity.
    pub fn world_id(&self) -> &str {
        &self.world
    }
    /// Read the explicitly observed epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

/// Producer metadata. Sequences are monotonic within each session's world/aim stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stamp {
    /// Caller monotonic milliseconds, not wall-clock time or a game pointer.
    pub observed_at_ms: u64,
    /// Ordering counter maintained across world/epoch changes by the producer.
    pub sequence: u64,
}

/// A bounded observation-age policy, independent of unverified native cadence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FreshnessPolicy {
    /// Maximum accepted age, inclusive.
    max_age_ms: u64,
}

impl Default for FreshnessPolicy {
    fn default() -> Self {
        Self { max_age_ms: 250 }
    }
}

impl FreshnessPolicy {
    /// Accept an explicit age from 1 through 60,000 ms.
    pub fn new(max_age_ms: u64) -> Result<Self, Error> {
        if !(1..=60_000).contains(&max_age_ms) {
            return Err(Error::InvalidPolicy);
        }
        Ok(Self { max_age_ms })
    }
    /// Read the maximum accepted age.
    pub fn max_age_ms(self) -> u64 {
        self.max_age_ms
    }
}

/// Why a real producer cannot assert a current world context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unavailable {
    /// Runtime contracts and a native producer are not supplied.
    NativeBinding,
    /// Current world identity cannot be established.
    World,
    /// Coordinate/origin lifetime cannot be established.
    Epoch,
}

impl Unavailable {
    /// Map a producer absence to a public unavailable status.
    pub(crate) fn error(self) -> Error {
        match self {
            Self::NativeBinding => Error::NativeUnavailable,
            Self::World => Error::UnknownWorld,
            Self::Epoch => Error::UnknownEpoch,
        }
    }
}

/// A caller assertion about a current canonical world frame. It is checked on receipt.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldSample {
    /// Explicit world identity and origin epoch.
    pub context: WorldEpoch,
    /// Reused W38 canonical units, axes and declared mapping.
    pub frame: Frame,
    /// Freshness and ordering metadata.
    pub stamp: Stamp,
}

/// A producer must report absence instead of inventing current coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum WorldObservation {
    /// No current contract can be asserted.
    Unavailable(Unavailable),
    /// A complete observed world frame.
    Current(WorldSample),
}

/// A typed current point or explicit ray miss supplied by a verified caller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Aim {
    /// A canonical surface position supplied by a future verified producer.
    Hit(Point),
    /// A canonical local-player position supplied by a future verified producer.
    PlayerPosition(Point),
    /// The current aim ray legitimately found no surface.
    Miss,
    /// Hit data could not be obtained.
    Unavailable,
}

/// One selected-point observation, with no native reader or ray caster in this crate.
#[derive(Clone, Debug, PartialEq)]
pub struct AimSample {
    /// Context in which this position, hit or absence was observed.
    pub context: WorldEpoch,
    /// Freshness and ordering metadata.
    pub stamp: Stamp,
    /// A real observation or explicit absence.
    pub aim: Aim,
}

/// A caller-validated 0.5 m lattice origin. There is no default zero origin.
///
/// Construction means the caller asserts prior calibration for this context.
/// It does not perform, replace or prove W42 native/live calibration.
#[derive(Clone, Debug, PartialEq)]
pub struct GridOrigin {
    /// Context of the calibration.
    context: WorldEpoch,
    /// Canonical origin in metres.
    point: Point,
    /// Last caller confirmation that this origin remains valid.
    confirmed_at_ms: u64,
}

impl GridOrigin {
    /// Read confirmation time for the session's separate activation barrier.
    pub(crate) fn confirmed_at_ms(&self) -> u64 {
        self.confirmed_at_ms
    }

    /// Supply a calibrated origin and its explicit context/time.
    pub fn caller_validated(context: WorldEpoch, point: Point, confirmed_at_ms: u64) -> Self {
        Self {
            context,
            point,
            confirmed_at_ms,
        }
    }
}

/// Snap only with a current caller-validated origin; never fall back to zero.
/// This stateless calculation has no activation history. Session snapping additionally
/// enforces origin reconfirmation after context changes or loss of knowledge.
pub fn snap_to_small(
    point: Point,
    origin: Option<&GridOrigin>,
    context: &WorldEpoch,
    now_ms: u64,
    policy: FreshnessPolicy,
) -> Result<Point, Error> {
    snap_with_deadline(point, origin, context, now_ms, policy).map(|(point, _)| point)
}

/// Share snap math and its validity deadline with preview and pick.
pub(crate) fn snap_with_deadline(
    point: Point,
    origin: Option<&GridOrigin>,
    context: &WorldEpoch,
    now_ms: u64,
    policy: FreshnessPolicy,
) -> Result<(Point, u64), Error> {
    let origin = origin.ok_or(Error::GridUnavailable)?;
    if &origin.context != context {
        return Err(Error::WorldMismatch);
    }
    let until = fresh_until(origin.confirmed_at_ms, now_ms, policy, Error::StaleGrid)?;
    let [x, y, z] = point.coordinates();
    let [ox, oy, oz] = origin.point.coordinates();
    // Bounded Point values keep differences and lattice quotients finite.
    // f64::round defines halfway ties away from zero in relative lattice coordinates.
    let snap = |value: f64, origin: f64| origin + ((value - origin) / 0.5).round() * 0.5;
    Ok((Point::new([snap(x, ox), snap(y, oy), snap(z, oz)])?, until))
}

/// Check age and an expressible inclusive validity deadline.
pub(crate) fn fresh_until(
    observed_at_ms: u64,
    now_ms: u64,
    policy: FreshnessPolicy,
    stale: Error,
) -> Result<u64, Error> {
    let age = now_ms
        .checked_sub(observed_at_ms)
        .ok_or(Error::FutureSample)?;
    if age > policy.max_age_ms {
        return Err(stale);
    }
    observed_at_ms
        .checked_add(policy.max_age_ms)
        .ok_or(Error::ClockRange)
}

/// Check the W38 frame rather than assuming incoming engine coordinates are metres.
pub(crate) fn validate_frame(frame: &Frame) -> Result<(), Error> {
    if frame.units != "metres"
        || frame.axes != "right_handed_x_east_y_up_z_south"
        || frame.source_mapping.is_empty()
        || frame.source_mapping.chars().take(1025).count() > 1024
        || frame.source_mapping.chars().any(char::is_control)
    {
        return Err(Error::UnsupportedFrame);
    }
    Ok(())
}
