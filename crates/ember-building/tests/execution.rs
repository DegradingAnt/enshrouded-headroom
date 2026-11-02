//! Contract tests at the unavailable native system boundary. Mocks prove no game behavior.
use ember_building::{
    Action, BuildPlan, Building, Command, GridPoint, InputContext, Limits, NativeAdapter,
    NativeError, SelectionId, Session, State,
};

/// Select a known four-cell fixture through the public command interface.
fn selected(building: &mut Building, caller: Session) -> SelectionId {
    for (sequence, index) in [(1, [0, 0, 0]), (2, [1, 0, 1])] {
        building
            .handle(
                caller,
                InputContext::Building,
                Command::new(sequence, 7, Action::Select(GridPoint::new(7, index))),
            )
            .unwrap();
    }
    let State::Selected(plan) = building.snapshot(caller).unwrap().state else {
        panic!("missing selection")
    };
    plan.selection()
}

/// Native-boundary fixture. Its balances and receipts are synthetic test data.
#[derive(Default)]
struct MockNative {
    /// Ordered callbacks observed at the external boundary.
    calls: Vec<&'static str>,
    /// Synthetic inventory remaining.
    balance: u32,
    /// Synthetic cells applied.
    committed: u32,
    /// Inject a nonmutating dry-run rejection.
    dry_run_error: Option<NativeError>,
    /// Inject a potentially partial payment failure.
    payment_error: Option<NativeError>,
    /// Synthetic debit before an injected payment failure.
    partial_debit: u32,
    /// Inject a failure after one synthetic committed cell.
    commit_error: Option<NativeError>,
    /// Replace the adapter's complete-success count.
    reported_cells: Option<u32>,
    /// Owners received by native dry-run preparation.
    owners: Vec<Session>,
    /// Inject a callback unwind at this external stage.
    panic_at: Option<ember_building::ExecutionStage>,
}

/// A mock-only prepared result.
struct DryRun {
    /// Complete synthetic area size preserved through ordering.
    cells: u32,
}

/// A mock-only paid result.
struct Paid {
    /// Complete synthetic area size preserved through ordering.
    cells: u32,
}

impl NativeAdapter for MockNative {
    type DryRun = DryRun;
    type Payment = Paid;

    fn dry_run(&mut self, plan: &BuildPlan) -> Result<DryRun, NativeError> {
        self.calls.push("dry_run");
        if self.panic_at == Some(ember_building::ExecutionStage::DryRun) {
            panic!("mock dry-run unwind");
        }
        self.owners.push(plan.owner());
        if let Some(error) = self.dry_run_error {
            return Err(error);
        }
        Ok(DryRun {
            cells: plan.area().cell_count(),
        })
    }

    fn pay_normal(&mut self, prepared: DryRun) -> Result<Paid, NativeError> {
        self.calls.push("normal_payment");
        if self.panic_at == Some(ember_building::ExecutionStage::Payment) {
            panic!("mock payment unwind");
        }
        if let Some(error) = self.payment_error {
            self.balance -= self.partial_debit;
            return Err(error);
        }
        let cost = prepared.cells * 2;
        if self.balance < cost {
            return Err(NativeError::CostRejected);
        }
        self.balance -= cost;
        Ok(Paid {
            cells: prepared.cells,
        })
    }

    fn commit(&mut self, paid: Paid) -> Result<u32, NativeError> {
        self.calls.push("commit");
        if self.panic_at == Some(ember_building::ExecutionStage::Commit) {
            panic!("mock commit unwind");
        }
        if let Some(error) = self.commit_error {
            self.committed += 1;
            return Err(error);
        }
        if let Some(cells) = self.reported_cells {
            self.committed += cells;
            return Ok(cells);
        }
        self.committed += paid.cells;
        Ok(paid.cells)
    }
}

/// Verify only a confirmed owner executes once after dry run and normal payment.
#[test]
fn only_a_confirmed_owner_executes_once_after_dry_run_and_normal_payment() {
    let mut building = Building::new(7, Limits::default()).unwrap();
    let owner = building.connect(101, 1).unwrap();
    let other = building.connect(202, 1).unwrap();
    let selection = selected(&mut building, owner);
    let mut native = MockNative {
        balance: 100,
        ..Default::default()
    };
    assert!(building.execute(owner, selection, &mut native).is_err());
    assert!(building.execute(other, selection, &mut native).is_err());
    assert!(native.calls.is_empty());
    building
        .handle(
            owner,
            InputContext::Building,
            Command::new(3, 7, Action::Confirm(selection)),
        )
        .unwrap();
    let report = building.execute(owner, selection, &mut native).unwrap();
    assert_eq!(report.cells, 4);
    assert_eq!(native.owners, vec![owner]);
    assert_eq!(native.balance, 92);
    assert_eq!(native.committed, 4);
    assert_eq!(native.calls, vec!["dry_run", "normal_payment", "commit"]);
    assert_eq!(building.snapshot(owner).unwrap().state, State::Empty);
    assert!(building.execute(owner, selection, &mut native).is_err());
    assert!(building
        .handle(
            owner,
            InputContext::Building,
            Command::new(3, 7, Action::Confirm(selection))
        )
        .is_err());
    assert_eq!(native.owners, vec![owner]);
    assert_eq!(native.balance, 92);
    assert_eq!(native.committed, 4);
}

/// Verify the only shipped adapter fails closed without native authority.
#[test]
fn the_only_shipped_adapter_fails_closed_without_native_authority() {
    use ember_building::{Error, ExecutionStage, NativeProof, UnavailableAdapter};
    let mut building = Building::new(7, Limits::default()).unwrap();
    let caller = building.connect(101, 1).unwrap();
    let selection = selected(&mut building, caller);
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(3, 7, Action::Confirm(selection)),
        )
        .unwrap();
    let result = building.execute(caller, selection, &mut UnavailableAdapter);
    assert!(
        matches!(result, Err(Error::Native(failure)) if failure.stage == ExecutionStage::DryRun && failure.error == NativeError::MissingProof(NativeProof::Authority))
    );
    assert_eq!(building.snapshot(caller).unwrap().state, State::Empty);
    assert!(building.snapshot(caller).unwrap().fault.is_none());
}

/// Verify missing native prerequisites and permission denial stop before any payment.
#[test]
fn missing_native_prerequisites_and_permission_denial_stop_before_any_payment() {
    use ember_building::{Error, ExecutionStage, NativeProof};
    for rejection in [
        NativeError::MissingProof(NativeProof::Authority),
        NativeError::MissingProof(NativeProof::World),
        NativeError::MissingProof(NativeProof::GameThread),
        NativeError::MissingProof(NativeProof::Permission),
        NativeError::MissingProof(NativeProof::DryRun),
        NativeError::PermissionDenied,
        NativeError::DryRunRejected,
    ] {
        let mut building = Building::new(7, Limits::default()).unwrap();
        let caller = building.connect(101, 1).unwrap();
        let selection = selected(&mut building, caller);
        building
            .handle(
                caller,
                InputContext::Building,
                Command::new(3, 7, Action::Confirm(selection)),
            )
            .unwrap();
        let mut native = MockNative {
            balance: 100,
            dry_run_error: Some(rejection),
            ..Default::default()
        };
        assert!(
            matches!(building.execute(caller, selection, &mut native), Err(Error::Native(failure)) if failure.stage == ExecutionStage::DryRun && failure.error == rejection)
        );
        assert_eq!(native.calls, vec!["dry_run"]);
        assert_eq!(native.balance, 100);
        assert_eq!(native.committed, 0);
        assert!(building.execute(caller, selection, &mut native).is_err());
        assert_eq!(native.calls, vec!["dry_run"]);
    }
}

/// Verify normal cost refusal never commits and cannot be cleared into a retry.
#[test]
fn normal_cost_refusal_never_commits_and_cannot_be_cleared_into_a_retry() {
    use ember_building::{Error, ExecutionStage};
    let mut building = Building::new(7, Limits::default()).unwrap();
    let caller = building.connect(101, 1).unwrap();
    let selection = selected(&mut building, caller);
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(3, 7, Action::Confirm(selection)),
        )
        .unwrap();
    let mut native = MockNative {
        balance: 7,
        ..Default::default()
    };
    assert!(
        matches!(building.execute(caller, selection, &mut native), Err(Error::Native(failure)) if failure.stage == ExecutionStage::Payment && failure.error == NativeError::CostRejected)
    );
    assert_eq!(native.balance, 7);
    assert_eq!(native.committed, 0);
    assert_eq!(native.calls, vec!["dry_run", "normal_payment"]);
    for action in [
        Action::Clear,
        Action::Cancel,
        Action::Confirm(selection),
        Action::Select(GridPoint::new(7, [1, 0, 0])),
    ] {
        assert_eq!(
            building.handle(caller, InputContext::Building, Command::new(4, 7, action)),
            Err(Error::NeedsReconciliation)
        );
    }
    assert!(building.execute(caller, selection, &mut native).is_err());
    assert_eq!(native.calls, vec!["dry_run", "normal_payment"]);
}

/// Verify partial payment or commit and missing proof latch through reconnect and world change.
#[test]
fn partial_payment_or_commit_and_missing_proof_latch_through_reconnect_and_world_change() {
    use ember_building::{Error, ExecutionStage, NativeProof};
    for (stage, error, debit) in [
        (ExecutionStage::Payment, NativeError::OutcomeUnknown, 3),
        (
            ExecutionStage::Payment,
            NativeError::MissingProof(NativeProof::NormalPayment),
            0,
        ),
        (ExecutionStage::Commit, NativeError::OutcomeUnknown, 0),
        (
            ExecutionStage::Commit,
            NativeError::MissingProof(NativeProof::Commit),
            0,
        ),
    ] {
        let mut building = Building::new(7, Limits::default()).unwrap();
        let caller = building.connect(101, 1).unwrap();
        let other = building.connect(202, 1).unwrap();
        let selection = selected(&mut building, caller);
        building
            .handle(
                caller,
                InputContext::Building,
                Command::new(3, 7, Action::Confirm(selection)),
            )
            .unwrap();
        let mut native = MockNative {
            balance: 100,
            ..Default::default()
        };
        if stage == ExecutionStage::Payment {
            native.payment_error = Some(error);
            native.partial_debit = debit;
        } else {
            native.commit_error = Some(error);
        }
        assert!(
            matches!(building.execute(caller, selection, &mut native), Err(Error::Native(failure)) if failure.stage == stage && failure.error == error)
        );
        let fault = building.snapshot(caller).unwrap().fault.unwrap();
        let calls = native.calls.clone();
        if stage == ExecutionStage::Payment {
            assert_eq!(native.balance, 100 - debit);
            assert_eq!(native.committed, 0);
        } else {
            assert_eq!(native.balance, 92);
            assert_eq!(native.committed, 1);
        }
        // An unrelated player's selection remains usable.
        building
            .handle(
                other,
                InputContext::Building,
                Command::new(1, 7, Action::Select(GridPoint::new(7, [3, 0, 0]))),
            )
            .unwrap();
        assert!(building.execute(caller, selection, &mut native).is_err());
        building.disconnect(caller).unwrap();
        let fresh = building.connect(101, 2).unwrap();
        assert_eq!(building.snapshot(fresh).unwrap().state, State::Empty);
        assert_eq!(building.snapshot(fresh).unwrap().fault, Some(fault));
        assert_eq!(
            building.handle(
                fresh,
                InputContext::Building,
                Command::new(1, 7, Action::Clear)
            ),
            Err(Error::NeedsReconciliation)
        );
        building.change_world(8).unwrap();
        let next_world = building.connect(101, 3).unwrap();
        assert_eq!(building.snapshot(next_world).unwrap().fault, Some(fault));
        assert!(building
            .execute(next_world, selection, &mut native)
            .is_err());
        assert_eq!(native.calls, calls);
    }
}

/// Verify an incomplete success report is an uncertain commit and cannot be replayed.
#[test]
fn an_incomplete_success_report_is_an_uncertain_commit_and_cannot_be_replayed() {
    use ember_building::{Error, ExecutionStage};
    for reported in [0, 2, 5] {
        let mut building = Building::new(7, Limits::default()).unwrap();
        let caller = building.connect(101, 1).unwrap();
        let selection = selected(&mut building, caller);
        building
            .handle(
                caller,
                InputContext::Building,
                Command::new(3, 7, Action::Confirm(selection)),
            )
            .unwrap();
        let mut native = MockNative {
            balance: 100,
            reported_cells: Some(reported),
            ..Default::default()
        };
        assert!(
            matches!(building.execute(caller, selection, &mut native), Err(Error::Native(failure)) if failure.stage == ExecutionStage::Commit && failure.error == NativeError::IncompleteCommit)
        );
        assert!(building.snapshot(caller).unwrap().fault.is_some());
        assert!(building.execute(caller, selection, &mut native).is_err());
        assert_eq!(native.balance, 92);
        assert_eq!(native.committed, reported);
        assert_eq!(native.calls, vec!["dry_run", "normal_payment", "commit"]);
    }
}

/// Verify cancel clear disconnect and wrong confirmed actor never reach native callbacks.
#[test]
fn cancel_clear_disconnect_and_wrong_confirmed_actor_never_reach_native_callbacks() {
    for invalidate in [Action::Cancel, Action::Clear] {
        let mut building = Building::new(7, Limits::default()).unwrap();
        let caller = building.connect(101, 1).unwrap();
        let other = building.connect(202, 1).unwrap();
        let selection = selected(&mut building, caller);
        let other_selection = selected(&mut building, other);
        for (actor, own) in [(caller, selection), (other, other_selection)] {
            building
                .handle(
                    actor,
                    InputContext::Building,
                    Command::new(3, 7, Action::Confirm(own)),
                )
                .unwrap();
        }
        let mut native = MockNative {
            balance: 100,
            ..Default::default()
        };
        assert!(building.execute(other, selection, &mut native).is_err());
        assert!(building
            .execute(caller, other_selection, &mut native)
            .is_err());
        building
            .handle(
                caller,
                InputContext::Building,
                Command::new(4, 7, invalidate),
            )
            .unwrap();
        assert!(building.execute(caller, selection, &mut native).is_err());
        building.disconnect(other).unwrap();
        let fresh = building.connect(202, 2).unwrap();
        assert!(building
            .execute(fresh, other_selection, &mut native)
            .is_err());
        assert!(building
            .execute(other, other_selection, &mut native)
            .is_err());
        assert!(native.calls.is_empty());
        assert_eq!(native.balance, 100);
    }
}

/// Verify a callback unwind leaves the operation consumed and blocked.
#[test]
fn a_callback_unwind_leaves_the_operation_consumed_and_blocked() {
    use ember_building::{Error, ExecutionStage};
    for stage in [
        ExecutionStage::DryRun,
        ExecutionStage::Payment,
        ExecutionStage::Commit,
    ] {
        let mut building = Building::new(7, Limits::default()).unwrap();
        let caller = building.connect(101, 1).unwrap();
        let selection = selected(&mut building, caller);
        building
            .handle(
                caller,
                InputContext::Building,
                Command::new(3, 7, Action::Confirm(selection)),
            )
            .unwrap();
        let mut native = MockNative {
            balance: 100,
            panic_at: Some(stage),
            ..Default::default()
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            building.execute(caller, selection, &mut native)
        }));
        assert!(result.is_err());
        let snapshot = building.snapshot(caller).unwrap();
        assert_eq!(snapshot.state, State::Empty);
        assert_eq!(snapshot.fault.unwrap().stage, stage);
        let calls = native.calls.clone();
        assert_eq!(
            building.execute(caller, selection, &mut native),
            Err(Error::NeedsReconciliation)
        );
        assert_eq!(native.calls, calls);
    }
}
