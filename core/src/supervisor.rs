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
use tokio::signal::unix::{signal, SignalKind};
use tracing::{error, info, warn};

use crate::cli::Args;

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

fn send(pid: Option<u32>, sig: libc::c_int) {
    if let Some(pid) = pid {
        unsafe {
            libc::kill(pid as libc::pid_t, sig);
        }
    }
}

pub async fn run(_args: &Args, pid_file: &std::path::Path) -> ExitCode {
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

    let (mut hup, mut term) = match (
        signal(SignalKind::hangup()),
        signal(SignalKind::terminate()),
    ) {
        (Ok(h), Ok(t)) => (h, t),
        _ => {
            error!("supervisor: cannot install signal handlers");
            return ExitCode::FAILURE;
        }
    };

    let _ = std::fs::write(pid_file, std::process::id().to_string());
    let mut backoff = FIRST_BACKOFF;

    let code = loop {
        let started = Instant::now();
        let mut child = match tokio::process::Command::new(&exe).args(&child_args).spawn() {
            Ok(c) => c,
            Err(e) => {
                error!("supervisor: failed to start the server process: {}", e);
                break ExitCode::FAILURE;
            }
        };
        let pid = child.id();

        let status = loop {
            tokio::select! {
                status = child.wait() => break status,
                _ = hup.recv() => send(pid, libc::SIGHUP),
                _ = term.recv() => {
                    send(pid, libc::SIGTERM);
                    let _ = child.wait().await;
                    let _ = std::fs::remove_file(pid_file);
                    return ExitCode::SUCCESS;
                }
                _ = tokio::signal::ctrl_c() => {
                    // The terminal sends Ctrl+C to the whole process group, so the
                    // child is already shutting down; just wait for it.
                    let _ = child.wait().await;
                    let _ = std::fs::remove_file(pid_file);
                    return ExitCode::SUCCESS;
                }
            }
        };

        use std::os::unix::process::ExitStatusExt;
        match status {
            Ok(s) if s.success() => break ExitCode::SUCCESS,
            Ok(s) if s.code() == Some(EXIT_FATAL as i32) => {
                // Restarting would fail the same way; the child already logged why.
                break ExitCode::FAILURE;
            }
            Ok(s) => {
                let why = match (s.code(), s.signal()) {
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
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                info!("server process restarted");
            }
            Err(e) => {
                error!("supervisor: lost track of the server process: {}", e);
                break ExitCode::FAILURE;
            }
        }
    };
    let _ = std::fs::remove_file(pid_file);
    code
}
