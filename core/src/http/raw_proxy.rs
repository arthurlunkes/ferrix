//! Raw proxy path for plain-HTTP/1.1 requests to a "plain" proxy route (see
//! `CompiledConfig::plain_proxy`), the case nginx is built around: the client's request
//! head is re-encoded straight into the upstream request and the upstream's response head
//! straight into the client response, without building `Request`/`Response` objects or
//! header maps in between. Retries, failover, health marks and timeouts behave exactly as
//! in `proxy_http_request`.

use axum::http::{header, Method};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tracing::warn;

use super::matcher::CompiledConfig;
use super::proxy::{should_retry_error, should_retry_status};
use super::server::{write_body, write_response, Framing};
use super::upstream::{ReadTimeoutBody, UpstreamClient};
use super::ProxyEngine;
use crate::config::RouteRule;

/// A request routed to a plain proxy route.
pub struct RawRoute {
    compiled: Arc<CompiledConfig>,
    vhost_index: usize,
    route_index: usize,
}

/// Request headers that never go upstream: hop-by-hop ones, the framing (requests on this
/// path have no body), `Host` (replaced by the upstream's authority) and the forwarding
/// headers Ferrix sets itself.
fn dropped_request_header(name: &[u8]) -> bool {
    const DROPPED: [&[u8]; 14] = [
        b"host",
        b"connection",
        b"keep-alive",
        b"proxy-authenticate",
        b"proxy-authorization",
        b"te",
        b"trailer",
        b"trailers",
        b"transfer-encoding",
        b"upgrade",
        b"content-length",
        b"x-real-ip",
        b"x-forwarded-for",
        b"x-forwarded-proto",
    ];
    DROPPED.iter().any(|d| d.eq_ignore_ascii_case(name))
}

impl ProxyEngine {
    /// The plain proxy route for this request, if it has one and nothing (metrics, a
    /// trusted-proxy setup) needs the full pipeline.
    pub(crate) fn raw_route(&self, port: u16, host: &str, path: &str) -> Option<RawRoute> {
        let compiled = self.store.compiled();
        let config = &compiled.config;
        if config.metrics.enabled
            || !config.real_ip.trusted_proxies.is_empty()
            || path.starts_with(crate::tls::acme::CHALLENGE_PREFIX)
        {
            return None;
        }
        let host = host.split(':').next().unwrap_or("");
        let vhost_index = compiled.find_vhost(host, port)?.vhost_index;
        let route_index = compiled.match_route(vhost_index, path)?.route_index;
        compiled
            .plain_proxy(vhost_index, route_index)
            .then_some(RawRoute {
                compiled,
                vhost_index,
                route_index,
            })
    }

    /// Proxies one request. `head` is the client's complete request head. Returns
    /// whether the client connection can stay open for another request.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn raw_proxy(
        &self,
        plan: RawRoute,
        head: &[u8],
        method: &Method,
        path: &str,
        client_addr: SocketAddr,
        tcp: &mut TcpStream,
        out: &mut Vec<u8>,
        keep_alive: bool,
        http10: bool,
    ) -> std::io::Result<bool> {
        let compiled = &*plan.compiled;
        let route = &compiled.config.vhosts[plan.vhost_index].routes[plan.route_index];
        let http = &compiled.config.http;
        let client_ip = client_addr.ip();

        let fail = |msg: String| {
            let mut resp = super::proxy::bad_gateway(&msg);
            super::filters::headers::server(compiled, resp.headers_mut());
            resp
        };

        let Some((upstream_name, upstream)) = route
            .upstream_name
            .as_ref()
            .and_then(|n| compiled.config.upstreams.get_key_value(n))
        else {
            let resp = fail("upstream not found".into());
            return write_response(tcp, out, resp, false, http10, keep_alive)
                .await
                .map(|_| keep_alive);
        };
        let targets = self.pool_manager.select_targets(upstream, client_ip);
        if targets.is_empty() {
            let resp = fail("No upstream targets available".into());
            return write_response(tcp, out, resp, false, http10, keep_alive)
                .await
                .map(|_| keep_alive);
        }

        let next_upstream = route.next_upstream();
        let max_tries = if next_upstream.conditions.is_empty() {
            1
        } else {
            next_upstream
                .tries
                .map(|t| t.max(1) as usize)
                .unwrap_or(targets.len())
                .min(targets.len())
        };
        let client = UpstreamClient::new(
            Duration::from_millis(
                route
                    .proxy_connect_timeout_ms
                    .unwrap_or(http.proxy_connect_timeout_ms),
            ),
            Duration::from_millis(
                route
                    .proxy_read_timeout_ms
                    .unwrap_or(http.proxy_read_timeout_ms),
            ),
            upstream.verify_tls,
        );
        let attempt_deadline = next_upstream
            .timeout_ms
            .map(|ms| crate::event::clock::now() + Duration::from_millis(ms));
        let track_conns = upstream.algorithm == crate::config::LoadBalanceAlgorithm::LeastConn;
        let target_path = target_path(route, path);
        let encode = |authority: &str, req: &mut Vec<u8>| {
            encode_request(req, head, method, &target_path, authority, client_ip)
        };

        let mut last_error: Option<String> = None;
        for (attempt, target) in targets.iter().enumerate() {
            if attempt >= max_tries {
                break;
            }
            if attempt_deadline.is_some_and(|d| crate::event::clock::now() >= d) {
                last_error = Some("tempo de failover esgotado".to_string());
                break;
            }
            let counted = track_conns || target.max_conns.is_some();
            if counted {
                self.pool_manager.inc_conn(&target.url);
            }
            let start_ms = crate::event::clock::mono_ms();
            out.clear();
            let result = client.send_raw(method, &target.url, encode, out).await;
            if counted {
                self.pool_manager.dec_conn(&target.url);
            }
            let elapsed_ms = crate::event::clock::mono_ms().saturating_sub(start_ms) as f64;

            match result {
                Ok((resp, read_timeout)) => {
                    if should_retry_status(&next_upstream.conditions, resp.status)
                        && attempt + 1 < max_tries
                    {
                        self.health_checker.mark_failure(
                            upstream_name,
                            &target.url,
                            target.max_fails,
                        );
                        last_error = Some(format!("upstream respondeu {}", resp.status));
                        continue;
                    }
                    self.health_checker
                        .mark_success(upstream_name, &target.url, elapsed_ms);

                    let target_index = upstream
                        .targets
                        .iter()
                        .position(|t| std::ptr::eq(t, *target))
                        .unwrap_or(0);
                    push_header(out, b"server", compiled.server_header.as_bytes());
                    if let Some(v) = compiled.target_header(upstream_name, target_index) {
                        push_header(
                            out,
                            super::X_FERRIX_UPSTREAM.as_str().as_bytes(),
                            v.as_bytes(),
                        );
                    }
                    if !resp.has_date {
                        push_header(
                            out,
                            header::DATE.as_str().as_bytes(),
                            crate::event::clock::http_date().as_bytes(),
                        );
                    }
                    let bodiless = (100..200).contains(&resp.status)
                        || resp.status == 204
                        || resp.status == 304;
                    let body = ReadTimeoutBody::new(resp.body, read_timeout);
                    let len = http_body::Body::size_hint(&body).exact();
                    let (framing, keep_alive) = match len {
                        _ if bodiless => (Framing::None, keep_alive),
                        Some(n) => (Framing::Length(n), keep_alive),
                        None if http10 => (Framing::None, false),
                        None => (Framing::Chunked, keep_alive),
                    };
                    super::server::finish_head(out, framing, keep_alive);
                    write_body(tcp, out, body, framing).await?;
                    return Ok(keep_alive);
                }
                Err(err) => {
                    let retryable = should_retry_error(&next_upstream.conditions, &err);
                    warn!(target = %target.url, error = %err, retryable, "failed to communicate with upstream");
                    self.health_checker
                        .mark_failure(upstream_name, &target.url, target.max_fails);
                    last_error = Some(err.to_string());
                    if !retryable {
                        break;
                    }
                }
            }
        }

        let msg = last_error.unwrap_or_else(|| "all upstream targets failed".to_string());
        let resp = fail(msg);
        write_response(tcp, out, resp, false, http10, keep_alive)
            .await
            .map(|_| keep_alive)
    }
}

fn target_path(route: &RouteRule, path: &str) -> String {
    let stripped = if route.strip_prefix {
        path.strip_prefix(route.path_prefix.as_str())
            .unwrap_or(path)
    } else {
        path
    };
    if stripped.starts_with('/') {
        stripped.to_string()
    } else {
        format!("/{}", stripped)
    }
}

fn push_header(out: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    out.extend_from_slice(name);
    out.extend_from_slice(b": ");
    out.extend_from_slice(value);
    out.extend_from_slice(b"\r\n");
}

/// The upstream request, like `prepare_forwarded_headers` + `h1::encode_head` produce it.
fn encode_request(
    out: &mut Vec<u8>,
    head: &[u8],
    method: &Method,
    path: &str,
    authority: &str,
    client_ip: IpAddr,
) {
    out.clear();
    out.extend_from_slice(method.as_str().as_bytes());
    out.push(b' ');
    out.extend_from_slice(path.as_bytes());
    out.extend_from_slice(b" HTTP/1.1\r\nhost: ");
    out.extend_from_slice(authority.as_bytes());
    out.extend_from_slice(b"\r\n");

    let mut headers = [httparse::EMPTY_HEADER; 100];
    let mut req = httparse::Request::new(&mut headers);
    // The head already parsed once on the way in, so this can't fail.
    let _ = req.parse(head);
    let mut xff = String::new();
    for h in req.headers.iter() {
        let name = h.name.as_bytes();
        if name.eq_ignore_ascii_case(b"x-forwarded-for") {
            for part in std::str::from_utf8(h.value)
                .unwrap_or("")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                xff.push_str(part);
                xff.push_str(", ");
            }
        }
        if dropped_request_header(name) {
            continue;
        }
        push_header(out, name, h.value);
    }
    let mut ip = [0u8; 64];
    let ip = {
        use std::io::Write;
        let mut cur = std::io::Cursor::new(&mut ip[..]);
        let _ = write!(cur, "{}", client_ip);
        let n = cur.position() as usize;
        &ip[..n]
    };
    push_header(out, b"x-real-ip", ip);
    out.extend_from_slice(b"x-forwarded-for: ");
    out.extend_from_slice(xff.as_bytes());
    out.extend_from_slice(ip);
    out.extend_from_slice(b"\r\nx-forwarded-proto: http\r\n");
    if matches!(*method, Method::POST | Method::PUT | Method::PATCH) {
        out.extend_from_slice(b"content-length: 0\r\n");
    }
    out.extend_from_slice(b"\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(head: &str, method: Method, ip: &str) -> String {
        let mut out = Vec::new();
        encode_request(
            &mut out,
            head.as_bytes(),
            &method,
            "/a?b=1",
            "10.0.0.1:8080",
            ip.parse().unwrap(),
        );
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn request_is_rewritten_like_the_full_pipeline() {
        let req = encoded(
            "GET /proxy/a?b=1 HTTP/1.1\r\nHost: example.com\r\nConnection: keep-alive\r\n\
             Accept: */*\r\nX-Real-IP: 6.6.6.6\r\nX-Forwarded-For: 1.1.1.1, 2.2.2.2\r\n\r\n",
            Method::GET,
            "9.9.9.9",
        );
        assert!(req.starts_with("GET /a?b=1 HTTP/1.1\r\nhost: 10.0.0.1:8080\r\n"));
        assert!(req.contains("Accept: */*\r\n"));
        assert!(req.contains("x-real-ip: 9.9.9.9\r\n"));
        assert!(req.contains("x-forwarded-for: 1.1.1.1, 2.2.2.2, 9.9.9.9\r\n"));
        assert!(req.contains("x-forwarded-proto: http\r\n"));
        assert!(!req.contains("example.com"));
        assert!(!req.contains("keep-alive"));
        assert!(!req.contains("6.6.6.6"));
        assert!(req.ends_with("\r\n\r\n"));
    }

    #[test]
    fn bodiless_post_gets_an_explicit_zero_length() {
        let req = encoded("POST /x HTTP/1.1\r\nHost: h\r\n\r\n", Method::POST, "::1");
        assert!(req.contains("x-forwarded-for: ::1\r\n"));
        assert!(req.contains("content-length: 0\r\n"));
    }

    #[test]
    fn target_path_strips_the_prefix() {
        let route = RouteRule {
            path_prefix: "/proxy".into(),
            strip_prefix: true,
            ..Default::default()
        };
        assert_eq!(target_path(&route, "/proxy/x?y"), "/x?y");
        assert_eq!(target_path(&route, "/proxy"), "/");
        assert_eq!(target_path(&route, "/proxyfoo"), "/foo");
    }
}
