//! Pure command and selection values.
use crate::ExecutionFailure;

/// A host-owned connection binding. It is not evidence of game authentication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Session {
    /// Host-assigned connection identity, never a client-selected game entity.
    pub(crate) key: u64,
    /// Host-assigned increasing incarnation of that identity.
    pub(crate) generation: u64,
}

impl Session {
    /// Read the trusted host's session identity.
    pub fn key(self) -> u64 {
        self.key
    }

    /// Read the connection incarnation.
    pub fn generation(self) -> u64 {
        self.generation
    }
}

/// An integer lattice point tagged with a caller-supplied world epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GridPoint {
    /// Host world incarnation. It does not prove a calibrated native frame.
    epoch: u64,
    /// Discrete grid indices. These are not canonical metre coordinates.
    index: [i32; 3],
}

impl GridPoint {
    /// Declare grid indices. Admission validates their epoch and bounds.
    pub fn new(epoch: u64, index: [i32; 3]) -> Self {
        Self { epoch, index }
    }

    /// Read the declared world incarnation.
    pub fn epoch(self) -> u64 {
        self.epoch
    }

    /// Read the discrete indices without converting units or axes.
    pub fn index(self) -> [i32; 3] {
        self.index
    }
}

/// An opaque selection identity bound to its original caller and first command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectionId {
    /// Owning connection.
    pub(crate) owner: Session,
    /// Sequence of the first accepted corner.
    pub(crate) sequence: u64,
}

/// A bounded inclusive cuboid in one declared grid epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Area {
    /// Declared world incarnation.
    epoch: u64,
    /// Inclusive minimum indices.
    min: [i32; 3],
    /// Inclusive maximum indices.
    max: [i32; 3],
    /// Validated total number of cells.
    cell_count: u32,
}

impl Area {
    /// Check the complete inclusive cuboid before publishing it.
    pub(crate) fn between(
        first: GridPoint,
        second: GridPoint,
        limits: Limits,
    ) -> Result<Self, Error> {
        let [ax, ay, az] = first.index;
        let [bx, by, bz] = second.index;
        let spans = [ax.abs_diff(bx), ay.abs_diff(by), az.abs_diff(bz)];
        let cell_count = spans
            .into_iter()
            .try_fold(1_u32, |count, span| {
                span.checked_add(1).and_then(|size| count.checked_mul(size))
            })
            .ok_or(Error::AreaTooLarge)?;
        if cell_count > limits.max_cells {
            return Err(Error::AreaTooLarge);
        }
        Ok(Self {
            epoch: first.epoch,
            min: [ax.min(bx), ay.min(by), az.min(bz)],
            max: [ax.max(bx), ay.max(by), az.max(bz)],
            cell_count,
        })
    }

    /// Read the inclusive minimum indices.
    pub fn min(self) -> [i32; 3] {
        self.min
    }

    /// Read the inclusive maximum indices.
    pub fn max(self) -> [i32; 3] {
        self.max
    }

    /// Read the validated bounded cell count.
    pub fn cell_count(self) -> u32 {
        self.cell_count
    }

    /// Visit every cell once, with X changing fastest and then Y and Z.
    pub fn cells(self) -> impl Iterator<Item = GridPoint> {
        let [min_x, min_y, min_z] = self.min;
        let [max_x, max_y, max_z] = self.max;
        (min_z..=max_z).flat_map(move |z| {
            (min_y..=max_y).flat_map(move |y| {
                (min_x..=max_x).map(move |x| GridPoint::new(self.epoch, [x, y, z]))
            })
        })
    }
}

/// A read-only candidate. It never grants native execution permission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuildPlan {
    /// Owning caller and selection incarnation.
    pub(crate) selection: SelectionId,
    /// Fully bounded area.
    pub(crate) area: Area,
}

impl BuildPlan {
    /// Read the selection identity required by confirmation.
    pub fn selection(self) -> SelectionId {
        self.selection
    }

    /// Read the host connection binding, not a native player entity.
    pub fn owner(self) -> Session {
        self.selection.owner
    }

    /// Read the bounded abstract area.
    pub fn area(self) -> Area {
        self.area
    }
}

/// Abstract host input context. No key or gamepad binding is implied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputContext {
    /// The host reports that area-building owns input.
    Building,
    /// A menu owns input.
    Menu,
    /// The client has no game focus.
    Unfocused,
    /// Another native control or mod owns the action.
    ConflictingControl,
}

/// A client intent without an actor or native entity field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    /// Supply the next corner in the declared grid.
    Select(GridPoint),
    /// Confirm exactly the caller's current complete selection.
    Confirm(SelectionId),
    /// Abandon a selection through the control layer's cancel action.
    Cancel,
    /// Explicitly clear a selection.
    Clear,
}

/// One ordered intent. The authenticated binding is passed separately by the host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Command {
    /// Sequence in the current connection generation.
    pub(crate) sequence: u64,
    /// Declared world incarnation.
    pub(crate) epoch: u64,
    /// Intent without native identity or payment information.
    pub(crate) action: Action,
}

impl Command {
    /// Declare an intent. Admission validates ordering, epoch and action.
    pub fn new(sequence: u64, epoch: u64, action: Action) -> Self {
        Self {
            sequence,
            epoch,
            action,
        }
    }
}

/// Admission acknowledgment for a future transport or input adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOutcome {
    /// The host context blocked input. State and sequence remain unchanged.
    Ignored,
    /// The sequence and state transition were accepted together.
    Accepted,
}

/// One caller's current abstract tool state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    /// No retained selection.
    Empty,
    /// One retained corner.
    FirstCorner {
        /// Opaque selection identity.
        selection: SelectionId,
        /// First declared point.
        point: GridPoint,
    },
    /// Both corners define a bounded candidate.
    Selected(BuildPlan),
    /// A confirmed candidate awaits the separate native boundary.
    Confirmed(BuildPlan),
}

/// An immutable caller state snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Snapshot {
    /// Last admitted command sequence.
    pub sequence: u64,
    /// Current selection state.
    pub state: State,
    /// A payment/commit uncertainty that requires native reconciliation.
    pub fault: Option<ExecutionFailure>,
}

/// Hard-bounded resource policy. These limits do not establish game permissions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Maximum tracked identities, including disconnected identities.
    pub(crate) max_sessions: usize,
    /// Maximum inclusive cells in one candidate.
    pub(crate) max_cells: u32,
    /// Maximum accepted commands in one connection generation.
    pub(crate) max_commands: u64,
    /// Maximum absolute coordinate index.
    pub(crate) max_abs_index: u32,
}

impl Limits {
    /// Set positive bounds below fixed caps: 64 identities, 4096 cells,
    /// 1,000,000 commands per generation and 1,000,000 absolute grid indices.
    pub fn new(
        max_sessions: usize,
        max_cells: u32,
        max_commands: u64,
        max_abs_index: u32,
    ) -> Result<Self, Error> {
        if !(1..=64).contains(&max_sessions)
            || !(1..=4096).contains(&max_cells)
            || !(1..=1_000_000).contains(&max_commands)
            || !(1..=1_000_000).contains(&max_abs_index)
        {
            return Err(Error::InvalidConfiguration);
        }
        Ok(Self {
            max_sessions,
            max_cells,
            max_commands,
            max_abs_index,
        })
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_sessions: 16,
            max_cells: 1024,
            max_commands: 4096,
            max_abs_index: 1_000_000,
        }
    }
}

/// Rejected pure state transitions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// An identity, generation, world epoch or policy is invalid.
    InvalidConfiguration,
    /// The bounded identity table is full.
    SessionLimit,
    /// A live binding must disconnect before its next generation is attached.
    SessionActive,
    /// The host binding is unknown or stale.
    StaleSession,
    /// The command or point belongs to a different world incarnation.
    StaleWorld,
    /// A declared grid index is outside the configured bound.
    PointOutOfBounds,
    /// The complete cuboid exceeds the cell bound or arithmetic range.
    AreaTooLarge,
    /// The action cannot follow the current state or selection identity.
    WrongState,
    /// The sequence is replayed, skipped, zero or exhausted.
    Sequence,
    /// The current generation has used its command budget.
    CommandLimit,
    /// Payment or commit uncertainty blocks all future work for this identity.
    NeedsReconciliation,
    /// The native boundary stopped. Later stages were not attempted.
    Native(ExecutionFailure),
}
