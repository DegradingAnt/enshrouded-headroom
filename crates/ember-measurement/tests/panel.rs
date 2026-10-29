//! Synthetic positions exercise the actual session, not a native sampling claim.
use ember_blueprint::Frame;
use ember_measurement::{
    Action, Aim, AimSample, AimStatus, Error, FreshnessPolicy, InputContext, InputOwner, Mode,
    NativeMenu, PanelControls, Point, Session, Snap, Stamp, Unavailable, UnitDisplay, ViewStatus,
    WorldEpoch, WorldObservation, WorldSample,
};

fn owned() -> InputContext {
    InputContext {
        focused: true,
        native_menu: NativeMenu::Closed,
        owner: InputOwner::Measurement,
    }
}
fn context() -> WorldEpoch {
    WorldEpoch::new("test:position-ruler", 1).unwrap()
}
fn world(session: &mut Session, time: u64, sequence: u64) {
    session
        .observe_world(
            time,
            WorldObservation::Current(WorldSample {
                context: context(),
                frame: Frame {
                    units: "metres".into(),
                    axes: "right_handed_x_east_y_up_z_south".into(),
                    source_mapping: "Offline position fixture.".into(),
                },
                stamp: Stamp {
                    observed_at_ms: time,
                    sequence,
                },
            }),
        )
        .unwrap();
}
fn ready() -> Session {
    let mut session = Session::new(FreshnessPolicy::new(10).unwrap());
    world(&mut session, 100, 1);
    assert!(session.act(100, owned(), Action::Begin).consumed());
    session
}
fn position(session: &mut Session, time: u64, sequence: u64, coordinates: [f64; 3]) {
    session
        .observe_aim(
            time,
            owned(),
            AimSample {
                context: context(),
                stamp: Stamp {
                    observed_at_ms: time,
                    sequence,
                },
                aim: Aim::PlayerPosition(Point::new(coordinates).unwrap()),
            },
        )
        .unwrap();
}
fn pair(a: [f64; 3], b: [f64; 3]) -> Session {
    let mut session = ready();
    position(&mut session, 100, 1, a);
    assert!(session
        .act(100, owned(), Action::Pick(Snap::Off))
        .consumed());
    position(&mut session, 101, 2, b);
    assert!(session
        .act(101, owned(), Action::Pick(Snap::Off))
        .consumed());
    session
}
fn standard_pair() -> Session {
    pair([0.0; 3], [3.0, 4.0, 12.0])
}

#[test]
fn no_native_source_never_displays_positions_or_actionable_measurement() {
    let mut session = Session::default();
    let panel = session.panel(0, owned(), Snap::Off, UnitDisplay::Metres);
    assert_eq!(
        panel.status(),
        ViewStatus::Unavailable(Error::NativeUnavailable)
    );
    assert!(panel.dimension_lines().is_empty());
    assert_eq!(panel.controls(), PanelControls::default());
}
#[test]
fn begin_requires_a_fresh_world_and_current_input_even_before_capture() {
    let mut session = Session::new(FreshnessPolicy::new(10).unwrap());
    world(&mut session, 100, 1);
    let panel = session.panel(100, owned(), Snap::Off, UnitDisplay::Metres);
    assert_eq!(panel.status(), ViewStatus::Unavailable(Error::Inactive));
    assert_eq!(
        panel.controls(),
        PanelControls {
            begin: true,
            ..PanelControls::default()
        }
    );
    assert!(
        !session
            .panel(111, owned(), Snap::Off, UnitDisplay::Metres)
            .controls()
            .begin
    );
}
#[test]
fn distance_units_are_fractional_spans_and_large_diagonal_stays_metres() {
    let mut session = standard_pair();
    for (units, expected) in [
        (
            UnitDisplay::Metres,
            [
                "X: 3.000 m",
                "Y: 4.000 m",
                "Z: 12.000 m",
                "Diagonal: 13.000 m",
            ],
        ),
        (
            UnitDisplay::SmallGrid,
            [
                "X: 6.000 spans",
                "Y: 8.000 spans",
                "Z: 24.000 spans",
                "Diagonal: 26.000 spans",
            ],
        ),
        (
            UnitDisplay::LargeGrid,
            [
                "X: 0.750 spans",
                "Y: 2.000 spans",
                "Z: 3.000 spans",
                "Diagonal: 13.000 m",
            ],
        ),
    ] {
        let panel = session.panel(101, owned(), Snap::Off, units);
        assert_eq!(panel.dimension_lines(), expected);
        assert_eq!(panel.status(), ViewStatus::Pinned);
        assert_eq!(panel.units(), units);
        assert!(!panel.unit_hint().is_empty());
        assert!(panel.controls().clear && panel.controls().exit);
        assert!(!panel.controls().mark, "a mark consumes its candidate");
    }
    assert!(UnitDisplay::LargeGrid.hint().contains("X/Z: 4 m"));
    assert!(UnitDisplay::LargeGrid.hint().contains("Y: 2 m"));
}
#[test]
fn preview_and_next_mark_are_distinct_from_an_accepted_pair() {
    let mut session = ready();
    position(&mut session, 100, 1, [0.0; 3]);
    let first = session.panel(100, owned(), Snap::Off, UnitDisplay::Metres);
    assert_eq!(first.mark_label(), "Mark A");
    assert!(first.controls().mark);
    session.act(100, owned(), Action::Pick(Snap::Off));
    position(&mut session, 101, 2, [3.0, 4.0, 12.0]);
    let preview = session.panel(101, owned(), Snap::Off, UnitDisplay::Metres);
    assert_eq!(preview.status(), ViewStatus::Preview);
    assert_eq!(preview.status_text(), "Preview (B is not marked).");
    assert_eq!(preview.mark_label(), "Mark B");
    assert!(preview.controls().mark);
    assert_eq!(preview.dimension_lines().len(), 4);
    session.act(101, owned(), Action::Pick(Snap::Off));
    position(&mut session, 102, 3, [9.0; 3]);
    let complete = session.panel(102, owned(), Snap::Off, UnitDisplay::Metres);
    assert_eq!(complete.status_text(), "A and B marked.");
    assert_eq!(complete.mark_label(), "New A");
    assert!(complete.controls().mark);
}
#[test]
fn every_frame_rechecks_world_expiry_and_hides_dormant_dimensions() {
    let mut session = standard_pair();
    assert_eq!(
        session
            .panel(110, owned(), Snap::Off, UnitDisplay::Metres)
            .dimension_lines()
            .len(),
        4
    );
    let stale = session.panel(111, owned(), Snap::Off, UnitDisplay::Metres);
    assert_eq!(stale.status(), ViewStatus::Unavailable(Error::StaleWorld));
    assert!(stale.dimension_lines().is_empty());
    assert_eq!(stale.controls(), PanelControls::default());
    world(&mut session, 112, 2);
    assert!(
        session
            .panel(112, owned(), Snap::Off, UnitDisplay::Metres)
            .controls()
            .begin
    );
    session.act(112, owned(), Action::Begin);
    assert_eq!(
        session
            .panel(112, owned(), Snap::Off, UnitDisplay::Metres)
            .dimension_lines()
            .len(),
        4
    );
}
#[test]
fn focus_menu_and_other_owners_suspend_all_measurement_controls_and_text() {
    let conflicts = [
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
    for input in conflicts {
        let mut session = standard_pair();
        let panel = session.panel(101, input, Snap::Off, UnitDisplay::Metres);
        assert!(matches!(panel.status(), ViewStatus::Unavailable(_)));
        assert!(panel.dimension_lines().is_empty());
        assert_eq!(panel.controls(), PanelControls::default());
        assert_eq!(session.mode(), Mode::Suspended);
        assert!(
            session
                .panel(102, owned(), Snap::Off, UnitDisplay::Metres)
                .controls()
                .begin
        );
    }
}
#[test]
fn unavailable_world_never_retains_visible_dimension_labels() {
    let mut session = standard_pair();
    session
        .observe_world(102, WorldObservation::Unavailable(Unavailable::Epoch))
        .unwrap();
    let panel = session.panel(102, owned(), Snap::Off, UnitDisplay::SmallGrid);
    assert!(panel.dimension_lines().is_empty());
    assert_eq!(panel.controls(), PanelControls::default());
}
#[test]
fn tiny_nonzero_distances_are_not_printed_as_zero_and_extremes_stay_finite() {
    let mut tiny = pair([0.0; 3], [0.000001, 0.0, 0.0]);
    let panel = tiny.panel(101, owned(), Snap::Off, UnitDisplay::Metres);
    assert_eq!(panel.dimension_lines().first().unwrap(), "X: <0.001 m");
    assert_eq!(panel.dimension_lines().get(1).unwrap(), "Y: 0.000 m");
    let mut huge = pair([-1_000_000.0; 3], [1_000_000.0; 3]);
    let huge = huge.panel(101, owned(), Snap::Off, UnitDisplay::Metres);
    assert_eq!(huge.dimension_lines().first().unwrap(), "X: 2000000.000 m");
    assert!(huge
        .dimension_lines()
        .iter()
        .all(|line| !line.contains("inf") && !line.contains("NaN")));
}
#[test]
fn display_units_do_not_snap_points_or_turn_spans_into_inclusive_cells() {
    let mut session = pair([-0.01, -0.01, -0.01], [0.12, 0.12, 0.12]);
    let panel = session.panel(101, owned(), Snap::Off, UnitDisplay::SmallGrid);
    assert_eq!(panel.dimension_lines().first().unwrap(), "X: 0.260 spans");
    assert!(panel
        .dimension_lines()
        .iter()
        .all(|line| !line.contains("cells") && !line.contains("blocks")));
    let measurement = session
        .view(101, owned(), Snap::Off)
        .measurement()
        .unwrap()
        .clone();
    assert_eq!(measurement.a().coordinates(), [-0.01; 3]);
    assert_eq!(measurement.b().coordinates(), [0.12; 3]);
}
#[test]
fn missing_snap_origin_blocks_mark_without_expiring_accepted_pins() {
    let mut session = standard_pair();
    position(&mut session, 102, 3, [0.2; 3]);
    let panel = session.panel(102, owned(), Snap::SmallGrid(None), UnitDisplay::Metres);
    assert_eq!(panel.status(), ViewStatus::Pinned);
    assert_eq!(
        panel.aim_status(),
        AimStatus::Unavailable(Error::GridUnavailable)
    );
    assert!(!panel.controls().mark);
    assert_eq!(panel.dimension_lines().len(), 4);
    assert!(
        session
            .panel(102, owned(), Snap::Off, UnitDisplay::Metres)
            .controls()
            .mark
    );
}
#[test]
fn clear_and_exit_remove_visible_numbers_without_leaving_a_mark_enabled() {
    let mut session = standard_pair();
    assert!(session.act(102, owned(), Action::Clear).consumed());
    let cleared = session.panel(102, owned(), Snap::Off, UnitDisplay::Metres);
    assert!(cleared.dimension_lines().is_empty());
    assert!(!cleared.controls().mark);
    assert!(cleared.controls().exit);
    assert!(session.act(103, owned(), Action::Exit).consumed());
    let exited = session.panel(103, owned(), Snap::Off, UnitDisplay::Metres);
    assert!(exited.dimension_lines().is_empty());
    assert!(exited.controls().begin);
}
#[test]
fn a_previous_panel_does_not_authorize_a_later_conflicting_mark() {
    let mut session = standard_pair();
    position(&mut session, 102, 3, [9.0; 3]);
    let previous = session.panel(102, owned(), Snap::Off, UnitDisplay::Metres);
    assert!(previous.controls().mark);
    let rejected = session.act(
        103,
        InputContext {
            native_menu: NativeMenu::Open,
            ..owned()
        },
        Action::Pick(Snap::Off),
    );
    assert_eq!(rejected.error(), Some(Error::NativeMenuOpen));
    assert!(session
        .panel(103, owned(), Snap::Off, UnitDisplay::Metres)
        .dimension_lines()
        .is_empty());
}

#[test]
fn player_position_is_typed_separately_from_a_surface_hit() {
    let mut session = ready();
    position(&mut session, 100, 1, [1.0, 2.0, 3.0]);
    assert_eq!(
        session.view(100, owned(), Snap::Off).aim_status(),
        AimStatus::PlayerPosition
    );
    assert!(
        session
            .panel(100, owned(), Snap::Off, UnitDisplay::Metres)
            .controls()
            .mark
    );
}
