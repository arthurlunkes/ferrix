use axum::http::{header, HeaderMap, Method, Uri};
use std::collections::HashMap;
use std::hash::Hasher;
use std::net::IpAddr;
use std::sync::{LazyLock, RwLock};

use crate::config::{ConditionConfig, RewriteFlag, RewriteRule, VariableDef};
use crate::net::cidr::IpRule;

type RegexCache = HashMap<(String, bool), Option<regex_lite::Regex>>;
static REGEX: LazyLock<RwLock<RegexCache>> = LazyLock::new(|| RwLock::new(HashMap::new()));

pub fn regex(pattern: &str, ci: bool) -> Option<regex_lite::Regex> {
    let key = (pattern.to_string(), ci);
    if let Some(r) = REGEX.read().ok().and_then(|m| m.get(&key).cloned()) {
        return r;
    }
    let r = regex_lite::RegexBuilder::new(pattern)
        .case_insensitive(ci)
        .build()
        .ok();
    if let Ok(mut m) = REGEX.write() {
        m.insert(key, r.clone());
    }
    r
}

#[derive(Clone, Debug)]
pub struct SetVars(pub HashMap<String, String>);

pub struct Ctx<'a> {
    pub method: &'a Method,
    pub uri: Uri,
    pub headers: &'a HeaderMap,
    pub client_ip: IpAddr,
    pub is_tls: bool,
    pub http: &'a crate::config::HttpConfig,
    pub set: HashMap<String, String>,
    pub captures: Vec<String>,
    pub client_cert: Option<std::sync::Arc<crate::tls::client_cert::ClientCert>>,
}

fn query_arg<'q>(q: &'q str, name: &str) -> Option<&'q str> {
    q.split('&')
        .map(|kv| kv.split_once('=').unwrap_or((kv, "")))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

impl Ctx<'_> {
    pub fn var(&self, name: &str) -> String {
        self.var_depth(name, 0)
    }

    fn var_depth(&self, name: &str, depth: u8) -> String {
        if let Ok(i) = name.parse::<usize>() {
            return self.captures.get(i).cloned().unwrap_or_default();
        }
        if let Some(v) = self.set.get(name) {
            return v.clone();
        }
        if let Some(def) = self.http.variables.get(name) {
            if depth < 8 {
                return self.eval_def(def, depth + 1);
            }
            return String::new();
        }
        let q = self.uri.query().unwrap_or("");
        let hdr = |h: &str| {
            self.headers
                .get(h)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string()
        };
        match name {
            "uri" | "document_uri" => self.uri.path().to_string(),
            "request_uri" => self
                .uri
                .path_and_query()
                .map(|p| p.as_str().to_string())
                .unwrap_or_else(|| "/".into()),
            "args" | "query_string" => q.to_string(),
            "is_args" => if q.is_empty() { "" } else { "?" }.to_string(),
            "host" => hdr("host")
                .split(':')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase(),
            "remote_addr" | "binary_remote_addr" => self.client_ip.to_string(),
            "scheme" => if self.is_tls { "https" } else { "http" }.to_string(),
            "https" => if self.is_tls { "on" } else { "" }.to_string(),
            "request_method" => self.method.to_string(),
            #[cfg(feature = "geoip")]
            n if n.starts_with("geoip_") => match &self.http.geoip {
                Some(g) => crate::http::geoip::variable(g, n, self.client_ip).unwrap_or_default(),
                None => String::new(),
            },
            "ssl_client_verify" => if self.client_cert.is_some() {
                "SUCCESS"
            } else {
                "NONE"
            }
            .to_string(),
            "ssl_client_s_dn" => self
                .client_cert
                .as_ref()
                .map(|c| c.subject.clone())
                .unwrap_or_default(),
            "ssl_client_i_dn" => self
                .client_cert
                .as_ref()
                .map(|c| c.issuer.clone())
                .unwrap_or_default(),
            "ssl_client_escaped_cert" => self
                .client_cert
                .as_ref()
                .map(|c| c.escaped_pem())
                .unwrap_or_default(),
            "content_type" => hdr("content-type"),
            "content_length" => hdr("content-length"),
            n if n.starts_with("arg_") => query_arg(q, &n[4..]).unwrap_or("").to_string(),
            n if n.starts_with("http_") => hdr(&n[5..].replace('_', "-")),
            n if n.starts_with("cookie_") => self
                .headers
                .get_all(header::COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|v| v.split(';'))
                .filter_map(|kv| kv.trim().split_once('='))
                .find(|(k, _)| *k == &n[7..])
                .map(|(_, v)| v.to_string())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    fn eval_def(&self, def: &VariableDef, depth: u8) -> String {
        match def {
            VariableDef::Map {
                source,
                entries,
                default,
            } => {
                let src = self.expand_depth(source, depth);
                for e in entries {
                    let hit = if let Some(p) = e.pattern.strip_prefix("~*") {
                        regex(p, true).is_some_and(|r| r.is_match(&src))
                    } else if let Some(p) = e.pattern.strip_prefix('~') {
                        regex(p, false).is_some_and(|r| r.is_match(&src))
                    } else {
                        e.pattern == src
                    };
                    if hit {
                        return self.expand_depth(&e.value, depth);
                    }
                }
                self.expand_depth(default, depth)
            }
            VariableDef::Geo {
                source,
                ranges,
                default,
            } => {
                let src = match source {
                    Some(s) => self.expand_depth(s, depth),
                    None => self.client_ip.to_string(),
                };
                let Ok(ip) = src.parse::<IpAddr>() else {
                    return default.clone();
                };
                ranges
                    .iter()
                    .filter_map(|r| {
                        let rule = IpRule::parse(&r.network).ok()?;
                        let len = match &rule {
                            IpRule::Net(n) => n.prefix_len(),
                            IpRule::Addr(_) => 128,
                            IpRule::Any => 0,
                        };
                        rule.matches(ip).then_some((len, &r.value))
                    })
                    .max_by_key(|(l, _)| *l)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| default.clone())
            }
            VariableDef::Json { source, path } => {
                let src = self.expand_depth(source, depth);
                json_path(&src, path).unwrap_or_default()
            }
            VariableDef::SplitClients { source, buckets } => {
                let src = self.expand_depth(source, depth);
                let mut h = crate::fasthash::FxHasher::default();
                h.write(src.as_bytes());
                let point = (h.finish() % 10_000) as f64 / 100.0;
                let mut acc = 0.0;
                for b in buckets {
                    acc += b.percent;
                    if point < acc {
                        return b.value.clone();
                    }
                }
                buckets
                    .iter()
                    .find(|b| b.percent == 0.0)
                    .map(|b| b.value.clone())
                    .unwrap_or_default()
            }
        }
    }

    pub fn expand(&self, template: &str) -> String {
        self.expand_depth(template, 0)
    }

    fn expand_depth(&self, template: &str, depth: u8) -> String {
        if !template.contains('$') {
            return template.to_string();
        }
        let mut out = String::with_capacity(template.len() + 32);
        let mut rest = template;
        while let Some(i) = rest.find('$') {
            out.push_str(&rest[..i]);
            let after = &rest[i + 1..];
            let (name, tail) = if let Some(b) = after.strip_prefix('{') {
                match b.find('}') {
                    Some(e) => (&b[..e], &b[e + 1..]),
                    None => ("", after),
                }
            } else if after.starts_with(|c: char| c.is_ascii_digit()) {
                (&after[..1], &after[1..])
            } else {
                let e = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                (&after[..e], &after[e..])
            };
            if name.is_empty() {
                out.push('$');
            } else {
                out.push_str(&self.var_depth(name, depth));
            }
            rest = tail;
        }
        out.push_str(rest);
        out
    }

    fn capture(&mut self, caps: regex_lite::Captures<'_>) {
        self.captures = (0..caps.len())
            .map(|i| {
                caps.get(i)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default()
            })
            .collect();
    }

    pub fn test(&mut self, cond: &str, root: Option<&str>) -> bool {
        let c = cond.trim();
        let toks: Vec<&str> = split_condition(c);
        match toks.as_slice() {
            [v] => {
                let s = self.expand(v);
                !s.is_empty() && s != "0"
            }
            [op, path] if op.ends_with('f') || op.ends_with('d') || op.ends_with('e') => {
                let neg = op.starts_with('!');
                let p = self.expand(path);
                let full = match root {
                    Some(r) if !p.starts_with(r) => {
                        format!("{}/{}", r.trim_end_matches('/'), p.trim_start_matches('/'))
                    }
                    _ => p,
                };
                let m = std::fs::metadata(&full);
                let ok = match op.trim_start_matches('!') {
                    "-f" => m.as_ref().is_ok_and(|m| m.is_file()),
                    "-d" => m.as_ref().is_ok_and(|m| m.is_dir()),
                    _ => m.is_ok(),
                };
                ok != neg
            }
            [l, op, r] => {
                let left = self.expand(l);
                let right = unquote(r);
                match *op {
                    "=" => left == self.expand(right),
                    "!=" => left != self.expand(right),
                    "~" | "~*" | "!~" | "!~*" => {
                        let ci = op.ends_with('*');
                        let neg = op.starts_with('!');
                        match regex(right, ci).and_then(|re| {
                            re.captures(&left).map(|c| {
                                let v: Vec<String> = (0..c.len())
                                    .map(|i| {
                                        c.get(i).map(|m| m.as_str().to_string()).unwrap_or_default()
                                    })
                                    .collect();
                                v
                            })
                        }) {
                            Some(caps) => {
                                if !neg {
                                    self.captures = caps;
                                }
                                !neg
                            }
                            None => neg,
                        }
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }
}

pub fn json_path(doc: &str, path: &str) -> Option<String> {
    let mut v: serde_json::Value = serde_json::from_str(doc).ok()?;
    let mut rest = path.trim();
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('.') {
            let end = r.find(['.', '[']).unwrap_or(r.len());
            let key = &r[..end];
            if !key.is_empty() {
                v = v.get_mut(key)?.take();
            }
            rest = &r[end..];
        } else if let Some(r) = rest.strip_prefix('[') {
            let end = r.find(']')?;
            let inner = r[..end].trim();
            v = match inner.trim_matches('"').parse::<usize>() {
                Ok(i) if !inner.starts_with('"') => v.get_mut(i)?.take(),
                _ => v.get_mut(inner.trim_matches('"'))?.take(),
            };
            rest = &r[end + 1..];
        } else {
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            v = v.get_mut(&rest[..end])?.take();
            rest = &rest[end..];
        }
    }
    Some(match v {
        serde_json::Value::String(s) => s,
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    })
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(s)
}

fn split_condition(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let start = i;
        if b[i] == b'"' || b[i] == b'\'' {
            let q = b[i];
            i += 1;
            while i < b.len() && b[i] != q {
                i += 1;
            }
            i = (i + 1).min(b.len());
        } else {
            while i < b.len() && !b[i].is_ascii_whitespace() {
                i += 1;
            }
        }
        out.push(&s[start..i]);
    }
    out
}

pub enum Phase {
    Continue,
    Rematch,
    Return { code: u16, location_or_body: String },
}

pub fn run_route_phase(
    ctx: &mut Ctx<'_>,
    conditions: &[ConditionConfig],
    set: &[(String, String)],
    rewrites: &[RewriteRule],
    root: Option<&str>,
) -> Phase {
    for c in conditions {
        if ctx.test(&c.condition, root) {
            for (k, v) in &c.set {
                let val = ctx.expand(v);
                ctx.set.insert(k.trim_start_matches('$').to_string(), val);
            }
            if let Some(code) = c.return_code {
                let text = ctx.expand(c.return_value.as_deref().unwrap_or(""));
                return Phase::Return {
                    code,
                    location_or_body: text,
                };
            }
            if let Some(rw) = &c.rewrite {
                if let Some(p) = apply_rewrite(ctx, rw) {
                    return p;
                }
            }
        }
    }
    for (k, v) in set {
        let val = ctx.expand(v);
        ctx.set.insert(k.trim_start_matches('$').to_string(), val);
    }
    for rw in rewrites {
        if let Some(p) = apply_rewrite(ctx, rw) {
            return p;
        }
    }
    Phase::Continue
}

fn apply_rewrite(ctx: &mut Ctx<'_>, rw: &RewriteRule) -> Option<Phase> {
    let re = regex(&rw.pattern, false)?;
    let path = ctx.uri.path().to_string();
    let caps = re.captures(&path)?;
    ctx.capture(caps);
    let target = ctx.expand(&rw.replacement);

    let external = target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with("$scheme");
    match rw.flag {
        RewriteFlag::Redirect | RewriteFlag::Permanent => {}
        _ if external => {}
        _ => {
            let new = if target.contains('?') || ctx.uri.query().is_none() {
                target.trim_end_matches('?').to_string()
            } else {
                format!("{}?{}", target, ctx.uri.query().unwrap_or(""))
            };
            if let Ok(u) = new.parse::<Uri>() {
                ctx.uri = u;
            }
            return match rw.flag {
                RewriteFlag::Last => Some(Phase::Rematch),
                RewriteFlag::Break => Some(Phase::Continue),
                _ => None,
            };
        }
    }
    let code = if rw.flag == RewriteFlag::Permanent {
        301
    } else {
        302
    };
    Some(Phase::Return {
        code,
        location_or_body: target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GeoRange, MapEntry, SplitBucket};

    fn ctx<'a>(defs: &'a HashMap<String, VariableDef>, h: &'a HeaderMap, uri: &str) -> Ctx<'a> {
        static GET: Method = Method::GET;
        let http: &'static crate::config::HttpConfig =
            Box::leak(Box::new(crate::config::HttpConfig {
                variables: defs.clone(),
                ..Default::default()
            }));
        Ctx {
            method: &GET,
            uri: uri.parse().unwrap(),
            headers: h,
            client_ip: "10.1.2.3".parse().unwrap(),
            is_tls: false,
            http,
            set: HashMap::new(),
            captures: vec![],
            client_cert: None,
        }
    }

    #[test]
    fn builtins_map_geo_split() {
        let mut defs = HashMap::new();
        defs.insert(
            "mobile".to_string(),
            VariableDef::Map {
                source: "$http_user_agent".into(),
                entries: vec![MapEntry {
                    pattern: "~*iphone|android".into(),
                    value: "1".into(),
                }],
                default: "0".into(),
            },
        );
        defs.insert(
            "office".to_string(),
            VariableDef::Geo {
                source: None,
                ranges: vec![
                    GeoRange {
                        network: "10.0.0.0/8".into(),
                        value: "lan".into(),
                    },
                    GeoRange {
                        network: "10.1.0.0/16".into(),
                        value: "office".into(),
                    },
                ],
                default: "wan".into(),
            },
        );
        defs.insert(
            "ab".to_string(),
            VariableDef::SplitClients {
                source: "$remote_addr".into(),
                buckets: vec![SplitBucket {
                    percent: 100.0,
                    value: "A".into(),
                }],
            },
        );
        let mut h = HeaderMap::new();
        h.insert("user-agent", "Mozilla (iPhone)".parse().unwrap());
        h.insert("host", "Ex.com:80".parse().unwrap());
        h.insert("cookie", "a=1; sid=xyz".parse().unwrap());
        let c = ctx(&defs, &h, "/p?id=7");
        assert_eq!(
            c.expand("$host$uri $arg_id $cookie_sid ${mobile}-$office-$ab $http_user_agent"),
            "ex.com/p 7 xyz 1-office-A Mozilla (iPhone)"
        );
    }

    #[test]
    fn conditions_and_rewrites() {
        let defs = HashMap::new();
        let h = HeaderMap::new();
        let mut c = ctx(&defs, &h, "/old/42?x=1");
        assert!(c.test("$request_uri ~ ^/old/(\\d+)", None));
        assert_eq!(c.expand("$1"), "42");
        assert!(c.test("$arg_x = 1", None));
        assert!(!c.test("$arg_y", None));
        assert!(c.test("$http_user_agent !~* bot", None));

        let rw = vec![RewriteRule {
            pattern: "^/old/(.*)$".into(),
            replacement: "/new/$1".into(),
            flag: RewriteFlag::Last,
        }];
        assert!(matches!(
            run_route_phase(&mut c, &[], &[], &rw, None),
            Phase::Rematch
        ));
        assert_eq!(c.uri.to_string(), "/new/42?x=1");

        let rw = vec![RewriteRule {
            pattern: "^/new/(.*)$".into(),
            replacement: "https://ex.com/$1".into(),
            flag: RewriteFlag::Permanent,
        }];
        match run_route_phase(&mut c, &[], &[], &rw, None) {
            Phase::Return {
                code,
                location_or_body,
            } => {
                assert_eq!(code, 301);
                assert_eq!(location_or_body, "https://ex.com/42");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn if_return_and_set() {
        let defs = HashMap::new();
        let h = HeaderMap::new();
        let mut c = ctx(&defs, &h, "/x");
        let conds = vec![ConditionConfig {
            condition: "$request_method = GET".into(),
            set: vec![],
            return_code: Some(403),
            return_value: Some("no $uri".into()),
            rewrite: None,
        }];
        match run_route_phase(&mut c, &conds, &[("$v".into(), "1".into())], &[], None) {
            Phase::Return {
                code,
                location_or_body,
            } => {
                assert_eq!((code, location_or_body.as_str()), (403, "no /x"));
            }
            _ => panic!(),
        }
        let mut c = ctx(&defs, &h, "/x");
        run_route_phase(&mut c, &[], &[("$v".into(), "a$uri".into())], &[], None);
        assert_eq!(c.var("v"), "a/x");
    }

    #[test]
    fn json_paths() {
        let doc = r#"{"user":{"name":"Ana","roles":["admin","dev"],"age":30},"a b":{"x":true}}"#;
        assert_eq!(json_path(doc, ".user.name").as_deref(), Some("Ana"));
        assert_eq!(json_path(doc, "user.roles[1]").as_deref(), Some("dev"));
        assert_eq!(json_path(doc, ".user.age").as_deref(), Some("30"));
        assert_eq!(json_path(doc, r#"["a b"].x"#).as_deref(), Some("true"));
        assert_eq!(
            json_path(doc, ".user.roles").as_deref(),
            Some(r#"["admin","dev"]"#)
        );
        assert_eq!(json_path(doc, ".nope"), None);
        assert_eq!(json_path("not json", ".a"), None);
    }
}
