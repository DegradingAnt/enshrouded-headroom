//! Source polling exercises actual bounded files, including interrupted writes.
use ember_blueprint::{
    convert_highcrown, parse, read_bounded, Freshness, Limits, RefreshOutcome, SelectionKind,
    Session,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

/// Unique cleanup guard; no real source or game directory is written.
struct FileFixture(PathBuf);

impl FileFixture {
    /// Create one isolated source file.
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ember-blueprint-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }
}

impl Drop for FileFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn guide_content_changes_propagate_through_the_real_poll_boundary() {
    let source = include_bytes!("fixtures/highcrown-recorded.json");
    let file = FileFixture::new(source);
    let first = convert_highcrown(source, "test:polling-guide", 10, Limits::default()).unwrap();
    let mut session = Session::new(first);
    session.select(SelectionKind::Object, "b_garrison").unwrap();
    assert_eq!(
        session.poll_highcrown(&file.0, Limits::default()).unwrap(),
        RefreshOutcome::Unchanged
    );
    let changed = std::str::from_utf8(source)
        .unwrap()
        .replace("\"w\": 15,", "\"w\": 16,");
    std::fs::write(&file.0, changed).unwrap();
    assert_eq!(
        session.poll_highcrown(&file.0, Limits::default()).unwrap(),
        RefreshOutcome::Replaced
    );
    let current = session.snapshot();
    assert_eq!(current.model().revision().sequence, 11);
    assert_eq!(current.model().document().source.content_version, "3.0");
    assert_ne!(
        current.model().revision().content_sha256,
        ember_blueprint::sha256(source)
    );
    std::fs::write(&file.0, b"{\"version\":").unwrap();
    assert!(session.poll_highcrown(&file.0, Limits::default()).is_err());
    assert_eq!(session.snapshot().model(), current.model());
    assert!(matches!(
        session.snapshot().freshness(),
        Freshness::Rejected(_)
    ));
    // A complete valid source recovers, and carries a changed footprint.
    std::fs::write(&file.0, source).unwrap();
    assert_eq!(
        session.poll_highcrown(&file.0, Limits::default()).unwrap(),
        RefreshOutcome::Replaced
    );
    assert_eq!(session.snapshot().model().revision().sequence, 12);
}

#[test]
fn file_read_failure_and_oversize_are_visibly_rejected() {
    let bytes = include_bytes!("../examples/watchtower.blueprint.json");
    let file = FileFixture::new(bytes);
    let mut session = Session::new(parse(bytes, Limits::default()).unwrap());
    let previous = session.snapshot();
    assert!(read_bounded(
        &file.0,
        Limits {
            max_input_bytes: 4,
            ..Limits::default()
        }
    )
    .is_err());
    assert!(session
        .poll_interchange(
            &file.0,
            Limits {
                max_input_bytes: 4,
                ..Limits::default()
            }
        )
        .is_err());
    assert_eq!(session.snapshot().model(), previous.model());
    assert!(matches!(
        session.snapshot().freshness(),
        Freshness::Rejected(_)
    ));
    std::fs::remove_file(&file.0).unwrap();
    assert!(session
        .poll_interchange(&file.0, Limits::default())
        .is_err());
    assert!(matches!(
        session.snapshot().freshness(),
        Freshness::Rejected(_)
    ));
}

#[test]
fn reorder_does_not_rename_objects_and_ambiguous_legacy_walls_stay_references() {
    let mut source: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/highcrown-recorded.json")).unwrap();
    source["walls"].as_array_mut().unwrap().reverse();
    let converted = convert_highcrown(
        &serde_json::to_vec(&source).unwrap(),
        "test:guide",
        1,
        Limits::default(),
    )
    .unwrap();
    let doc = converted.document();
    assert!(doc.objects.iter().any(|o| o.id == "w_outer_w"));
    for id in ["r_upper", "p_upper_s1"] {
        let object = doc.objects.iter().find(|o| o.id == id).unwrap();
        assert!(object
            .primitives
            .iter()
            .all(|p| !matches!(p.geometry, ember_blueprint::Geometry::WallPath { .. })));
        assert!(!object.omissions.is_empty());
    }
}

#[test]
fn declared_null_floor_regions_rejects_refresh_and_keeps_the_visible_revision() {
    let source = include_bytes!("fixtures/highcrown-recorded.json");
    let file = FileFixture::new(source);
    let first = convert_highcrown(source, "test:null-mask-refresh", 1, Limits::default()).unwrap();
    let mut session = Session::new(first);
    session
        .select(SelectionKind::Object, "keep_stage36")
        .unwrap();
    let previous = session.snapshot();
    let mut malformed: serde_json::Value = serde_json::from_slice(source).unwrap();
    let floor = malformed["content"]["guide"]["measurements"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|object| object["id"] == "keep_stage36")
        .unwrap();
    floor["floor_regions"] = serde_json::Value::Null;
    std::fs::write(&file.0, serde_json::to_vec(&malformed).unwrap()).unwrap();
    let error = session
        .poll_highcrown(&file.0, Limits::default())
        .unwrap_err();
    assert_eq!(error.kind, ember_blueprint::ErrorKind::InvalidJson);
    let rejected = session.snapshot();
    assert_eq!(rejected.model(), previous.model());
    assert_eq!(
        rejected.selection(SelectionKind::Object),
        previous.selection(SelectionKind::Object)
    );
    assert!(matches!(rejected.freshness(), Freshness::Rejected(_)));
    std::fs::write(&file.0, source).unwrap();
    assert_eq!(
        session.poll_highcrown(&file.0, Limits::default()).unwrap(),
        RefreshOutcome::Unchanged
    );
    assert_eq!(session.snapshot().freshness(), &Freshness::Current);
}

#[test]
fn phase_order_representation_changes_refresh_bytes_without_renaming_stages() {
    let source = include_bytes!("fixtures/highcrown-recorded.json");
    let file = FileFixture::new(source);
    let mut session = Session::new(
        convert_highcrown(source, "test:phase-byte-refresh", 10, Limits::default()).unwrap(),
    );
    let first_stage = session
        .snapshot()
        .model()
        .document()
        .stages
        .first()
        .unwrap()
        .id
        .clone();
    session.select(SelectionKind::Stage, &first_stage).unwrap();
    let before = session.snapshot();
    let mut changed: serde_json::Value = serde_json::from_slice(source).unwrap();
    for phase in changed["content"]["guide"]["phases"]
        .as_array_mut()
        .unwrap()
    {
        phase["num"] = serde_json::json!(phase["num"].as_u64().unwrap().to_string());
    }
    let changed = serde_json::to_vec(&changed).unwrap();
    std::fs::write(&file.0, &changed).unwrap();
    assert_eq!(
        session.poll_highcrown(&file.0, Limits::default()).unwrap(),
        RefreshOutcome::Replaced
    );
    let after = session.snapshot();
    assert_eq!(after.model().revision().sequence, 11);
    assert_eq!(
        after.model().revision().content_sha256,
        ember_blueprint::sha256(&changed)
    );
    assert_eq!(after.model().document().source.content_version, "3.0");
    assert_eq!(
        after.model().document().stages,
        before.model().document().stages
    );
    assert_eq!(
        after.model().document().objects,
        before.model().document().objects
    );
    assert_eq!(
        after.selection(SelectionKind::Stage),
        before.selection(SelectionKind::Stage)
    );
    assert_eq!(
        session.poll_highcrown(&file.0, Limits::default()).unwrap(),
        RefreshOutcome::Unchanged
    );
}
