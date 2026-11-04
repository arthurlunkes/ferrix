use arc_swap::ArcSwap;
use std::sync::Arc;

use super::FerrixConfig;
use crate::http::matcher::CompiledConfig;

pub struct ConfigStore {
    compiled: ArcSwap<CompiledConfig>,
    path: String,
}

impl ConfigStore {
    pub fn new(mut config: FerrixConfig, path: impl Into<String>) -> Self {
        config.resolve_ports();
        let compiled = CompiledConfig::build(Arc::new(config));
        Self {
            compiled: ArcSwap::from_pointee(compiled),
            path: path.into(),
        }
    }

    pub fn compiled(&self) -> Arc<CompiledConfig> {
        self.compiled.load_full()
    }

    pub fn config(&self) -> Arc<FerrixConfig> {
        self.compiled.load().config.clone()
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn apply(&self, mut config: FerrixConfig) {
        config.resolve_ports();
        let compiled = CompiledConfig::build(Arc::new(config));
        self.compiled.store(Arc::new(compiled));
    }

    pub fn persist_and_apply(&self, config: FerrixConfig) -> Result<(), String> {
        let path = std::path::Path::new(&self.path);
        if path.exists() {
            let mut backup = path.as_os_str().to_owned();
            backup.push(".bak");
            std::fs::copy(path, &backup).map_err(|e| {
                format!(
                    "Failed to keep a backup of {} before saving: {}",
                    path.display(),
                    e
                )
            })?;
        }
        config.save_to_file(&self.path)?;
        self.apply(config);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RouteMatchType, RouteTargetType};

    #[test]
    fn apply_swaps_config_and_recompiles_derived_state() {
        let store = ConfigStore::new(FerrixConfig::default(), "ferrix.yaml");
        assert_eq!(store.config().vhosts.len(), 1);

        let mut next = FerrixConfig::default();
        next.vhosts[0].name = "renomeado".to_string();

        next.vhosts[0].routes.truncate(1);
        next.vhosts[0].routes[0].match_type = RouteMatchType::Exact;
        next.vhosts[0].routes[0].path_prefix = "/exato".to_string();
        store.apply(next);

        let compiled = store.compiled();
        assert_eq!(compiled.config.vhosts[0].name, "renomeado");

        let m = compiled.match_route(0, "/exato").expect("deveria casar");
        assert_eq!(
            compiled.config.vhosts[0].routes[m.route_index].target_type,
            RouteTargetType::ProxyUpstream
        );

        assert!(compiled.match_route(0, "/exato/mais").is_none());
    }

    #[test]
    fn acl_is_recompiled_on_apply() {
        let store = ConfigStore::new(FerrixConfig::default(), "ferrix.yaml");
        let mut next = FerrixConfig::default();
        next.vhosts[0].denied_ips = vec!["172.31.0.0/16".to_string()];
        store.apply(next);

        let compiled = store.compiled();
        let (_allow, deny) = compiled.vhost_acl(0).unwrap();
        assert!(deny.matches("172.31.5.5".parse().unwrap()));
    }

    #[test]
    fn a_held_snapshot_survives_a_reload() {
        let store = ConfigStore::new(FerrixConfig::default(), "ferrix.yaml");
        let held = store.compiled();

        let mut next = FerrixConfig::default();
        next.vhosts[0].name = "novo".to_string();
        store.apply(next);

        assert_eq!(held.config.vhosts[0].name, "Default Virtual Host");
        assert_eq!(store.config().vhosts[0].name, "novo");
    }

    #[test]
    fn saving_keeps_the_previous_file_as_a_backup() {
        let dir = std::env::temp_dir().join(format!("ferrix-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ferrix.yaml");
        std::fs::write(&path, "# my notes\nhttp:\n  listen: 0.0.0.0:80\n").unwrap();
        let store = ConfigStore::new(FerrixConfig::default(), path.to_str().unwrap());

        store.persist_and_apply(FerrixConfig::default()).unwrap();

        let backup = std::fs::read_to_string(dir.join("ferrix.yaml.bak")).unwrap();
        assert!(backup.starts_with("# my notes"));
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("ferrix.yaml.bak"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
