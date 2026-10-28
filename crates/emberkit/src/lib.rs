//! Shared Enshrouded infrastructure, without loader exports.
pub mod build_id;
pub mod config;
pub mod guard;
pub mod hooks;
pub mod iat;
pub mod logging;
pub mod patch;
pub mod scan;

/// Process identity and target-aware startup.
pub mod process;
pub use process::Process;
/// Identify the current executable by filename and known build identity.
pub fn process() -> Process {
    let path = std::env::current_exe().ok();
    let build = path
        .as_deref()
        .and_then(|p| build_id::sha256_file(p).ok())
        .map(build_id::classify);
    process::classify(path.as_deref(), build.as_ref())
}
