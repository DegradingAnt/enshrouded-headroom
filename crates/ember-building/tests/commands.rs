//! Public per-session command behavior.
use ember_building::{Action, Building, Command, GridPoint, InputContext, Limits, State};

/// Verify two players select independent areas and cannot confirm each others selection.
#[test]
fn two_players_select_independent_areas_and_cannot_confirm_each_others_selection() {
    let mut building = Building::new(7, Limits::default()).unwrap();
    let alice = building.connect(101, 1).unwrap();
    let bob = building.connect(202, 1).unwrap();
    let input = InputContext::Building;
    building
        .handle(
            alice,
            input,
            Command::new(1, 7, Action::Select(GridPoint::new(7, [1, 2, 3]))),
        )
        .unwrap();
    building
        .handle(
            bob,
            input,
            Command::new(1, 7, Action::Select(GridPoint::new(7, [8, 4, 9]))),
        )
        .unwrap();
    building
        .handle(
            alice,
            input,
            Command::new(2, 7, Action::Select(GridPoint::new(7, [2, 2, 4]))),
        )
        .unwrap();
    let State::Selected(plan) = building.snapshot(alice).unwrap().state else {
        panic!("area was not selected")
    };
    assert_eq!(plan.area().cell_count(), 4);
    assert_eq!(
        plan.area()
            .cells()
            .map(|cell| cell.index())
            .collect::<Vec<_>>(),
        vec![[1, 2, 3], [2, 2, 3], [1, 2, 4], [2, 2, 4]]
    );
    assert!(building
        .handle(
            bob,
            input,
            Command::new(2, 7, Action::Confirm(plan.selection()))
        )
        .is_err());
    assert!(
        matches!(building.snapshot(bob).unwrap().state, State::FirstCorner { point, .. } if point.index() == [8,4,9])
    );
    building
        .handle(
            alice,
            input,
            Command::new(3, 7, Action::Confirm(plan.selection())),
        )
        .unwrap();
    assert!(matches!(
        building.snapshot(alice).unwrap().state,
        State::Confirmed(_)
    ));
}

/// Verify replay and sequence gaps do not mutate selection.
#[test]
fn replay_and_sequence_gaps_do_not_mutate_selection() {
    let mut building = Building::new(7, Limits::default()).unwrap();
    let caller = building.connect(101, 1).unwrap();
    let first = Command::new(1, 7, Action::Select(GridPoint::new(7, [0, 0, 0])));
    building
        .handle(caller, InputContext::Building, first)
        .unwrap();
    let before = building.snapshot(caller).unwrap();
    assert!(building
        .handle(caller, InputContext::Building, first)
        .is_err());
    assert_eq!(building.snapshot(caller).unwrap(), before);
    assert!(building
        .handle(
            caller,
            InputContext::Building,
            Command::new(3, 7, Action::Select(GridPoint::new(7, [1, 0, 0])))
        )
        .is_err());
    assert_eq!(building.snapshot(caller).unwrap(), before);
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(2, 7, Action::Select(GridPoint::new(7, [1, 0, 0]))),
        )
        .unwrap();
    assert!(matches!(
        building.snapshot(caller).unwrap().state,
        State::Selected(_)
    ));
}

/// Verify disconnect and reconnect reject old generations and clear only the departing player.
#[test]
fn disconnect_and_reconnect_reject_old_generations_and_clear_only_the_departing_player() {
    let mut building = Building::new(7, Limits::default()).unwrap();
    let old = building.connect(101, 4).unwrap();
    let other = building.connect(202, 1).unwrap();
    for caller in [old, other] {
        building
            .handle(
                caller,
                InputContext::Building,
                Command::new(1, 7, Action::Select(GridPoint::new(7, [3, 2, 1]))),
            )
            .unwrap();
    }
    let other_before = building.snapshot(other).unwrap();
    assert!(building.connect(101, 5).is_err());
    building.disconnect(old).unwrap();
    assert!(building.snapshot(old).is_err());
    assert!(building.connect(101, 4).is_err());
    assert!(building.connect(101, 3).is_err());
    let fresh = building.connect(101, 5).unwrap();
    assert_eq!(building.snapshot(fresh).unwrap().state, State::Empty);
    assert_eq!(building.snapshot(fresh).unwrap().sequence, 0);
    assert!(building
        .handle(
            old,
            InputContext::Building,
            Command::new(2, 7, Action::Select(GridPoint::new(7, [4, 2, 1])))
        )
        .is_err());
    assert!(building.disconnect(old).is_err());
    assert_eq!(building.snapshot(other).unwrap(), other_before);
}

/// Verify context conflicts do not consume input and cancel or clear invalidate confirmed work.
#[test]
fn context_conflicts_do_not_consume_input_and_cancel_or_clear_invalidate_confirmed_work() {
    let mut building = Building::new(7, Limits::default()).unwrap();
    let caller = building.connect(101, 1).unwrap();
    let select = Command::new(1, 7, Action::Select(GridPoint::new(7, [0, 0, 0])));
    for context in [
        InputContext::Menu,
        InputContext::Unfocused,
        InputContext::ConflictingControl,
    ] {
        let before = building.snapshot(caller).unwrap();
        building.handle(caller, context, select).unwrap();
        assert_eq!(building.snapshot(caller).unwrap(), before);
    }
    building
        .handle(caller, InputContext::Building, select)
        .unwrap();
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(2, 7, Action::Select(GridPoint::new(7, [0, 0, 0]))),
        )
        .unwrap();
    let State::Selected(plan) = building.snapshot(caller).unwrap().state else {
        panic!("missing selection")
    };
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(3, 7, Action::Confirm(plan.selection())),
        )
        .unwrap();
    let confirmed = building.snapshot(caller).unwrap();
    for context in [
        InputContext::Menu,
        InputContext::Unfocused,
        InputContext::ConflictingControl,
    ] {
        for action in [
            Action::Select(GridPoint::new(7, [5, 0, 0])),
            Action::Confirm(plan.selection()),
            Action::Cancel,
            Action::Clear,
        ] {
            building
                .handle(caller, context, Command::new(4, 7, action))
                .unwrap();
            assert_eq!(building.snapshot(caller).unwrap(), confirmed);
        }
    }
    building
        .handle(
            caller,
            InputContext::Menu,
            Command::new(4, 7, Action::Cancel),
        )
        .unwrap();
    assert_eq!(building.snapshot(caller).unwrap(), confirmed);
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(4, 7, Action::Cancel),
        )
        .unwrap();
    assert_eq!(building.snapshot(caller).unwrap().state, State::Empty);
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(5, 7, Action::Select(GridPoint::new(7, [1, 1, 1]))),
        )
        .unwrap();
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(6, 7, Action::Clear),
        )
        .unwrap();
    assert_eq!(building.snapshot(caller).unwrap().state, State::Empty);
}

/// Verify world epoch changes invalidate all connections without reusing old corner state.
#[test]
fn world_epoch_changes_invalidate_all_connections_without_reusing_old_corner_state() {
    let mut building = Building::new(7, Limits::default()).unwrap();
    let caller = building.connect(101, 1).unwrap();
    for (sequence, command_epoch, point_epoch) in [(1, 6, 7), (1, 7, 6)] {
        assert!(building
            .handle(
                caller,
                InputContext::Building,
                Command::new(
                    sequence,
                    command_epoch,
                    Action::Select(GridPoint::new(point_epoch, [0, 0, 0]))
                )
            )
            .is_err());
        assert_eq!(building.snapshot(caller).unwrap().sequence, 0);
    }
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(1, 7, Action::Select(GridPoint::new(7, [0, 0, 0]))),
        )
        .unwrap();
    let before = building.snapshot(caller).unwrap();
    assert!(building.change_world(7).is_err());
    assert!(building.change_world(6).is_err());
    assert_eq!(building.snapshot(caller).unwrap(), before);
    building.change_world(8).unwrap();
    assert!(building.snapshot(caller).is_err());
    assert!(building.connect(101, 1).is_err());
    let fresh = building.connect(101, 2).unwrap();
    assert_eq!(building.snapshot(fresh).unwrap().state, State::Empty);
    assert!(building
        .handle(
            fresh,
            InputContext::Building,
            Command::new(1, 7, Action::Select(GridPoint::new(7, [0, 0, 0])))
        )
        .is_err());
    building
        .handle(
            fresh,
            InputContext::Building,
            Command::new(1, 8, Action::Select(GridPoint::new(8, [1, 0, 0]))),
        )
        .unwrap();
}

/// Verify identity cell coordinate and command budgets are hard bounded.
#[test]
fn identity_cell_coordinate_and_command_budgets_are_hard_bounded() {
    assert!(Limits::new(0, 1, 1, 1).is_err());
    assert!(Limits::new(65, 1, 1, 1).is_err());
    assert!(Limits::new(1, 4097, 1, 1).is_err());
    assert!(Limits::new(1, 1, 1_000_001, 1).is_err());
    assert!(Limits::new(1, 1, 1, 1_000_001).is_err());
    let limits = Limits::new(2, 4, 4, 10).unwrap();
    let mut building = Building::new(7, limits).unwrap();
    let caller = building.connect(101, 1).unwrap();
    let other = building.connect(202, 1).unwrap();
    assert!(building.connect(303, 1).is_err());
    building.disconnect(other).unwrap();
    assert!(building.connect(303, 1).is_err());
    for index in [[11, 0, 0], [0, i32::MIN, 0], [0, 0, i32::MAX]] {
        assert!(building
            .handle(
                caller,
                InputContext::Building,
                Command::new(1, 7, Action::Select(GridPoint::new(7, index)))
            )
            .is_err());
        assert_eq!(building.snapshot(caller).unwrap().sequence, 0);
    }
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(1, 7, Action::Select(GridPoint::new(7, [-1, 1, 0]))),
        )
        .unwrap();
    let before = building.snapshot(caller).unwrap();
    assert!(building
        .handle(
            caller,
            InputContext::Building,
            Command::new(2, 7, Action::Select(GridPoint::new(7, [10, 10, 10])))
        )
        .is_err());
    assert_eq!(building.snapshot(caller).unwrap(), before);
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(2, 7, Action::Select(GridPoint::new(7, [0, 1, 1]))),
        )
        .unwrap();
    let State::Selected(plan) = building.snapshot(caller).unwrap().state else {
        panic!("missing bounded area")
    };
    assert_eq!(plan.area().cell_count(), 4);
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(3, 7, Action::Clear),
        )
        .unwrap();
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(4, 7, Action::Cancel),
        )
        .unwrap();
    let exhausted = building.snapshot(caller).unwrap();
    assert!(building
        .handle(
            caller,
            InputContext::Building,
            Command::new(5, 7, Action::Clear)
        )
        .is_err());
    assert_eq!(building.snapshot(caller).unwrap(), exhausted);
}

/// Verify ignored context is distinguishable from an accepted transport sequence.
#[test]
fn ignored_context_is_distinguishable_from_an_accepted_transport_sequence() {
    use ember_building::CommandOutcome;
    let mut building = Building::new(7, Limits::default()).unwrap();
    let caller = building.connect(101, 1).unwrap();
    let select = Command::new(1, 7, Action::Select(GridPoint::new(7, [0, 0, 0])));
    assert_eq!(
        building.handle(caller, InputContext::Menu, select),
        Ok(CommandOutcome::Ignored)
    );
    assert_eq!(
        building.handle(caller, InputContext::Building, select),
        Ok(CommandOutcome::Accepted)
    );
}

/// Verify reversed corners and overflowing cuboids preserve the bounded grid contract.
#[test]
fn reversed_corners_and_overflowing_cuboids_preserve_the_bounded_grid_contract() {
    let mut building = Building::new(7, Limits::default()).unwrap();
    let caller = building.connect(101, 1).unwrap();
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(1, 7, Action::Select(GridPoint::new(7, [2, 2, 4]))),
        )
        .unwrap();
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(2, 7, Action::Select(GridPoint::new(7, [1, 2, 3]))),
        )
        .unwrap();
    let State::Selected(plan) = building.snapshot(caller).unwrap().state else {
        panic!("missing reversed area")
    };
    assert_eq!(plan.area().min(), [1, 2, 3]);
    assert_eq!(plan.area().max(), [2, 2, 4]);
    assert_eq!(
        plan.area()
            .cells()
            .map(|point| (point.epoch(), point.index()))
            .collect::<Vec<_>>(),
        vec![
            (7, [1, 2, 3]),
            (7, [2, 2, 3]),
            (7, [1, 2, 4]),
            (7, [2, 2, 4])
        ]
    );
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(3, 7, Action::Clear),
        )
        .unwrap();
    building
        .handle(
            caller,
            InputContext::Building,
            Command::new(4, 7, Action::Select(GridPoint::new(7, [-1_000_000; 3]))),
        )
        .unwrap();
    let before = building.snapshot(caller).unwrap();
    assert!(building
        .handle(
            caller,
            InputContext::Building,
            Command::new(5, 7, Action::Select(GridPoint::new(7, [1_000_000; 3])))
        )
        .is_err());
    assert_eq!(building.snapshot(caller).unwrap(), before);
}

/// Verify zero and exhausted generations and sequences cannot wrap.
#[test]
fn zero_and_exhausted_generations_and_sequences_cannot_wrap() {
    assert!(Building::new(0, Limits::default()).is_err());
    let mut building = Building::new(u64::MAX, Limits::default()).unwrap();
    assert!(building.connect(0, 1).is_err());
    assert!(building.connect(101, 0).is_err());
    let caller = building.connect(101, u64::MAX).unwrap();
    for sequence in [0, u64::MAX] {
        assert!(building
            .handle(
                caller,
                InputContext::Building,
                Command::new(sequence, u64::MAX, Action::Clear)
            )
            .is_err());
        assert_eq!(building.snapshot(caller).unwrap().sequence, 0);
    }
    assert!(building.change_world(1).is_err());
    building.disconnect(caller).unwrap();
    assert!(building.connect(101, u64::MAX).is_err());
    assert!(building.connect(101, 1).is_err());
}
