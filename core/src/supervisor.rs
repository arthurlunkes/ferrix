//! Master process, like nginx's: `ferrix run` keeps a small parent process that starts
//! the real server as a child and restarts it if it dies, so a crash (Ferrix is built
//! with `panic = "abort"`) costs the in-flight connections of that moment instead of
//! the whole service.
//!
//! The parent owns the PID file, so `ferrix reload` / `stop` signal it and it forwards
//! SIGHUP / SIGTERM to the child. Set `FERRIX_SUPERVISOR=0` to run the server directly
//! (for example under systemd or Docker, which already restart it).

use std::process::ExitCode;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

/// Exit status the child uses for errors a restart can't fix (invalid config, port
/// already in use...): sysexits' EX_CONFIG.
pub const EXIT_FATAL: u8 = 78;

const FIRST_BACKOFF: Duration = Duration::from_millis(100);
const MAX_BACKOFF: Duration = Duration::from_secs(5);
/// A child that stayed up this long is considered healthy; the backoff starts over.
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

/// The signals the master handles: blocked in the master (so they queue for
/// `sigwait`) and unblocked again in the child before it runs.
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

/// Waits for one of the handled signals, up to `timeout` (`None` = forever).
fn wait_signal(set: &libc::sigset_t, timeout: Option<Duration>) -> Option<libc::c_int> {
    unsafe {
        match timeout {
            None => {
                let mut sig = 0;
                (libc::sigwait(set, &mut sig) == 0).then_some(sig)
            }
            Some(t) => {
                let ts = libc::timespec {
                    tv_sec: t.as_secs() as libc::time_t,
                    tv_nsec: t.subsec_nanos() as libc::c_long,
                };
                let sig = libc::sigtimedwait(set, std::ptr::null_mut(), &ts);
                (sig > 0).then_some(sig)
            }
        }
    }
}

/// Runs the master loop. Deliberately plain: no async runtime and no threads, just
/// `sigwait` and `waitpid`, so the master costs almost no memory.
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
                    // Ctrl+C in a terminal already reached the child (same process
                    // group); forwarding again is harmless and covers `kill -INT`.
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
            // Restarting would fail the same way; the child already logged why.
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

        // Wait out the backoff, but still honour stop requests meanwhile.
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
