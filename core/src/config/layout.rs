use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{ConfigFormat, FerrixConfig, StreamConfig, UpstreamConfig, VirtualHost};

pub const DIRS: [&str; 3] = ["sites", "upstreams", "streams"];

pub fn base_dir(config_path: &Path) -> PathBuf {
    match config_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

pub fn is_split(config_path: &Path) -> bool {
    let base = base_dir(config_path);
    DIRS.iter().any(|d| base.join(d).is_dir())
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("yaml" | "yml" | "json")
            )
        })
        .collect();
    out.sort();
    out
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    Wrapped {
        #[serde(default = "enabled")]
        enabled: bool,
        #[serde(alias = "vhosts", alias = "streams", alias = "items")]
        list: Vec<T>,
    },
    Many(Vec<T>),
    One(Box<T>),
}

fn enabled() -> bool {
    true
}

#[derive(Deserialize)]
struct Flag {
    #[serde(default = "enabled")]
    enabled: bool,
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let parse = |t: &str| -> Result<Option<T>, String> {
        match ConfigFormat::of(path) {
            ConfigFormat::Yaml => {
                if let Ok(Flag { enabled: false }) = serde_saphyr::from_str::<Flag>(t) {
                    return Ok(None);
                }
                serde_saphyr::from_str(t)
                    .map(Some)
                    .map_err(|e| e.to_string())
            }
            ConfigFormat::Json => {
                if let Ok(Flag { enabled: false }) = serde_json::from_str::<Flag>(t) {
                    return Ok(None);
                }
                serde_json::from_str(t).map(Some).map_err(|e| e.to_string())
            }
        }
    };
    parse(&text).map_err(|e| format!("Invalid config {}: {}", path.display(), e))
}

fn list<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>, String> {
    Ok(match read::<OneOrMany<T>>(path)? {
        None | Some(OneOrMany::Wrapped { enabled: false, .. }) => Vec::new(),
        Some(OneOrMany::Wrapped { list, .. }) | Some(OneOrMany::Many(list)) => list,
        Some(OneOrMany::One(one)) => vec![*one],
    })
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Upstreams {
    One(Box<UpstreamConfig>),
    Map(HashMap<String, UpstreamConfig>),
}

pub fn merge(cfg: &mut FerrixConfig, config_path: &Path) -> Result<(), String> {
    let base = base_dir(config_path);
    for f in files(&base.join("sites")) {
        cfg.vhosts.extend(list::<VirtualHost>(&f)?);
    }
    for f in files(&base.join("streams")) {
        cfg.streams.extend(list::<StreamConfig>(&f)?);
    }
    for f in files(&base.join("upstreams")) {
        match read::<Upstreams>(&f)? {
            None => {}
            Some(Upstreams::One(u)) => {
                cfg.upstreams.insert(u.name.clone(), *u);
            }
            Some(Upstreams::Map(m)) => cfg.upstreams.extend(m),
        }
    }
    Ok(())
}

fn safe_name(id: &str) -> String {
    let s: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "unnamed".into()
    } else {
        s
    }
}

fn render<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_saphyr::to_string(value).map_err(|e| e.to_string())
}

fn write_dir(dir: &Path, wanted: Vec<(String, String)>) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
    let mut keep = HashSet::new();
    for (name, text) in wanted {
        let path = dir.join(format!("{}.yaml", name));
        let unchanged = std::fs::read_to_string(&path).is_ok_and(|old| old == text);
        if !unchanged {
            let tmp = dir.join(format!(".{}.tmp", name));
            std::fs::write(&tmp, &text)
                .and_then(|_| std::fs::rename(&tmp, &path))
                .map_err(|e| format!("{}: {}", path.display(), e))?;
        }
        keep.insert(path);
    }
    for f in files(dir) {
        if keep.contains(&f) {
            continue;
        }
        let disabled = match ConfigFormat::of(&f) {
            ConfigFormat::Yaml => std::fs::read_to_string(&f)
                .ok()
                .and_then(|t| serde_saphyr::from_str::<Flag>(&t).ok())
                .is_some_and(|fl| !fl.enabled),
            ConfigFormat::Json => false,
        };
        if !disabled {
            let _ = std::fs::remove_file(&f);
        }
    }
    Ok(())
}

pub fn save(cfg: &FerrixConfig, config_path: &Path) -> Result<(), String> {
    let base = base_dir(config_path);
    let mut names = HashSet::new();
    let mut unique = |id: &str| {
        let mut name = safe_name(id);
        let mut n = 2;
        while !names.insert(name.clone()) {
            name = format!("{}-{}", safe_name(id), n);
            n += 1;
        }
        name
    };
    let mut sites = Vec::new();
    for v in &cfg.vhosts {
        sites.push((unique(&v.id), render(v)?));
    }
    write_dir(&base.join("sites"), sites)?;

    let mut ups = Vec::new();
    let mut sorted: Vec<_> = cfg.upstreams.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    for (name, u) in sorted {
        let mut one = HashMap::new();
        one.insert(name.clone(), u.clone());
        ups.push((safe_name(name), render(&one)?));
    }
    write_dir(&base.join("upstreams"), ups)?;

    if !cfg.streams.is_empty() || base.join("streams").is_dir() {
        let mut streams = Vec::new();
        for s in &cfg.streams {
            streams.push((safe_name(&s.id), render(s)?));
        }
        write_dir(&base.join("streams"), streams)?;
    }

    let mut core = cfg.clone();
    core.vhosts.clear();
    core.upstreams.clear();
    core.streams.clear();
    let text = core
        .render(ConfigFormat::of(config_path))
        .map_err(|e| format!("Failed to serialize config: {}", e))?;
    std::fs::write(config_path, text)
        .map_err(|e| format!("Failed to write config to {}: {}", config_path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_roundtrip_and_disabled_sites() {
        let dir = std::env::temp_dir().join(format!("ferrix-split-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("ferrix.yaml");
        let cfg = FerrixConfig::default();
        assert!(!is_split(&main));
        save(&cfg, &main).unwrap();
        assert!(is_split(&main));
        assert!(dir.join("sites").is_dir());

        let loaded = FerrixConfig::load(&main).unwrap();
        assert_eq!(loaded.vhosts.len(), cfg.vhosts.len());
        assert_eq!(loaded.upstreams.len(), cfg.upstreams.len());
        assert_eq!(loaded.vhosts[0].id, cfg.vhosts[0].id);

        let mut off = cfg.vhosts[0].clone();
        off.id = "parked".into();
        let text = format!(
            "enabled: false\nvhosts:\n- {}",
            render(&off).unwrap().replace('\n', "\n  ")
        );
        std::fs::write(dir.join("sites/parked.yaml"), text).unwrap();
        let again = FerrixConfig::load(&main).unwrap();
        assert_eq!(again.vhosts.len(), cfg.vhosts.len());

        again.save_to_file(&main).unwrap();
        assert!(
            dir.join("sites/parked.yaml").is_file(),
            "disabled files survive a save"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
