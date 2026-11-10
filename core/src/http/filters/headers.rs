use axum::http::{header, HeaderMap};

use crate::http::matcher::CompiledConfig;

pub fn add(
    compiled: &CompiledConfig,
    vhost_index: usize,
    route_index: usize,
    status: axum::http::StatusCode,
    headers: &mut HeaderMap,
) {
    let success = matches!(
        status.as_u16(),
        200 | 201 | 204 | 206 | 301 | 302 | 303 | 304 | 307 | 308
    );
    for (name, value, always) in compiled.route_response_headers(vhost_index, route_index) {
        if success || *always {
            headers.insert(name.clone(), value.clone());
        }
    }
}

pub fn server(compiled: &CompiledConfig, headers: &mut HeaderMap) {
    headers.insert(header::SERVER, compiled.server_header.clone());
}
