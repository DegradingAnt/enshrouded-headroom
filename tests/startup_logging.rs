//! What the log file holds after the start-up pieces run: the level limit is obeyed, and a panic
//! caught by `guard` is written to the log and not to stderr.
//!
//! The `log` crate takes one logger and one panic hook per process, and `cargo test` runs the
//! tests of one file in one process. So there is one test, and it runs the scenario in a child
//! process: the test binary starts itself again with `CHILD_ENV` set. That also lets the test look
//! at the child's real stderr, which the test harness would otherwise swallow.

use std::path::{Path, PathBuf};
use std::process::Command;

use enshrouded_headroom::guard::{guard, install_panic_hook, Feature};

/// Names the folder the child process logs into. Unset in the parent.
const CHILD_ENV: &str = "ENSHROUDED_HEADROOM_STARTUP_LOGGING_DIR";

const TEST_NAME: &str = "level_filter_and_panic_hook_write_the_log_not_stderr";

/// The work of the child process: log at two levels, then panic inside a guard.
fn scenario(dir: &Path) {
    let used = enshrouded_headroom::log::init(dir, log::LevelFilter::Warn);
    assert_eq!(
        used, dir,
        "the log must be in the folder that was asked for"
    );

    log::info!("info line below the limit");
    log::warn!("warn line at the limit");

    install_panic_hook();
    let value = guard(Feature::Governor, || 1, || panic!("hook test"));
    assert_eq!(value, 1, "guard must give the fallback value after a panic");
}

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir()
        .join("enshrouded-headroom-tests")
        .join(format!("startup-logging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn level_filter_and_panic_hook_write_the_log_not_stderr() {
    if let Some(dir) = std::env::var_os(CHILD_ENV) {
        scenario(Path::new(&dir));
        return;
    }

    let dir = scratch_dir();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, &dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Once the panic hook is installed, a failure in the child is written to the log and not to
    // stderr, so the log (if there is one) belongs in the message.
    let log_text = std::fs::read_to_string(dir.join(enshrouded_headroom::log::LOG_FILE_NAME));
    let log_shown = log_text
        .as_deref()
        .unwrap_or("<no log file, or it cannot be read>");
    assert!(
        output.status.success(),
        "child failed: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}\nlog:\n{log_shown}",
        output.status
    );
    assert!(
        stdout.contains("1 passed"),
        "the child must have run the scenario\nstdout:\n{stdout}\nlog:\n{log_shown}"
    );

    // (b) The level limit: `info!` is cut, `warn!` is kept.
    let log = log_text.unwrap();
    assert!(
        !log.contains("info line below the limit"),
        "an info line got through a Warn limit:\n{log}"
    );
    assert!(log.contains("warn line at the limit"), "{log}");

    // (c) The panic hook: the message and where it happened are in the log ...
    let panic_line = log
        .lines()
        .find(|line| line.contains("hook test") && line.contains("panic in thread"))
        .unwrap_or_else(|| panic!("no panic line in the log:\n{log}"));
    let after_file = panic_line
        .split_once("startup_logging.rs:")
        .unwrap_or_else(|| panic!("no file name in {panic_line:?}"))
        .1;
    assert!(
        after_file.starts_with(|c: char| c.is_ascii_digit()),
        "no line number in {panic_line:?}"
    );
    //     ... and so is the feature that was switched off ...
    assert!(log.contains("Governor disabled"), "{log}");
    //     ... and nothing went to stderr.
    assert!(
        !stderr.contains("hook test") && !stderr.contains("panicked"),
        "panic text reached stderr:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
