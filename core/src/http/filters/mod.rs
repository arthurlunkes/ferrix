//! Response filters, modelled on nginx's filter chain. A content handler (proxy, static
//! files, redirect) produces the response; the filters then adjust it in a fixed order.
//!
//! Some filters need facts about the request that are gone once the handler consumes
//! it, so they are captured up front in [`RequestInfo`] (which also answers CORS
//! preflights before any handler runs).

pub mod compress;
pub mod cors;
pub mod error_page;
pub mod headers;
pub mod request_id;

use axum::http::request::Parts;
use axum::http::{header, HeaderValue, Method};
use axum::response::Response;

use crate::config::{CompressionAlgorithm, RouteRule};
use crate::http::matcher::CompiledConfig;

/// Request facts the output filters need.
pub struct RequestInfo {
    /// `Origin`, when the route has CORS configured.
    pub origin: Option<HeaderValue>,
    /// `X-Request-Id` (kept or generated), when `http.request_id` is on.
    pub request_id: Option<HeaderValue>,
    /// Encoding negotiated from `Accept-Encoding`, when compression applies.
    pub encoding: Option<CompressionAlgorithm>,
}

impl RequestInfo {
    /// Request-side phase. Returns `Err` with the response for a CORS preflight, which
    /// is answered without running any handler.
    pub fn capture(
        compiled: &CompiledConfig,
        route: &RouteRule,
        parts: &mut Parts,
    ) -> Result<Self, Box<Response>> {
        if let Some(cors_cfg) = &route.cors {
            if let Some(r) = cors::preflight(cors_cfg, &parts.method, &parts.headers) {
                return Err(Box::new(r));
            }
        }
        let origin = route
            .cors
            .as_ref()
            .and_then(|_| parts.headers.get(header::ORIGIN).cloned());

        let request_id = compiled.config.http.request_id.then(|| {
            let id = parts
                .headers
                .get(request_id::X_REQUEST_ID)
                .filter(|v| !v.is_empty() && v.len() <= 200)
                .cloned()
                .unwrap_or_else(request_id::new_request_id);
            parts.headers.insert(request_id::X_REQUEST_ID, id.clone());
            id
        });

        let compression = &compiled.config.http.compression;
        let encoding =
            if route.compression.unwrap_or(compression.enabled) && parts.method != Method::HEAD {
                compress::negotiate(&parts.headers, compression)
            } else {
                None
            };

        Ok(Self {
            origin,
            request_id,
            encoding,
        })
    }
}

/// Output phase for a route's response, in nginx's order: compression (`gzip`), the
/// route/vhost headers (`add_header`), CORS, then the request ID.
pub fn run(
    compiled: &CompiledConfig,
    vhost_index: usize,
    route_index: usize,
    route: &RouteRule,
    info: RequestInfo,
    mut resp: Response,
) -> Response {
    let compression = &compiled.config.http.compression;
    if let Some(algo) = info.encoding {
        if compress::should_compress(&resp, compression) {
            resp = compress::compress(resp, algo, compression.level);
        }
    }
    headers::add(compiled, vhost_index, route_index, resp.headers_mut());
    if let Some(cors_cfg) = &route.cors {
        cors::decorate(cors_cfg, info.origin.as_ref(), &mut resp);
    }
    if let Some(id) = info.request_id {
        resp.headers_mut().insert(request_id::X_REQUEST_ID, id);
    }
    resp
}
