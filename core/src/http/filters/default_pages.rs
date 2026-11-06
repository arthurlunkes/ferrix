use axum::body::Body;
use axum::http::{header, HeaderValue, Response, StatusCode};

use crate::http::X_FERRIX_UPSTREAM;

#[derive(Clone, Copy)]
pub struct KeepBody;

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn render(code: StatusCode, location: Option<&str>, server_tokens: bool) -> String {
    let n = code.as_u16();
    let reason = code.canonical_reason().unwrap_or("Error");
    let (accent, hint) = match n {
        300..=399 => ("#3b82f6", "This page has moved."),
        400 => ("#ce422b", "The request could not be understood."),
        401 | 403 => ("#ce422b", "You do not have access to this resource."),
        404 => ("#ce422b", "The page you are looking for does not exist."),
        405 => ("#ce422b", "This method is not allowed here."),
        408 | 504 => ("#d97706", "The server took too long to answer."),
        413 => ("#ce422b", "The request body is too large."),
        414 => ("#ce422b", "The address is too long."),
        416 => ("#ce422b", "The requested range is not available."),
        429 => ("#d97706", "Too many requests. Try again in a moment."),
        502 | 503 => ("#d97706", "The service behind this site is not answering."),
        500..=599 => ("#ce422b", "Something went wrong on the server."),
        _ => ("#ce422b", ""),
    };
    let link = location
        .map(|l| {
            let l = escape(l);
            format!("<p class=\"go\"><a href=\"{l}\">{l}</a></p>")
        })
        .unwrap_or_default();
    let footer = if server_tokens {
        concat!("ferrix/", env!("CARGO_PKG_VERSION"))
    } else {
        "ferrix"
    };
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>{n} {reason}</title><style>\
:root{{--bg:#f7f5f2;--fg:#1c1917;--mute:#78716c;--card:#fff;--line:#e7e5e4}}\
@media(prefers-color-scheme:dark){{:root{{--bg:#0c0a09;--fg:#f5f5f4;--mute:#a8a29e;--card:#1c1917;--line:#292524}}}}\
*{{box-sizing:border-box}}body{{margin:0;min-height:100vh;display:grid;place-items:center;background:var(--bg);color:var(--fg);\
font:16px/1.5 system-ui,-apple-system,\"Segoe UI\",Roboto,sans-serif;padding:16px}}\
main{{width:100%;max-width:520px;background:var(--card);border:1px solid var(--line);border-radius:16px;padding:40px 32px;text-align:center;\
box-shadow:0 10px 30px rgba(0,0,0,.06)}}\
.code{{font:800 72px/1 ui-monospace,SFMono-Regular,Menlo,monospace;color:{accent};letter-spacing:-2px}}\
h1{{margin:12px 0 6px;font-size:22px}}p{{margin:0;color:var(--mute)}}.go{{margin-top:16px}}a{{color:{accent};word-break:break-all}}\
footer{{margin-top:28px;padding-top:16px;border-top:1px solid var(--line);font:13px ui-monospace,monospace;color:var(--mute)}}\
</style></head><body><main><div class=\"code\">{n}</div><h1>{reason}</h1><p>{hint}</p>{link}<footer>{footer}</footer></main></body></html>\n"
    )
}

pub fn apply(resp: Response<Body>, server_tokens: bool) -> Response<Body> {
    let status = resp.status();
    let n = status.as_u16();
    let is_redirect = (300..400).contains(&n) && n != 304;
    if !(n >= 400 || is_redirect)
        || resp.headers().contains_key(X_FERRIX_UPSTREAM)
        || resp.extensions().get::<KeepBody>().is_some()
    {
        return resp;
    }
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !(ct.is_empty() || ct.starts_with("text/plain")) {
        return resp;
    }
    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let page = render(status, location.as_deref(), server_tokens);
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    #[test]
    fn replaces_only_ferrix_bodies() {
        let r = apply((StatusCode::NOT_FOUND, "404").into_response(), true);
        assert_eq!(
            r.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        let mut keep = (StatusCode::NOT_FOUND, "custom").into_response();
        keep.extensions_mut().insert(KeepBody);
        assert_eq!(
            apply(keep, true).headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        let mut up = (StatusCode::BAD_GATEWAY, "x").into_response();
        up.headers_mut()
            .insert(X_FERRIX_UPSTREAM, HeaderValue::from_static("u"));
        assert_eq!(
            apply(up, true).headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        let ok = apply((StatusCode::OK, "fine").into_response(), true);
        assert_eq!(
            ok.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        let page = render(StatusCode::MOVED_PERMANENTLY, Some("/new?<x>"), false);
        assert!(page.contains("href=\"/new?&lt;x&gt;\"") && page.contains(">ferrix<"));
    }
}
