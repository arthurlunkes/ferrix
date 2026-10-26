//! Process-level tuning, the equivalent of nginx's `worker_rlimit_nofile` and
//! `worker_cpu_affinity`.

use crate::config::CpuAffinity;

/// Raises the open-file limit (`RLIMIT_NOFILE`) to `wanted`. The soft limit can always go
/// up to the hard limit; going past it needs root (or `CAP_SYS_RESOURCE`). Returns the
/// soft limit actually in effect.
#[cfg(unix)]
// `rlim_t` is u64 on Linux but not on every Unix, so the casts stay.
#[allow(clippy::unnecessary_cast)]
pub fn set_nofile_limit(wanted: u64) -> Result<u64, String> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return Err(format!("getrlimit: {}", std::io::Error::last_os_error()));
    }
    let wanted = wanted as libc::rlim_t;
    let mut next = libc::rlimit {
        rlim_cur: wanted,
        rlim_max: lim.rlim_max.max(wanted),
    };
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &next) } != 0 {
        // Not allowed to raise the hard limit: settle for the highest soft limit we can.
        next.rlim_max = lim.rlim_max;
        next.rlim_cur = wanted.min(lim.rlim_max);
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &next) } != 0 {
            return Err(format!("setrlimit: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(next.rlim_cur as u64)
}

#[cfg(not(unix))]
pub fn set_nofile_limit(_wanted: u64) -> Result<u64, String> {
    Err("worker_rlimit_nofile is only supported on Unix".into())
}

/// The core each worker should be pinned to, or `None` when affinity is off.
pub fn worker_cores(affinity: &CpuAffinity, workers: usize) -> Option<Vec<usize>> {
    let pool = match affinity {
        CpuAffinity::Off => return None,
        CpuAffinity::Cores(c) if !c.is_empty() => c.clone(),
        CpuAffinity::Cores(_) => return None,
        CpuAffinity::Auto => allowed_cores(),
    };
    if pool.is_empty() {
        return None;
    }
    Some((0..workers).map(|i| pool[i % pool.len()]).collect())
}

/// Cores this process may run on (respects `taskset`, cgroup `cpuset` limits, etc.).
#[cfg(target_os = "linux")]
fn allowed_cores() -> Vec<usize> {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
            return Vec::new();
        }
        (0..libc::CPU_SETSIZE as usize)
            .filter(|&c| libc::CPU_ISSET(c, &set))
            .collect()
    }
}

#[cfg(not(target_os = "linux"))]
fn allowed_cores() -> Vec<usize> {
    Vec::new()
}

/// Pins the calling thread to one core.
#[cfg(target_os = "linux")]
pub fn pin_current_thread(core: usize) -> Result<(), String> {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(core, &mut set);
        if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn pin_current_thread(_core: usize) -> Result<(), String> {
    Err("worker_cpu_affinity is only supported on Linux".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_cores_wrap_around_workers() {
        let cores = worker_cores(&CpuAffinity::Cores(vec![2, 5]), 3).unwrap();
        assert_eq!(cores, vec![2, 5, 2]);
        assert!(worker_cores(&CpuAffinity::Off, 4).is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn auto_uses_allowed_cores() {
        let cores = worker_cores(&CpuAffinity::Auto, 2).unwrap();
        assert_eq!(cores.len(), 2);
        assert!(allowed_cores().contains(&cores[0]));
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::unnecessary_cast)]
    fn nofile_limit_can_be_set_within_the_hard_limit() {
        // Re-apply the current soft limit: lowering it would starve other tests of sockets.
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) };
        let got = set_nofile_limit(lim.rlim_cur as u64).unwrap();
        assert_eq!(got, lim.rlim_cur as u64);
    }

    #[test]
    fn affinity_config_parses() {
        let a: CpuAffinity = serde_json::from_str("\"auto\"").unwrap();
        assert_eq!(a, CpuAffinity::Auto);
        let c: CpuAffinity = serde_json::from_str("[0,1]").unwrap();
        assert_eq!(c, CpuAffinity::Cores(vec![0, 1]));
        assert!(serde_json::from_str::<CpuAffinity>("\"sometimes\"").is_err());
    }
}
