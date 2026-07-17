//! Platform memory probe: process RSS + total system RAM, via `sysinfo`.
//! Dev-diagnostics only (behind `diag::enabled()` at call sites). Never panics:
//! any failure returns 0 so the memory overlay simply shows 0 rather than
//! crashing the app.

use std::sync::Mutex;
use std::sync::OnceLock;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

/// Total physical RAM in bytes (queried once; it does not change at runtime).
#[allow(dead_code)] // called from diag_mem (Task 2), not wired into main.rs yet
pub fn total_ram_bytes() -> u64 {
    static TOTAL: OnceLock<u64> = OnceLock::new();
    *TOTAL.get_or_init(|| {
        let sys = System::new_with_specifics(
            RefreshKind::new().with_memory(sysinfo::MemoryRefreshKind::everything()),
        );
        sys.total_memory()
    })
}

/// Resident set size (bytes) of the current process. Refreshes a cached
/// single-process `System` each call; cheap enough at the ~1/sec diag cadence.
/// Returns 0 if the process cannot be read.
#[allow(dead_code)] // called from diag_mem (Task 2), not wired into main.rs yet
pub fn process_rss_bytes() -> u64 {
    static SYS: OnceLock<Mutex<System>> = OnceLock::new();
    let pid = Pid::from_u32(std::process::id());
    let lock = SYS.get_or_init(|| Mutex::new(System::new()));
    let Ok(mut sys) = lock.lock() else {
        return 0;
    };
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::new().with_memory(),
    );
    sys.process(pid).map(|p| p.memory()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_ram_is_positive_on_this_host() {
        assert!(
            total_ram_bytes() > 0,
            "a real host reports nonzero total RAM"
        );
    }

    #[test]
    fn process_rss_is_positive_for_this_process() {
        assert!(process_rss_bytes() > 0, "this process has a nonzero RSS");
    }
}
