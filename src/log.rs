//! Headroom's fixed filename adapter.
use std::path::{Path, PathBuf};
/// Headroom log filename.
pub const LOG_FILE_NAME: &str = "enshrouded-headroom.log";
/// Start logging with the existing filename and fallback folder.
pub fn init(preferred: &Path, level: ::log::LevelFilter) -> PathBuf {
    emberkit::logging::init_named(LOG_FILE_NAME, "enshrouded-headroom", preferred, level)
}
