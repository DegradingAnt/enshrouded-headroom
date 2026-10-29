//! Explicit unavailable and rejected states, without a logging or native dependency.
use std::fmt;

/// A bounded calculation, freshness or ownership failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// Coordinates are not finite canonical metres within the supported bound.
    InvalidPoint,
    /// Units, axes or mapping metadata do not satisfy the W38 frame contract.
    UnsupportedFrame,
    /// The opaque world identity is empty, too long or not a stable ASCII ID.
    InvalidWorld,
    /// The maximum sample age is outside the explicit bounded policy.
    InvalidPolicy,
    /// Integer inclusive counting cannot fit its result.
    Overflow,
    /// No verified native adapter supplies current observations.
    NativeUnavailable,
    /// Current world identity is unknown.
    UnknownWorld,
    /// Current coordinate/origin epoch is unknown.
    UnknownEpoch,
    /// The world observation expired.
    StaleWorld,
    /// The aim observation expired.
    StaleAim,
    /// The caller-validated grid origin expired.
    StaleGrid,
    /// A world confirmation was not observed after the latest loss of knowledge.
    WorldConfirmationRequired,
    /// The origin was not confirmed for the current context activation.
    GridConfirmationRequired,
    /// The aim observation predates or cannot be ordered after this activation.
    AimConfirmationRequired,
    /// An observation claims a time later than the query.
    FutureSample,
    /// A sample sequence or observation time is older than the accepted stream.
    OutOfOrder,
    /// A point or grid origin belongs to another world or epoch.
    WorldMismatch,
    /// An old epoch attempted to replace a newer epoch in the same world.
    EpochRegression,
    /// The declared mapping changed without an epoch change.
    FrameChanged,
    /// Caller monotonic time moved backwards.
    ClockRegression,
    /// A freshness deadline cannot be represented.
    ClockRange,
    /// The game is not focused.
    FocusLost,
    /// A native menu is open.
    NativeMenuOpen,
    /// Native menu state is unknown.
    MenuUnknown,
    /// Exclusive input ownership was not granted to measurement.
    InputNotOwned,
    /// Measurement mode requires an explicit allowed Begin.
    Inactive,
    /// No unconsumed current aim candidate exists.
    NoAim,
    /// A current aim sample legitimately missed.
    NoHit,
    /// The aim producer explicitly reported unavailable data.
    AimUnavailable,
    /// Snapping was requested without a caller-validated origin.
    GridUnavailable,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "measurement core: {self:?}")
    }
}

impl std::error::Error for Error {}
