//! Deterministic filesystem locations for a project's daemon.
//!
//! Every path here is a **pure function of the project root** — two processes that resolve the
//! same root land on the same socket, lockfile, and pidfile with no shared state. That is what
//! lets an independently-launched proxy find (or elect) the one daemon for a project.
//!
//! # The socket-path length trap
//!
//! A Unix domain socket path is bounded by `sockaddr_un.sun_path` — ~104 bytes on macOS, 108 on
//! Linux. A deep project (`/Users/.../very/long/path/.selene/daemon.sock`) can blow that and the
//! `bind` fails with a cryptic `EINVAL`. So when the in-project socket path would exceed
//! [`POSIX_SOCKET_PATH_LIMIT`], we relocate to a short `tmpdir` path keyed by a hash of the root.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Above this many bytes, the in-project `.selene/daemon.sock` is too long for `sun_path` and we
/// fall back to a `tmpdir` socket. Matches CodeGraph's `POSIX_SOCKET_PATH_LIMIT` (map §Wire).
pub const POSIX_SOCKET_PATH_LIMIT: usize = 100;

/// A short, stable identifier for a project root: `sha256(root)` truncated to 16 hex chars. Used
/// for the tmp-socket name and the registry filename — never for security, only for uniqueness.
pub fn root_hash(root: &Path) -> String {
    let digest = Sha256::digest(root.to_string_lossy().as_bytes());
    let hex = format!("{digest:x}");
    hex[..16].to_string()
}

/// The project's `.selene/` data directory.
pub fn data_dir(root: &Path) -> PathBuf {
    root.join(".selene")
}

/// The JSON pidfile the elected daemon writes (pid, version, socket, started_at). This one file
/// is **both** the lock and the record: its atomic creation *is* the election (see [`super::lock`]).
pub fn pid_path(root: &Path) -> PathBuf {
    data_dir(root).join("daemon.pid")
}

/// The socket the daemon binds and the proxy connects to. In-project by default; relocated to a
/// `tmpdir` path when the in-project path would overflow `sun_path` ([`POSIX_SOCKET_PATH_LIMIT`]).
pub fn socket_path(root: &Path) -> PathBuf {
    let in_project = data_dir(root).join("daemon.sock");
    if in_project.as_os_str().len() > POSIX_SOCKET_PATH_LIMIT {
        std::env::temp_dir().join(format!("selene-{}.sock", root_hash(root)))
    } else {
        in_project
    }
}

/// The cross-project registry directory: `~/.selene/daemons/`. `selene daemon` lists the records
/// here so it can show daemons for *other* projects, not just the current one.
pub fn registry_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".selene").join("daemons"))
}

/// This project's registry record path: `~/.selene/daemons/<root_hash>.json`.
pub fn registry_path(root: &Path) -> Option<PathBuf> {
    registry_dir().map(|d| d.join(format!("{}.json", root_hash(root))))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn same_root_is_deterministic_different_roots_differ() {
        let a = Path::new("/tmp/project-a");
        let b = Path::new("/tmp/project-b");
        assert_eq!(root_hash(a), root_hash(a), "pure function of the path");
        assert_ne!(root_hash(a), root_hash(b));
        assert_eq!(root_hash(a).len(), 16);
    }

    #[test]
    fn a_short_root_keeps_the_socket_in_project() {
        let root = Path::new("/tmp/p");
        assert_eq!(socket_path(root), root.join(".selene/daemon.sock"));
    }

    #[test]
    fn a_deep_root_relocates_the_socket_to_tmp() {
        // A root long enough that `<root>/.selene/daemon.sock` > 100 bytes.
        let deep = "/Users/someone/".to_string() + &"nested/".repeat(15) + "project";
        let root = PathBuf::from(deep);
        let sock = socket_path(&root);
        assert!(
            sock.starts_with(std::env::temp_dir()),
            "relocated to tmp: {}",
            sock.display()
        );
        assert!(sock.to_string_lossy().contains(&root_hash(&root)));
        assert!(
            sock.as_os_str().len() <= POSIX_SOCKET_PATH_LIMIT + 20,
            "short enough for sun_path"
        );
    }

    #[test]
    fn pid_and_data_paths_live_under_dot_selene() {
        let root = Path::new("/tmp/p");
        assert_eq!(pid_path(root), root.join(".selene/daemon.pid"));
        assert_eq!(data_dir(root), root.join(".selene"));
    }
}
