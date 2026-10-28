//! Interchange, refresh and authentic adapter regression tests.
use ember_blueprint::{
    convert_highcrown, parse, sha256, Blueprint, ErrorKind, Fidelity, Freshness, Geometry, IdState,
    Limits, Placement, RefreshOutcome, SelectionKind, Session,
};

const EXAMPLE: &[u8] = include_bytes!("../examples/watchtower.blueprint.json");
const HIGHCROWN: &[u8] = include_bytes!("fixtures/highcrown-recorded.json");

fn model() -> ember_blueprint::ValidatedBlueprint {
    parse(EXAMPLE, Limits::default()).unwrap()
}

fn changed(sequence: u64) -> Blueprint {
    let mut doc = model().document().clone();
    doc.source.revision.sequence = sequence;
    doc.source.revision.content_sha256 = sha256(format!("source revision {sequence}").as_bytes());
    doc
}

#[test]
fn generic_other_guide_round_trips_losslessly() {
    let first = model();
    let bytes = first.to_json().unwrap();
    let second = parse(&bytes, Limits::default()).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.document().frame.units, "metres");
    assert_eq!(first.document().objects[0].id, "watchtower");
}

#[test]
fn unknown_versions_units_axes_and_fields_are_rejected() {
    for (old, new) in [
        ("\"schema_version\": \"1.0\"", "\"schema_version\": \"2.0\""),
        ("\"units\": \"metres\"", "\"units\": \"feet\""),
        ("right_handed_x_east_y_up_z_south", "engine_camera_guessed"),
    ] {
        let text = std::str::from_utf8(EXAMPLE).unwrap().replace(old, new);
        assert!(parse(text.as_bytes(), Limits::default()).is_err(), "{new}");
    }
    let text = std::str::from_utf8(EXAMPLE).unwrap().replacen(
        "\"title\":",
        "\"unrecognized\": true, \"title\":",
        1,
    );
    assert_eq!(
        parse(text.as_bytes(), Limits::default()).unwrap_err().kind,
        ErrorKind::InvalidJson
    );
}

#[test]
fn runtime_nonfinite_values_cannot_serialize_as_null() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut doc = changed(2);
        doc.layers[0].elevation = value;
        assert!(doc.validate(Limits::default()).is_err());
    }
    let text = std::str::from_utf8(EXAMPLE)
        .unwrap()
        .replace("0.5", "1e9999");
    assert!(parse(text.as_bytes(), Limits::default()).is_err());
}

#[test]
fn oversized_sources_and_model_counts_are_bounded() {
    assert_eq!(
        parse(
            EXAMPLE,
            Limits {
                max_input_bytes: 8,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Limit
    );
    assert_eq!(
        parse(
            EXAMPLE,
            Limits {
                max_objects: 0,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Limit
    );
    assert_eq!(
        parse(
            EXAMPLE,
            Limits {
                max_primitives: 0,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Limit
    );
    assert_eq!(
        parse(
            EXAMPLE,
            Limits {
                max_points: 0,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Limit
    );
}

#[test]
fn ids_and_references_are_checked_in_every_namespace() {
    let mut duplicate = changed(2);
    duplicate.objects.push(duplicate.objects[0].clone());
    assert_eq!(
        duplicate.validate(Limits::default()).unwrap_err().kind,
        ErrorKind::DuplicateId
    );
    let mut missing = changed(2);
    missing.objects[0].stages.push("absent-stage".to_owned());
    assert_eq!(
        missing.validate(Limits::default()).unwrap_err().kind,
        ErrorKind::MissingReference
    );
    let mut invalid = changed(2);
    invalid.objects[0].id = "not a stable id".to_owned();
    assert_eq!(
        invalid.validate(Limits::default()).unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    let mut primitive = changed(2);
    primitive.objects[1].primitives[0].id = primitive.objects[0].primitives[0].id.clone();
    assert_eq!(
        primitive.validate(Limits::default()).unwrap_err().kind,
        ErrorKind::DuplicateId
    );
}

#[test]
fn invalid_dimensions_outside_bounds_and_cutouts_are_rejected() {
    for size in [[0.0, 1.0, 1.0], [-1.0, 1.0, 1.0], [1e12, 1.0, 1.0]] {
        let mut doc = changed(2);
        doc.objects[0].primitives[0].geometry = Geometry::Box {
            min: [0.0; 3],
            size,
        };
        assert!(doc.validate(Limits::default()).is_err());
    }
    let mut doc = changed(2);
    doc.objects[0].primitives[0].geometry = Geometry::Box {
        min: [1e6, 0.0, 0.0],
        size: [1.0; 3],
    };
    assert!(doc.validate(Limits::default()).is_err());
    let mut doc = changed(2);
    if let Geometry::Slab { openings, .. } = &mut doc.objects[1].primitives[0].geometry {
        openings[0].min = [900.0, 900.0];
    } else {
        panic!("example must exercise slabs");
    }
    assert!(doc.validate(Limits::default()).is_err());
}

#[test]
fn valid_revision_replaces_one_complete_snapshot() {
    let mut session = Session::new(model());
    session.select(SelectionKind::Object, "watchtower").unwrap();
    session.select(SelectionKind::Stage, "shell").unwrap();
    session.select(SelectionKind::Layer, "ground").unwrap();
    session
        .align(
            "altar",
            Placement {
                target_metres: [10.0, 20.0, 30.0],
                ..Placement::default()
            },
        )
        .unwrap();
    let previous = session.snapshot();
    let next = changed(2).validate(Limits::default()).unwrap();
    assert_eq!(
        session.apply(previous.model().revision(), next).unwrap(),
        RefreshOutcome::Replaced
    );
    let visible = session.snapshot();
    assert_eq!(visible.model().revision().sequence, 2);
    assert_eq!(previous.model().revision().sequence, 1);
    assert_eq!(
        visible.selection(SelectionKind::Object),
        &IdState::Active("watchtower".to_owned())
    );
    assert_eq!(
        visible.selection(SelectionKind::Stage),
        &IdState::Active("shell".to_owned())
    );
    assert_eq!(
        visible.selection(SelectionKind::Layer),
        &IdState::Active("ground".to_owned())
    );
    assert_eq!(
        visible.aligned_point([0.0, 0.0, 0.0]).unwrap(),
        [10.0, 20.0, 30.0]
    );
    assert_eq!(visible.freshness(), &Freshness::Current);
}

#[test]
fn invalid_partial_revision_leaves_old_geometry_visibly_stale() {
    let mut session = Session::new(model());
    let previous = session.snapshot();
    assert!(session
        .refresh_json(
            previous.model().revision(),
            b"{\"schema_version\":",
            Limits::default()
        )
        .is_err());
    let visible = session.snapshot();
    assert_eq!(visible.model(), previous.model());
    assert!(matches!(visible.freshness(), Freshness::Rejected(_)));
    // A subsequent complete unchanged file clears the failure without fake replacement.
    assert_eq!(
        session
            .refresh_json(previous.model().revision(), EXAMPLE, Limits::default())
            .unwrap(),
        RefreshOutcome::Unchanged
    );
    assert_eq!(session.snapshot().freshness(), &Freshness::Current);
}

#[test]
fn stale_conflicting_mismatched_and_racing_revisions_are_rejected() {
    let mut session = Session::new(model());
    let original = session.snapshot();
    session
        .apply(
            original.model().revision(),
            changed(2).validate(Limits::default()).unwrap(),
        )
        .unwrap();
    let current = session.snapshot();
    assert_eq!(
        session
            .apply(current.model().revision(), model())
            .unwrap_err()
            .kind,
        ErrorKind::StaleRevision
    );
    assert_eq!(
        session
            .apply(
                original.model().revision(),
                changed(3).validate(Limits::default()).unwrap()
            )
            .unwrap_err()
            .kind,
        ErrorKind::RevisionConflict
    );
    let mut wrong = changed(3);
    wrong.source.id = "other-guide".to_owned();
    assert_eq!(
        session
            .apply(
                current.model().revision(),
                wrong.validate(Limits::default()).unwrap()
            )
            .unwrap_err()
            .kind,
        ErrorKind::SourceMismatch
    );
    let mut conflict = changed(2);
    conflict.title = "changed but reused the exact revision".to_owned();
    conflict.source.revision = current.model().revision().clone();
    assert_eq!(
        session
            .apply(
                current.model().revision(),
                conflict.validate(Limits::default()).unwrap()
            )
            .unwrap_err()
            .kind,
        ErrorKind::RevisionConflict
    );
    assert_eq!(session.snapshot().model().revision().sequence, 2);
}

#[test]
fn removal_is_explicit_and_does_not_silently_choose_something_else() {
    let mut session = Session::new(model());
    session.select(SelectionKind::Object, "watchtower").unwrap();
    session.select(SelectionKind::Stage, "shell").unwrap();
    session.select(SelectionKind::Layer, "ground").unwrap();
    session.align("altar", Placement::default()).unwrap();
    let previous = session.snapshot();
    let mut doc = changed(2);
    doc.objects.clear();
    doc.stages.clear();
    doc.layers.clear();
    doc.anchors.clear();
    session
        .apply(
            previous.model().revision(),
            doc.validate(Limits::default()).unwrap(),
        )
        .unwrap();
    let visible = session.snapshot();
    for kind in [
        SelectionKind::Object,
        SelectionKind::Stage,
        SelectionKind::Layer,
    ] {
        assert!(matches!(
            visible.selection(kind),
            IdState::Removed { sequence: 2, .. }
        ));
    }
    assert!(matches!(
        visible.anchor(),
        IdState::Removed { sequence: 2, .. }
    ));
    assert!(visible.aligned_point([0.0; 3]).is_err());
}

#[test]
fn anchor_identity_survives_a_changed_local_position() {
    let mut session = Session::new(model());
    session
        .align(
            "altar",
            Placement {
                target_metres: [10.0, 20.0, 30.0],
                height_offset_metres: 2.0,
                ..Placement::default()
            },
        )
        .unwrap();
    let previous = session.snapshot();
    let mut doc = changed(2);
    doc.anchors[0].point = [3.0, 4.0, 5.0];
    session
        .apply(
            previous.model().revision(),
            doc.validate(Limits::default()).unwrap(),
        )
        .unwrap();
    assert_eq!(
        session.snapshot().aligned_point([3.0, 4.0, 5.0]).unwrap(),
        [10.0, 22.0, 30.0]
    );
    assert!(session
        .align(
            "altar",
            Placement {
                opacity: f64::NAN,
                ..Placement::default()
            }
        )
        .is_err());
}

#[test]
fn sha256_pins_exact_source_bytes() {
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn highcrown_preserves_actual_dimensions_axes_and_envelope_fidelity() {
    let mut input: serde_json::Value = serde_json::from_slice(HIGHCROWN).unwrap();
    input["world"] = serde_json::json!({"private_anchor": [987654.25, -765432.125, 654321.875]});
    let bytes = serde_json::to_vec(&input).unwrap();
    let converted = convert_highcrown(&bytes, "highcrown:test", 1, Limits::default()).unwrap();
    let doc = converted.document();
    assert_eq!(doc.source.revision.content_sha256, sha256(&bytes));
    assert_eq!(doc.source.content_version, "3.0");
    let building = doc.objects.iter().find(|o| o.id == "b_garrison").unwrap();
    let body = building
        .primitives
        .iter()
        .find(|p| p.id.ends_with("/envelope"))
        .unwrap();
    assert_eq!(
        body.geometry,
        Geometry::Box {
            min: [24.0, 5.0, -13.0],
            size: [15.0, 8.0, 7.0]
        }
    );
    assert_eq!(body.fidelity, Fidelity::RecordedEnvelope);
    assert!(!building.omissions.is_empty());
    let roof = building
        .primitives
        .iter()
        .find(|p| p.id.ends_with("/roof-profile"))
        .unwrap();
    assert_eq!(roof.fidelity, Fidelity::ReferenceOutline);
    if let Geometry::Polyline { points, .. } = &roof.geometry {
        assert_eq!(points[1], [24.0, 16.5, -9.5]);
    } else {
        panic!("recorded cross-section must not become an invented complete roof");
    }
    let wall = doc.objects.iter().find(|o| o.id == "w_outer_w").unwrap();
    let wall = wall
        .primitives
        .iter()
        .find(|p| matches!(p.geometry, Geometry::WallPath { .. }))
        .unwrap();
    if let Geometry::WallPath {
        height, thickness, ..
    } = wall.geometry
    {
        assert_eq!(height, 12.0); // height_total is absolute plan top +17, base +5.
        assert_eq!(thickness, 3.0);
    }
    let sentinel = b"987654.25";
    let contains_anchor = |candidate: &[u8]| {
        candidate
            .windows(sentinel.len())
            .any(|window| window == sentinel)
    };
    assert!(
        contains_anchor(&bytes),
        "positive control: source must contain the synthetic world anchor"
    );
    let exported = converted.to_json().unwrap();
    let mut controlled_leak = converted.document().clone();
    controlled_leak.anchors[0].point[0] = input["world"]["private_anchor"][0].as_f64().unwrap();
    let controlled_leak = controlled_leak
        .validate(Limits::default())
        .unwrap()
        .to_json()
        .unwrap();
    assert!(
        contains_anchor(&controlled_leak),
        "positive control: matcher must detect a synthetic anchor leaked into a valid model"
    );
    assert!(
        !contains_anchor(&exported),
        "source world anchor must not leak into the interchange"
    );
}

#[test]
fn exact_guide_floor_layers_keep_the_recorded_openings_and_stage_ids() {
    let doc = convert_highcrown(HIGHCROWN, "highcrown:test", 1, Limits::default()).unwrap();
    let floor = doc
        .document()
        .objects
        .iter()
        .find(|o| o.id == "keep_stage36")
        .unwrap();
    assert!(floor.stages.iter().any(|s| s == "tall"));
    let slab = floor
        .primitives
        .iter()
        .find(|p| matches!(p.geometry, Geometry::Slab { .. }))
        .unwrap();
    assert_eq!(slab.fidelity, Fidelity::ExactLayer);
    if let Geometry::Slab {
        min,
        size,
        openings,
    } = &slab.geometry
    {
        assert_eq!(*min, [80.0, 35.5, -111.0]);
        assert_eq!(*size, [16.0, 0.5, 16.0]);
        assert_eq!(openings.len(), 2);
        assert_eq!(openings[0].min, [83.0, -102.0]);
        assert_eq!(openings[0].size, [4.0, 4.0]);
    }
}

#[test]
fn same_guide_version_with_changed_content_is_a_new_source_revision() {
    let first = convert_highcrown(HIGHCROWN, "highcrown:test", 1, Limits::default()).unwrap();
    let bytes = std::str::from_utf8(HIGHCROWN)
        .unwrap()
        .replace("\"w\": 15,", "\"w\": 16,");
    let next = convert_highcrown(bytes.as_bytes(), "highcrown:test", 2, Limits::default()).unwrap();
    assert_ne!(
        first.revision().content_sha256,
        next.revision().content_sha256
    );
    assert_eq!(
        first.document().source.content_version,
        next.document().source.content_version
    );
    let first_id = first
        .document()
        .objects
        .iter()
        .find(|o| o.id == "b_garrison")
        .unwrap()
        .id
        .clone();
    let mut session = Session::new(first);
    session.select(SelectionKind::Object, &first_id).unwrap();
    let previous = session.snapshot();
    session.apply(previous.model().revision(), next).unwrap();
    assert_eq!(
        session.snapshot().selection(SelectionKind::Object),
        &IdState::Active(first_id)
    );
}

#[test]
fn adapter_missing_dimensions_does_not_invent_them() {
    let bytes = std::str::from_utf8(HIGHCROWN)
        .unwrap()
        .replace("\"abs_eaves\": 13,", "");
    let doc = convert_highcrown(bytes.as_bytes(), "highcrown:test", 1, Limits::default()).unwrap();
    let building = doc
        .document()
        .objects
        .iter()
        .find(|o| o.id == "b_garrison")
        .unwrap();
    assert!(!building
        .primitives
        .iter()
        .any(|p| p.id.ends_with("/envelope")));
    assert!(!building.omissions.is_empty());
}

#[test]
fn json_schema_is_an_open_versioned_contract() {
    let schema: serde_json::Value = serde_json::from_str(ember_blueprint::JSON_SCHEMA).unwrap();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["properties"]["schema_version"]["const"], "1.0");
    assert_eq!(schema["additionalProperties"], false);
}

#[test]
fn measured_compass_roof_axes_use_absolute_profile_heights() {
    let converted = convert_highcrown(HIGHCROWN, "test:roof-axes", 1, Limits::default()).unwrap();
    for (id, expected) in [
        ("xp_barn", [-6.0, 14.5, -40.0]),
        ("xp_exhibition", [66.0, 17.5, -126.0]),
    ] {
        let object = converted
            .document()
            .objects
            .iter()
            .find(|o| o.id == id)
            .unwrap();
        let profile = object
            .primitives
            .iter()
            .find(|p| p.id.ends_with("/roof-profile"))
            .unwrap();
        if let Geometry::Polyline { points, .. } = &profile.geometry {
            assert_eq!(points[1], expected);
        } else {
            panic!("profile must remain a line");
        }
    }
}

#[test]
fn unnamed_authentic_source_elements_keep_their_stable_id_as_label() {
    let model = convert_highcrown(HIGHCROWN, "test:unnamed", 1, Limits::default()).unwrap();
    let object = model
        .document()
        .objects
        .iter()
        .find(|o| o.id == "z_lane_jog")
        .unwrap();
    assert_eq!(object.name, "z_lane_jog");
    assert_eq!(object.source_ref, "zones/z_lane_jog");
}

#[test]
fn local_part_ids_cannot_borrow_an_unrelated_root_objects_stage_or_floor() {
    let mut source: serde_json::Value = serde_json::from_slice(HIGHCROWN).unwrap();
    source["buildings"][0]["parts"] = serde_json::json!([{
        "id":"measure_deep_sw", "name":"Local part with a repeated short ID",
        "x":24, "y":6, "w":1, "d":1, "abs_z0":5, "abs_z1":6
    }]);
    let model = convert_highcrown(
        &serde_json::to_vec(&source).unwrap(),
        "test:part-id",
        1,
        Limits::default(),
    )
    .unwrap();
    let part = model
        .document()
        .objects
        .iter()
        .find(|o| o.id == "b_garrison/measure_deep_sw")
        .unwrap();
    assert!(!part.stages.iter().any(|s| s == "pit"));
    assert!(!part
        .primitives
        .iter()
        .any(|p| matches!(p.geometry, Geometry::Slab { .. })));
}

#[test]
fn all_id_namespaces_and_layer_references_are_validated() {
    let mut doc = changed(2);
    doc.stages.push(doc.stages[0].clone());
    assert_eq!(
        doc.validate(Limits::default()).unwrap_err().kind,
        ErrorKind::DuplicateId
    );
    let mut doc = changed(2);
    doc.layers.push(doc.layers[0].clone());
    assert_eq!(
        doc.validate(Limits::default()).unwrap_err().kind,
        ErrorKind::DuplicateId
    );
    let mut doc = changed(2);
    doc.anchors.push(doc.anchors[0].clone());
    assert_eq!(
        doc.validate(Limits::default()).unwrap_err().kind,
        ErrorKind::DuplicateId
    );
    let mut doc = changed(2);
    doc.objects[0].primitives[0].layer = Some("missing-layer".to_owned());
    assert_eq!(
        doc.validate(Limits::default()).unwrap_err().kind,
        ErrorKind::MissingReference
    );
    for sequence in [0, 9_007_199_254_740_992] {
        let mut doc = changed(sequence);
        doc.source.revision.sequence = sequence;
        assert!(doc.validate(Limits::default()).is_err());
    }
}

#[test]
fn changed_source_contracts_and_reused_source_hashes_do_not_reanchor_silently() {
    let mut session = Session::new(model());
    let current = session.snapshot();
    let mut reused = changed(2);
    reused.source.revision.content_sha256 = current.model().revision().content_sha256.clone();
    assert_eq!(
        session
            .apply(
                current.model().revision(),
                reused.validate(Limits::default()).unwrap()
            )
            .unwrap_err()
            .kind,
        ErrorKind::RevisionConflict
    );
    let mut adapter = changed(2);
    adapter.source.adapter = "new-interpretation-v2".to_owned();
    assert_eq!(
        session
            .apply(
                current.model().revision(),
                adapter.validate(Limits::default()).unwrap()
            )
            .unwrap_err()
            .kind,
        ErrorKind::SourceMismatch
    );
    let mut frame = changed(2);
    frame.frame.source_mapping = "Moved the source origin".to_owned();
    assert_eq!(
        session
            .apply(
                current.model().revision(),
                frame.validate(Limits::default()).unwrap()
            )
            .unwrap_err()
            .kind,
        ErrorKind::SourceMismatch
    );
    assert_eq!(session.snapshot().model(), current.model());
}

#[test]
fn generated_floor_layer_ids_cannot_overwrite_declared_source_layers() {
    let mut source: serde_json::Value = serde_json::from_slice(HIGHCROWN).unwrap();
    source["levels"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id":"floor/keep_stage36/36", "name":"Colliding source namespace", "elev":-999
        }));
    let result = convert_highcrown(
        &serde_json::to_vec(&source).unwrap(),
        "test:layer-collision",
        1,
        Limits::default(),
    );
    assert_eq!(result.unwrap_err().kind, ErrorKind::DuplicateId);
}
