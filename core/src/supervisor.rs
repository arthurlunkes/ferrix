use std::process::ExitCode;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

pub const EXIT_FATAL: u8 = 78;

const FIRST_BACKOFF: Duration = Duration::from_millis(100);
const MAX_BACKOFF: Duration = Duration::from_secs(5);
const STABLE_AFTER: Duration = Duration::from_secs(30);

pub fn enabled() -> bool {
    !matches!(
        std::env::var("FERRIX_SUPERVISOR").as_deref(),
        Ok("0") | Ok("false") | Ok("off")
    )
}

fn send(pid: u32, sig: libc::c_int) {
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

fn handled_signals() -> libc::sigset_t {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for sig in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT, libc::SIGCHLD] {
            libc::sigaddset(&mut set, sig);
        }
        set
    }
}

fn wait_signal(set: &libc::sigset_t, timeout: Option<Duration>) -> Option<libc::c_int> {
    match timeout {
        None => {
            let mut sig = 0;
            (unsafe { libc::sigwait(set, &mut sig) } == 0).then_some(sig)
        }
        Some(t) => wait_signal_for(set, t),
    }
}

#[cfg(target_os = "linux")]
fn wait_signal_for(set: &libc::sigset_t, t: Duration) -> Option<libc::c_int> {
    let ts = libc::timespec {
        tv_sec: t.as_secs().min(i32::MAX as u64) as _,
        tv_nsec: t.subsec_nanos() as libc::c_long,
    };
    let sig = unsafe { libc::sigtimedwait(set, std::ptr::null_mut(), &ts) };
    (sig > 0).then_some(sig)
}

#[cfg(not(target_os = "linux"))]
fn wait_signal_for(set: &libc::sigset_t, t: Duration) -> Option<libc::c_int> {
    let deadline = Instant::now() + t;
    loop {
        unsafe {
            let mut pending: libc::sigset_t = std::mem::zeroed();
            libc::sigpending(&mut pending);
            let ours = [libc::SIGHUP, libc::SIGTERM, libc::SIGINT, libc::SIGCHLD]
                .into_iter()
                .any(|s| libc::sigismember(&pending, s) == 1 && libc::sigismember(set, s) == 1);
            if ours {
                let mut sig = 0;
                return (libc::sigwait(set, &mut sig) == 0).then_some(sig);
            }
        }
        let left = deadline.checked_duration_since(Instant::now())?;
        std::thread::sleep(left.min(Duration::from_millis(20)));
    }
}

pub fn run(pid_file: &std::path::Path) -> ExitCode {
    use std::os::unix::process::{CommandExt, ExitStatusExt};

    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            error!("supervisor: cannot find the ferrix executable: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let child_args: Vec<String> = std::env::args()
        .skip(1)
        .chain(std::iter::once("--worker-process".to_string()))
        .collect();

    let set = handled_signals();
    unsafe {
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
    let _ = std::fs::write(pid_file, std::process::id().to_string());
    let mut backoff = FIRST_BACKOFF;

    let code = 'supervise: loop {
        let started = Instant::now();
        let mut command = std::process::Command::new(&exe);
        command.args(&child_args);
        unsafe {
            command.pre_exec(move || {
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
                Ok(())
            });
        }
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                error!("supervisor: failed to start the server process: {}", e);
                break ExitCode::FAILURE;
            }
        };
        let pid = child.id();

        let status = loop {
            match wait_signal(&set, None) {
                Some(libc::SIGHUP) => send(pid, libc::SIGHUP),
                Some(sig @ (libc::SIGTERM | libc::SIGINT)) => {
                    send(pid, sig);
                    let _ = child.wait();
                    break 'supervise ExitCode::SUCCESS;
                }
                Some(libc::SIGCHLD) => {
                    if let Ok(Some(status)) = child.try_wait() {
                        break status;
                    }
                }
                _ => {}
            }
        };

        if status.success() {
            break ExitCode::SUCCESS;
        }
        if status.code() == Some(EXIT_FATAL as i32) {
            break ExitCode::FAILURE;
        }
        let why = match (status.code(), status.signal()) {
            (Some(c), _) => format!("exited with status {}", c),
            (_, Some(sig)) => format!("was killed by signal {}", sig),
            _ => "stopped".to_string(),
        };
        if started.elapsed() >= STABLE_AFTER {
            backoff = FIRST_BACKOFF;
        }
        warn!(
            "server process {}; restarting in {} ms",
            why,
            backoff.as_millis()
        );

        let deadline = Instant::now() + backoff;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if let Some(libc::SIGTERM | libc::SIGINT) = wait_signal(&set, Some(left)) {
                break 'supervise ExitCode::SUCCESS;
            }
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
        info!("server process restarted");
    };
    let _ = std::fs::remove_file(pid_file);
    code
}
