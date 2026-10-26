#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use ferrix::{admin, config, health, http as proxy, metrics, tls as tls_plan};

use clap::Parser;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use admin::create_admin_router;
use admin::handlers::AdminState;
use config::store::ConfigStore;
use config::validate::validate;
use config::{ConfigLoad, FerrixConfig};
use health::HealthChecker;
use metrics::MetricsCollector;
use proxy::ProxyEngine;

#[derive(Parser, Debug)]
#[command(
    name = "ferrix",
    version,
    about = "⚡ Ferrix: High-performance standalone reverse proxy & web server in Rust (Nginx-inspired)",
    after_help = "💡 Usage examples:\n  ferrix --help            Show all commands and detailed options\n  ferrix run               Start the server in foreground\n  ferrix start             Start the server in background (daemon)\n  ferrix stop              Stop the background server\n  ferrix restart           Restart the background server\n  ferrix reload            Reload configuration without dropping active connections\n  ferrix status            Check if the server is running\n  ferrix -t                Test configuration file syntax\n"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short, long, default_value = "ferrix.json", global = true)]
    config: PathBuf,

    #[arg(long, global = true)]
    admin_listen: Option<String>,

    #[arg(long, global = true)]
    http_listen: Option<String>,

    #[arg(short = 't', long, global = true)]
    test: bool,

    #[arg(short = 's', long, value_name = "SIGNAL")]
    signal: Option<String>,
}

#[derive(clap::Subcommand, Debug, Clone)]
enum Commands {
    Run,

    Start,

    Stop,

    Restart,

    Reload,

    Status,

    Test,

    Import {
        #[arg(long)]
        from: PathBuf,

        #[arg(long)]
        out: Option<PathBuf>,

        #[arg(long)]
        report: Option<PathBuf>,
    },
}

#[cfg(unix)]
fn spawn_workers(
    addr: SocketAddr,
    workers: usize,
    engine: Arc<ProxyEngine>,
    tls: Option<tokio_rustls::TlsAcceptor>,
) {
    let cores = ferrix::os::worker_cores(
        &engine.store.config().http.worker_cpu_affinity,
        workers.max(1),
    );
    for worker in 0..workers.max(1) {
        let core = cores.as_ref().map(|c| c[worker]);
        // Each worker gets its own handle so per-request Arc clones touch a counter
        // owned by that thread instead of one shared by every core.
        let engine = Arc::new((*engine).clone());
        let tls = tls.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("ferrix-worker-{}", worker))
            .spawn(move || {
                if let Some(core) = core {
                    if let Err(e) = ferrix::os::pin_current_thread(core) {
                        warn!(
                            "Worker {} could not be pinned to CPU {}: {}",
                            worker, core, e
                        );
                    }
                }
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    // File reads that can't use sendfile are rare; don't let bursts
                    // grow a large pool of idle threads (each with its own stack/heap).
                    .max_blocking_threads(2)
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        error!("Worker {} failed to create runtime: {}", worker, e);
                        return;
                    }
                };
                let listener = match bind_reuseport(addr) {
                    Ok(l) => l,
                    Err(e) => return error!("Failed to bind Proxy listener to {}: {}", addr, e),
                };
                runtime.block_on(async move {
                    ferrix::event::clock::enable_local_timers();
                    match tokio::net::TcpListener::from_std(listener) {
                        Ok(l) => ferrix::event::accept::serve(l, engine, addr.port(), tls).await,
                        Err(e) => error!("Proxy listener on {}: {}", addr, e),
                    }
                });
            });
        if let Err(e) = spawned {
            error!("Failed to start worker {} of {}: {}", worker, addr, e);
        }
    }
}

#[cfg(unix)]
fn bind_reuseport(addr: SocketAddr) -> std::io::Result<std::net::TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(4096)?;
    Ok(socket.into())
}

#[cfg(not(unix))]
fn spawn_workers(
    addr: SocketAddr,
    _workers: usize,
    engine: Arc<ProxyEngine>,
    tls: Option<tokio_rustls::TlsAcceptor>,
) {
    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => ferrix::event::accept::serve(l, engine, addr.port(), tls).await,
            Err(e) => error!("Failed to bind Proxy listener to {}: {}", addr, e),
        }
    });
}

fn spawn_plain_listener(addr: SocketAddr, engine: Arc<ProxyEngine>, workers: usize) {
    info!(
        "Reverse Proxy Traffic listening on http://{} ({} workers)",
        addr, workers
    );
    spawn_workers(addr, workers, engine, None);
}

fn spawn_tls_listener(
    addr: SocketAddr,
    engine: Arc<ProxyEngine>,
    source: tls_plan::TlsSource,
    workers: usize,
) -> Result<(), String> {
    let resolver = ferrix::tls::sni::SniResolver::new(engine.store.clone(), addr.port())
        .and_then(ferrix::tls::sni::tls_acceptor)
        .map_err(|e| {
            format!(
                "Failed to load TLS certificates for port {} (first: '{}', cert: {}): {}. \
                 This port will not start; other ports continue normally.",
                addr.port(),
                source.vhost_name,
                source.cert_path,
                e
            )
        })?;
    info!(
        "Reverse Proxy Traffic listening on https://{} (SNI certificates, {} workers)",
        addr, workers
    );
    spawn_workers(addr, workers, engine, Some(resolver));
    Ok(())
}

fn main() -> ExitCode {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(configured_worker_threads())
        .enable_all()
        .build()
        .expect("failed to create Tokio runtime");
    runtime.block_on(async_main())
}

#[cfg(unix)]
fn configured_worker_threads() -> usize {
    // On Unix the proxy runs on its own per-core worker threads; this shared runtime
    // only carries the admin API, health checks, ACME and TCP/UDP streams.
    2
}

#[cfg(not(unix))]
fn configured_worker_threads() -> usize {
    let default = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let Ok(args) = Args::try_parse() else {
        return default;
    };
    std::fs::read_to_string(&args.config)
        .ok()
        .and_then(|data| serde_json::from_str::<serde_json::Value>(&data).ok())
        .and_then(|v| v.get("http")?.get("worker_threads")?.as_u64())
        .filter(|&n| n > 0)
        .map(|n| n as usize)
        .unwrap_or(default)
}

async fn async_main() -> ExitCode {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let raw_args: Vec<String> = std::env::args().collect();

    if raw_args.len() == 1 {
        print_welcome_guide();
        return ExitCode::SUCCESS;
    }

    let args = Args::parse();

    if args.test {
        return match run_test(&args.config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", e);
                ExitCode::FAILURE
            }
        };
    }

    if let Some(ref sig) = args.signal {
        return match handle_signal(sig, &args).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", e);
                ExitCode::FAILURE
            }
        };
    }

    match args.command {
        Some(Commands::Test) => match run_test(&args.config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::Import {
            ref from,
            ref out,
            ref report,
        }) => match run_import(from, out.as_deref(), report.as_deref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::Start) => match handle_start(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("Error starting Ferrix: {}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::Stop) => match handle_stop(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::Restart) => match handle_restart(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("Error restarting Ferrix: {}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::Reload) => match handle_reload(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::Status) => match handle_status(&args).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::Run) | None => {
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "ferrix=info,tower_http=info".into()),
                )
                .with(tracing_subscriber::fmt::layer())
                .init();

            match run_server(args).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(message) => {
                    error!("{}", message);
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn print_welcome_guide() {
    println!(
        r#"⚡ Ferrix v{} — High-Performance Standalone Reverse Proxy & Web Server

Usage: ferrix [COMMAND] [OPTIONS]

Main commands:
  run       Start the server in foreground
  start     Start the server in background (daemon)
  stop      Stop the background server
  restart   Restart the background server
  reload    Reload configuration without dropping active connections
  status    Check if the server is currently running
  test      Test configuration file syntax (shortcut: ferrix -t)

Global options:
  -c, --config <FILE>      Configuration file path [default: ferrix.json]
  -t, --test               Test configuration file syntax and exit
  -s, --signal <SIGNAL>    Send signal to Ferrix (stop, reload, restart, status)
  -h, --help               Display all detailed help options
  -V, --version            Display installed Ferrix version

💡 Quick tips:
  • To see all available options:      ferrix --help
  • To test your config syntax:       ferrix -t
  • To start in foreground:           ferrix run
  • To start in background:           ferrix start
"#,
        env!("CARGO_PKG_VERSION")
    );
}

fn pid_file_path(config: &Path) -> PathBuf {
    if let Some(parent) = config.parent() {
        if parent == Path::new("") {
            PathBuf::from(".ferrix.pid")
        } else {
            parent.join(".ferrix.pid")
        }
    } else {
        PathBuf::from(".ferrix.pid")
    }
}

fn read_pid(pid_file: &Path) -> Option<u32> {
    if let Ok(content) = std::fs::read_to_string(pid_file) {
        content.trim().parse::<u32>().ok()
    } else {
        None
    }
}

fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn run_test(config_path: &Path) -> Result<(), String> {
    let config_path_str = config_path.to_string_lossy().to_string();
    let config = FerrixConfig::load(config_path)
        .map_err(|e| format!("ferrix: failed to load {}:\n  {}", config_path_str, e))?;

    let report = validate(&config);
    for issue in report.warnings() {
        eprintln!("ferrix: [warn] {}: {}", issue.path, issue.message);
    }

    if report.is_valid() {
        println!(
            "ferrix: the configuration file {} syntax is ok",
            config_path_str
        );
        println!(
            "ferrix: configuration file {} test is successful",
            config_path_str
        );
        Ok(())
    } else {
        let details = report
            .errors()
            .map(|i| format!("  - {}: {}", i.path, i.message))
            .collect::<Vec<_>>()
            .join("\n");
        Err(format!(
            "ferrix: the configuration file {} test failed:\n{}",
            config_path_str, details
        ))
    }
}

fn run_import(from: &Path, out: Option<&Path>, report_path: Option<&Path>) -> Result<(), String> {
    let result = ferrix::nginx::convert::import_file(from)
        .map_err(|e| format!("ferrix: failed to import {}:\n  {}", from.display(), e))?;

    let json = serde_json::to_string_pretty(&result.config)
        .map_err(|e| format!("ferrix: failed to serialize converted configuration: {}", e))?;

    match out {
        Some(path) => {
            std::fs::write(path, &json)
                .map_err(|e| format!("ferrix: failed to write {}: {}", path.display(), e))?;
            println!("✔ Converted configuration written to {}", path.display());
        }
        None => {
            println!("{}", json);
        }
    }

    if let Some(path) = report_path {
        std::fs::write(path, result.report.to_markdown())
            .map_err(|e| format!("ferrix: failed to write {}: {}", path.display(), e))?;
        println!("✔ Migration report written to {}", path.display());
    }

    println!("\n{}", result.report.to_text_summary());

    let validation = validate(&result.config);
    if !validation.is_valid() {
        println!(
            "\n⚠️  The converted configuration would not yet pass 'ferrix -t':\n{}",
            validation.summary()
        );
    }

    Ok(())
}

fn handle_start(args: &Args) -> Result<(), String> {
    let pid_file = pid_file_path(&args.config);
    if let Some(pid) = read_pid(&pid_file) {
        if is_pid_alive(pid) {
            println!("⚠️  Ferrix is already running (PID: {}).", pid);
            println!("💡 Use 'ferrix status' to check or 'ferrix stop' to stop.");
            return Ok(());
        } else {
            let _ = std::fs::remove_file(&pid_file);
        }
    }

    let (config, _) = match FerrixConfig::load_or_create(&args.config) {
        Ok(c) => match c {
            ConfigLoad::Loaded(cfg) => (cfg, None),
            ConfigLoad::Created {
                config,
                admin_token,
            } => (config, Some(admin_token)),
        },
        Err(e) => return Err(format!("Configuration error: {}", e)),
    };
    let report = validate(&config);
    if !report.is_valid() {
        return Err("Configuration has errors. Run 'ferrix -t' to see details.".into());
    }

    let exe = std::env::current_exe().map_err(|e| format!("Failed to get executable: {}", e))?;
    let log_path = "ferrix.log";
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| format!("Failed to open {}: {}", log_path, e))?;
    let err_file = log_file.try_clone().map_err(|e| e.to_string())?;

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("run");
    cmd.arg("--config").arg(&args.config);
    if let Some(ref l) = args.admin_listen {
        cmd.arg("--admin-listen").arg(l);
    }
    if let Some(ref l) = args.http_listen {
        cmd.arg("--http-listen").arg(l);
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(log_file);
    cmd.stderr(err_file);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn background process: {}", e))?;
    let pid = child.id();
    std::fs::write(&pid_file, pid.to_string())
        .map_err(|e| format!("Failed to write PID file: {}", e))?;

    std::thread::sleep(std::time::Duration::from_millis(250));
    if !is_pid_alive(pid) {
        let _ = std::fs::remove_file(&pid_file);
        return Err(format!(
            "Process exited immediately after starting. Check {}",
            log_path
        ));
    }

    println!("✔ Ferrix successfully started in background (PID: {})", pid);
    println!("  🌐 HTTP Proxy:        http://{}", config.http.listen);
    println!("  ⚙️  Control Plane API: http://{}", config.admin.listen);
    println!("  📄 Configuration:     {}", args.config.display());
    println!("  📝 Logs written to:   {}", log_path);
    println!("\n💡 Use 'ferrix status' to check service or 'ferrix stop' to stop.");

    Ok(())
}

fn handle_stop(args: &Args) -> Result<(), String> {
    let pid_file = pid_file_path(&args.config);
    let pid = match read_pid(&pid_file) {
        Some(p) => p,
        None => return Err("No running Ferrix process found (.ferrix.pid is missing).".into()),
    };

    if !is_pid_alive(pid) {
        let _ = std::fs::remove_file(&pid_file);
        return Err(format!(
            "Process with PID {} is not running. PID file removed.",
            pid
        ));
    }

    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }

        for _ in 0..30 {
            if !is_pid_alive(pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        if is_pid_alive(pid) {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    }

    let _ = std::fs::remove_file(&pid_file);
    println!("✔ Ferrix (PID: {}) stopped successfully.", pid);
    Ok(())
}

fn handle_reload(args: &Args) -> Result<(), String> {
    let pid_file = pid_file_path(&args.config);
    let pid = match read_pid(&pid_file) {
        Some(p) => p,
        None => return Err("No running Ferrix process found (.ferrix.pid is missing).".into()),
    };

    if !is_pid_alive(pid) {
        let _ = std::fs::remove_file(&pid_file);
        return Err(format!("Process with PID {} is not running.", pid));
    }

    run_test(&args.config)?;

    #[cfg(unix)]
    {
        let res = unsafe { libc::kill(pid as i32, libc::SIGHUP) };

        if res == 0 {
            println!("✔ Reload signal sent to Ferrix (PID: {}).", pid);
            println!("  Configuration reloaded without dropping active connections.");
            Ok(())
        } else {
            Err("Failed to send signal to process.".into())
        }
    }
    #[cfg(not(unix))]
    {
        Err("Signal reload not supported on this platform.".into())
    }
}

fn handle_restart(args: &Args) -> Result<(), String> {
    println!("Restarting Ferrix...");
    let _ = handle_stop(args);
    std::thread::sleep(std::time::Duration::from_millis(300));
    handle_start(args)
}

async fn handle_status(args: &Args) -> Result<(), String> {
    let pid_file = pid_file_path(&args.config);
    let pid = read_pid(&pid_file);

    match pid {
        Some(p) if is_pid_alive(p) => {
            println!("● Ferrix is running (PID: {})", p);
            println!("  📄 Configuration:   {}", args.config.display());
            println!("  📝 PID file:        {}", pid_file.display());
            if Path::new("ferrix.log").exists() {
                println!("  📋 Log file:        ferrix.log");
            }

            let admin_url = if let Some(ref listen) = args.admin_listen {
                format!("http://{}", listen)
            } else if let Ok(c) = FerrixConfig::load(&args.config) {
                format!("http://{}", c.admin.listen)
            } else {
                "http://127.0.0.1:8081".to_string()
            };

            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(1))
                .build()
                .ok();

            if let Some(c) = client {
                if let Ok(resp) = c.get(&admin_url).send().await {
                    if resp.status().is_success() {
                        println!("  ⚙️  Control Plane API: {} (Online)", admin_url);
                    }
                }
            }
            println!("\n💡 To stop the server: ferrix stop");
            println!("💡 To reload config:   ferrix reload");
            Ok(())
        }
        _ => {
            if pid.is_some() {
                let _ = std::fs::remove_file(&pid_file);
            }
            println!("○ Ferrix is stopped.");
            println!("\n💡 To start:");
            println!("   ferrix run      (run in foreground)");
            println!("   ferrix start    (run in background)");
            Ok(())
        }
    }
}

async fn handle_signal(signal: &str, args: &Args) -> Result<(), String> {
    match signal.to_lowercase().as_str() {
        "stop" | "quit" => handle_stop(args),
        "reload" | "hup" => handle_reload(args),
        "restart" => handle_restart(args),
        "status" => handle_status(args).await,
        other => Err(format!(
            "Unknown signal '{}'. Valid signals: stop, reload, restart, status",
            other
        )),
    }
}

async fn run_server(args: Args) -> Result<(), String> {
    let config_path = args.config.to_string_lossy().to_string();
    let config_path_buf = args.config.clone();

    info!("Starting Ferrix v{}", env!("CARGO_PKG_VERSION"));
    info!("Loading configuration from: {}", config_path);

    let (mut config, generated_token) = match FerrixConfig::load_or_create(&args.config)? {
        ConfigLoad::Loaded(c) => (c, None),
        ConfigLoad::Created {
            config,
            admin_token,
        } => (config, Some(admin_token)),
    };

    if let Some(addr) = args.admin_listen {
        config.admin.listen = addr;
    }
    if let Some(addr) = args.http_listen {
        config.http.listen = addr;
    }

    let report = validate(&config);
    for issue in report.warnings() {
        warn!(path = %issue.path, "{}", issue.message);
    }

    if !report.is_valid() {
        let details = report
            .errors()
            .map(|i| format!("  - {}: {}", i.path, i.message))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!(
            "Configuration in {} has errors and Ferrix will not start:\n{}",
            config_path, details
        ));
    }

    if let Some(token) = &generated_token {
        info!("──────────────────────────────────────────────────────────────");
        info!(" Initial configuration created at {}", config_path);
        info!(" Generated admin token: {}", token);
        info!(
            " Use it in dashboard or as: Authorization: Bearer {}",
            token
        );
        info!("──────────────────────────────────────────────────────────────");
    }

    let admin_listen = config.admin.listen.clone();
    let proxy_listen = config.http.listen.clone();

    let proxy_host = proxy_listen
        .rsplit_once(':')
        .map(|(host, _)| host.to_string())
        .unwrap_or_else(|| "0.0.0.0".to_string());
    let port_plan = tls_plan::plan_ports(&config);

    if let Some(n) = config.http.worker_rlimit_nofile {
        match ferrix::os::set_nofile_limit(n) {
            Ok(got) if got < n => warn!(
                "worker_rlimit_nofile: wanted {}, the hard limit allows {} (raise it as root or with ulimit)",
                n, got
            ),
            Ok(got) => info!("Open-file limit set to {}", got),
            Err(e) => warn!("worker_rlimit_nofile: {}", e),
        }
    }

    let store = Arc::new(ConfigStore::new(config, config_path.clone()));
    let metrics = MetricsCollector::new(1000);
    metrics.start_rps_ticker();
    let health_checker = Arc::new(HealthChecker::new());

    health_checker
        .clone()
        .start_background_checker(store.clone());

    let proxy_engine = Arc::new(ProxyEngine::new(
        store.clone(),
        metrics.clone(),
        health_checker.clone(),
    ));
    proxy_engine.start_background_sweeper();
    ferrix::tls::acme::AcmeManager::new(store.clone(), proxy_engine.acme_challenges.clone())
        .start();

    let admin_router = create_admin_router(AdminState {
        store: store.clone(),
        metrics: metrics.clone(),
        health_checker: health_checker.clone(),
    });

    let admin_enabled = store.config().admin.enabled;
    let admin_listen_clone = admin_listen.clone();
    let admin_handle = tokio::spawn(async move {
        if !admin_enabled {
            info!("Control Plane API disabled (admin.enabled = false)");
            return;
        }
        match tokio::net::TcpListener::bind(&admin_listen_clone).await {
            Ok(listener) => {
                info!(
                    "Control Plane API listening on http://{}",
                    admin_listen_clone
                );
                if let Err(e) = axum::serve(listener, admin_router).await {
                    error!("Admin server encountered an error: {}", e);
                }
            }
            Err(e) => error!(
                "Failed to bind Admin listener to {}: {}",
                admin_listen_clone, e
            ),
        }
    });

    let proxy_workers = store.config().http.worker_threads;
    let mut proxy_ports_summary = Vec::with_capacity(port_plan.len());
    for plan in &port_plan {
        let addr: SocketAddr = format!("{}:{}", proxy_host, plan.port)
            .parse()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], plan.port)));

        match &plan.tls {
            Some(source) => {
                proxy_ports_summary.push(format!("https://{}", addr));
                if let Err(e) =
                    spawn_tls_listener(addr, proxy_engine.clone(), source.clone(), proxy_workers)
                {
                    error!("{}", e);
                }
            }
            None => {
                proxy_ports_summary.push(format!("http://{}", addr));

                spawn_plain_listener(addr, proxy_engine.clone(), proxy_workers);
            }
        }
    }

    for stream in store.config().streams.clone() {
        proxy_ports_summary.push(format!(
            "{}://{} -> {}",
            match stream.protocol {
                config::StreamProtocol::Tcp => "tcp",
                config::StreamProtocol::Udp => "udp",
            },
            stream.listen,
            stream.upstream
        ));
        ferrix::stream::spawn(proxy_engine.clone(), stream);
    }

    let pid_file = pid_file_path(&args.config);
    let pid = std::process::id();
    let _ = std::fs::write(&pid_file, pid.to_string());

    println!("\n┌──────────────────────────────────────────────────────────────────┐");
    println!("│  ⚡ Ferrix v{:<52}│", env!("CARGO_PKG_VERSION"));
    println!("│     Reverse Proxy & Control Plane running                        │");
    println!("├──────────────────────────────────────────────────────────────────┤");
    for port_url in &proxy_ports_summary {
        println!("│  🌐 Proxy:             {:<42}│", port_url);
    }
    if admin_enabled {
        println!("│  ⚙️  Control Plane API: http://{:<42}│", admin_listen);
    }
    println!("│  📄 Configuration:     {:<42}│", config_path);
    println!("│                                                                  │");
    println!("│  💡 Tip: Run 'ferrix --help' to see all commands.                │");
    println!("│  🛑 Press Ctrl+C to shut down the server.                        │");
    println!("└──────────────────────────────────────────────────────────────────┘\n");

    #[cfg(unix)]
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .map_err(|e| format!("Failed to configure SIGHUP listener: {}", e))?;

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("Failed to configure SIGTERM listener: {}", e))?;

    loop {
        #[cfg(unix)]
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\n🛑 Gracefully shutting down Ferrix... Goodbye!");
                info!("Shutdown signal received (Ctrl+C), gracefully exiting Ferrix...");
                break;
            }
            _ = sigterm.recv() => {
                info!("Shutdown signal received (SIGTERM), gracefully exiting Ferrix...");
                break;
            }
            _ = sighup.recv() => {
                info!("SIGHUP received: reloading configuration from {}", config_path);
                match FerrixConfig::load_or_create(&config_path_buf) {
                    Ok(ConfigLoad::Loaded(new_cfg)) | Ok(ConfigLoad::Created { config: new_cfg, .. }) => {
                        let report = validate(&new_cfg);
                        if report.is_valid() {
                            store.apply(new_cfg);
                            info!("✔ Configuration reloaded successfully!");
                        } else {
                            warn!("New configuration has validation errors; keeping current config.");
                        }
                    }
                    Err(e) => {
                        warn!("Error reloading configuration: {}", e);
                    }
                }
            }
        }

        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c()
                .await
                .map_err(|e| format!("failed to wait for shutdown signal: {}", e))?;
            println!("\n🛑 Gracefully shutting down Ferrix... Goodbye!");
            info!("Shutdown signal received, gracefully exiting Ferrix...");
            break;
        }
    }

    let _ = std::fs::remove_file(&pid_file);
    admin_handle.abort();

    Ok(())
}
