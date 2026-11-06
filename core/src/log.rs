use std::fmt::Write as _;
use std::io::Write as _;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    default: Option<Level>,
    targets: Vec<(String, Option<Level>)>,
}

fn parse_level(s: &str) -> Option<Option<Level>> {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" => Some(None),
        "error" => Some(Some(Level::ERROR)),
        "warn" => Some(Some(Level::WARN)),
        "info" => Some(Some(Level::INFO)),
        "debug" => Some(Some(Level::DEBUG)),
        "trace" => Some(Some(Level::TRACE)),
        _ => None,
    }
}

impl Filter {
    pub fn parse(spec: &str) -> Self {
        let mut f = Filter {
            default: Some(Level::ERROR),
            targets: Vec::new(),
        };
        for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match part.split_once('=') {
                Some((target, level)) => {
                    if let Some(l) = parse_level(level) {
                        f.targets.push((target.trim().to_string(), l));
                    }
                }
                None => match parse_level(part) {
                    Some(l) => f.default = l,
                    None => f.targets.push((part.to_string(), Some(Level::TRACE))),
                },
            }
        }
        f.targets.sort_by_key(|(t, _)| std::cmp::Reverse(t.len()));
        f
    }

    fn max_for(&self, target: &str) -> Option<Level> {
        self.targets
            .iter()
            .find(|(t, _)| target == t || target.starts_with(&format!("{}::", t)))
            .map(|(_, l)| *l)
            .unwrap_or(self.default)
    }

    pub fn enabled(&self, target: &str, level: &Level) -> bool {
        self.max_for(target).is_some_and(|max| *level <= max)
    }
}

pub struct LineLogger;

struct State {
    filter: Filter,
    json: bool,
    targets: Vec<crate::config::LogTarget>,
}

static STATE: std::sync::LazyLock<arc_swap::ArcSwap<State>> = std::sync::LazyLock::new(|| {
    arc_swap::ArcSwap::from_pointee(State {
        filter: Filter::parse("ferrix=info"),
        json: false,
        targets: vec![crate::config::LogTarget::Stderr],
    })
});

static DEFAULT_SPEC: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn init(default: &str) {
    let _ = DEFAULT_SPEC.set(default.to_string());
    let spec = std::env::var("RUST_LOG").unwrap_or_else(|_| default.to_string());
    STATE.store(std::sync::Arc::new(State {
        filter: Filter::parse(&spec),
        json: false,
        targets: vec![crate::config::LogTarget::Stderr],
    }));
    let _ = tracing::subscriber::set_global_default(LineLogger);
}

pub fn configure(cfg: &crate::config::ErrorLogConfig) {
    let spec = std::env::var("RUST_LOG").ok().unwrap_or_else(|| {
        cfg.level.clone().unwrap_or_else(|| {
            DEFAULT_SPEC
                .get()
                .cloned()
                .unwrap_or_else(|| "ferrix=info".into())
        })
    });
    let spec = match spec.trim() {
        "error" | "warn" | "info" | "debug" | "trace" if !spec.contains('=') => {
            format!("error,ferrix={}", spec.trim())
        }
        _ => spec,
    };
    let targets = cfg
        .targets
        .clone()
        .unwrap_or_else(|| vec![crate::config::LogTarget::Stderr]);
    let next = State {
        filter: Filter::parse(&spec),
        json: cfg.format == crate::config::LogFormat::Json,
        targets,
    };
    let cur = STATE.load();
    if cur.filter == next.filter && cur.json == next.json && cur.targets == next.targets {
        return;
    }
    STATE.store(std::sync::Arc::new(next));
    tracing::callsite::rebuild_interest_cache();
}

#[derive(Default)]
struct Fields {
    message: String,
    rest: Vec<(&'static str, String)>,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            self.rest.push((field.name(), value.to_string()));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{:?}", value);
        } else {
            self.rest.push((field.name(), format!("{:?}", value)));
        }
    }
}

fn severity(level: &Level) -> u8 {
    match *level {
        Level::ERROR => 3,
        Level::WARN => 4,
        Level::INFO => 6,
        _ => 7,
    }
}

fn render(json: bool, time: &str, level: &Level, target: &str, f: &Fields) -> String {
    if json {
        let mut m = serde_json::Map::new();
        m.insert("time".into(), time.into());
        m.insert("level".into(), level.as_str().to_ascii_lowercase().into());
        m.insert("target".into(), target.into());
        m.insert("msg".into(), f.message.clone().into());
        for (k, v) in &f.rest {
            m.insert((*k).into(), v.clone().into());
        }
        let mut s = serde_json::Value::Object(m).to_string();
        s.push('\n');
        return s;
    }
    let mut line = format!("{} {:>5} {}: {}", time, level, target, f.message);
    for (k, v) in &f.rest {
        let _ = write!(line, " {}={}", k, v);
    }
    line.push('\n');
    line
}

impl Subscriber for LineLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        STATE
            .load()
            .filter
            .enabled(metadata.target(), metadata.level())
    }

    fn event(&self, event: &Event<'_>) {
        let state = STATE.load();
        if state.targets.is_empty() {
            return;
        }
        let meta = event.metadata();
        let mut fields = Fields::default();
        event.record(&mut fields);
        let time = crate::event::clock::utc_now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        let line = render(state.json, &time, meta.level(), meta.target(), &fields);
        let sev = severity(meta.level());
        for t in &state.targets {
            match t {
                crate::config::LogTarget::Stderr => {
                    let _ = std::io::stderr().lock().write_all(line.as_bytes());
                }
                crate::config::LogTarget::Off => {}
                t => crate::log_sink::send(t, sev, line.clone()),
            }
        }
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _span: &Id, _values: &Record<'_>) {}
    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}
    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_levels_and_targets() {
        let f = Filter::parse("warn,ferrix=debug,ferrix::http::upstream=off");
        assert!(f.enabled("hyper::proto", &Level::WARN));
        assert!(!f.enabled("hyper::proto", &Level::INFO));
        assert!(f.enabled("ferrix::event", &Level::DEBUG));
        assert!(!f.enabled("ferrix::event", &Level::TRACE));
        assert!(!f.enabled("ferrix::http::upstream::h1", &Level::ERROR));
        assert!(
            !f.enabled("ferrixx", &Level::DEBUG),
            "prefix must match whole segments"
        );
    }

    #[test]
    fn bare_level_and_garbage() {
        assert!(Filter::parse("info").enabled("anything", &Level::INFO));
        let f = Filter::parse("nonsense=loud");
        assert!(f.enabled("x", &Level::ERROR) && !f.enabled("x", &Level::WARN));
    }

    #[test]
    fn json_and_text_lines() {
        let f = Fields {
            message: "up".into(),
            rest: vec![("port", "80".into())],
        };
        let t = render(false, "T", &Level::INFO, "ferrix", &f);
        assert_eq!(t, " T  INFO ferrix: up port=80\n".trim_start());
        let j: serde_json::Value =
            serde_json::from_str(render(true, "T", &Level::WARN, "ferrix", &f).trim()).unwrap();
        assert_eq!(j["level"], "warn");
        assert_eq!(j["port"], "80");
    }
}
