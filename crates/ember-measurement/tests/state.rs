//! Explicit synthetic observations only in tests. No native producer is supplied.
use ember_blueprint::Frame;
use ember_measurement::{
    Action, Aim, AimSample, AimStatus, Error, FreshnessPolicy, GridOrigin, InputContext,
    InputOwner, Mode, NativeMenu, Point, Session, Snap, Stamp, Unavailable, ViewStatus, WorldEpoch,
    WorldObservation, WorldSample,
};
fn point(p: [f64; 3]) -> Point {
    Point::new(p).unwrap()
}
fn epoch(world: &str, epoch: u64) -> WorldEpoch {
    WorldEpoch::new(world, epoch).unwrap()
}
fn frame() -> Frame {
    Frame {
        units: "metres".to_owned(),
        axes: "right_handed_x_east_y_up_z_south".to_owned(),
        source_mapping: "Offline state fixture; no native mapping.".to_owned(),
    }
}
fn owned() -> InputContext {
    InputContext {
        focused: true,
        native_menu: NativeMenu::Closed,
        owner: InputOwner::Measurement,
    }
}
fn world(context: WorldEpoch, time: u64, sequence: u64) -> WorldObservation {
    WorldObservation::Current(WorldSample {
        context,
        frame: frame(),
        stamp: Stamp {
            observed_at_ms: time,
            sequence,
        },
    })
}
fn ready() -> Session {
    let mut session = Session::new(FreshnessPolicy::new(10).unwrap());
    session
        .observe_world(100, world(epoch("test:a", 1), 100, 1))
        .unwrap();
    assert!(session.act(100, owned(), Action::Begin).consumed());
    session
}
fn aim(session: &mut Session, time: u64, sequence: u64, value: Aim) {
    session
        .observe_aim(
            time,
            owned(),
            AimSample {
                context: epoch("test:a", 1),
                stamp: Stamp {
                    observed_at_ms: time,
                    sequence,
                },
                aim: value,
            },
        )
        .unwrap();
}
fn pair() -> Session {
    let mut session = ready();
    aim(&mut session, 100, 1, Aim::Hit(point([1.0, 2.0, 3.0])));
    assert!(session
        .act(100, owned(), Action::Pick(Snap::Off))
        .consumed());
    aim(&mut session, 101, 2, Aim::Hit(point([5.0, 4.0, 7.0])));
    assert!(session
        .act(101, owned(), Action::Pick(Snap::Off))
        .consumed());
    session
}

#[test]
fn default_has_no_runtime_world_or_fake_geometry() {
    let mut session = Session::default();
    assert_eq!(session.mode(), Mode::Inactive);
    let view = session.view(0, owned(), Snap::Off);
    assert_eq!(
        view.status(),
        ViewStatus::Unavailable(Error::NativeUnavailable)
    );
    assert!(view.measurement().is_none());
    assert!(view.first().is_none());
    assert!(view.context().is_none());
    assert!(!session.act(0, owned(), Action::Begin).consumed());
}

#[test]
fn two_picks_third_restart_clear_and_exit_have_explicit_state() {
    let mut session = ready();
    assert_eq!(
        session.view(100, owned(), Snap::Off).status(),
        ViewStatus::AwaitingFirst
    );
    aim(&mut session, 100, 1, Aim::Hit(point([1.0; 3])));
    session.act(100, owned(), Action::Pick(Snap::Off));
    assert_eq!(
        session.view(100, owned(), Snap::Off).status(),
        ViewStatus::FirstPinned
    );
    assert_eq!(
        session.act(100, owned(), Action::Pick(Snap::Off)).error(),
        Some(Error::NoAim)
    );
    aim(&mut session, 101, 2, Aim::Hit(point([4.0, 5.0, 6.0])));
    assert_eq!(
        session.view(101, owned(), Snap::Off).status(),
        ViewStatus::Preview
    );
    session.act(101, owned(), Action::Pick(Snap::Off));
    assert_eq!(
        session.view(101, owned(), Snap::Off).status(),
        ViewStatus::Pinned
    );
    aim(&mut session, 102, 3, Aim::Hit(point([9.0; 3])));
    session.act(102, owned(), Action::Pick(Snap::Off));
    let view = session.view(102, owned(), Snap::Off);
    assert_eq!(view.status(), ViewStatus::FirstPinned);
    assert_eq!(view.first().unwrap().coordinates(), [9.0; 3]);
    assert!(view.second().is_none());
    assert!(session.act(102, owned(), Action::Clear).consumed());
    assert_eq!(
        session.view(102, owned(), Snap::Off).status(),
        ViewStatus::AwaitingFirst
    );
    assert!(session.act(102, owned(), Action::Exit).consumed());
    assert_eq!(session.mode(), Mode::Inactive);
    assert!(session
        .view(102, owned(), Snap::Off)
        .measurement()
        .is_none());
}

#[test]
fn aim_miss_drops_preview_but_keeps_pinned_points() {
    let mut session = ready();
    aim(&mut session, 100, 1, Aim::Hit(point([1.0; 3])));
    session.act(100, owned(), Action::Pick(Snap::Off));
    aim(&mut session, 101, 2, Aim::Hit(point([4.0; 3])));
    assert_eq!(
        session.view(101, owned(), Snap::Off).status(),
        ViewStatus::Preview
    );
    aim(&mut session, 102, 3, Aim::Miss);
    let view = session.view(102, owned(), Snap::Off);
    assert_eq!(view.status(), ViewStatus::FirstPinned);
    assert!(view.measurement().is_none());
    assert_eq!(view.aim_status(), AimStatus::Miss);
    assert_eq!(
        session.act(102, owned(), Action::Pick(Snap::Off)).error(),
        Some(Error::NoHit)
    );
    let mut session = pair();
    let before = session
        .view(101, owned(), Snap::Off)
        .measurement()
        .unwrap()
        .clone();
    aim(&mut session, 102, 3, Aim::Miss);
    let after = session.view(102, owned(), Snap::Off);
    assert_eq!(after.status(), ViewStatus::Pinned);
    assert_eq!(after.measurement().unwrap(), &before);
    assert_eq!(
        session.act(102, owned(), Action::Pick(Snap::Off)).error(),
        Some(Error::NoHit)
    );
}

#[test]
fn pins_outlive_aim_samples_but_not_the_world_freshness_gate() {
    let mut session = pair();
    aim(&mut session, 101, 3, Aim::Hit(point([8.0; 3])));
    session
        .observe_world(112, world(epoch("test:a", 1), 112, 2))
        .unwrap();
    let view = session.view(112, owned(), Snap::Off);
    assert_eq!(view.status(), ViewStatus::Pinned);
    assert_eq!(view.aim_status(), AimStatus::Unavailable(Error::StaleAim));
    assert!(view.measurement().is_some());
    assert_eq!(view.valid_until_ms(), Some(122));
    let view = session.view(123, owned(), Snap::Off);
    assert_eq!(view.status(), ViewStatus::Unavailable(Error::StaleWorld));
    assert!(view.measurement().is_none());
    assert!(view.first().is_none());
    assert!(view.second().is_none());
    assert!(view.context().is_none());
}

#[test]
fn stale_aim_cannot_reuse_a_previously_fresh_preview() {
    let mut session = ready();
    aim(&mut session, 100, 1, Aim::Hit(point([1.0; 3])));
    session.act(100, owned(), Action::Pick(Snap::Off));
    aim(&mut session, 101, 2, Aim::Hit(point([4.0; 3])));
    let sample = AimSample {
        context: epoch("test:a", 1),
        stamp: Stamp {
            observed_at_ms: 90,
            sequence: 3,
        },
        aim: Aim::Hit(point([99.0; 3])),
    };
    assert_eq!(
        session.observe_aim(102, owned(), sample).unwrap_err(),
        Error::StaleAim
    );
    let view = session.view(102, owned(), Snap::Off);
    assert_eq!(view.status(), ViewStatus::FirstPinned);
    assert!(view.measurement().is_none());
    assert_eq!(
        session.act(102, owned(), Action::Pick(Snap::Off)).error(),
        Some(Error::StaleAim)
    );
}

#[test]
fn world_or_epoch_change_resets_selection_and_blocks_old_context() {
    for new in [epoch("test:b", 1), epoch("test:a", 2)] {
        let mut session = pair();
        session
            .observe_world(102, world(new.clone(), 102, 2))
            .unwrap();
        assert_eq!(session.mode(), Mode::Inactive);
        assert!(session.view(102, owned(), Snap::Off).first().is_none());
        session.act(102, owned(), Action::Begin);
        assert_eq!(
            session.view(102, owned(), Snap::Off).status(),
            ViewStatus::AwaitingFirst
        );
        let old = AimSample {
            context: epoch("test:a", 1),
            stamp: Stamp {
                observed_at_ms: 102,
                sequence: 3,
            },
            aim: Aim::Hit(point([9.0; 3])),
        };
        assert_eq!(
            session.observe_aim(102, owned(), old).unwrap_err(),
            Error::WorldMismatch
        );
        assert!(session.view(102, owned(), Snap::Off).first().is_none());
        let origin = GridOrigin::caller_validated(epoch("test:a", 1), point([0.25; 3]), 103);
        session
            .observe_aim(
                103,
                owned(),
                AimSample {
                    context: new,
                    stamp: Stamp {
                        observed_at_ms: 103,
                        sequence: 4,
                    },
                    aim: Aim::Hit(point([1.0; 3])),
                },
            )
            .unwrap();
        assert_eq!(
            session
                .act(103, owned(), Action::Pick(Snap::SmallGrid(Some(&origin))))
                .error(),
            Some(Error::WorldMismatch)
        );
        assert!(session.view(103, owned(), Snap::Off).first().is_none());
    }
}

#[test]
fn unknown_world_epoch_and_native_unavailability_never_expose_dormant_pins() {
    for reason in [
        Unavailable::World,
        Unavailable::Epoch,
        Unavailable::NativeBinding,
    ] {
        let mut session = pair();
        session
            .observe_world(102, WorldObservation::Unavailable(reason))
            .unwrap();
        let view = session.view(102, owned(), Snap::Off);
        assert!(matches!(view.status(), ViewStatus::Unavailable(_)));
        assert!(view.measurement().is_none());
        assert!(view.first().is_none());
        assert!(!session
            .act(102, owned(), Action::Pick(Snap::Off))
            .consumed());
        session
            .observe_world(103, world(epoch("test:a", 1), 103, 2))
            .unwrap();
        assert!(!session
            .view(103, owned(), Snap::Off)
            .measurement()
            .is_some());
        session.act(103, owned(), Action::Begin);
        assert_eq!(
            session.view(103, owned(), Snap::Off).status(),
            ViewStatus::Pinned
        );
    }
}

#[test]
fn menu_focus_and_other_owners_reject_without_moving_pins_and_require_reacquisition() {
    let contexts = [
        InputContext {
            focused: false,
            ..owned()
        },
        InputContext {
            native_menu: NativeMenu::Open,
            ..owned()
        },
        InputContext {
            native_menu: NativeMenu::Unknown,
            ..owned()
        },
        InputContext {
            owner: InputOwner::Other,
            ..owned()
        },
        InputContext {
            owner: InputOwner::Unknown,
            ..owned()
        },
    ];
    for conflict in contexts {
        let mut session = pair();
        let before = session
            .view(101, owned(), Snap::Off)
            .measurement()
            .unwrap()
            .clone();
        assert!(!session.act(102, conflict, Action::Clear).consumed());
        assert_eq!(session.mode(), Mode::Suspended);
        assert!(session
            .view(102, conflict, Snap::Off)
            .measurement()
            .is_none());
        assert!(session
            .view(102, owned(), Snap::Off)
            .measurement()
            .is_none());
        session.act(102, owned(), Action::Begin);
        assert_eq!(
            session.view(102, owned(), Snap::Off).measurement().unwrap(),
            &before
        );
    }
}

#[test]
fn every_abstract_action_and_aim_observation_rejects_menu_conflicts() {
    for action in [
        Action::Begin,
        Action::Pick(Snap::Off),
        Action::Clear,
        Action::Exit,
    ] {
        let mut session = pair();
        let conflict = InputContext {
            native_menu: NativeMenu::Open,
            ..owned()
        };
        assert!(!session.act(102, conflict, action).consumed());
        assert_eq!(session.mode(), Mode::Suspended);
    }
    let mut session = ready();
    let sample = AimSample {
        context: epoch("test:a", 1),
        stamp: Stamp {
            observed_at_ms: 100,
            sequence: 1,
        },
        aim: Aim::Hit(point([1.0; 3])),
    };
    assert_eq!(
        session
            .observe_aim(
                100,
                InputContext {
                    native_menu: NativeMenu::Open,
                    ..owned()
                },
                sample
            )
            .unwrap_err(),
        Error::NativeMenuOpen
    );
    assert!(session
        .view(100, owned(), Snap::Off)
        .measurement()
        .is_none());
}

#[test]
fn out_of_order_future_and_clock_regressing_observations_fail_closed() {
    let mut session = ready();
    let future = AimSample {
        context: epoch("test:a", 1),
        stamp: Stamp {
            observed_at_ms: 101,
            sequence: 1,
        },
        aim: Aim::Hit(point([1.0; 3])),
    };
    assert_eq!(
        session.observe_aim(100, owned(), future).unwrap_err(),
        Error::FutureSample
    );
    aim(&mut session, 100, 1, Aim::Hit(point([1.0; 3])));
    let replay = AimSample {
        context: epoch("test:a", 1),
        stamp: Stamp {
            observed_at_ms: 100,
            sequence: 1,
        },
        aim: Aim::Hit(point([2.0; 3])),
    };
    assert_eq!(
        session.observe_aim(100, owned(), replay).unwrap_err(),
        Error::OutOfOrder
    );
    assert_eq!(
        session.act(100, owned(), Action::Pick(Snap::Off)).error(),
        Some(Error::OutOfOrder)
    );
    assert_eq!(
        session
            .observe_world(100, world(epoch("test:a", 1), 100, 1))
            .unwrap_err(),
        Error::OutOfOrder
    );
    assert!(session
        .view(100, owned(), Snap::Off)
        .measurement()
        .is_none());
    let mut session = pair();
    assert_eq!(
        session.view(100, owned(), Snap::Off).status(),
        ViewStatus::Unavailable(Error::ClockRegression)
    );
    assert!(session
        .view(101, owned(), Snap::Off)
        .measurement()
        .is_none());
}

#[test]
fn old_epochs_and_changed_frame_contracts_do_not_restore_old_selection() {
    let mut session = pair();
    session
        .observe_world(102, world(epoch("test:a", 2), 102, 2))
        .unwrap();
    assert_eq!(
        session
            .observe_world(103, world(epoch("test:a", 1), 103, 3))
            .unwrap_err(),
        Error::EpochRegression
    );
    assert!(session.view(103, owned(), Snap::Off).first().is_none());
    session
        .observe_world(104, world(epoch("test:a", 2), 104, 4))
        .unwrap();
    session.act(104, owned(), Action::Begin);
    assert_eq!(
        session.view(104, owned(), Snap::Off).status(),
        ViewStatus::AwaitingFirst
    );
    let mut bad_frame = frame();
    bad_frame.source_mapping = "Changed origin without changing epoch".to_owned();
    assert_eq!(
        session
            .observe_world(
                105,
                WorldObservation::Current(WorldSample {
                    context: epoch("test:a", 2),
                    frame: bad_frame,
                    stamp: Stamp {
                        observed_at_ms: 105,
                        sequence: 5
                    }
                })
            )
            .unwrap_err(),
        Error::FrameChanged
    );
    assert!(session
        .view(105, owned(), Snap::Off)
        .measurement()
        .is_none());
}

#[test]
fn invalid_world_frame_and_clock_range_never_produce_geometry() {
    let mut session = pair();
    let mut wrong = frame();
    wrong.units = "feet".to_owned();
    assert_eq!(
        session
            .observe_world(
                102,
                WorldObservation::Current(WorldSample {
                    context: epoch("test:a", 1),
                    frame: wrong,
                    stamp: Stamp {
                        observed_at_ms: 102,
                        sequence: 2
                    }
                })
            )
            .unwrap_err(),
        Error::UnsupportedFrame
    );
    assert!(session
        .view(102, owned(), Snap::Off)
        .measurement()
        .is_none());
    let mut session = Session::default();
    assert_eq!(
        session
            .observe_world(100, world(epoch("test:a", 1), 101, 1))
            .unwrap_err(),
        Error::FutureSample
    );
    let mut session = Session::default();
    assert_eq!(
        session
            .observe_world(u64::MAX, world(epoch("test:a", 1), u64::MAX, 1))
            .unwrap_err(),
        Error::ClockRange
    );
}

#[test]
fn snap_failure_retains_pins_and_never_exposes_an_unsnapped_preview() {
    let mut session = ready();
    aim(&mut session, 100, 1, Aim::Hit(point([0.24; 3])));
    assert_eq!(
        session
            .act(100, owned(), Action::Pick(Snap::SmallGrid(None)))
            .error(),
        Some(Error::GridUnavailable)
    );
    assert!(session
        .view(100, owned(), Snap::SmallGrid(None))
        .first()
        .is_none());
    let origin = GridOrigin::caller_validated(epoch("test:a", 1), point([0.25; 3]), 100);
    session.act(100, owned(), Action::Pick(Snap::SmallGrid(Some(&origin))));
    assert_eq!(
        session
            .view(100, owned(), Snap::Off)
            .first()
            .unwrap()
            .coordinates(),
        [0.25; 3]
    );
    aim(&mut session, 101, 2, Aim::Hit(point([2.24; 3])));
    let view = session.view(101, owned(), Snap::SmallGrid(None));
    assert_eq!(view.status(), ViewStatus::FirstPinned);
    assert!(view.measurement().is_none());
    assert_eq!(
        view.aim_status(),
        AimStatus::Unavailable(Error::GridUnavailable)
    );
    let view = session.view(101, owned(), Snap::SmallGrid(Some(&origin)));
    assert_eq!(view.status(), ViewStatus::Preview);
    assert_eq!(view.second().unwrap().coordinates(), [2.25; 3]);
    assert_eq!(view.valid_until_ms(), Some(110));
}

#[test]
fn known_context_change_with_an_unusable_frame_cannot_revive_old_pins() {
    let mut session = pair();
    let mut wrong = frame();
    wrong.units = "unknown_native_units".to_owned();
    let changed = WorldObservation::Current(WorldSample {
        context: epoch("test:b", 2),
        frame: wrong,
        stamp: Stamp {
            observed_at_ms: 102,
            sequence: 2,
        },
    });
    assert_eq!(
        session.observe_world(102, changed).unwrap_err(),
        Error::UnsupportedFrame
    );
    assert!(session.view(102, owned(), Snap::Off).first().is_none());
    // The fresh ordered identity change is known even though its metre mapping failed.
    // Confirming the old world must not resurrect its selection after that transition.
    session
        .observe_world(103, world(epoch("test:a", 1), 103, 3))
        .unwrap();
    assert!(session.act(103, owned(), Action::Begin).consumed());
    assert_eq!(
        session.view(103, owned(), Snap::Off).status(),
        ViewStatus::AwaitingFirst
    );
}

#[test]
fn unusable_frame_does_not_allow_a_known_new_epoch_to_regress() {
    let mut session = pair();
    let mut wrong = frame();
    wrong.units = "unknown_native_units".to_owned();
    let changed = WorldObservation::Current(WorldSample {
        context: epoch("test:b", 2),
        frame: wrong,
        stamp: Stamp {
            observed_at_ms: 102,
            sequence: 2,
        },
    });
    assert_eq!(
        session.observe_world(102, changed).unwrap_err(),
        Error::UnsupportedFrame
    );
    assert_eq!(
        session
            .observe_world(103, world(epoch("test:b", 1), 103, 3))
            .unwrap_err(),
        Error::EpochRegression
    );
    assert!(session.view(103, owned(), Snap::Off).first().is_none());
}

#[test]
fn each_action_yields_every_conflicting_input_context_without_moving_pins() {
    let conflicts = [
        (
            InputContext {
                focused: false,
                ..owned()
            },
            Error::FocusLost,
        ),
        (
            InputContext {
                native_menu: NativeMenu::Open,
                ..owned()
            },
            Error::NativeMenuOpen,
        ),
        (
            InputContext {
                native_menu: NativeMenu::Unknown,
                ..owned()
            },
            Error::MenuUnknown,
        ),
        (
            InputContext {
                owner: InputOwner::Other,
                ..owned()
            },
            Error::InputNotOwned,
        ),
        (
            InputContext {
                owner: InputOwner::Unknown,
                ..owned()
            },
            Error::InputNotOwned,
        ),
    ];
    for (input, error) in conflicts {
        for action in [
            Action::Begin,
            Action::Pick(Snap::Off),
            Action::Clear,
            Action::Exit,
        ] {
            let mut session = pair();
            let before = session
                .view(101, owned(), Snap::Off)
                .measurement()
                .unwrap()
                .clone();
            let result = session.act(102, input, action);
            assert_eq!(result.error(), Some(error));
            assert!(!result.consumed());
            assert_eq!(session.mode(), Mode::Suspended);
            let view = session.view(102, input, Snap::Off);
            assert_eq!(view.status(), ViewStatus::Unavailable(error));
            assert!(view.context().is_none());
            assert!(view.first().is_none());
            assert!(view.second().is_none());
            assert!(view.measurement().is_none());
            assert!(view.valid_until_ms().is_none());
            assert_eq!(
                session.act(102, owned(), Action::Pick(Snap::Off)).error(),
                Some(Error::Inactive)
            );
            assert!(session.act(102, owned(), Action::Begin).consumed());
            assert_eq!(
                session.view(102, owned(), Snap::Off).measurement(),
                Some(&before)
            );
        }
    }
}

#[test]
fn view_alone_revokes_input_on_conflict_and_requires_explicit_begin() {
    for input in [
        InputContext {
            focused: false,
            ..owned()
        },
        InputContext {
            native_menu: NativeMenu::Open,
            ..owned()
        },
        InputContext {
            native_menu: NativeMenu::Unknown,
            ..owned()
        },
        InputContext {
            owner: InputOwner::Other,
            ..owned()
        },
        InputContext {
            owner: InputOwner::Unknown,
            ..owned()
        },
    ] {
        let mut session = pair();
        assert!(session.view(102, input, Snap::Off).measurement().is_none());
        assert_eq!(session.mode(), Mode::Suspended);
        assert!(session
            .view(102, owned(), Snap::Off)
            .measurement()
            .is_none());
        session.act(102, owned(), Action::Begin);
        assert_eq!(
            session.view(102, owned(), Snap::Off).status(),
            ViewStatus::Pinned
        );
    }
}

#[test]
fn freshness_boundary_is_inclusive_and_refresh_does_not_reacquire_capture() {
    let mut session = pair();
    assert_eq!(
        session.view(110, owned(), Snap::Off).status(),
        ViewStatus::Pinned
    );
    assert_eq!(
        session.view(111, owned(), Snap::Off).status(),
        ViewStatus::Unavailable(Error::StaleWorld)
    );
    assert_eq!(session.mode(), Mode::Suspended);
    session
        .observe_world(112, world(epoch("test:a", 1), 112, 2))
        .unwrap();
    assert!(session.view(112, owned(), Snap::Off).first().is_none());
    session.act(112, owned(), Action::Begin);
    assert_eq!(
        session.view(112, owned(), Snap::Off).status(),
        ViewStatus::Pinned
    );
}

#[test]
fn unavailable_aim_and_clock_regression_never_reuse_a_transient_hit() {
    let mut session = ready();
    aim(&mut session, 100, 1, Aim::Hit(point([1.0; 3])));
    session.act(100, owned(), Action::Pick(Snap::Off));
    aim(&mut session, 101, 2, Aim::Hit(point([4.0; 3])));
    assert_eq!(
        session.view(101, owned(), Snap::Off).status(),
        ViewStatus::Preview
    );
    aim(&mut session, 102, 3, Aim::Unavailable);
    let view = session.view(102, owned(), Snap::Off);
    assert_eq!(view.status(), ViewStatus::FirstPinned);
    assert_eq!(
        view.aim_status(),
        AimStatus::Unavailable(Error::AimUnavailable)
    );
    assert!(view.measurement().is_none());
    assert_eq!(
        session.act(102, owned(), Action::Pick(Snap::Off)).error(),
        Some(Error::AimUnavailable)
    );
    assert_eq!(
        session.act(101, owned(), Action::Clear).error(),
        Some(Error::ClockRegression)
    );
    session
        .observe_world(103, world(epoch("test:a", 1), 103, 2))
        .unwrap();
    session.act(103, owned(), Action::Begin);
    assert_eq!(
        session.view(103, owned(), Snap::Off).status(),
        ViewStatus::FirstPinned
    );
}

#[test]
fn context_changes_reset_pins_during_suspension_and_never_reset_stream_order() {
    let mut session = pair();
    let conflict = InputContext {
        native_menu: NativeMenu::Open,
        ..owned()
    };
    session.view(102, conflict, Snap::Off);
    session
        .observe_world(103, world(epoch("test:b", 1), 103, 2))
        .unwrap();
    assert_eq!(session.mode(), Mode::Inactive);
    assert!(!session.act(103, conflict, Action::Begin).consumed());
    session.act(103, owned(), Action::Begin);
    assert_eq!(
        session.view(103, owned(), Snap::Off).status(),
        ViewStatus::AwaitingFirst
    );
    let replay = AimSample {
        context: epoch("test:b", 1),
        stamp: Stamp {
            observed_at_ms: 103,
            sequence: 2,
        },
        aim: Aim::Hit(point([9.0; 3])),
    };
    assert_eq!(
        session.observe_aim(103, owned(), replay).unwrap_err(),
        Error::OutOfOrder
    );
    assert!(session.view(103, owned(), Snap::Off).first().is_none());
}

#[test]
fn aim_time_regression_with_a_higher_sequence_rejects_and_clears_preview() {
    let mut session = ready();
    aim(&mut session, 100, 1, Aim::Hit(point([1.0; 3])));
    session.act(100, owned(), Action::Pick(Snap::Off));
    aim(&mut session, 102, 2, Aim::Hit(point([4.0; 3])));
    let sample = AimSample {
        context: epoch("test:a", 1),
        stamp: Stamp {
            observed_at_ms: 101,
            sequence: 3,
        },
        aim: Aim::Hit(point([9.0; 3])),
    };
    assert_eq!(
        session.observe_aim(102, owned(), sample).unwrap_err(),
        Error::OutOfOrder
    );
    let view = session.view(102, owned(), Snap::Off);
    assert_eq!(view.status(), ViewStatus::FirstPinned);
    assert_eq!(view.aim_status(), AimStatus::Unavailable(Error::OutOfOrder));
    assert!(view.measurement().is_none());
}

#[test]
fn requested_snap_rechecks_its_origin_on_every_preview_and_pick() {
    let mut session = ready();
    aim(&mut session, 100, 1, Aim::Hit(point([0.24; 3])));
    session.act(100, owned(), Action::Pick(Snap::Off));
    let origin = GridOrigin::caller_validated(epoch("test:a", 1), point([0.25; 3]), 100);
    aim(&mut session, 110, 2, Aim::Hit(point([2.24; 3])));
    assert_eq!(
        session
            .view(110, owned(), Snap::SmallGrid(Some(&origin)))
            .status(),
        ViewStatus::Preview
    );
    session
        .observe_world(111, world(epoch("test:a", 1), 111, 2))
        .unwrap();
    let view = session.view(111, owned(), Snap::SmallGrid(Some(&origin)));
    assert_eq!(view.status(), ViewStatus::FirstPinned);
    assert_eq!(view.aim_status(), AimStatus::Unavailable(Error::StaleGrid));
    assert!(view.measurement().is_none());
    assert_eq!(
        session
            .act(111, owned(), Action::Pick(Snap::SmallGrid(Some(&origin))))
            .error(),
        Some(Error::StaleGrid)
    );
    assert_eq!(
        session
            .view(111, owned(), Snap::Off)
            .first()
            .unwrap()
            .coordinates(),
        [0.24; 3]
    );
}

#[test]
fn grid_confirmation_barrier_rejects_round_trip_origins_and_accepts_new_calibration() {
    for context in [epoch("test:a", 1), epoch("test:a", 2)] {
        let mut session = Session::new(FreshnessPolicy::new(10).unwrap());
        session
            .observe_world(100, world(context.clone(), 100, 1))
            .unwrap();
        session.act(100, owned(), Action::Begin);
        let old_origin = GridOrigin::caller_validated(context.clone(), point([0.25; 3]), 100);
        session
            .observe_world(102, world(epoch("test:b", 1), 102, 2))
            .unwrap();
        session
            .observe_world(103, world(context.clone(), 103, 3))
            .unwrap();
        session.act(103, owned(), Action::Begin);
        session
            .observe_aim(
                104,
                owned(),
                AimSample {
                    context: context.clone(),
                    stamp: Stamp {
                        observed_at_ms: 104,
                        sequence: 1,
                    },
                    aim: Aim::Hit(point([1.24; 3])),
                },
            )
            .unwrap();
        session.act(104, owned(), Action::Pick(Snap::Off));
        session
            .observe_aim(
                104,
                owned(),
                AimSample {
                    context: context.clone(),
                    stamp: Stamp {
                        observed_at_ms: 104,
                        sequence: 2,
                    },
                    aim: Aim::Hit(point([2.24; 3])),
                },
            )
            .unwrap();
        let tied_origin = GridOrigin::caller_validated(context.clone(), point([0.25; 3]), 103);
        for origin in [&old_origin, &tied_origin] {
            let view = session.view(104, owned(), Snap::SmallGrid(Some(origin)));
            assert_eq!(view.status(), ViewStatus::FirstPinned);
            assert!(view.measurement().is_none());
            assert!(!session
                .act(104, owned(), Action::Pick(Snap::SmallGrid(Some(origin))))
                .consumed());
            assert_eq!(
                session
                    .view(104, owned(), Snap::Off)
                    .first()
                    .unwrap()
                    .coordinates(),
                [1.24; 3]
            );
        }
        let confirmed = GridOrigin::caller_validated(context, point([0.25; 3]), 105);
        assert_eq!(
            session
                .view(105, owned(), Snap::SmallGrid(Some(&confirmed)))
                .status(),
            ViewStatus::Preview
        );
        assert!(session
            .act(
                105,
                owned(),
                Action::Pick(Snap::SmallGrid(Some(&confirmed)))
            )
            .consumed());
        assert_eq!(
            session
                .view(105, owned(), Snap::Off)
                .second()
                .unwrap()
                .coordinates(),
            [2.25; 3]
        );
    }
}

#[test]
fn world_confirmation_barrier_rejects_observations_from_before_or_at_knowledge_loss() {
    for lost in [
        Unavailable::World,
        Unavailable::Epoch,
        Unavailable::NativeBinding,
    ] {
        for observed_at_ms in [100, 102] {
            let mut session = pair();
            let before = session
                .view(101, owned(), Snap::Off)
                .measurement()
                .unwrap()
                .clone();
            session
                .observe_world(102, WorldObservation::Unavailable(lost))
                .unwrap();
            assert!(session
                .observe_world(103, world(epoch("test:a", 1), observed_at_ms, 2))
                .is_err());
            assert!(!session.act(103, owned(), Action::Begin).consumed());
            let view = session.view(103, owned(), Snap::Off);
            assert!(view.context().is_none());
            assert!(view.first().is_none());
            assert!(view.measurement().is_none());
            // Rejected observations preserve dormant pins, but actual new confirmation
            // and an explicit Begin are needed before those pins are visible again.
            session
                .observe_world(104, world(epoch("test:a", 1), 104, 3))
                .unwrap();
            assert!(session
                .view(104, owned(), Snap::Off)
                .measurement()
                .is_none());
            session.act(104, owned(), Action::Begin);
            assert_eq!(
                session.view(104, owned(), Snap::Off).measurement(),
                Some(&before)
            );
        }
    }
}

#[test]
fn world_confirmation_barrier_is_not_advanced_by_unavailable_view_queries() {
    let mut session = pair();
    session
        .observe_world(102, WorldObservation::Unavailable(Unavailable::World))
        .unwrap();
    assert!(session
        .view(105, owned(), Snap::Off)
        .measurement()
        .is_none());
    assert!(!session.act(105, owned(), Action::Begin).consumed());
    session
        .observe_world(106, world(epoch("test:a", 1), 103, 2))
        .unwrap();
    session.act(106, owned(), Action::Begin);
    assert_eq!(
        session.view(106, owned(), Snap::Off).status(),
        ViewStatus::Pinned
    );
}

#[test]
fn grid_confirmation_barrier_requires_reconfirmation_after_temporary_unknown_context() {
    let mut session = pair();
    let before = session
        .view(101, owned(), Snap::Off)
        .measurement()
        .unwrap()
        .clone();
    let origin = GridOrigin::caller_validated(epoch("test:a", 1), point([0.25; 3]), 100);
    session
        .observe_world(102, WorldObservation::Unavailable(Unavailable::Epoch))
        .unwrap();
    session
        .observe_world(103, world(epoch("test:a", 1), 103, 2))
        .unwrap();
    session.act(103, owned(), Action::Begin);
    aim(&mut session, 104, 3, Aim::Hit(point([9.24; 3])));
    assert!(!session
        .act(104, owned(), Action::Pick(Snap::SmallGrid(Some(&origin))))
        .consumed());
    assert_eq!(
        session.view(104, owned(), Snap::Off).measurement(),
        Some(&before)
    );
    let confirmed = GridOrigin::caller_validated(epoch("test:a", 1), point([0.25; 3]), 105);
    assert!(session
        .act(
            105,
            owned(),
            Action::Pick(Snap::SmallGrid(Some(&confirmed)))
        )
        .consumed());
    assert_eq!(
        session
            .view(105, owned(), Snap::Off)
            .first()
            .unwrap()
            .coordinates(),
        [9.25; 3]
    );
}

#[test]
fn world_confirmation_barrier_applies_after_invalid_current_observations() {
    let mut session = pair();
    let before = session
        .view(101, owned(), Snap::Off)
        .measurement()
        .unwrap()
        .clone();
    let mut wrong = frame();
    wrong.units = "unknown".to_owned();
    assert_eq!(
        session
            .observe_world(
                102,
                WorldObservation::Current(WorldSample {
                    context: epoch("test:a", 1),
                    frame: wrong,
                    stamp: Stamp {
                        observed_at_ms: 102,
                        sequence: 2
                    },
                })
            )
            .unwrap_err(),
        Error::UnsupportedFrame
    );
    assert!(session
        .observe_world(103, world(epoch("test:a", 1), 102, 3))
        .is_err());
    assert!(session
        .view(103, owned(), Snap::Off)
        .measurement()
        .is_none());
    session
        .observe_world(104, world(epoch("test:a", 1), 104, 4))
        .unwrap();
    session.act(104, owned(), Action::Begin);
    assert_eq!(
        session.view(104, owned(), Snap::Off).measurement(),
        Some(&before)
    );
}

#[test]
fn world_confirmation_barrier_preserves_dormant_pins_after_out_of_order_observation() {
    let mut session = pair();
    let before = session
        .view(101, owned(), Snap::Off)
        .measurement()
        .unwrap()
        .clone();
    assert_eq!(
        session
            .observe_world(102, world(epoch("test:a", 1), 101, 1))
            .unwrap_err(),
        Error::OutOfOrder
    );
    assert!(session
        .view(102, owned(), Snap::Off)
        .measurement()
        .is_none());
    session
        .observe_world(103, world(epoch("test:a", 1), 103, 2))
        .unwrap();
    assert!(!session
        .view(103, owned(), Snap::Off)
        .measurement()
        .is_some());
    session.act(103, owned(), Action::Begin);
    assert_eq!(
        session.view(103, owned(), Snap::Off).measurement(),
        Some(&before)
    );
}

#[test]
fn world_confirmation_barrier_uses_clock_high_water_after_clock_regression() {
    let mut session = pair();
    let before = session
        .view(101, owned(), Snap::Off)
        .measurement()
        .unwrap()
        .clone();
    assert_eq!(
        session.view(100, owned(), Snap::Off).status(),
        ViewStatus::Unavailable(Error::ClockRegression)
    );
    assert_eq!(
        session
            .observe_world(101, world(epoch("test:a", 1), 101, 2))
            .unwrap_err(),
        Error::WorldConfirmationRequired
    );
    assert!(session
        .view(102, owned(), Snap::Off)
        .measurement()
        .is_none());
    session
        .observe_world(103, world(epoch("test:a", 1), 102, 3))
        .unwrap();
    session.act(103, owned(), Action::Begin);
    assert_eq!(
        session.view(103, owned(), Snap::Off).measurement(),
        Some(&before)
    );
}

#[test]
fn aim_activation_barrier_rejects_queued_samples_after_world_round_trips() {
    for context in [epoch("test:a", 1), epoch("test:a", 2)] {
        for old_aim in [Aim::Hit(point([9.0; 3])), Aim::Miss, Aim::Unavailable] {
            for observed_at_ms in [101, 103] {
                let mut session = Session::new(FreshnessPolicy::new(10).unwrap());
                session
                    .observe_world(100, world(context.clone(), 100, 1))
                    .unwrap();
                session.act(100, owned(), Action::Begin);
                session
                    .observe_aim(
                        100,
                        owned(),
                        AimSample {
                            context: context.clone(),
                            stamp: Stamp {
                                observed_at_ms: 100,
                                sequence: 1,
                            },
                            aim: Aim::Hit(point([1.0; 3])),
                        },
                    )
                    .unwrap();
                session.act(100, owned(), Action::Pick(Snap::Off));
                session
                    .observe_world(102, world(epoch("test:b", 1), 102, 2))
                    .unwrap();
                session
                    .observe_world(103, world(context.clone(), 103, 3))
                    .unwrap();
                session.act(103, owned(), Action::Begin);
                // Higher sequence and acceptable age do not prove that the sample
                // belongs to the newly activated occurrence of this world context.
                assert!(session
                    .observe_aim(
                        104,
                        owned(),
                        AimSample {
                            context: context.clone(),
                            stamp: Stamp {
                                observed_at_ms,
                                sequence: 2
                            },
                            aim: old_aim,
                        }
                    )
                    .is_err());
                let view = session.view(104, owned(), Snap::Off);
                assert_eq!(view.status(), ViewStatus::AwaitingFirst);
                assert!(matches!(view.aim_status(), AimStatus::Unavailable(_)));
                assert!(view.first().is_none());
                assert!(view.measurement().is_none());
                assert!(!session
                    .act(104, owned(), Action::Pick(Snap::Off))
                    .consumed());
                session
                    .observe_aim(
                        105,
                        owned(),
                        AimSample {
                            context: context.clone(),
                            stamp: Stamp {
                                observed_at_ms: 105,
                                sequence: 3,
                            },
                            aim: Aim::Hit(point([2.0; 3])),
                        },
                    )
                    .unwrap();
                assert!(session
                    .act(105, owned(), Action::Pick(Snap::Off))
                    .consumed());
                session
                    .observe_aim(
                        106,
                        owned(),
                        AimSample {
                            context: context.clone(),
                            stamp: Stamp {
                                observed_at_ms: 106,
                                sequence: 4,
                            },
                            aim: Aim::Hit(point([4.0; 3])),
                        },
                    )
                    .unwrap();
                assert_eq!(
                    session.view(106, owned(), Snap::Off).status(),
                    ViewStatus::Preview
                );
                session
                    .observe_aim(
                        107,
                        owned(),
                        AimSample {
                            context: context.clone(),
                            stamp: Stamp {
                                observed_at_ms: 107,
                                sequence: 5,
                            },
                            aim: Aim::Miss,
                        },
                    )
                    .unwrap();
                let view = session.view(107, owned(), Snap::Off);
                assert_eq!(view.status(), ViewStatus::FirstPinned);
                assert_eq!(view.aim_status(), AimStatus::Miss);
                assert_eq!(view.first().unwrap().coordinates(), [2.0; 3]);
                assert!(view.measurement().is_none());
                assert_eq!(
                    session.act(107, owned(), Action::Pick(Snap::Off)).error(),
                    Some(Error::NoHit)
                );
            }
        }
    }
}

#[test]
fn aim_activation_barrier_rejects_queued_samples_after_unknown_context_recovery() {
    for lost in [
        Unavailable::World,
        Unavailable::Epoch,
        Unavailable::NativeBinding,
    ] {
        for old_aim in [Aim::Hit(point([9.0; 3])), Aim::Miss, Aim::Unavailable] {
            let mut session = pair();
            let before = session
                .view(101, owned(), Snap::Off)
                .measurement()
                .unwrap()
                .clone();
            session
                .observe_world(102, WorldObservation::Unavailable(lost))
                .unwrap();
            session
                .observe_world(103, world(epoch("test:a", 1), 103, 2))
                .unwrap();
            session.act(103, owned(), Action::Begin);
            assert!(session
                .observe_aim(
                    104,
                    owned(),
                    AimSample {
                        context: epoch("test:a", 1),
                        stamp: Stamp {
                            observed_at_ms: 101,
                            sequence: 3
                        },
                        aim: old_aim,
                    }
                )
                .is_err());
            let view = session.view(104, owned(), Snap::Off);
            assert_eq!(view.status(), ViewStatus::Pinned);
            assert_eq!(view.measurement(), Some(&before));
            assert!(matches!(view.aim_status(), AimStatus::Unavailable(_)));
            assert!(!session
                .act(104, owned(), Action::Pick(Snap::Off))
                .consumed());
            aim(&mut session, 105, 4, Aim::Hit(point([9.0; 3])));
            assert!(session
                .act(105, owned(), Action::Pick(Snap::Off))
                .consumed());
            aim(&mut session, 106, 5, Aim::Hit(point([10.0; 3])));
            assert_eq!(
                session.view(106, owned(), Snap::Off).status(),
                ViewStatus::Preview
            );
            aim(&mut session, 107, 6, Aim::Miss);
            let view = session.view(107, owned(), Snap::Off);
            assert_eq!(view.status(), ViewStatus::FirstPinned);
            assert_eq!(view.aim_status(), AimStatus::Miss);
            assert_eq!(view.first().unwrap().coordinates(), [9.0; 3]);
            assert!(view.measurement().is_none());
        }
    }
}
