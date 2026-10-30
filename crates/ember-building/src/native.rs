//! Abstract ordered native boundary. No working game adapter is provided.
use crate::{BuildPlan, SelectionId};

/// A native fact that the host adapter must establish using actual game receipts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeProof {
    /// The authenticated connection maps to the authoritative native player.
    Authority,
    /// The declared grid and epoch match the live native world and placement record.
    World,
    /// Execution is on the original authoritative game thread.
    GameThread,
    /// Native inventory and base permissions are preserved for every cell.
    Permission,
    /// The complete area passed the native dry run without mutation.
    DryRun,
    /// The exact native normal costs were paid once.
    NormalPayment,
    /// The native commit outcome is known for the complete area.
    Commit,
}

/// A native adapter's failure report. This enum does not itself prove any game fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeError {
    /// The adapter lacks a required native receipt and must stop.
    MissingProof(NativeProof),
    /// Native base or inventory permissions rejected the dry run.
    PermissionDenied,
    /// The native dry run rejected at least one cell.
    DryRunRejected,
    /// Native normal payment was refused. Its outcome must be reconciled.
    CostRejected,
    /// A native callback failed or its mutation outcome is unknown.
    OutcomeUnknown,
    /// The reported commit count differs from the complete candidate.
    IncompleteCommit,
}

/// The stage at which the native pipeline stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionStage {
    /// The authoritative, permission-preserving dry run.
    DryRun,
    /// Normal native payment, after the complete dry run.
    Payment,
    /// Native commit, after successful payment.
    Commit,
}

/// A bounded terminal report for one consumed operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionFailure {
    /// The consumed selection identity.
    pub selection: SelectionId,
    /// The last attempted stage.
    pub stage: ExecutionStage,
    /// The adapter's failure or missing native evidence.
    pub error: NativeError,
}

/// The adapter's report of complete success. It is not independent native proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionReport {
    /// The consumed selection identity.
    pub selection: SelectionId,
    /// Reported committed cells, checked against the complete plan.
    pub cells: u32,
}

/// A trusted native system boundary, with no usable implementation in this crate.
///
/// Implementing this trait does not authenticate a caller or establish native proof.
/// The future adapter must independently verify the native actor, world/grid binding,
/// original game thread, authoritative transport, inventory and base permissions.
/// It must preserve the game's dry-run/payment/commit and undo contracts for the
/// complete area. Associated values must retain that exact actor, world, area and
/// native placement/cost record through all stages. Revalidate any native context
/// that can change between stages. Never accept a client-selected entity or cost.
///
/// This interface only tests ordering and fail-closed state transitions with mocks.
/// Do not connect it to native mutations before the missing runtime receipts exist.
/// Implementations must not panic or mutate during a failed dry run. A payment or
/// commit error may have changed the world or inventory; the core latches a fault.
/// The host must wrap future native callbacks in emberkit's panic guard.
///
/// Tokens are consumed in order. The core never constructs them, alters costs,
/// invokes a direct commit endpoint, retries payment or invents a refund/undo.
pub trait NativeAdapter {
    /// Adapter-owned complete dry-run result and native normal cost record.
    type DryRun;
    /// Adapter-owned successfully paid native placement record.
    type Payment;

    /// Verify all prerequisites and dry-run every cell without mutation.
    /// Missing authority, world, game-thread or permission proof must reject here.
    fn dry_run(&mut self, plan: &BuildPlan) -> Result<Self::DryRun, NativeError>;

    /// Preserve normal native payment for the complete prepared record exactly once.
    /// Missing payment proof must reject. Partial debit is an uncertain failure.
    fn pay_normal(&mut self, prepared: Self::DryRun) -> Result<Self::Payment, NativeError>;

    /// Commit only the exact paid native record and report its complete cell count.
    /// Missing commit proof or partial application must return an error.
    fn commit(&mut self, paid: Self::Payment) -> Result<u32, NativeError>;
}

/// The only shipped adapter. It always rejects before native work or payment.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableAdapter;

impl NativeAdapter for UnavailableAdapter {
    type DryRun = std::convert::Infallible;
    type Payment = std::convert::Infallible;

    fn dry_run(&mut self, _plan: &BuildPlan) -> Result<Self::DryRun, NativeError> {
        Err(NativeError::MissingProof(NativeProof::Authority))
    }

    fn pay_normal(&mut self, prepared: Self::DryRun) -> Result<Self::Payment, NativeError> {
        match prepared {}
    }

    fn commit(&mut self, paid: Self::Payment) -> Result<u32, NativeError> {
        match paid {}
    }
}
