//! Serialized per-caller command admission.
use crate::{ExecutionFailure, ExecutionReport, ExecutionStage, NativeAdapter, NativeError};
use std::collections::BTreeMap;

use crate::{
    Action, Area, BuildPlan, Command, CommandOutcome, Error, GridPoint, InputContext, Limits,
    SelectionId, Session, Snapshot, State,
};

/// One bounded identity record.
#[derive(Clone, Copy, Debug)]
struct Record {
    /// Current connection generation.
    generation: u64,
    /// Whether this generation still has a live host binding.
    active: bool,
    /// Last admitted sequence.
    sequence: u64,
    /// Retained selection state.
    state: State,
    /// Retained uncertainty, including across disconnect and world changes.
    fault: Option<ExecutionFailure>,
}

/// Pure serialized state. Its caller must supply independently authenticated bindings.
#[derive(Debug)]
pub struct Building {
    /// Current host-declared world incarnation.
    epoch: u64,
    /// Bounded admission policy.
    limits: Limits,
    /// Bounded identity records.
    sessions: BTreeMap<u64, Record>,
}

impl Building {
    /// Start an empty world session. No native world calibration is implied.
    pub fn new(epoch: u64, limits: Limits) -> Result<Self, Error> {
        if epoch == 0 {
            return Err(Error::InvalidConfiguration);
        }
        Ok(Self {
            epoch,
            limits,
            sessions: BTreeMap::new(),
        })
    }

    /// Bind a trusted host connection identity and generation, never a payload entity.
    pub fn connect(&mut self, key: u64, generation: u64) -> Result<Session, Error> {
        if key == 0 || generation == 0 {
            return Err(Error::InvalidConfiguration);
        }
        if !self.sessions.contains_key(&key) && self.sessions.len() >= self.limits.max_sessions {
            return Err(Error::SessionLimit);
        }
        if let Some(record) = self.sessions.get(&key) {
            if generation <= record.generation {
                return Err(Error::StaleSession);
            }
            if record.active {
                return Err(Error::SessionActive);
            }
        }
        let fault = self.sessions.get(&key).and_then(|record| record.fault);
        self.sessions.insert(
            key,
            Record {
                generation,
                active: true,
                sequence: 0,
                state: State::Empty,
                fault,
            },
        );
        Ok(Session { key, generation })
    }

    /// Forget a departing caller's selection while retaining its generation tombstone.
    pub fn disconnect(&mut self, caller: Session) -> Result<(), Error> {
        let record = self.record_mut(caller)?;
        record.active = false;
        record.sequence = 0;
        record.state = State::Empty;
        Ok(())
    }

    /// Advance the trusted world's epoch and invalidate every live connection.
    ///
    /// Increasing epochs prevent accidental reuse of an earlier grid declaration.
    /// The host must authenticate fresh connection generations before resuming.
    pub fn change_world(&mut self, epoch: u64) -> Result<(), Error> {
        if epoch <= self.epoch {
            return Err(Error::StaleWorld);
        }
        self.epoch = epoch;
        for record in self.sessions.values_mut() {
            record.active = false;
            record.sequence = 0;
            record.state = State::Empty;
        }
        Ok(())
    }

    /// Read one caller's immutable pure state.
    pub fn snapshot(&self, caller: Session) -> Result<Snapshot, Error> {
        let record = self.record(caller)?;
        Ok(Snapshot {
            sequence: record.sequence,
            state: record.state,
            fault: record.fault,
        })
    }

    /// Admit one abstract intent using the trusted binding passed outside its payload.
    pub fn handle(
        &mut self,
        caller: Session,
        context: InputContext,
        command: Command,
    ) -> Result<CommandOutcome, Error> {
        self.record(caller)?;
        if context != InputContext::Building {
            return Ok(CommandOutcome::Ignored);
        }
        if self.record(caller)?.fault.is_some() {
            return Err(Error::NeedsReconciliation);
        }
        if command.epoch != self.epoch {
            return Err(Error::StaleWorld);
        }
        if let Action::Select(point) = command.action {
            self.validate_point(point)?;
        }
        let limits = self.limits;
        let record = self.record_mut(caller)?;
        let expected = record.sequence.checked_add(1).ok_or(Error::Sequence)?;
        if command.sequence != expected {
            return Err(Error::Sequence);
        }
        if command.sequence > limits.max_commands {
            return Err(Error::CommandLimit);
        }
        let next = match (record.state, command.action) {
            (_, Action::Clear | Action::Cancel) => State::Empty,
            (State::Empty, Action::Select(point)) => State::FirstCorner {
                selection: SelectionId {
                    owner: caller,
                    sequence: command.sequence,
                },
                point,
            },
            (State::FirstCorner { selection, point }, Action::Select(second)) => {
                State::Selected(BuildPlan {
                    selection,
                    area: Area::between(point, second, limits)?,
                })
            }
            (State::Selected(plan), Action::Confirm(selection)) if selection == plan.selection => {
                State::Confirmed(plan)
            }
            _ => return Err(Error::WrongState),
        };
        record.sequence = command.sequence;
        record.state = next;
        Ok(CommandOutcome::Accepted)
    }

    /// Consume the caller's confirmed operation through the trusted native boundary.
    ///
    /// No default native adapter is available. A plan is never authority to commit.
    /// A failed dry run consumes the operation without payment. Payment/commit
    /// failures latch a fault; clear, cancel, reconnect and world changes cannot
    /// make that identity eligible again. This slice has no reconciliation API.
    pub fn execute<A: NativeAdapter>(
        &mut self,
        caller: Session,
        selection: SelectionId,
        adapter: &mut A,
    ) -> Result<ExecutionReport, Error> {
        let record = self.record_mut(caller)?;
        if record.fault.is_some() {
            return Err(Error::NeedsReconciliation);
        }
        let State::Confirmed(plan) = record.state else {
            return Err(Error::WrongState);
        };
        if plan.selection != selection || selection.owner != caller {
            return Err(Error::WrongState);
        }
        // Consume the ticket and pessimistically block before any external callback.
        // If the host catches a callback panic, this fault prevents a silent retry.
        record.state = State::Empty;
        self.set_fault(
            caller,
            selection,
            ExecutionStage::DryRun,
            NativeError::OutcomeUnknown,
        )?;
        let prepared = match adapter.dry_run(&plan) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.record_mut(caller)?.fault = None;
                return Err(Error::Native(ExecutionFailure {
                    selection,
                    stage: ExecutionStage::DryRun,
                    error,
                }));
            }
        };
        self.set_fault(
            caller,
            selection,
            ExecutionStage::Payment,
            NativeError::OutcomeUnknown,
        )?;
        let paid = match adapter.pay_normal(prepared) {
            Ok(paid) => paid,
            Err(error) => {
                return Err(Error::Native(self.set_fault(
                    caller,
                    selection,
                    ExecutionStage::Payment,
                    error,
                )?))
            }
        };
        self.set_fault(
            caller,
            selection,
            ExecutionStage::Commit,
            NativeError::OutcomeUnknown,
        )?;
        let cells = match adapter.commit(paid) {
            Ok(cells) if cells == plan.area.cell_count() => cells,
            Ok(_) => {
                return Err(Error::Native(self.set_fault(
                    caller,
                    selection,
                    ExecutionStage::Commit,
                    NativeError::IncompleteCommit,
                )?))
            }
            Err(error) => {
                return Err(Error::Native(self.set_fault(
                    caller,
                    selection,
                    ExecutionStage::Commit,
                    error,
                )?))
            }
        };
        self.record_mut(caller)?.fault = None;
        Ok(ExecutionReport { selection, cells })
    }

    /// Persist one bounded uncertainty report before returning it to the host.
    fn set_fault(
        &mut self,
        caller: Session,
        selection: SelectionId,
        stage: ExecutionStage,
        error: NativeError,
    ) -> Result<ExecutionFailure, Error> {
        let failure = ExecutionFailure {
            selection,
            stage,
            error,
        };
        self.record_mut(caller)?.fault = Some(failure);
        Ok(failure)
    }

    /// Validate a declared point without assuming game units or an origin.
    fn validate_point(&self, point: GridPoint) -> Result<(), Error> {
        if point.epoch() != self.epoch {
            return Err(Error::StaleWorld);
        }
        if point
            .index()
            .into_iter()
            .any(|value| value.unsigned_abs() > self.limits.max_abs_index)
        {
            return Err(Error::PointOutOfBounds);
        }
        Ok(())
    }

    /// Find a current host binding.
    fn record(&self, caller: Session) -> Result<&Record, Error> {
        self.sessions
            .get(&caller.key)
            .filter(|record| record.active && record.generation == caller.generation)
            .ok_or(Error::StaleSession)
    }

    /// Find a current host binding for a serialized update.
    fn record_mut(&mut self, caller: Session) -> Result<&mut Record, Error> {
        self.sessions
            .get_mut(&caller.key)
            .filter(|record| record.active && record.generation == caller.generation)
            .ok_or(Error::StaleSession)
    }
}
