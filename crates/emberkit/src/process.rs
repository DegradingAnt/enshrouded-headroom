//! Process identity and target gating for native mods.
use crate::build_id::Build;
use std::path::Path;
/// The positively identified process role, or an unrecognized host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Process {
    /// Enshrouded's rendering client.
    Client,
    /// Enshrouded's dedicated server.
    DedicatedServer,
    /// An unrecognized executable, including development smoke hosts.
    Unknown,
}
/// Classify a process without reading a file.
pub fn classify(path: Option<&Path>, build: Option<&Build>) -> Process {
    if matches!(
        build,
        Some(Build::Known("d7baa768-server" | "1283531-server"))
    ) {
        return Process::DedicatedServer;
    }
    let name = path
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if name.eq_ignore_ascii_case("enshrouded_server.exe") {
        Process::DedicatedServer
    } else if name.eq_ignore_ascii_case("enshrouded.exe")
        || matches!(
            build,
            Some(Build::Known(
                "f68140a3" | "f68140a3-steam" | "1283531-steam"
            ))
        )
    {
        Process::Client
    } else {
        Process::Unknown
    }
}
/// Start only on a supported target. Unknown hosts preserve existing build-gated behavior.
pub fn start_for_target<T>(
    process: Process,
    targets: &[Process],
    name: &str,
    mut emit: impl FnMut(&str),
    start: impl FnOnce() -> T,
) -> Option<T> {
    if process != Process::Unknown && !targets.contains(&process) {
        let role = match process {
            Process::DedicatedServer => "a dedicated server",
            Process::Client => "a client",
            Process::Unknown => return Some(start()),
        };
        emit(&format!("{name}: not used on {role}"));
        None
    } else {
        Some(start())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_and_known_server_identity() {
        assert_eq!(
            classify(Some(Path::new("C:/host/ENSHROUDED.EXE")), None),
            Process::Client
        );
        assert_eq!(
            classify(Some(Path::new("C:/host/Enshrouded_Server.exe")), None),
            Process::DedicatedServer
        );
        assert_eq!(
            classify(
                Some(Path::new("renamed.exe")),
                Some(&Build::Known("d7baa768-server"))
            ),
            Process::DedicatedServer
        );
        assert_eq!(
            classify(Some(Path::new("asi_host.exe")), None),
            Process::Unknown
        );
    }
    /// Even a renamed verified server must never start a client-only rendering mod.
    #[test]
    fn patch_1283531_server_never_starts_client_mods() {
        let role = classify(
            Some(Path::new("renamed.exe")),
            Some(&Build::Known("1283531-server")),
        );
        assert_eq!(role, Process::DedicatedServer);
        for path in [None, Some(Path::new("enshrouded.exe"))] {
            assert_eq!(
                classify(path, Some(&Build::Known("1283531-server"))),
                Process::DedicatedServer
            );
        }
        let mut lines = Vec::new();
        assert_eq!(
            start_for_target(
                role,
                &[Process::Client],
                "enshrouded-headroom",
                |line| lines.push(line.to_owned()),
                || panic!("server must not enter renderer startup")
            ),
            None::<()>
        );
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn rejected_target_emits_once_without_starting() {
        let mut lines = Vec::new();
        assert_eq!(
            start_for_target(
                Process::DedicatedServer,
                &[Process::Client],
                "enshrouded-headroom",
                |s| lines.push(s.to_owned()),
                || panic!("must not start")
            ),
            None::<()>
        );
        assert_eq!(
            lines,
            ["enshrouded-headroom: not used on a dedicated server"]
        );
    }
}
