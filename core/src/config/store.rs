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
}
