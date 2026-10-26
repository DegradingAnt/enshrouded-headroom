//! What the log file holds after the start-up pieces run:
//! - the level limit is obeyed, and a panic caught by `guard` is written to the log and not to
//!   stderr;
//! - the whole start-up (`InitializeASI`, in the plugin DLL that `cargo test` builds) logs its
//!   steps in the plan's order.
//!
//! The `log` crate takes one logger and one panic hook per process, and `cargo test` runs the
//! tests of one file in one process. So each test runs its scenario in a child process: the test
//! binary starts itself again with the test's own environment variable set. That also lets the
//! test look at the child's real stderr, which the test harness would otherwise swallow.

use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use enshrouded_headroom::config::{DEFAULT_INI, INI_FILE_NAME};
use enshrouded_headroom::guard::{guard, install_panic_hook, Feature};
use enshrouded_headroom::log::LOG_FILE_NAME;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

/// Names the folder the child process logs into. Unset in the parent.
const CHILD_ENV: &str = "ENSHROUDED_HEADROOM_STARTUP_LOGGING_DIR";

const TEST_NAME: &str = "level_filter_and_panic_hook_write_the_log_not_stderr";

/// Names the folder the child process of the start-up test loads the plugin from. Unset in the
/// parent.
const START_CHILD_ENV: &str = "ENSHROUDED_HEADROOM_STARTUP_STEPS_DIR";

const START_TEST_NAME: &str = "start_up_logs_one_grass_distance_line_after_object_distance";

/// The file name of the plugin DLL that cargo builds (the crate is also a `cdylib`).
const PLUGIN_DLL: &str = "enshrouded_headroom.dll";

/// The plugin DLL that cargo built with this test.
///
/// `cargo test` builds the library, its `cdylib` included, into `deps`, the folder of this test
/// binary, so that copy is the one from this build. Only `cargo build` also copies it one folder
/// up, so that place is the fallback. Panics, naming both places, if neither has the DLL.
fn built_plugin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    let places = [
        deps.join(PLUGIN_DLL),
        deps.parent().unwrap().join(PLUGIN_DLL),
    ];
    places
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "the plugin DLL is in neither {} nor {}; cargo builds it with the tests because \
                 the crate type list in Cargo.toml has \"cdylib\"",
                places[0].display(),
                places[1].display()
            )
        })
}

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

/// A fresh, empty folder for one test, named `name` and this process's id.
fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("enshrouded-headroom-tests")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run the test `name` alone in a child copy of this test binary, with `env` set to `dir`.
fn run_child(name: &str, env: &str, dir: &Path) -> Output {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(env, dir)
        .output()
        .unwrap()
}

#[test]
fn level_filter_and_panic_hook_write_the_log_not_stderr() {
    if let Some(dir) = std::env::var_os(CHILD_ENV) {
        scenario(Path::new(&dir));
        return;
    }

    let dir = scratch_dir("startup-logging");
    let output = run_child(TEST_NAME, CHILD_ENV, &dir);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Once the panic hook is installed, a failure in the child is written to the log and not to
    // stderr, so the log (if there is one) belongs in the message.
    let log_text = std::fs::read_to_string(dir.join(LOG_FILE_NAME));
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

/// The work of the start-up test's child: load the plugin DLL from `dir` and call its
/// `InitializeASI`, as Ultimate ASI Loader does. The DLL is never unloaded: its start-up has put
/// its own function into this exe's `GetProcAddress` import slot.
fn load_plugin_and_start(dir: &Path) {
    let path = dir.join(PLUGIN_DLL);
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let module = unsafe { LoadLibraryW(wide.as_ptr()) };
    assert!(
        !module.is_null(),
        "LoadLibraryW failed for {}",
        path.display()
    );
    // SAFETY: `module` is a live module handle and the name is NUL-terminated.
    let init = unsafe { GetProcAddress(module, c"InitializeASI".as_ptr().cast()) }
        .expect("the plugin exports InitializeASI");
    // SAFETY: the export is `extern "C" fn()` (src/lib.rs), the contract with the ASI loader.
    let init: extern "C" fn() = unsafe { std::mem::transmute(init) };
    init();
}

/// The message of a log line: the text after `[   0.001s INFO ] `.
fn message(line: &str) -> &str {
    line.split_once("] ").map_or(line, |(_, text)| text)
}

#[test]
fn start_up_logs_one_grass_distance_line_after_object_distance() {
    if let Some(dir) = std::env::var_os(START_CHILD_ENV) {
        load_plugin_and_start(Path::new(&dir));
        return;
    }

    // A copy of the DLL in a scratch folder, so the plugin finds its ini there and writes its log
    // there.
    let built = built_plugin();
    let dir = scratch_dir("startup-steps");
    std::fs::copy(&built, dir.join(PLUGIN_DLL))
        .unwrap_or_else(|e| panic!("copy {}: {e}", built.display()));
    // This test binary is not a known game build, so code patches need `AllowUnknownBuild = 1`.
    // Then every code step scans this exe, finds no game code, and says so.
    assert!(
        DEFAULT_INI.contains("AllowUnknownBuild=0")
            && DEFAULT_INI.contains("EnableScatterDistance=1")
    );
    let ini = DEFAULT_INI.replace("AllowUnknownBuild=0", "AllowUnknownBuild=1");
    std::fs::write(dir.join(INI_FILE_NAME), ini).unwrap();

    let output = run_child(START_TEST_NAME, START_CHILD_ENV, &dir);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = std::fs::read_to_string(dir.join(LOG_FILE_NAME))
        .unwrap_or_else(|e| format!("<no log file: {e}>"));
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "child failed: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}\nlog:\n{log}",
        output.status
    );

    let messages: Vec<&str> = log.lines().map(message).collect();
    let at = |prefix: &str| -> Vec<usize> {
        (0..messages.len())
            .filter(|&i| messages[i].starts_with(prefix))
            .collect()
    };
    assert!(
        messages.contains(
            &"AllowUnknownBuild = 1: code patches are on for a build this plugin \
                            has not been checked against; each patch still checks the game's \
                            bytes before it writes"
        ),
        "code patches must be on, so the grass step runs its scan:\n{log}"
    );
    let grass = at("grass distance:");
    let object = at("object distance:");
    let probe = at("scatter probe:");
    assert_eq!(probe.len(), 1, "one scatter probe line:\n{log}");
    assert_eq!(grass.len(), 1, "one grass distance line:\n{log}");
    assert_eq!(object.len(), 1, "one object distance line:\n{log}");
    assert!(
        grass[0] < probe[0],
        "scatter probe follows grass distance:\n{log}"
    );
    assert!(
        object[0] < grass[0],
        "grass distance comes after object distance:\n{log}"
    );
    for hooks in at("hooks:") {
        assert!(
            grass[0] < hooks,
            "grass distance comes before the hooks line:\n{log}"
        );
    }
    assert_eq!(
        messages[grass[0]],
        "grass distance: off: the game code was not found (another tool or a different build \
         may have changed it)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
