use crate::config::CpuAffinity;

#[cfg(unix)]
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

static TRIM_HOOK: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

pub fn set_trim_hook(hook: fn()) {
    let _ = TRIM_HOOK.set(hook);
}

pub fn trim_allocator() {
    if let Some(hook) = TRIM_HOOK.get() {
        hook();
    }
}

#[cfg(target_os = "linux")]
pub fn release_startup_pages() {
    unsafe extern "C" fn visit(
        info: *mut libc::dl_phdr_info,
        _size: libc::size_t,
        _data: *mut libc::c_void,
    ) -> libc::c_int {
        let info = &*info;
        let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;
        for i in 0..info.dlpi_phnum as usize {
            let ph = &*info.dlpi_phdr.add(i);
            if ph.p_type != libc::PT_LOAD || ph.p_flags & libc::PF_W != 0 {
                continue;
            }
            let start = info.dlpi_addr as usize + ph.p_vaddr as usize;
            let end = start + ph.p_memsz as usize;
            let start = start.div_ceil(page) * page;
            let end = end / page * page;
            if end > start {
                libc::madvise(start as *mut libc::c_void, end - start, libc::MADV_DONTNEED);
            }
        }
        1
    }
    unsafe {
        libc::dl_iterate_phdr(Some(visit), std::ptr::null_mut());
    }
}

#[cfg(not(target_os = "linux"))]
pub fn release_startup_pages() {}

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
