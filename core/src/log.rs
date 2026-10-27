//! A minimal `tracing` subscriber: one line per event on stderr, filtered by `RUST_LOG`.
//!
//! `tracing-subscriber` with its environment filter would pull in a full regex engine
//! and formatting machinery that stays resident in every server; Ferrix only needs
//! "level per target prefix", which this does in a few lines.

use std::fmt::Write as _;
use std::io::Write as _;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// Parsed `RUST_LOG`: a default level plus `target=level` overrides, e.g.
/// `info`, `warn,ferrix=debug` or `ferrix::http=trace`.
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
                    // A bare target means "everything for it".
                    None => f.targets.push((part.to_string(), Some(Level::TRACE))),
                },
            }
        }
        // Longest prefix wins.
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

pub struct LineLogger {
    filter: Filter,
}

/// Installs the logger as the global `tracing` subscriber. `default` is used when
/// `RUST_LOG` isn't set.
pub fn init(default: &str) {
    let spec = std::env::var("RUST_LOG").unwrap_or_else(|_| default.to_string());
    let _ = tracing::subscriber::set_global_default(LineLogger {
        filter: Filter::parse(&spec),
    });
}

struct Fields {
    message: String,
    rest: String,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.rest, " {}={}", field.name(), value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{:?}", value);
        } else {
            let _ = write!(self.rest, " {}={:?}", field.name(), value);
        }
    }
}

impl Subscriber for LineLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.filter.enabled(metadata.target(), metadata.level())
    }

    fn event(&self, event: &Event<'_>) {
        let meta = event.metadata();
        let mut fields = Fields {
            message: String::new(),
            rest: String::new(),
        };
        event.record(&mut fields);
        let time = crate::event::clock::utc_now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
        let line = format!(
            "{} {:>5} {}: {}{}\n",
            time,
            meta.level(),
            meta.target(),
            fields.message,
            fields.rest
        );
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }

    // Ferrix logs events only; spans are accepted and ignored.
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
}
