//! The "Welcome to Ferrix" page, the counterpart of nginx's default site: shown when no
//! virtual host matches (or the matching one has no routes yet), so a fresh install
//! visibly works. Disabled with `http.default_page: false`.

use axum::http::header;
use axum::response::{IntoResponse, Response};

/// nginx-style welcome page for requests that match no virtual host, so a fresh install
/// shows that the server is up. Disable with `http.default_page: false`.
pub fn render(show_version: bool) -> Response {
    let version = if show_version {
        concat!(" ", env!("CARGO_PKG_VERSION"))
    } else {
        ""
    };
    let html = format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<title>Welcome to Ferrix!</title>\n<style>\n\
         html {{ color-scheme: light dark; }}\n\
         body {{ width: 35em; margin: 0 auto; font-family: Tahoma, Verdana, Arial, sans-serif; }}\n\
         </style>\n</head>\n<body>\n<h1>Welcome to Ferrix{version}!</h1>\n\
         <p>If you see this page, the Ferrix web server is successfully installed and working. \
         No virtual host matched this request yet.</p>\n\
         <p>Add a virtual host in <code>ferrix.json</code> or the dashboard, and see the \
         documentation at <a href=\"https://arthurlunkes.github.io/ferrix\">arthurlunkes.github.io/ferrix</a>.</p>\n\
         <p>To hide this page set <code>\"default_page\": false</code> in the <code>http</code> \
         section; <code>\"server_tokens\": false</code> hides the version.</p>\n\
         <p><em>Thank you for using Ferrix.</em></p>\n</body>\n</html>\n"
    );
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}
