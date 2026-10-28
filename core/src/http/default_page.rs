//! The "Ferrix is running" page, the counterpart of nginx's default site: shown when no
//! virtual host matches (or nothing is configured for `/`), so a fresh install visibly
//! works. Disabled with `http.default_page: false`.

use axum::http::header;
use axum::response::{IntoResponse, Response};
use std::sync::LazyLock;

const TEMPLATE: &str = include_str!("assets/default_page.html");
/// The Ferrix crab, a small WebP inlined so the page needs no other request.
const LOGO: &str = include_str!("assets/ferrix-crab.webp.b64");

static WITH_VERSION: LazyLock<String> =
    LazyLock::new(|| page(concat!("v", env!("CARGO_PKG_VERSION"))));
static WITHOUT_VERSION: LazyLock<String> = LazyLock::new(|| page(""));

fn page(version: &str) -> String {
    TEMPLATE
        .replace("@LOGO@", LOGO.trim())
        .replace("@VERSION@", version)
}

/// Renders the page; `show_version` follows `http.server_tokens`.
pub fn render(show_version: bool) -> Response {
    let html = if show_version {
        WITH_VERSION.as_str()
    } else {
        WITHOUT_VERSION.as_str()
    };
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_follows_server_tokens() {
        assert!(WITH_VERSION.contains(env!("CARGO_PKG_VERSION")));
        assert!(!WITHOUT_VERSION.contains(env!("CARGO_PKG_VERSION")));
        assert!(!WITH_VERSION.contains("@LOGO@"));
    }
}
