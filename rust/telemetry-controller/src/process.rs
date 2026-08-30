//! Process-liveness checks, used to detect a stale PID file (native
//! runtime) instead of assuming a recorded PID is still valid.

use sysinfo::{Pid, System};

/// Return `true` if a process with `pid` currently exists.
pub fn is_process_running(pid: u32) -> bool {
    let mut system = System::new();
    let pid = Pid::from_u32(pid);
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_reported_as_running() {
        assert!(is_process_running(std::process::id()));
    }

    #[test]
    fn an_implausibly_large_pid_is_reported_as_not_running() {
        // PIDs this large are not assignable on any supported platform, so
        // this should reliably read back as "not found" rather than
        // flaking based on whatever happens to be running on the test
        // machine.
        assert!(!is_process_running(u32::MAX - 1));
    }
}
