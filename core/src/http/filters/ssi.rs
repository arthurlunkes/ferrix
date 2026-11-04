//! nginx `ngx_http_ssi_filter_module` (common subset) for text/html:
//! `include file|virtual`, `echo var [default]`, `set var value`,
//! `if expr` / `elif` / `else` / `endif`, and `config` (ignored).
//!
//! Includes are read from the route's root (a `virtual` path is resolved as a
//! file there, no subrequest), nested up to 10 levels.

use axum::body::Body;
use axum::http::{header, HeaderValue, Response};
use std::path::{Path, PathBuf};

use crate::http::vars::Ctx;

const MAX_BODY: usize = 8 << 20;
const MAX_DEPTH: u8 = 10;
const ERROR: &str = "[an error occurred while processing the directive]";

fn params(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq].trim().to_string();
        let after = rest[eq + 1..].trim_start();
        let (val, tail) = match after.chars().next() {
            Some(q @ ('"' | '\'')) => match after[1..].find(q) {
                Some(e) => (&after[1..1 + e], &after[e + 2..]),
                None => (&after[1..], ""),
            },
            _ => {
                let e = after.find(char::is_whitespace).unwrap_or(after.len());
                (&after[..e], &after[e..])
            }
        };
        out.push((key, val.to_string()));
        rest = tail.trim_start();
    }
    out
}

fn resolve(root: &Path, dir: &str, p: &str) -> Option<PathBuf> {
    let joined = if p.starts_with('/') {
        root.join(p.trim_start_matches('/'))
    } else {
        root.join(dir.trim_start_matches('/')).join(p)
    };
    let canon = joined.canonicalize().ok()?;
    canon
        .starts_with(root.canonicalize().ok()?)
        .then_some(canon)
}

/// Processes SSI commands in `text`; `dir` is the document's URL directory.
pub fn process(text: &str, root: Option<&Path>, dir: &str, ctx: &mut Ctx<'_>, depth: u8) -> String {
    let mut out = String::with_capacity(text.len());
    // Stack of (this branch active, some branch already taken).
    let mut cond: Vec<(bool, bool)> = Vec::new();
    let active = |c: &Vec<(bool, bool)>| c.iter().all(|(a, _)| *a);
    let mut rest = text;
    while let Some(i) = rest.find("<!--#") {
        if active(&cond) {
            out.push_str(&rest[..i]);
        }
        let after = &rest[i + 5..];
        let Some(end) = after.find("-->") else {
            if active(&cond) {
                out.push_str(&rest[i..]);
            }
            return out;
        };
        let body = after[..end].trim();
        rest = &after[end + 3..];
        let (cmd, args) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
        let p = params(args);
        let get = |k: &str| p.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        match cmd {
            "if" => {
                let on = active(&cond) && eval(get("expr").unwrap_or(""), ctx);
                cond.push((on, on));
            }
            "elif" => {
                if let Some(top) = cond.pop() {
                    let parent = active(&cond);
                    let on = parent && !top.1 && eval(get("expr").unwrap_or(""), ctx);
                    cond.push((on, top.1 || on));
                }
            }
            "else" => {
                if let Some(top) = cond.pop() {
                    let on = active(&cond) && !top.1;
                    cond.push((on, true));
                }
            }
            "endif" => {
                cond.pop();
            }
            _ if !active(&cond) => {}
            "echo" => {
                let v = ctx.var(get("var").unwrap_or("").trim_start_matches('$'));
                if v.is_empty() {
                    out.push_str(get("default").unwrap_or("(none)"));
                } else {
                    out.push_str(&html_escape(&v));
                }
            }
            "set" => {
                if let (Some(k), Some(v)) = (get("var"), get("value")) {
                    let val = ctx.expand(v);
                    ctx.set.insert(k.trim_start_matches('$').to_string(), val);
                }
            }
            "include" => {
                let target = get("file").or(get("virtual")).map(|t| ctx.expand(t));
                let content = match (root, target) {
                    (Some(r), Some(t)) if depth < MAX_DEPTH => {
                        let t = t.split('?').next().unwrap_or("").to_string();
                        resolve(r, dir, &t)
                            .and_then(|f| std::fs::read_to_string(f).ok())
                            .map(|c| {
                                let sub_dir = t.rsplit_once('/').map(|(d, _)| d).unwrap_or(dir);
                                process(&c, root, sub_dir, ctx, depth + 1)
                            })
                    }
                    _ => None,
                };
                out.push_str(content.as_deref().unwrap_or(ERROR));
            }
            "config" => {}
            _ => out.push_str(ERROR),
        }
    }
    if active(&cond) {
        out.push_str(rest);
    }
    out
}

/// SSI `expr`: `$var`, `$var = text`, `$var != text`, `$var = /regex/`.
fn eval(expr: &str, ctx: &mut Ctx<'_>) -> bool {
    let e = expr.trim();
    for op in ["!=", "="] {
        if let Some((l, r)) = e.split_once(op) {
            let left = ctx.expand(l.trim());
            let r = r.trim();
            let hit = match r.strip_prefix('/').and_then(|x| x.strip_suffix('/')) {
                Some(re) => {
                    crate::http::vars::regex(re, false).is_some_and(|re| re.is_match(&left))
                }
                None => left == ctx.expand(r),
            };
            return if op == "!=" { !hit } else { hit };
        }
    }
    let v = ctx.expand(e);
    !v.is_empty()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn applies(resp: &Response<Body>, types: &[String]) -> bool {
    if resp.headers().contains_key(header::CONTENT_ENCODING) {
        return false;
    }
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mime = ct.split(';').next().unwrap_or("").trim();
    mime.eq_ignore_ascii_case("text/html")
        || types
            .iter()
            .any(|t| t == "*" || t.eq_ignore_ascii_case(mime))
}

pub async fn apply(
    resp: Response<Body>,
    root: Option<&str>,
    url_dir: &str,
    ctx: &mut Ctx<'_>,
) -> Response<Body> {
    let (mut parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_BODY).await else {
        return Response::from_parts(parts, Body::empty());
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    if !text.contains("<!--#") {
        return Response::from_parts(parts, Body::from(bytes));
    }
    let root = root.map(PathBuf::from);
    let out = process(text, root.as_deref(), url_dir, ctx, 0);
    for h in [header::ETAG, header::LAST_MODIFIED, header::ACCEPT_RANGES] {
        parts.headers.remove(h);
    }
    parts
        .headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from(out.len()));
    Response::from_parts(parts, Body::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, Method};
    use std::collections::HashMap;

    #[test]
    fn commands() {
        let root = std::env::temp_dir().join(format!("ferrix-ssi-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("inc")).unwrap();
        std::fs::write(root.join("inc/h.html"), "<h1><!--# echo var=\"t\" --></h1>").unwrap();
        let http = crate::config::HttpConfig::default();
        let h = HeaderMap::new();
        let mut ctx = Ctx {
            method: &Method::GET,
            uri: "/p/index.html?x=1".parse().unwrap(),
            headers: &h,
            client_ip: "127.0.0.1".parse().unwrap(),
            is_tls: false,
            http: &http,
            set: HashMap::new(),
            captures: vec![],
            client_cert: None,
        };
        let page = r#"<!--# set var="t" value="Hi <b>" --><!--# include virtual="/inc/h.html" -->
<!--# if expr="$arg_x = 1" -->one<!--# elif expr="$arg_x" -->other<!--# else -->none<!--# endif -->
<!--# echo var="missing" default="dflt" --><!--# include file="../../etc/passwd" -->"#;
        let out = process(page, Some(&root), "/p", &mut ctx, 0);
        assert_eq!(out, format!("<h1>Hi &lt;b&gt;</h1>\none\ndflt{ERROR}"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
