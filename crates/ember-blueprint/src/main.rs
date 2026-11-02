//! Bounded local conversion and validation. Output is written only to stdout.
use ember_blueprint::{convert_highcrown, parse, read_bounded, Limits, JSON_SCHEMA};
use std::{ffi::OsString, io::Write, path::Path, process::ExitCode};

/// Run a local data operation and report failures without a panic.
fn main() -> ExitCode {
    match run(std::env::args_os().collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ember-blueprint: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Interpret explicit arguments; no directories or installed files are chosen implicitly.
fn run(args: Vec<OsString>) -> Result<(), Box<dyn std::error::Error>> {
    let limits = Limits::default();
    let bytes = match args.as_slice() {
        [_, command] if command == "schema" => JSON_SCHEMA.as_bytes().to_vec(),
        [_, command, file] if command == "validate" => {
            let model = parse(&read_bounded(Path::new(file), limits)?, limits)?;
            let doc = model.document();
            let primitives = doc.objects.iter().fold(0_usize, |n, o| n.saturating_add(o.primitives.len()));
            format!(
                "valid: source={} sequence={} sha256={} objects={} primitives={}\n",
                doc.source.id, doc.source.revision.sequence, doc.source.revision.content_sha256,
                doc.objects.len(), primitives,
            ).into_bytes()
        }
        [_, command, file, source_id, sequence] if command == "convert-highcrown" => {
            let source_id = source_id.to_str().ok_or("source ID must be UTF-8 ASCII")?;
            let sequence: u64 = sequence.to_str().ok_or("sequence must be UTF-8")?.parse()?;
            let source = read_bounded(Path::new(file), limits)?;
            let model = convert_highcrown(&source, source_id, sequence, limits)?;
            let mut bytes = model.to_json()?;
            bytes.push(b'\n');
            bytes
        }
        _ => return Err("usage: ember-blueprint schema | validate FILE | convert-highcrown PLAN SOURCE_ID SEQUENCE".into()),
    };
    std::io::stdout().lock().write_all(&bytes)?;
    Ok(())
}
