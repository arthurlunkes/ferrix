//! O crate como biblioteca: expõe os módulos do motor para o binário `ferrix` e para os
//! testes de integração em `tests/`, que de outra forma não teriam acesso a nada além do
//! que um binário exporta (nada).

pub mod admin;
pub mod cache;
pub mod clock;
pub mod config;
pub mod health;
pub mod metrics;
pub mod net;
pub mod nginx;
pub mod proxy;
pub mod tls;
