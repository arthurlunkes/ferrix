//! Guarda a configuração ativa e a versão pré-compilada dela.
//!
//! Antes, cada componente carregava um `ArcSwap<FerrixConfig>` cru. Como agora existe
//! material derivado que custa caro para montar — expressões regulares de rota, blocos
//! CIDR, a lista de proxies confiáveis —, ele precisa ser reconstruído junto com a
//! configuração e trocado no mesmo instante. Deixar isso a cargo de quem chama seria
//! convidar o esquecimento; centralizar aqui garante que os dois nunca divirjam.

use arc_swap::ArcSwap;
use std::sync::Arc;

use super::FerrixConfig;
use crate::proxy::matcher::CompiledConfig;

pub struct ConfigStore {
    compiled: ArcSwap<CompiledConfig>,
    path: String,
}

impl ConfigStore {
    pub fn new(config: FerrixConfig, path: impl Into<String>) -> Self {
        let compiled = CompiledConfig::build(Arc::new(config));
        Self {
            compiled: ArcSwap::from_pointee(compiled),
            path: path.into(),
        }
    }

    /// A configuração compilada em vigor. Uma requisição segura este `Arc` do começo ao
    /// fim, então um hot-reload no meio do caminho não a afeta.
    pub fn compiled(&self) -> Arc<CompiledConfig> {
        self.compiled.load_full()
    }

    /// Só os dados de configuração, para quem não precisa do material derivado.
    pub fn config(&self) -> Arc<FerrixConfig> {
        self.compiled.load().config.clone()
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Troca a configuração em memória, recompilando o que deriva dela.
    pub fn apply(&self, config: FerrixConfig) {
        let compiled = CompiledConfig::build(Arc::new(config));
        self.compiled.store(Arc::new(compiled));
    }

    /// Grava em disco e só então aplica.
    ///
    /// A ordem importa: se a gravação falhar, o processo continua com a configuração
    /// anterior em vez de rodar com algo que não sobreviveria a um reinício.
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
        let store = ConfigStore::new(FerrixConfig::default(), "ferrix.json");
        assert_eq!(store.config().vhosts.len(), 1);

        let mut next = FerrixConfig::default();
        next.vhosts[0].name = "renomeado".to_string();
        // Uma rota só, para que o teste fale apenas do match exato.
        next.vhosts[0].routes.truncate(1);
        next.vhosts[0].routes[0].match_type = RouteMatchType::Exact;
        next.vhosts[0].routes[0].path_prefix = "/exato".to_string();
        store.apply(next);

        let compiled = store.compiled();
        assert_eq!(compiled.config.vhosts[0].name, "renomeado");
        // O material derivado acompanhou: o match exato já vale.
        let m = compiled.match_route(0, "/exato").expect("deveria casar");
        assert_eq!(
            compiled.config.vhosts[0].routes[m.route_index].target_type,
            RouteTargetType::ProxyUpstream
        );
        // Exato é exato: um caminho mais longo não pertence a ele.
        assert!(compiled.match_route(0, "/exato/mais").is_none());
    }

    #[test]
    fn acl_is_recompiled_on_apply() {
        let store = ConfigStore::new(FerrixConfig::default(), "ferrix.json");
        let mut next = FerrixConfig::default();
        next.vhosts[0].denied_ips = vec!["172.31.0.0/16".to_string()];
        store.apply(next);

        let compiled = store.compiled();
        let (_allow, deny) = compiled.vhost_acl(0).unwrap();
        assert!(deny.matches("172.31.5.5".parse().unwrap()));
    }

    #[test]
    fn a_held_snapshot_survives_a_reload() {
        let store = ConfigStore::new(FerrixConfig::default(), "ferrix.json");
        let held = store.compiled();

        let mut next = FerrixConfig::default();
        next.vhosts[0].name = "novo".to_string();
        store.apply(next);

        // Quem já estava atendendo continua com a configuração com que começou.
        assert_eq!(held.config.vhosts[0].name, "Default Virtual Host");
        assert_eq!(store.config().vhosts[0].name, "novo");
    }
}
