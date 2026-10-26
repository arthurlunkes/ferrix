//! Header filters: the route's `add_headers` / vhost `headers` (nginx `add_header`) and
//! the `Server` header (nginx's header filter writes it, honouring `server_tokens`).

use axum::http::{header, HeaderMap};

use crate::http::matcher::CompiledConfig;

pub fn add(
    compiled: &CompiledConfig,
    vhost_index: usize,
    route_index: usize,
    headers: &mut HeaderMap,
) {
    for (name, value) in compiled.route_response_headers(vhost_index, route_index) {
        headers.insert(name.clone(), value.clone());
    }
}

/// Ferrix answers with its own `Server` header, replacing an upstream's, like nginx.
pub fn server(compiled: &CompiledConfig, headers: &mut HeaderMap) {
    headers.insert(header::SERVER, compiled.server_header.clone());
}
