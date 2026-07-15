//! Process identity and liveness — the POSIX primitives the daemon lifecycle is built on.
//!
//! Stale-lock takeover and dead-client sweeping both reduce to one question: *is pid N alive?*
//! The answer is `kill(pid, 0)` — it sends no signal, it only reports whether the process exists
//! and we may signal it. `EPERM` (exists but not ours) counts as **alive**: a foreign process is
//! very much not a stale lock to steal.

/// This process's pid.
pub fn own_pid() -> i32 {
    std::process::id() as i32
}

/// This process's parent pid, captured for the PPID watchdog. On POSIX this is `getppid()`.
#[allow(unsafe_code)] // one libc FFI call; see SAFETY.
pub fn parent_pid() -> i32 {
    // SAFETY: `getppid` takes no arguments, touches no memory, and cannot fail.
    unsafe { libc::getppid() }
}

/// Is `pid` a live process? `kill(pid, 0)` sends nothing; it just probes existence.
///
/// - returns 0 → the process exists and is signalable → **alive**
/// - `EPERM` → it exists but belongs to someone else → **alive** (never a lock to steal)
/// - `ESRCH` (or anything else) → no such process → **dead**
///
/// A non-positive pid is never alive (guards against a malformed pidfile with pid 0 or -1, which
/// `kill` would otherwise interpret as "the whole process group").
#[allow(unsafe_code)] // one libc FFI call; see SAFETY.
pub fn is_alive(pid: i32) -> bool {
    if pid <= 1 {
        return false;
    }
    // SAFETY: `kill` with signal 0 performs only the permission/existence check; no signal is
    // delivered and no memory is touched.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn own_process_is_alive() {
        assert!(is_alive(own_pid()));
    }

    #[test]
    fn pid_zero_and_one_and_negative_are_never_alive() {
        // pid 0 = "this process group", pid 1 = init (unstealable) — neither is a stale daemon.
        assert!(!is_alive(0));
        assert!(!is_alive(1));
        assert!(!is_alive(-1));
    }

    #[test]
    fn a_reaped_child_pid_is_dead() {
        // Spawn a process that exits immediately, reap it, then its pid must read dead.
        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id() as i32;
        let mut child = child;
        child.wait().unwrap();
        assert!(!is_alive(pid), "a reaped pid is dead");
    }

    #[test]
    fn parent_pid_is_positive() {
        assert!(parent_pid() > 0);
    }
}
