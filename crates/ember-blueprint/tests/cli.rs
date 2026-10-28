//! Exercise the actual CLI, not a second implementation of its argument parser.
use ember_blueprint::{parse, Limits};
use std::{path::Path, process::Command};

/// Resolve a checked-in local fixture without assuming a working directory.
fn path(relative: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

#[test]
fn cli_converts_the_authentic_fixture_to_the_shared_format() {
    let result = Command::new(env!("CARGO_BIN_EXE_ember-blueprint"))
        .arg("convert-highcrown")
        .arg(path("tests/fixtures/highcrown-recorded.json"))
        .args(["test:cli", "9"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let model = parse(&result.stdout, Limits::default()).unwrap();
    assert_eq!(model.revision().sequence, 9);
    assert!(model
        .document()
        .objects
        .iter()
        .any(|o| o.id == "keep_stage36"));
}

#[test]
fn cli_schema_and_validation_are_usable_by_other_guide_authors() {
    let schema = Command::new(env!("CARGO_BIN_EXE_ember-blueprint"))
        .arg("schema")
        .output()
        .unwrap();
    assert!(schema.status.success());
    assert_eq!(schema.stdout, ember_blueprint::JSON_SCHEMA.as_bytes());
    let valid = Command::new(env!("CARGO_BIN_EXE_ember-blueprint"))
        .arg("validate")
        .arg(path("examples/watchtower.blueprint.json"))
        .output()
        .unwrap();
    assert!(valid.status.success());
    assert!(String::from_utf8_lossy(&valid.stdout).contains("source=example:watchtower"));
}

#[test]
fn cli_failure_has_no_partial_success_payload() {
    let result = Command::new(env!("CARGO_BIN_EXE_ember-blueprint"))
        .arg("convert-highcrown")
        .arg(path("tests/fixtures/highcrown-recorded.json"))
        .args(["test:cli", "0"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    let usage = Command::new(env!("CARGO_BIN_EXE_ember-blueprint"))
        .output()
        .unwrap();
    assert!(!usage.status.success());
    assert!(String::from_utf8_lossy(&usage.stderr).contains("usage:"));
}
