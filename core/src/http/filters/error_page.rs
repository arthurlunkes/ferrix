//! Custom error pages (nginx `error_page`).

use axum::body::Body;
use axum::http::{header, HeaderValue};
use axum::response::Response;
use tracing::warn;

use crate::config::VirtualHost;
use crate::http::X_FERRIX_UPSTREAM;

/// Swaps the body of an error response for the vhost's custom page (`error_pages`),
/// keeping the status. Responses that came from an upstream are passed through, like
/// nginx without `proxy_intercept_errors`.
pub async fn apply(vhost: &VirtualHost, resp: Response) -> Response {
    let status = resp.status();
    if status.as_u16() < 400
        || vhost.error_pages.is_empty()
        || resp.headers().contains_key(X_FERRIX_UPSTREAM)
    {
        return resp;
    }
    let code = status.as_u16().to_string();
    let class = format!("{}xx", status.as_u16() / 100);
    let Some(path) = vhost
        .error_pages
        .get(&code)
        .or_else(|| vhost.error_pages.get(&class))
    else {
        return resp;
    };
    let page = match tokio::fs::read(path).await {
        Ok(p) => p,
        Err(e) => {
            warn!(path = %path, error = %e, "custom error page not readable");
            return resp;
        }
    };
    let (mut parts, _) = resp.into_parts();
    parts.extensions.remove::<crate::http::server::SendFile>();
    parts.headers.remove(header::CONTENT_ENCODING);
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    parts
        .headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from(page.len()));
    Response::from_parts(parts, Body::from(page))
}
