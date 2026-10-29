//! Behavioral tests use only private temporary regular files and real notify events.
use ember_blueprint::{
    convert_highcrown, parse, ErrorKind, Freshness, IdState, Limits, SelectionKind, Session,
};
use ember_guide_feed::{Config, Failure, Format, GuideFeed, Source, SourceSnapshot};
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

/// An isolated directory whose contents are owned by this test alone.
struct Fixture(PathBuf);
impl Fixture {
    /// Create a unique private directory without reusing an existing path.
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ember-guide-feed-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    /// Path to the single guide under test.
    fn path(&self) -> PathBuf {
        self.0.join("guide.json")
    }
    /// Write a complete or deliberately rejected fixture.
    fn write(&self, bytes: impl AsRef<[u8]>) {
        fs::write(self.path(), bytes).unwrap();
    }
    /// Start an interchange feed with a short bounded test cadence.
    fn feed(&self, limits: Limits) -> GuideFeed {
        GuideFeed::start(
            Source {
                path: self.path(),
                format: Format::Interchange,
            },
            config(limits),
            None,
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
/// Existing generic fixture; no new parser or guide format is introduced.
fn generic() -> &'static [u8] {
    include_bytes!("../../ember-blueprint/examples/watchtower.blueprint.json")
}
/// Existing representative Highcrown fixture.
fn highcrown() -> &'static [u8] {
    include_bytes!("../../ember-blueprint/tests/fixtures/highcrown-recorded.json")
}
/// A deterministic bounded cadence used only by this test suite.
fn config(limits: Limits) -> Config {
    Config {
        limits,
        debounce: Duration::from_millis(25),
        rescan: Duration::from_millis(150),
    }
}
/// Wait for a semantic publication rather than assuming native event timing.
fn until(feed: &GuideFeed, predicate: impl Fn(&SourceSnapshot) -> bool) -> Arc<SourceSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let value = feed.snapshot();
        if predicate(&value) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "publication timed out: {value:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
/// Produce a valid later interchange revision while leaving its display version unchanged.
fn revision(sequence: u64, title: &str) -> Vec<u8> {
    String::from_utf8(generic().to_vec())
        .unwrap()
        .replace("\"sequence\": 1", &format!("\"sequence\": {sequence}"))
        .replace("Generic watchtower example", title)
        .replace(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            &format!("{sequence:064x}"),
        )
        .into_bytes()
}
/// Match a rejected existing-parser category without accepting retained data as current.
fn rejected(value: &SourceSnapshot, kind: ErrorKind) -> bool {
    matches!(value, SourceSnapshot::Stale { reason: Failure::Source(error), .. } | SourceSnapshot::Unavailable(Failure::Source(error)) if error.kind == kind)
        && value.current().is_none()
}
#[test]
fn generic_initial_load_and_same_display_version_edit_publish_complete_immutable_models() {
    let fixture = Fixture::new();
    fixture.write(generic());
    let mut feed = fixture.feed(Limits::default());
    let first = until(&feed, |value| value.current().is_some());
    fixture.write(revision(2, "Changed tower"));
    let second = until(&feed, |value| {
        value
            .current()
            .is_some_and(|model| model.model().revision().sequence == 2)
    });
    assert_eq!(
        second.current().unwrap().model().document().title,
        "Changed tower"
    );
    assert_eq!(first.current().unwrap().model().revision().sequence, 1);
    assert_eq!(
        second
            .current()
            .unwrap()
            .model()
            .document()
            .source
            .content_version,
        "example-1"
    );
    feed.stop().unwrap();
}
#[test]
fn highcrown_raw_byte_changes_reuse_selection_and_keep_fidelity_with_version_three_unchanged() {
    let fixture = Fixture::new();
    fixture.write(highcrown());
    let mut session =
        Session::new(convert_highcrown(highcrown(), "test:feed", 10, Limits::default()).unwrap());
    session.select(SelectionKind::Object, "b_garrison").unwrap();
    let mut feed = GuideFeed::start(
        Source {
            path: fixture.path(),
            format: Format::Highcrown {
                source_id: "test:feed".to_owned(),
                first_sequence: 1,
            },
        },
        config(Limits::default()),
        Some(session),
    )
    .unwrap();
    let first = until(&feed, |value| value.current().is_some());
    fixture.write(
        std::str::from_utf8(highcrown())
            .unwrap()
            .replace("\"w\": 15,", "\"w\": 16,"),
    );
    let changed = until(&feed, |value| {
        value
            .current()
            .is_some_and(|model| model.model().revision().sequence == 11)
    });
    let model = changed.current().unwrap();
    assert_eq!(model.model().document().source.content_version, "3.0");
    assert_eq!(
        model.selection(SelectionKind::Object),
        &IdState::Active("b_garrison".to_owned())
    );
    assert_ne!(
        model.model().revision().content_sha256,
        first.current().unwrap().model().revision().content_sha256
    );
    let expected = convert_highcrown(
        &fs::read(fixture.path()).unwrap(),
        "test:feed",
        11,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(model.model(), &expected);
    feed.stop().unwrap();
}
#[test]
fn malformed_oversized_deleted_and_recreated_files_never_return_stale_geometry_as_current() {
    let fixture = Fixture::new();
    fixture.write(generic());
    let limits = Limits {
        max_input_bytes: generic().len() + 64,
        ..Limits::default()
    };
    let mut feed = fixture.feed(limits);
    let first = until(&feed, |value| value.current().is_some());
    fixture.write(b"{incomplete");
    let stale = until(&feed, |value| rejected(value, ErrorKind::InvalidJson));
    let SourceSnapshot::Stale { retained, .. } = stale.as_ref() else {
        panic!("last good model was lost")
    };
    assert_eq!(retained.model(), first.current().unwrap().model());
    assert!(matches!(retained.freshness(), Freshness::Rejected(_)));
    fixture.write(vec![b' '; limits.max_input_bytes + 1]);
    until(&feed, |value| rejected(value, ErrorKind::Limit));
    fs::remove_file(fixture.path()).unwrap();
    until(&feed, |value| rejected(value, ErrorKind::Io));
    fixture.write(generic());
    until(&feed, |value| {
        value
            .current()
            .is_some_and(|model| model.model().revision().sequence == 1)
    });
    feed.stop().unwrap();
}
#[test]
fn missing_or_malformed_initial_source_can_recover_without_a_seed_model() {
    let fixture = Fixture::new();
    let mut feed = fixture.feed(Limits::default());
    until(&feed, |value| rejected(value, ErrorKind::Io));
    fixture.write(b"{");
    until(&feed, |value| rejected(value, ErrorKind::InvalidJson));
    fixture.write(generic());
    until(&feed, |value| value.current().is_some());
    feed.stop().unwrap();
}
#[test]
fn atomic_replace_and_rename_away_then_back_follow_the_named_source_path() {
    let fixture = Fixture::new();
    fixture.write(generic());
    let mut feed = fixture.feed(Limits::default());
    until(&feed, |value| value.current().is_some());
    let replacement = fixture.0.join("replacement.json");
    fs::write(&replacement, revision(2, "Atomic tower")).unwrap();
    fs::rename(&replacement, fixture.path()).unwrap();
    until(&feed, |value| {
        value
            .current()
            .is_some_and(|model| model.model().revision().sequence == 2)
    });
    let moved = fixture.0.join("moved.json");
    fs::rename(fixture.path(), &moved).unwrap();
    until(&feed, |value| rejected(value, ErrorKind::Io));
    fs::rename(moved, fixture.path()).unwrap();
    until(&feed, |value| {
        value
            .current()
            .is_some_and(|model| model.model().revision().sequence == 2)
    });
    feed.stop().unwrap();
}
#[test]
fn rapid_writes_coalesce_and_eventually_publish_the_latest_complete_source() {
    let fixture = Fixture::new();
    fixture.write(generic());
    let mut feed = fixture.feed(Limits::default());
    until(&feed, |value| value.current().is_some());
    for sequence in 2..=40 {
        fixture.write(revision(sequence, "Latest complete tower"));
    }
    let current = until(&feed, |value| {
        value
            .current()
            .is_some_and(|model| model.model().revision().sequence == 40)
    });
    assert_eq!(
        current.current().unwrap().model().document().title,
        "Latest complete tower"
    );
    feed.stop().unwrap();
}
#[test]
fn source_identity_conflicts_stay_stale_until_a_compatible_revision_arrives() {
    let fixture = Fixture::new();
    fixture.write(generic());
    let mut feed = fixture.feed(Limits::default());
    until(&feed, |value| value.current().is_some());
    fixture.write(
        String::from_utf8(revision(2, "Wrong source"))
            .unwrap()
            .replace("example:watchtower", "example:other"),
    );
    until(&feed, |value| rejected(value, ErrorKind::SourceMismatch));
    fixture.write(revision(2, "Compatible source"));
    until(&feed, |value| {
        value
            .current()
            .is_some_and(|model| model.model().revision().sequence == 2)
    });
    feed.stop().unwrap();
}
#[test]
fn explicit_stop_and_drop_freeze_the_publication_after_file_changes() {
    let fixture = Fixture::new();
    fixture.write(generic());
    let mut feed = fixture.feed(Limits::default());
    until(&feed, |value| value.current().is_some());
    feed.stop().unwrap();
    let stopped = feed.snapshot();
    assert!(matches!(stopped.as_ref(), SourceSnapshot::Stopped { .. }));
    assert!(stopped.current().is_none());
    fixture.write(revision(2, "After stop"));
    thread::sleep(Duration::from_millis(350));
    assert!(Arc::ptr_eq(&stopped, &feed.snapshot()));
    feed.stop().unwrap();
    drop(feed);
    fixture.write(revision(3, "After drop"));
    assert!(stopped.current().is_none());
}
#[test]
fn limits_cannot_exceed_the_existing_blueprint_policy_and_zero_cadences_are_rejected() {
    let fixture = Fixture::new();
    let source = || Source {
        path: fixture.path(),
        format: Format::Interchange,
    };
    let too_large = Limits {
        max_input_bytes: Limits::default().max_input_bytes + 1,
        ..Limits::default()
    };
    assert!(GuideFeed::start(source(), config(too_large), None).is_err());
    assert!(GuideFeed::start(
        source(),
        Config {
            debounce: Duration::ZERO,
            ..Config::default()
        },
        None
    )
    .is_err());
    assert!(GuideFeed::start(
        source(),
        Config {
            rescan: Duration::ZERO,
            ..Config::default()
        },
        None
    )
    .is_err());
}
#[test]
fn highcrown_resume_rejects_an_unrelated_session_before_starting_a_worker() {
    let fixture = Fixture::new();
    let unrelated = Session::new(parse(generic(), Limits::default()).unwrap());
    let source = Source {
        path: fixture.path(),
        format: Format::Highcrown {
            source_id: "test:feed".to_owned(),
            first_sequence: 1,
        },
    };
    assert!(GuideFeed::start(source, config(Limits::default()), Some(unrelated)).is_err());
}
#[test]
fn watching_a_temporarily_absent_parent_recovers_when_the_directory_is_created() {
    let fixture = Fixture::new();
    let parent = fixture.0.join("later");
    let source = Source {
        path: parent.join("guide.json"),
        format: Format::Interchange,
    };
    let mut feed = GuideFeed::start(source, config(Limits::default()), None).unwrap();
    until(&feed, |value| {
        value.current().is_none()
            && !matches!(value, SourceSnapshot::Unavailable(Failure::Starting))
    });
    fs::create_dir(&parent).unwrap();
    fs::write(parent.join("guide.json"), generic()).unwrap();
    until(&feed, |value| value.current().is_some());
    feed.stop().unwrap();
}

#[test]
fn native_parent_events_reload_before_the_periodic_recovery_deadline() {
    let fixture = Fixture::new();
    fixture.write(generic());
    let mut feed = GuideFeed::start(
        Source {
            path: fixture.path(),
            format: Format::Interchange,
        },
        Config {
            rescan: Duration::from_secs(60),
            ..config(Limits::default())
        },
        None,
    )
    .unwrap();
    until(&feed, |value| value.current().is_some());
    let replacement = fixture.0.join("event-only.json");
    fs::write(&replacement, revision(2, "Native event tower")).unwrap();
    fs::rename(&replacement, fixture.path()).unwrap();
    until(&feed, |value| {
        value
            .current()
            .is_some_and(|model| model.model().revision().sequence == 2)
    });
    feed.stop().unwrap();
}

#[test]
fn default_limits_accept_a_raw_plan_of_the_current_six_megabyte_size() {
    let fixture = Fixture::new();
    let mut source = highcrown().to_vec();
    source.resize(6_260_428, b' ');
    fixture.write(&source);
    let mut feed = GuideFeed::start(
        Source {
            path: fixture.path(),
            format: Format::Highcrown {
                source_id: "test:large-plan".to_owned(),
                first_sequence: 1,
            },
        },
        config(Limits::default()),
        None,
    )
    .unwrap();
    let value = until(&feed, |value| value.current().is_some());
    assert_eq!(
        value.current().unwrap().model().revision().content_sha256,
        ember_blueprint::sha256(&source)
    );
    feed.stop().unwrap();
}

#[test]
fn existing_reconciliation_marks_a_removed_selection_without_choosing_another_object() {
    let fixture = Fixture::new();
    fixture.write(generic());
    let mut session = Session::new(parse(generic(), Limits::default()).unwrap());
    session.select(SelectionKind::Object, "watchtower").unwrap();
    let mut feed = GuideFeed::start(
        Source {
            path: fixture.path(),
            format: Format::Interchange,
        },
        config(Limits::default()),
        Some(session),
    )
    .unwrap();
    until(&feed, |value| value.current().is_some());
    fixture.write(
        String::from_utf8(revision(2, "Replacement object"))
            .unwrap()
            .replace("\"id\": \"watchtower\"", "\"id\": \"other\""),
    );
    let current = until(&feed, |value| {
        value
            .current()
            .is_some_and(|model| model.model().revision().sequence == 2)
    });
    assert_eq!(
        current.current().unwrap().selection(SelectionKind::Object),
        &IdState::Removed {
            id: "watchtower".to_owned(),
            sequence: 2
        }
    );
    feed.stop().unwrap();
}

#[test]
fn dropping_a_running_owner_joins_its_worker_and_leaves_readers_stopped() {
    let fixture = Fixture::new();
    fixture.write(generic());
    let feed = fixture.feed(Limits::default());
    until(&feed, |value| value.current().is_some());
    let reader = feed.reader();
    drop(feed);
    let stopped = reader.snapshot();
    assert!(matches!(
        stopped.as_ref(),
        SourceSnapshot::Stopped { retained: Some(_) }
    ));
    assert!(stopped.current().is_none());
    fixture.write(revision(2, "After owner drop"));
    thread::sleep(Duration::from_millis(350));
    assert!(Arc::ptr_eq(&stopped, &reader.snapshot()));
}

#[test]
fn a_resumed_model_cannot_bypass_tightened_object_limits_on_an_unchanged_highcrown_file() {
    let fixture = Fixture::new();
    fixture.write(highcrown());
    let session = Session::new(
        convert_highcrown(highcrown(), "test:resume-limits", 1, Limits::default()).unwrap(),
    );
    let limited = Limits {
        max_objects: 0,
        ..Limits::default()
    };
    let result = GuideFeed::start(
        Source {
            path: fixture.path(),
            format: Format::Highcrown {
                source_id: "test:resume-limits".to_owned(),
                first_sequence: 1,
            },
        },
        config(limited),
        Some(session),
    );
    assert!(
        result.is_err(),
        "an accepted seed must satisfy the configured resource policy"
    );
}
