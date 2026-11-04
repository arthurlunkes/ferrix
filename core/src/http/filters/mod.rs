pub mod charset;
pub mod compress;
pub mod cors;
pub mod error_page;
pub mod gunzip;
pub mod headers;
#[cfg(feature = "image-filter")]
pub mod image;
pub mod limit_rate;
pub mod request_id;
pub mod ssi;
pub mod sub;
pub mod upstream_headers;
pub mod userid;

use axum::http::request::Parts;
use axum::http::{header, HeaderValue, Method};
use axum::response::Response;

use crate::config::{CompressionAlgorithm, RouteRule};
use crate::http::matcher::CompiledConfig;

pub struct RequestInfo {
    pub origin: Option<HeaderValue>,
    pub request_id: Option<HeaderValue>,
    pub encoding: Option<CompressionAlgorithm>,
}

impl RequestInfo {
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
