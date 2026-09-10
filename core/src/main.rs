use ferrix::{admin, config, health, metrics, proxy, tls as tls_plan};

use axum::extract::{ConnectInfo, Request, State};
use axum::response::Response;
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use clap::Parser;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use tokio::task::JoinHandle;
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
    about = "⚡ Ferrix: Standalone Reverse Proxy & Web Server de alta performance em Rust (estilo Nginx)",
    after_help = "💡 Exemplos de uso:\n  ferrix --help            Exibe todos os comandos e opções detalhadas\n  ferrix run               Inicia o servidor no terminal atual (foreground)\n  ferrix start             Inicia o servidor em segundo plano (daemon)\n  ferrix stop              Para o servidor em segundo plano\n  ferrix restart           Reinicia o servidor em segundo plano\n  ferrix reload            Recarrega o arquivo de configuração\n  ferrix status            Verifica se o servidor está em execução\n  ferrix -t                Testa a sintaxe do arquivo de configuração\n"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Caminho do arquivo de configuração JSON
    #[arg(short, long, default_value = "ferrix.json", global = true)]
    config: PathBuf,

    /// Sobrescreve o endereço de escuta da API de administração
    #[arg(long, global = true)]
    admin_listen: Option<String>,

    /// Sobrescreve o endereço de escuta do proxy HTTP
    #[arg(long, global = true)]
    http_listen: Option<String>,

    /// Testa a sintaxe do arquivo de configuração e sai (estilo nginx -t)
    #[arg(short = 't', long, global = true)]
    test: bool,

    /// Sinal de controle para o processo em execução (stop, reload, restart, status)
    #[arg(short = 's', long, value_name = "SIGNAL")]
    signal: Option<String>,
}

#[derive(clap::Subcommand, Debug, Clone)]
enum Commands {
    /// Inicia o servidor no terminal atual (foreground)
    Run,
    /// Inicia o servidor em segundo plano (background daemon)
    Start,
    /// Para o servidor em execução em segundo plano
    Stop,
    /// Reinicia o servidor em segundo plano
    Restart,
    /// Recarrega as configurações do servidor sem reiniciar o processo
    Reload,
    /// Exibe o status atual do servidor Ferrix
    Status,
    /// Testa a sintaxe e integridade do arquivo de configuração (atalho: ferrix -t)
    Test,
    /// Converte um nginx.conf existente para o formato do Ferrix
    Import {
        /// Caminho do nginx.conf a importar (segue os `include` que existirem)
        #[arg(long)]
        from: PathBuf,
        /// Onde gravar o JSON convertido. Sem isto, o resultado só é impresso na tela.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Onde gravar o relatório de migração em Markdown, além do resumo no terminal.
        #[arg(long)]
        report: Option<PathBuf>,
    },
}

/// Cada listener de porta tem seu próprio `Router`, mas todos compartilham o mesmo
/// `ProxyEngine` — o pool de upstreams, o limitador de taxa e o cache de clientes HTTP
/// são únicos para o processo inteiro, não duplicados por porta.
#[derive(Clone)]
struct ProxyListenerState {
    engine: Arc<ProxyEngine>,
    listen_port: u16,
    is_tls: bool,
}

#[axum::debug_handler]
async fn proxy_handler(
    State(state): State<ProxyListenerState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
) -> Response {
    state
        .engine
        .handle_request(state.listen_port, state.is_tls, addr, req)
        .await
}

fn proxy_router(engine: Arc<ProxyEngine>, listen_port: u16, is_tls: bool) -> Router {
    Router::new().fallback(proxy_handler).with_state(ProxyListenerState {
        engine,
        listen_port,
        is_tls,
    })
}

/// Sobe um listener HTTP simples numa porta, com a mesma lógica de proxy de sempre.
fn spawn_plain_listener(addr: SocketAddr, engine: Arc<ProxyEngine>) -> JoinHandle<()> {
    let app = proxy_router(engine, addr.port(), false);
    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(listener) => {
                info!("Reverse Proxy Traffic listening on http://{}", addr);
                let service = app.into_make_service_with_connect_info::<SocketAddr>();
                if let Err(e) = axum::serve(listener, service).await {
                    error!("Proxy server on {} encountered an error: {}", addr, e);
                }
            }
            Err(e) => error!("Failed to bind Proxy listener to {}: {}", addr, e),
        }
    })
}

/// Sobe um listener HTTPS: mesma lógica de proxy de `spawn_plain_listener`, só que a
/// conexão TCP passa primeiro pelo handshake TLS. Um certificado ilegível não derruba o
/// processo inteiro — só esta porta fica sem subir, e o motivo vai para o log.
fn spawn_tls_listener(
    addr: SocketAddr,
    engine: Arc<ProxyEngine>,
    source: tls_plan::TlsSource,
) -> JoinHandle<()> {
    let app = proxy_router(engine, addr.port(), true);
    tokio::spawn(async move {
        match RustlsConfig::from_pem_file(&source.cert_path, &source.key_path).await {
            Ok(tls_config) => {
                info!(
                    "Reverse Proxy Traffic listening on https://{} (certificado de '{}')",
                    addr, source.vhost_name
                );
                let service = app.into_make_service_with_connect_info::<SocketAddr>();
                if let Err(e) = axum_server::bind_rustls(addr, tls_config).serve(service).await {
                    error!("Proxy HTTPS server on {} encountered an error: {}", addr, e);
                }
            }
            Err(e) => error!(
                "Falha ao carregar o certificado TLS de '{}' (cert: {}, key: {}) para a porta {}: {}. \
                 Esta porta não vai subir; as demais continuam normalmente.",
                source.vhost_name, source.cert_path, source.key_path, addr.port(), e
            ),
        }
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    // O rustls precisa de exatamente um provedor de criptografia escolhido — e como
    // `reqwest` (cliente, para os upstreams) e `axum-server` (aqui, para os listeners
    // HTTPS) trazem `ring` e `aws-lc-rs` juntos no grafo de dependências, ele não
    // consegue decidir sozinho e entra em pânico na primeira conexão TLS sem isto. Só
    // importa que seja instalado uma vez, antes de qualquer TLS — `ring` evita depender
    // de cmake para compilar em máquinas que não o têm.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let raw_args: Vec<String> = std::env::args().collect();

    // Se o usuário chamou apenas `ferrix` sem argumentos:
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
                eprintln!("Erro ao iniciar Ferrix: {}", e);
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
                eprintln!("Erro ao reiniciar Ferrix: {}", e);
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

Uso: ferrix [COMANDO] [OPÇÕES]

Comandos principais:
  run       Inicia o servidor no terminal atual (foreground)
  start     Inicia o servidor em segundo plano (background/daemon)
  stop      Para o servidor em segundo plano
  restart   Reinicia o servidor em segundo plano
  reload    Recarrega as configurações sem derrubar conexões ativas
  status    Verifica se o servidor está em execução
  test      Testa a sintaxe do arquivo de configuração (atalho: ferrix -t)

Opções globais:
  -c, --config <ARQUIVO>   Caminho do arquivo de configuração [padrão: ferrix.json]
  -t, --test               Testa a sintaxe do arquivo de configuração e sai
  -s, --signal <SINAL>     Envia sinal ao Ferrix (stop, reload, restart, status)
  -h, --help               Exibe todas as opções detalhadas de ajuda
  -V, --version            Exibe a versão instalada do Ferrix

💡 Dicas rápidas:
  • Para ver todas as opções disponíveis:  ferrix --help
  • Para testar a sintaxe da sua config:   ferrix -t
  • Para iniciar agora neste terminal:    ferrix run
  • Para iniciar em segundo plano:        ferrix start
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
        .map_err(|e| format!("ferrix: falha ao carregar {}:\n  {}", config_path_str, e))?;

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

/// `ferrix import --from nginx.conf --out ferrix.json --report report.md`
///
/// Não sobrescreve nada por conta própria: sem `--out`, o JSON só é impresso; sem
/// `--report`, só o resumo aparece no terminal. Quem decide aplicar a configuração
/// resultante é sempre uma ação explícita depois de olhar o relatório.
fn run_import(from: &Path, out: Option<&Path>, report_path: Option<&Path>) -> Result<(), String> {
    let result = ferrix::nginx::convert::import_file(from)
        .map_err(|e| format!("ferrix: falha ao importar {}:\n  {}", from.display(), e))?;

    let json = serde_json::to_string_pretty(&result.config).map_err(|e| {
        format!(
            "ferrix: falha ao serializar a configuração convertida: {}",
            e
        )
    })?;

    match out {
        Some(path) => {
            std::fs::write(path, &json)
                .map_err(|e| format!("ferrix: falha ao gravar {}: {}", path.display(), e))?;
            println!("✔ Configuração convertida gravada em {}", path.display());
        }
        None => {
            println!("{}", json);
        }
    }

    if let Some(path) = report_path {
        std::fs::write(path, result.report.to_markdown())
            .map_err(|e| format!("ferrix: falha ao gravar {}: {}", path.display(), e))?;
        println!("✔ Relatório de migração gravado em {}", path.display());
    }

    println!("\n{}", result.report.to_text_summary());

    // A validação aqui é diagnóstico, não bloqueio: o resultado ainda não foi aplicado a
    // nada, então um erro só avisa o que precisa de ajuste antes de usar a configuração.
    let validation = validate(&result.config);
    if !validation.is_valid() {
        println!(
            "\n⚠️  A configuração convertida ainda não passaria em 'ferrix -t':\n{}",
            validation.summary()
        );
    }

    Ok(())
}

fn handle_start(args: &Args) -> Result<(), String> {
    let pid_file = pid_file_path(&args.config);
    if let Some(pid) = read_pid(&pid_file) {
        if is_pid_alive(pid) {
            println!("⚠️  O Ferrix já está em execução (PID: {}).", pid);
            println!("💡 Use 'ferrix status' para verificar ou 'ferrix stop' para parar.");
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
        Err(e) => return Err(format!("Erro na configuração: {}", e)),
    };
    let report = validate(&config);
    if !report.is_valid() {
        return Err("A configuração tem erros. Execute 'ferrix -t' para ver os detalhes.".into());
    }

    let exe = std::env::current_exe().map_err(|e| format!("Falha ao obter executável: {}", e))?;
    let log_path = "ferrix.log";
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| format!("Falha ao abrir {}: {}", log_path, e))?;
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
        .map_err(|e| format!("Falha ao iniciar processo em segundo plano: {}", e))?;
    let pid = child.id();
    std::fs::write(&pid_file, pid.to_string())
        .map_err(|e| format!("Falha ao gravar arquivo de PID: {}", e))?;

    std::thread::sleep(std::time::Duration::from_millis(250));
    if !is_pid_alive(pid) {
        let _ = std::fs::remove_file(&pid_file);
        return Err(format!(
            "O processo caiu imediatamente após iniciar. Verifique o arquivo {}",
            log_path
        ));
    }

    println!(
        "✔ Ferrix iniciado com sucesso em segundo plano (PID: {})",
        pid
    );
    println!("  🌐 Proxy HTTP:        http://{}", config.http.listen);
    println!("  ⚙️  Control Plane API: http://{}", config.admin.listen);
    println!("  📄 Configuração:      {}", args.config.display());
    println!("  📝 Logs gravados em:  {}", log_path);
    println!("\n💡 Use 'ferrix status' para verificar o serviço ou 'ferrix stop' para parar.");

    Ok(())
}

fn handle_stop(args: &Args) -> Result<(), String> {
    let pid_file = pid_file_path(&args.config);
    let pid = match read_pid(&pid_file) {
        Some(p) => p,
        None => {
            return Err(
                "Nenhum processo do Ferrix em execução foi encontrado (.ferrix.pid ausente)."
                    .into(),
            )
        }
    };

    if !is_pid_alive(pid) {
        let _ = std::fs::remove_file(&pid_file);
        return Err(format!(
            "O processo com PID {} não está em execução. O arquivo de PID foi removido.",
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
    println!("✔ Ferrix (PID: {}) encerrado com sucesso.", pid);
    Ok(())
}

fn handle_reload(args: &Args) -> Result<(), String> {
    let pid_file = pid_file_path(&args.config);
    let pid = match read_pid(&pid_file) {
        Some(p) => p,
        None => {
            return Err(
                "Nenhum processo do Ferrix em execução foi encontrado (.ferrix.pid ausente)."
                    .into(),
            )
        }
    };

    if !is_pid_alive(pid) {
        let _ = std::fs::remove_file(&pid_file);
        return Err(format!("O processo com PID {} não está em execução.", pid));
    }

    run_test(&args.config)?;

    #[cfg(unix)]
    {
        let res = unsafe { libc::kill(pid as i32, libc::SIGHUP) };

        if res == 0 {
            println!("✔ Sinal de recarga enviado para o Ferrix (PID: {}).", pid);
            println!("  As configurações foram atualizadas sem interromper conexões ativas.");
            Ok(())
        } else {
            Err("Falha ao enviar sinal para o processo.".into())
        }
    }
    #[cfg(not(unix))]
    {
        Err("Recarga via sinal não suportada nesta plataforma.".into())
    }
}

fn handle_restart(args: &Args) -> Result<(), String> {
    println!("Reiniciando Ferrix...");
    let _ = handle_stop(args);
    std::thread::sleep(std::time::Duration::from_millis(300));
    handle_start(args)
}

async fn handle_status(args: &Args) -> Result<(), String> {
    let pid_file = pid_file_path(&args.config);
    let pid = read_pid(&pid_file);

    match pid {
        Some(p) if is_pid_alive(p) => {
            println!("● Ferrix está em execução (PID: {})", p);
            println!("  📄 Configuração:      {}", args.config.display());
            println!("  📝 Arquivo de PID:    {}", pid_file.display());
            if Path::new("ferrix.log").exists() {
                println!("  📋 Arquivo de log:    ferrix.log");
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
            println!("\n💡 Para parar o servidor: ferrix stop");
            println!("💡 Para recarregar config: ferrix reload");
            Ok(())
        }
        _ => {
            if pid.is_some() {
                let _ = std::fs::remove_file(&pid_file);
            }
            println!("○ Ferrix está parado.");
            println!("\n💡 Para iniciar:");
            println!("   ferrix run      (rodar neste terminal)");
            println!("   ferrix start    (rodar em segundo plano)");
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
            "Sinal desconhecido '{}'. Sinais válidos: stop, reload, restart, status",
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
            "a configuração em {} tem erros e o Ferrix não vai subir:\n{}",
            config_path, details
        ));
    }

    if let Some(token) = &generated_token {
        info!("──────────────────────────────────────────────────────────────");
        info!(" Configuração inicial criada em {}", config_path);
        info!(" Token de administração gerado: {}", token);
        info!(" Use-o no painel ou como: Authorization: Bearer {}", token);
        info!("──────────────────────────────────────────────────────────────");
    }

    let admin_listen = config.admin.listen.clone();
    let proxy_listen = config.http.listen.clone();
    // Todo listener de proxy escuta no mesmo host que `http.listen` — só a porta muda,
    // uma por porta distinta declarada entre `http.listen` e os vhosts.
    let proxy_host = proxy_listen
        .rsplit_once(':')
        .map(|(host, _)| host.to_string())
        .unwrap_or_else(|| "0.0.0.0".to_string());
    let port_plan = tls_plan::plan_ports(&config);

    let store = Arc::new(ConfigStore::new(config, config_path.clone()));
    let metrics = MetricsCollector::new(1000);
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

    let admin_router = create_admin_router(AdminState {
        store: store.clone(),
        metrics: metrics.clone(),
        health_checker: health_checker.clone(),
    });

    let admin_listen_clone = admin_listen.clone();
    let admin_handle = tokio::spawn(async move {
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

    // Uma porta por entrada do plano — TLS quando algum vhost ali declarou certificado,
    // HTTP simples quando não. `ferrix.json` com um site em 443/HTTPS e outro redirecionando
    // de 80 vira, literalmente, dois listeners aqui.
    let mut proxy_handles: Vec<JoinHandle<()>> = Vec::with_capacity(port_plan.len());
    let mut proxy_ports_summary = Vec::with_capacity(port_plan.len());
    for plan in &port_plan {
        let addr: SocketAddr = format!("{}:{}", proxy_host, plan.port)
            .parse()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], plan.port)));

        match &plan.tls {
            Some(source) => {
                proxy_ports_summary.push(format!("https://{}", addr));
                proxy_handles.push(spawn_tls_listener(addr, proxy_engine.clone(), source.clone()));
            }
            None => {
                proxy_ports_summary.push(format!("http://{}", addr));
                proxy_handles.push(spawn_plain_listener(addr, proxy_engine.clone()));
            }
        }
    }

    let pid_file = pid_file_path(&args.config);
    let pid = std::process::id();
    let _ = std::fs::write(&pid_file, pid.to_string());

    println!("\n┌──────────────────────────────────────────────────────────────────┐");
    println!("│  ⚡ Ferrix v{:<52}│", env!("CARGO_PKG_VERSION"));
    println!("│     Reverse Proxy & Control Plane em execução                    │");
    println!("├──────────────────────────────────────────────────────────────────┤");
    for port_url in &proxy_ports_summary {
        println!("│  🌐 Proxy:             {:<42}│", port_url);
    }
    println!("│  ⚙️  Control Plane API: http://{:<42}│", admin_listen);
    println!("│  📄 Configuração:      {:<42}│", config_path);
    println!("│                                                                  │");
    println!("│  💡 Dica: Execute 'ferrix --help' para ver todos os comandos.   │");
    println!("│  🛑 Pressione Ctrl+C para encerrar o servidor.                   │");
    println!("└──────────────────────────────────────────────────────────────────┘\n");

    #[cfg(unix)]
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .map_err(|e| format!("Falha ao configurar listener SIGHUP: {}", e))?;

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("Falha ao configurar listener SIGTERM: {}", e))?;

    loop {
        #[cfg(unix)]
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\n🛑 Encerrando Ferrix com segurança... Até logo!");
                info!("Shutdown signal received (Ctrl+C), gracefully exiting Ferrix...");
                break;
            }
            _ = sigterm.recv() => {
                info!("Shutdown signal received (SIGTERM), gracefully exiting Ferrix...");
                break;
            }
            _ = sighup.recv() => {
                info!("SIGHUP recebido: recarregando configuração de {}", config_path);
                match FerrixConfig::load_or_create(&config_path_buf) {
                    Ok(ConfigLoad::Loaded(new_cfg)) | Ok(ConfigLoad::Created { config: new_cfg, .. }) => {
                        let report = validate(&new_cfg);
                        if report.is_valid() {
                            store.apply(new_cfg);
                            info!("✔ Configuração recarregada com sucesso!");
                        } else {
                            warn!("A nova configuração tem erros de validação; mantendo a anterior.");
                        }
                    }
                    Err(e) => {
                        warn!("Erro ao recarregar configuração: {}", e);
                    }
                }
            }
        }

        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c()
                .await
                .map_err(|e| format!("falha ao aguardar o sinal de encerramento: {}", e))?;
            println!("\n🛑 Encerrando Ferrix com segurança... Até logo!");
            info!("Shutdown signal received, gracefully exiting Ferrix...");
            break;
        }
    }

    let _ = std::fs::remove_file(&pid_file);
    admin_handle.abort();
    for handle in &proxy_handles {
        handle.abort();
    }

    Ok(())
}

