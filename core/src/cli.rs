//! Command-line parsing. Hand-written instead of using a CLI framework: Ferrix has a
//! handful of commands and flags, and a framework's code would stay resident in every
//! running server for no benefit.

use std::path::PathBuf;

/// The configuration is always read from here unless `-c` names another file, like
/// nginx's built-in `/etc/nginx/nginx.conf`, so every command finds the running server.
#[cfg(unix)]
pub const DEFAULT_CONFIG: &str = "/etc/ferrix/ferrix.yaml";
#[cfg(not(unix))]
pub const DEFAULT_CONFIG: &str = "ferrix.yaml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run,
    Start,
    Stop,
    Restart,
    Reload,
    Status,
    Test,
    ConvertConfig {
        from: PathBuf,
        to: PathBuf,
    },
    Import {
        from: PathBuf,
        out: Option<PathBuf>,
        report: Option<PathBuf>,
    },
}

#[derive(Debug, Clone)]
pub struct Args {
    pub command: Option<Command>,
    pub config: PathBuf,
    pub admin_listen: Option<String>,
    pub http_listen: Option<String>,
    pub test: bool,
    pub signal: Option<String>,
    /// Internal: set by the supervisor on the server process it starts.
    pub worker_process: bool,
}

/// What the process should do instead of running a command.
pub enum Early {
    Help,
    Version,
}

pub const HELP: &str = "\
⚡ Ferrix: high-performance reverse proxy & web server in Rust (nginx-inspired)

Usage: ferrix [OPTIONS] [COMMAND]

Commands:
  run                         Start the server in the foreground
  start                       Start the server in the background (daemon)
  stop                        Stop the background server
  restart                     Restart the background server
  reload                      Reload the configuration without dropping connections
  status                      Show whether the server is running
  test                        Check the configuration file (same as -t)
  convert-config <FROM> <TO>  Rewrite a config file in another format
                              (e.g. an old ferrix.json as ferrix.yaml)
  import --from <nginx.conf> [--out <ferrix.yaml>] [--report <report.md>]
                              Convert an nginx configuration

Options:
  -c, --config <FILE>         Configuration file [default: /etc/ferrix/ferrix.yaml]
      --admin-listen <ADDR>   Override admin.listen
      --http-listen <ADDR>    Override http.listen
  -t, --test                  Check the configuration file and exit
  -s, --signal <SIGNAL>       Send a signal to the running server: stop, reload, restart, status
  -h, --help                  Print this help
  -V, --version               Print the version
";

impl Args {
    pub fn from_env() -> Result<Result<Self, Early>, String> {
        Self::parse(std::env::args().skip(1))
    }

    pub fn parse(argv: impl IntoIterator<Item = String>) -> Result<Result<Self, Early>, String> {
        let mut args = Args {
            command: None,
            config: PathBuf::from(DEFAULT_CONFIG),
            admin_listen: None,
            http_listen: None,
            test: false,
            signal: None,
            worker_process: false,
        };
        let mut positional: Vec<String> = Vec::new();
        let mut import_from = None;
        let mut import_out = None;
        let mut import_report = None;

        let mut it = argv.into_iter();
        while let Some(arg) = it.next() {
            // Accept both `--flag value` and `--flag=value`.
            let (flag, inline) = match arg.split_once('=') {
                Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
                _ => (arg.clone(), None),
            };
            let mut value = |name: &str| -> Result<String, String> {
                inline
                    .clone()
                    .or_else(|| it.next())
                    .ok_or_else(|| format!("{} needs a value", name))
            };
            match flag.as_str() {
                "-h" | "--help" => return Ok(Err(Early::Help)),
                "-V" | "--version" => return Ok(Err(Early::Version)),
                "-c" | "--config" => args.config = PathBuf::from(value("--config")?),
                "--admin-listen" => args.admin_listen = Some(value("--admin-listen")?),
                "--http-listen" => args.http_listen = Some(value("--http-listen")?),
                "-t" | "--test" => args.test = true,
                "--worker-process" => args.worker_process = true,
                "-s" | "--signal" => args.signal = Some(value("--signal")?),
                "--from" => import_from = Some(PathBuf::from(value("--from")?)),
                "--out" => import_out = Some(PathBuf::from(value("--out")?)),
                "--report" => import_report = Some(PathBuf::from(value("--report")?)),
                f if f.starts_with('-') && f.len() > 1 => {
                    return Err(format!("unknown option '{}' (see ferrix --help)", f))
                }
                _ => positional.push(arg),
            }
        }

        let mut pos = positional.into_iter();
        args.command = match pos.next().as_deref() {
            None => None,
            Some("help") => return Ok(Err(Early::Help)),
            Some("run") => Some(Command::Run),
            Some("start") => Some(Command::Start),
            Some("stop") => Some(Command::Stop),
            Some("restart") => Some(Command::Restart),
            Some("reload") => Some(Command::Reload),
            Some("status") => Some(Command::Status),
            Some("test") => Some(Command::Test),
            Some("convert-config") => {
                let (Some(from), Some(to)) = (pos.next(), pos.next()) else {
                    return Err("usage: ferrix convert-config <FROM> <TO>".into());
                };
                Some(Command::ConvertConfig {
                    from: from.into(),
                    to: to.into(),
                })
            }
            Some("import") => Some(Command::Import {
                from: import_from.take().ok_or(
                    "usage: ferrix import --from <nginx.conf> [--out <file>] [--report <file>]",
                )?,
                out: import_out.take(),
                report: import_report.take(),
            }),
            Some(other) => return Err(format!("unknown command '{}' (see ferrix --help)", other)),
        };
        if let Some(extra) = pos.next() {
            return Err(format!("unexpected argument '{}'", extra));
        }
        if import_from.is_some() || import_out.is_some() || import_report.is_some() {
            return Err("--from, --out and --report only apply to 'ferrix import'".into());
        }
        Ok(Ok(args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Args, String> {
        match Args::parse(s.split_whitespace().map(String::from))? {
            Ok(a) => Ok(a),
            Err(_) => Err("early".into()),
        }
    }

    #[test]
    fn commands_and_global_options() {
        let a = parse("run -c /etc/ferrix.yaml --http-listen 0.0.0.0:80").unwrap();
        assert_eq!(a.command, Some(Command::Run));
        assert_eq!(a.config, PathBuf::from("/etc/ferrix.yaml"));
        assert_eq!(a.http_listen.as_deref(), Some("0.0.0.0:80"));

        let a = parse("--config=x.yaml -t").unwrap();
        assert!(a.test && a.command.is_none());
        assert_eq!(a.config, PathBuf::from("x.yaml"));

        assert_eq!(
            parse("-s reload").unwrap().signal.as_deref(),
            Some("reload")
        );
        assert_eq!(
            parse("status").unwrap().config,
            PathBuf::from(DEFAULT_CONFIG)
        );
    }

    #[test]
    fn import_and_convert() {
        let a = parse("import --from nginx.conf --out f.yaml").unwrap();
        assert_eq!(
            a.command,
            Some(Command::Import {
                from: "nginx.conf".into(),
                out: Some("f.yaml".into()),
                report: None
            })
        );
        let a = parse("convert-config a.json b.yaml").unwrap();
        assert_eq!(
            a.command,
            Some(Command::ConvertConfig {
                from: "a.json".into(),
                to: "b.yaml".into()
            })
        );
    }

    #[test]
    fn mistakes_are_reported() {
        assert!(parse("bogus").is_err());
        assert!(parse("run --nope").is_err());
        assert!(parse("-c").is_err());
        assert!(parse("import").is_err());
        assert!(parse("run --from x").is_err());
        assert!(matches!(parse("--help"), Err(e) if e == "early"));
    }
}
