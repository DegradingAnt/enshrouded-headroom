//! Abstract input ownership and fail-closed, world-tagged measurement sessions.
use crate::{
    context::{fresh_until, snap_with_deadline, validate_frame},
    measure, Aim, AimSample, Error, FreshnessPolicy, GridOrigin, Measurement, Point, Stamp,
    WorldEpoch, WorldObservation, WorldSample,
};

/// Native menu state supplied by a future verified adapter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NativeMenu {
    /// No native menu is open.
    Closed,
    /// A native menu is open; measurement must yield input.
    Open,
    /// Menu state cannot be established; measurement must yield input.
    #[default]
    Unknown,
}

/// Exclusive input grant supplied by the caller, not acquired by this crate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InputOwner {
    /// The adapter explicitly granted measurement exclusive capture.
    Measurement,
    /// A competing mod or native input mode owns capture.
    Other,
    /// Exclusive ownership cannot be established.
    #[default]
    Unknown,
}

/// Current input permission. A stale cached input assertion is not sufficient.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InputContext {
    /// The game currently has focus.
    pub focused: bool,
    /// Native menu availability, without key codes or menu detection here.
    pub native_menu: NativeMenu,
    /// The adapter's current exclusive input grant.
    pub owner: InputOwner,
}

impl InputContext {
    /// Check every permission before consuming an abstract action.
    fn check(self) -> Result<(), Error> {
        if !self.focused {
            return Err(Error::FocusLost);
        }
        match self.native_menu {
            NativeMenu::Open => return Err(Error::NativeMenuOpen),
            NativeMenu::Unknown => return Err(Error::MenuUnknown),
            NativeMenu::Closed => {}
        }
        if self.owner != InputOwner::Measurement {
            return Err(Error::InputNotOwned);
        }
        Ok(())
    }
}

/// Requested snap policy for a new candidate. Accepted pins are immutable.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Snap<'a> {
    /// Preserve the validated canonical position without grid snapping.
    #[default]
    Off,
    /// Require a fresh caller-validated 0.5 m lattice origin in this world epoch.
    SmallGrid(Option<&'a GridOrigin>),
}

/// Item-free abstract intent. No global keys or controller mapping are defined.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action<'a> {
    /// Explicitly begin or reacquire measurement after all permissions pass.
    Begin,
    /// Pin one unconsumed current hit, optionally snapped.
    Pick(Snap<'a>),
    /// Clear accepted pins and the transient aim candidate.
    Clear,
    /// Leave the mode and release capture. Pins remain dormant in this epoch.
    Exit,
}

/// Whether the adapter should hold the measurement input claim.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Mode {
    /// No capture is requested. A new session starts here.
    #[default]
    Inactive,
    /// Capture is requested while world and input assertions remain valid.
    Active,
    /// A conflict or unavailable context revoked capture; Begin is required.
    Suspended,
}

/// An action either consumed a permitted intent or rejected it without moving pins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActionResult {
    /// An explicit rejection, or None for an accepted action.
    error: Option<Error>,
}

impl ActionResult {
    /// Whether the abstract action was accepted and may be consumed by the adapter.
    pub fn consumed(self) -> bool {
        self.error.is_none()
    }

    /// Read the rejection reason.
    pub fn error(self) -> Option<Error> {
        self.error
    }
}

/// Availability of the latest unconsumed aim candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AimStatus {
    /// A fresh, unconsumed hit is available under the requested snap policy.
    Hit,
    /// A fresh legitimate ray miss cleared the previous preview.
    Miss,
    /// An unavailable, consumed, invalid or expired candidate cannot be picked.
    Unavailable(Error),
}

/// Geometry availability is explicit and separate from aim availability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewStatus {
    /// A current world or permitted active mode cannot be asserted.
    Unavailable(Error),
    /// Active and current, with no accepted first point.
    AwaitingFirst,
    /// A is accepted; no current transient B can be displayed.
    FirstPinned,
    /// A is accepted and B is a transient current hit.
    Preview,
    /// Both A and B are accepted pins in the current world epoch.
    Pinned,
}

/// One checked snapshot, with render-neutral W38 geometry and an explicit deadline.
///
/// Call Session::view with current world, time and input assertions for every frame.
/// The deadline is an upper bound, not a lease: a later menu, world or focus event
/// invalidates a cached snapshot immediately. This type supplies no renderer.
#[derive(Debug, PartialEq)]
pub struct View {
    /// Checked geometry availability.
    status: ViewStatus,
    /// Context is deliberately absent when the view is unavailable.
    context: Option<WorldEpoch>,
    /// Accepted A, when the current mode is renderable.
    first: Option<Point>,
    /// Accepted B or the explicitly labelled transient candidate.
    second: Option<Point>,
    /// A reference outline, never a solid placement instruction.
    measurement: Option<Measurement>,
    /// Latest aim availability, independent of accepted pins.
    aim_status: AimStatus,
    /// Inclusive bound from current world, aim and grid confirmations as applicable.
    valid_until_ms: Option<u64>,
}

impl View {
    /// Read whether this is a pinned pair, transient preview or unavailable view.
    pub fn status(&self) -> ViewStatus {
        self.status
    }

    /// Read the checked world epoch. Unavailable snapshots expose no coordinates.
    pub fn context(&self) -> Option<&WorldEpoch> {
        self.context.as_ref()
    }

    /// Read accepted A only when the checked mode is renderable.
    pub fn first(&self) -> Option<Point> {
        self.first
    }

    /// Read pinned B or the transient B identified by status().
    pub fn second(&self) -> Option<Point> {
        self.second
    }

    /// Read checked reference geometry. Requery the session for each render frame.
    pub fn measurement(&self) -> Option<&Measurement> {
        self.measurement.as_ref()
    }

    /// Read aim availability. A stale aim does not expire already accepted pins.
    pub fn aim_status(&self) -> AimStatus {
        self.aim_status
    }

    /// Read the inclusive upper freshness bound; never reuse it as an input grant.
    pub fn valid_until_ms(&self) -> Option<u64> {
        self.valid_until_ms
    }

    /// Construct an unavailable view without leaking dormant context or pins.
    fn unavailable(error: Error) -> Self {
        Self {
            status: ViewStatus::Unavailable(error),
            context: None,
            first: None,
            second: None,
            measurement: None,
            aim_status: AimStatus::Unavailable(error),
            valid_until_ms: None,
        }
    }
}

/// Pure measurement state. Every observation and view is a caller assertion.
///
/// The adapter must release its native input claim whenever mode() is not Active.
/// World changes reset independently of input permission. Unknown/stale contexts
/// suspend capture and hide dormant pins until fresh identical-context confirmation
/// followed by an explicit Begin. There is no native provider in this crate.
#[derive(Debug)]
pub struct Session {
    /// Bounded caller-selected age policy.
    policy: FreshnessPolicy,
    /// Last accepted world contract, retained only to tag dormant pins.
    world: Option<WorldSample>,
    /// Latest fresh ordered identity, even when its canonical frame is unsupported.
    known_context: Option<WorldEpoch>,
    /// A current unavailable reason, including rejected world observations.
    world_error: Option<Error>,
    /// High-water mark of accepted world observations across world changes.
    world_stamp: Option<Stamp>,
    /// High-water mark of accepted aim observations across world changes.
    aim_stamp: Option<Stamp>,
    /// Latest unconsumed candidate.
    aim: Option<AimSample>,
    /// Explicit candidate absence or rejection.
    aim_error: Error,
    /// Accepted points in the last accepted world context.
    pins: (Option<Point>, Option<Point>),
    /// Capture request; actual native ownership remains the adapter's job.
    mode: Mode,
    /// High-water mark of caller monotonic query time.
    last_now_ms: Option<u64>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new(FreshnessPolicy::default())
    }
}

impl Session {
    /// Start inactive and unavailable, without a world, zero origin or sample source.
    pub fn new(policy: FreshnessPolicy) -> Self {
        Self {
            policy,
            world: None,
            known_context: None,
            world_error: Some(Error::NativeUnavailable),
            world_stamp: None,
            aim_stamp: None,
            aim: None,
            aim_error: Error::NoAim,
            pins: (None, None),
            mode: Mode::Inactive,
            last_now_ms: None,
        }
    }

    /// Read the capture request after every call. Release native capture unless Active.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Accept an explicit current world or fail closed on a missing/rejected contract.
    pub fn observe_world(
        &mut self,
        now_ms: u64,
        observation: WorldObservation,
    ) -> Result<(), Error> {
        self.clock(now_ms)?;
        let sample = match observation {
            WorldObservation::Unavailable(reason) => {
                self.invalidate_world(reason.error());
                return Ok(());
            }
            WorldObservation::Current(sample) => sample,
        };
        if let Err(error) = self.check_world_context(now_ms, &sample) {
            self.invalidate_world(error);
            return Err(error);
        }
        if self
            .known_context
            .as_ref()
            .is_some_and(|old| old != &sample.context)
        {
            self.pins = (None, None);
            self.clear_aim(Error::NoAim);
            self.mode = Mode::Inactive;
        }
        self.known_context = Some(sample.context.clone());
        self.world_stamp = Some(sample.stamp);
        let frame = validate_frame(&sample.frame).and_then(|()| {
            if self
                .world
                .as_ref()
                .is_some_and(|old| old.context == sample.context && old.frame != sample.frame)
            {
                Err(Error::FrameChanged)
            } else {
                Ok(())
            }
        });
        if let Err(error) = frame {
            self.invalidate_world(error);
            return Err(error);
        }
        self.world = Some(sample);
        self.world_error = None;
        Ok(())
    }

    /// Accept a current hit/miss only while input and measurement mode are permitted.
    /// A rejected observation clears the old transient candidate, never accepted pins.
    pub fn observe_aim(
        &mut self,
        now_ms: u64,
        input: InputContext,
        sample: AimSample,
    ) -> Result<(), Error> {
        let checked = self.permit(now_ms, input, true).and_then(|_| {
            let world = self.world.as_ref().ok_or(Error::NativeUnavailable)?;
            if sample.context != world.context {
                return Err(Error::WorldMismatch);
            }
            fresh_until(
                sample.stamp.observed_at_ms,
                now_ms,
                self.policy,
                Error::StaleAim,
            )?;
            ordered(self.aim_stamp, sample.stamp)
        });
        if let Err(error) = checked {
            self.clear_aim(error);
            return Err(error);
        }
        self.aim_stamp = Some(sample.stamp);
        self.aim = Some(sample);
        Ok(())
    }

    /// Check and consume one abstract action. Rejection never moves accepted pins.
    pub fn act(&mut self, now_ms: u64, input: InputContext, action: Action<'_>) -> ActionResult {
        let result = self
            .permit(now_ms, input, !matches!(action, Action::Begin))
            .and_then(|_| match action {
                Action::Begin => {
                    if self.mode != Mode::Active {
                        self.clear_aim(Error::NoAim);
                    }
                    self.mode = Mode::Active;
                    Ok(())
                }
                Action::Pick(snap) => self.candidate(now_ms, snap).map(|(point, _)| {
                    self.pins = match self.pins {
                        (Some(a), None) => (Some(a), Some(point)),
                        _ => (Some(point), None),
                    };
                    self.clear_aim(Error::NoAim);
                }),
                Action::Clear => {
                    self.pins = (None, None);
                    self.clear_aim(Error::NoAim);
                    Ok(())
                }
                Action::Exit => {
                    self.mode = Mode::Inactive;
                    self.clear_aim(Error::NoAim);
                    Ok(())
                }
            });
        ActionResult {
            error: result.err(),
        }
    }

    /// Recheck time, world and input for every render-neutral view.
    /// Pin pairs depend on current world confirmation, not the age of the picked ray.
    pub fn view(&mut self, now_ms: u64, input: InputContext, snap: Snap<'_>) -> View {
        let world_until = match self.permit(now_ms, input, true) {
            Ok(until) => until,
            Err(error) => return View::unavailable(error),
        };
        let candidate = self.candidate(now_ms, snap);
        let aim_status = match candidate {
            Ok(_) => AimStatus::Hit,
            Err(Error::NoHit) => AimStatus::Miss,
            Err(error) => AimStatus::Unavailable(error),
        };
        let context = self.world.as_ref().map(|world| world.context.clone());
        let (status, second, until) = match (self.pins, candidate) {
            ((Some(_), Some(b)), _) => (ViewStatus::Pinned, Some(b), world_until),
            ((Some(_), None), Ok((b, until))) => {
                (ViewStatus::Preview, Some(b), world_until.min(until))
            }
            ((Some(_), None), Err(_)) => (ViewStatus::FirstPinned, None, world_until),
            _ => (ViewStatus::AwaitingFirst, None, world_until),
        };
        let measurement = match (self.world.as_ref(), self.pins.0, second) {
            (Some(world), Some(a), Some(b)) => match measure(&world.frame, a, b) {
                Ok(measurement) => Some(measurement),
                Err(error) => return View::unavailable(error),
            },
            _ => None,
        };
        View {
            status,
            context,
            first: self.pins.0,
            second,
            measurement,
            aim_status,
            valid_until_ms: Some(until),
        }
    }

    /// Admit only fresh ordered context facts, before requiring a usable metre frame.
    fn check_world_context(&self, now_ms: u64, sample: &WorldSample) -> Result<(), Error> {
        fresh_until(
            sample.stamp.observed_at_ms,
            now_ms,
            self.policy,
            Error::StaleWorld,
        )?;
        ordered(self.world_stamp, sample.stamp)?;
        if self.known_context.as_ref().is_some_and(|old| {
            old.world_id() == sample.context.world_id() && sample.context.epoch() < old.epoch()
        }) {
            return Err(Error::EpochRegression);
        }
        Ok(())
    }

    /// Enforce the monotonic caller clock and revoke a previous live assertion on error.
    fn clock(&mut self, now_ms: u64) -> Result<(), Error> {
        if self.last_now_ms.is_some_and(|last| now_ms < last) {
            self.invalidate_world(Error::ClockRegression);
            return Err(Error::ClockRegression);
        }
        self.last_now_ms = Some(now_ms);
        Ok(())
    }

    /// Check world freshness even without a new producer event, then current input.
    fn permit(&mut self, now_ms: u64, input: InputContext, active: bool) -> Result<u64, Error> {
        self.clock(now_ms)?;
        let world = self.world_error.map_or_else(
            || {
                self.world
                    .as_ref()
                    .map_or(Err(Error::NativeUnavailable), |world| {
                        fresh_until(
                            world.stamp.observed_at_ms,
                            now_ms,
                            self.policy,
                            Error::StaleWorld,
                        )
                    })
            },
            Err,
        );
        let until = match world {
            Ok(until) => until,
            Err(error) => {
                self.invalidate_world(error);
                return Err(error);
            }
        };
        if let Err(error) = input.check() {
            self.suspend();
            self.clear_aim(error);
            return Err(error);
        }
        if active && self.mode != Mode::Active {
            return Err(Error::Inactive);
        }
        Ok(until)
    }

    /// Read only a current matching-context hit; snapping cannot silently fall back.
    fn candidate(&self, now_ms: u64, snap: Snap<'_>) -> Result<(Point, u64), Error> {
        let sample = self.aim.as_ref().ok_or(self.aim_error)?;
        let world = self.world.as_ref().ok_or(Error::NativeUnavailable)?;
        if sample.context != world.context {
            return Err(Error::WorldMismatch);
        }
        let until = fresh_until(
            sample.stamp.observed_at_ms,
            now_ms,
            self.policy,
            Error::StaleAim,
        )?;
        let point = match sample.aim {
            Aim::Hit(point) => point,
            Aim::Miss => return Err(Error::NoHit),
            Aim::Unavailable => return Err(Error::AimUnavailable),
        };
        match snap {
            Snap::Off => Ok((point, until)),
            Snap::SmallGrid(origin) => {
                snap_with_deadline(point, origin, &world.context, now_ms, self.policy)
                    .map(|(point, grid_until)| (point, until.min(grid_until)))
            }
        }
    }

    /// Preserve accepted pins as nonrenderable context-tagged data.
    fn invalidate_world(&mut self, error: Error) {
        self.world_error = Some(error);
        self.clear_aim(error);
        self.suspend();
    }

    /// Revoke capture without silently reacquiring it on the next valid assertion.
    fn suspend(&mut self) {
        if self.mode == Mode::Active {
            self.mode = Mode::Suspended;
        }
    }

    /// Remove a transient candidate without changing accepted pins or stream order.
    fn clear_aim(&mut self, error: Error) {
        self.aim = None;
        self.aim_error = error;
    }
}

/// Accepted stream sequence strictly increases; observation time may stay equal.
fn ordered(previous: Option<Stamp>, sample: Stamp) -> Result<(), Error> {
    if previous.is_some_and(|last| {
        sample.sequence <= last.sequence || sample.observed_at_ms < last.observed_at_ms
    }) {
        return Err(Error::OutOfOrder);
    }
    Ok(())
}
