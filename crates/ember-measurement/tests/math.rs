//! Offline arithmetic fixtures. These are not a runtime world/sample producer.
use ember_blueprint::{Frame, Geometry};
use ember_measurement::{
    inclusive_cells, measure, snap_to_small, CellAddress, Error, FreshnessPolicy, GridOrigin,
    Point, WorldEpoch,
};
use std::collections::BTreeSet;

fn frame() -> Frame {
    Frame {
        units: "metres".to_owned(),
        axes: "right_handed_x_east_y_up_z_south".to_owned(),
        source_mapping: "Offline canonical-coordinate test fixture.".to_owned(),
    }
}
fn p(value: [f64; 3]) -> Point {
    Point::new(value).unwrap()
}
fn context() -> WorldEpoch {
    WorldEpoch::new("test:world", 1).unwrap()
}

#[test]
fn xyz_signed_direction_and_diagonal_have_axis_specific_units() {
    let measurement = measure(&frame(), p([0.0; 3]), p([3.0, 4.0, 12.0])).unwrap();
    assert_eq!(measurement.signed_delta(), [3.0, 4.0, 12.0]);
    assert_eq!(measurement.axes().map(|axis| axis.metres), [3.0, 4.0, 12.0]);
    assert_eq!(
        measurement.axes().map(|axis| axis.small_spans),
        [6.0, 8.0, 24.0]
    );
    assert_eq!(
        measurement.axes().map(|axis| axis.large_spans),
        [0.75, 2.0, 3.0]
    );
    assert_eq!(measurement.diagonal().metres, 13.0);
    assert_eq!(measurement.diagonal().small_spans, 26.0);
}

#[test]
fn fractional_spans_are_not_rounded_to_inclusive_cells() {
    let measurement = measure(&frame(), p([0.0; 3]), p([0.25; 3])).unwrap();
    assert_eq!(measurement.axes().map(|axis| axis.small_spans), [0.5; 3]);
    assert_eq!(
        measurement.axes().map(|axis| axis.large_spans),
        [0.0625, 0.125, 0.0625]
    );
    let spans = measure(&frame(), p([0.0; 3]), p([2.0, 0.0, 0.0])).unwrap();
    let cells = inclusive_cells(
        CellAddress { x: 0, y: 0, z: 0 },
        CellAddress { x: 4, y: 0, z: 0 },
    )
    .unwrap();
    assert_eq!(spans.axes()[0].small_spans, 4.0);
    assert_eq!((cells.x, cells.y, cells.z), (5, 1, 1));
    assert_eq!(
        inclusive_cells(
            CellAddress { x: -4, y: 0, z: 0 },
            CellAddress { x: 0, y: 0, z: 0 }
        )
        .unwrap()
        .x,
        5
    );
    assert_eq!(
        inclusive_cells(
            CellAddress {
                x: i64::MIN,
                y: 0,
                z: 0
            },
            CellAddress {
                x: i64::MAX,
                y: 0,
                z: 0
            }
        )
        .unwrap_err(),
        Error::Overflow
    );
}

#[test]
fn ordinary_outline_has_twelve_unique_world_aligned_edges_and_eight_corners() {
    let measurement = measure(&frame(), p([-2.0, 3.0, -7.0]), p([4.0, 8.0, 5.0])).unwrap();
    assert_eq!(measurement.outline().len(), 12);
    let mut edges = BTreeSet::new();
    let mut corners = BTreeSet::new();
    for geometry in measurement.outline() {
        let Geometry::Polyline { points, closed } = geometry else {
            panic!("outline must use shared line geometry")
        };
        assert!(!closed);
        assert_eq!(points.len(), 2);
        assert_ne!(points[0], points[1]);
        assert_eq!(
            points[0]
                .iter()
                .zip(points[1])
                .filter(|(a, b)| **a != *b)
                .count(),
            1
        );
        for point in points {
            assert!([-2.0, 4.0].contains(&point[0]));
            assert!([3.0, 8.0].contains(&point[1]));
            assert!([-7.0, 5.0].contains(&point[2]));
            corners.insert(format!("{point:?}"));
        }
        assert!(edges.insert(format!("{points:?}")));
    }
    assert_eq!(corners.len(), 8);
    let reversed = measure(&frame(), p([4.0, 8.0, 5.0]), p([-2.0, 3.0, -7.0])).unwrap();
    assert_eq!(measurement.outline(), reversed.outline());
    assert_eq!(reversed.signed_delta(), [-6.0, -5.0, -12.0]);
}

#[test]
fn flat_line_and_zero_outlines_do_not_invent_thickness_or_duplicate_edges() {
    for (b, expected) in [([0.0, 4.0, 12.0], 4), ([0.0, 0.0, 12.0], 1), ([0.0; 3], 0)] {
        let measurement = measure(&frame(), p([0.0; 3]), p(b)).unwrap();
        assert_eq!(measurement.outline().len(), expected);
        let unique: BTreeSet<_> = measurement
            .outline()
            .iter()
            .map(|g| format!("{g:?}"))
            .collect();
        assert_eq!(unique.len(), expected);
    }
    let zero = measure(&frame(), p([-2.0; 3]), p([-2.0; 3])).unwrap();
    assert_eq!(zero.diagonal().metres, 0.0);
    assert_eq!(zero.axes().map(|axis| axis.small_spans), [0.0; 3]);
    assert_eq!(
        inclusive_cells(
            CellAddress { x: 3, y: 3, z: 3 },
            CellAddress { x: 3, y: 3, z: 3 }
        )
        .unwrap()
        .x,
        1
    );
}

#[test]
fn nonfinite_huge_and_wrong_frame_inputs_reject_before_geometry() {
    for value in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MAX,
        1_000_000.01,
        -1_000_000.01,
    ] {
        assert_eq!(
            Point::new([value, 0.0, 0.0]).unwrap_err(),
            Error::InvalidPoint
        );
    }
    let bound = measure(&frame(), p([-1_000_000.0; 3]), p([1_000_000.0; 3])).unwrap();
    assert!(bound.diagonal().metres.is_finite());
    let mut wrong = frame();
    wrong.units = "feet".to_owned();
    assert_eq!(
        measure(&wrong, p([0.0; 3]), p([1.0; 3])).unwrap_err(),
        Error::UnsupportedFrame
    );
    wrong = frame();
    wrong.axes = "guessed_engine_axes".to_owned();
    assert!(measure(&wrong, p([0.0; 3]), p([1.0; 3])).is_err());
    assert!(WorldEpoch::new("", 0).is_err());
    assert!(FreshnessPolicy::new(0).is_err());
    assert!(FreshnessPolicy::new(u64::MAX).is_err());
}

#[test]
fn snap_uses_the_declared_origin_and_explicit_negative_halfway_rule() {
    let ctx = context();
    let policy = FreshnessPolicy::new(10).unwrap();
    let origin = GridOrigin::caller_validated(ctx.clone(), p([0.25, -0.25, 1.25]), 100);
    assert_eq!(
        snap_to_small(p([0.5, -0.5, 1.0]), Some(&origin), &ctx, 100, policy)
            .unwrap()
            .coordinates(),
        [0.75, -0.75, 0.75]
    );
    assert_eq!(
        snap_to_small(p([0.49, -0.49, 1.49]), Some(&origin), &ctx, 100, policy)
            .unwrap()
            .coordinates(),
        [0.25, -0.25, 1.25]
    );
    let offset = [100.125, -50.25, 8.75];
    let shifted_origin = GridOrigin::caller_validated(
        ctx.clone(),
        p([0.25 + offset[0], -0.25 + offset[1], 1.25 + offset[2]]),
        100,
    );
    let shifted = snap_to_small(
        p([0.5 + offset[0], -0.5 + offset[1], 1.0 + offset[2]]),
        Some(&shifted_origin),
        &ctx,
        100,
        policy,
    )
    .unwrap()
    .coordinates();
    assert_eq!(
        shifted,
        [0.75 + offset[0], -0.75 + offset[1], 0.75 + offset[2]]
    );
}

#[test]
fn absent_stale_wrong_epoch_and_future_grid_origins_never_fall_back_to_zero() {
    let ctx = context();
    let policy = FreshnessPolicy::new(10).unwrap();
    assert_eq!(
        snap_to_small(p([0.2; 3]), None, &ctx, 100, policy).unwrap_err(),
        Error::GridUnavailable
    );
    let origin = GridOrigin::caller_validated(ctx.clone(), p([0.25; 3]), 100);
    assert_eq!(
        snap_to_small(p([0.2; 3]), Some(&origin), &ctx, 111, policy).unwrap_err(),
        Error::StaleGrid
    );
    assert_eq!(
        snap_to_small(p([0.2; 3]), Some(&origin), &ctx, 99, policy).unwrap_err(),
        Error::FutureSample
    );
    let other = WorldEpoch::new("test:world", 2).unwrap();
    assert_eq!(
        snap_to_small(p([0.2; 3]), Some(&origin), &other, 100, policy).unwrap_err(),
        Error::WorldMismatch
    );
    let edge = GridOrigin::caller_validated(ctx.clone(), p([0.25; 3]), 100);
    assert_eq!(
        snap_to_small(p([1_000_000.0; 3]), Some(&edge), &ctx, 100, policy).unwrap_err(),
        Error::InvalidPoint
    );
}

#[test]
fn degeneracy_is_honest_on_every_axis_and_zero_sign_is_canonical() {
    for end in [[0.0, 2.0, 3.0], [2.0, 0.0, 3.0], [2.0, 3.0, 0.0]] {
        let measurement = measure(&frame(), p([-0.0; 3]), p(end)).unwrap();
        assert_eq!(measurement.outline().len(), 4);
        for line in measurement.outline() {
            let Geometry::Polyline { points, closed } = line else {
                panic!("reference line required")
            };
            assert!(!closed);
            assert_ne!(points[0], points[1]);
        }
    }
    for end in [[2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 2.0]] {
        assert_eq!(
            measure(&frame(), p([0.0; 3]), p(end))
                .unwrap()
                .outline()
                .len(),
            1
        );
    }
    assert!(p([-0.0; 3])
        .coordinates()
        .iter()
        .all(|coordinate| !coordinate.is_sign_negative()));
}

#[test]
fn inclusive_cells_accept_the_largest_representable_count_in_both_directions() {
    let low = CellAddress {
        x: i64::MIN,
        y: i64::MIN,
        z: i64::MIN,
    };
    let high = CellAddress {
        x: i64::MAX - 1,
        y: i64::MAX - 1,
        z: i64::MAX - 1,
    };
    for (a, b) in [(low, high), (high, low)] {
        let counts = inclusive_cells(a, b).unwrap();
        assert_eq!(
            (counts.x, counts.y, counts.z),
            (u64::MAX, u64::MAX, u64::MAX)
        );
    }
}

#[test]
fn frame_metadata_and_freshness_boundaries_reject_unsupported_assertions() {
    for mapping in [String::new(), "line\nbreak".to_owned(), "a".repeat(1025)] {
        let mut unsupported = frame();
        unsupported.source_mapping = mapping;
        assert_eq!(
            measure(&unsupported, p([0.0; 3]), p([1.0; 3])).unwrap_err(),
            Error::UnsupportedFrame
        );
    }
    for bad_world in ["contains space", "<tag>", &"x".repeat(129)] {
        assert_eq!(
            WorldEpoch::new(bad_world, 0).unwrap_err(),
            Error::InvalidWorld
        );
    }
    let ctx = context();
    let policy = FreshnessPolicy::new(10).unwrap();
    let origin = GridOrigin::caller_validated(ctx.clone(), p([-3.25; 3]), 100);
    assert_eq!(
        snap_to_small(p([-3.25; 3]), Some(&origin), &ctx, 110, policy).unwrap(),
        p([-3.25; 3])
    );
    let edge = GridOrigin::caller_validated(ctx.clone(), p([0.0; 3]), u64::MAX);
    assert_eq!(
        snap_to_small(p([0.0; 3]), Some(&edge), &ctx, u64::MAX, policy).unwrap_err(),
        Error::ClockRange
    );
}
