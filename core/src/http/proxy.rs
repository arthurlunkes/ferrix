//! Reverse proxying (nginx `ngx_http_proxy_module`): buffering the request body,
//! building the upstream request headers, failover across targets
//! (`proxy_next_upstream`) and cleaning up the upstream response.

use super::*;

#[derive(Debug)]
pub(super) enum RequestBody {
    Buffered(Bytes),

    Streaming(Body),
}

impl ProxyEngine {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn proxy_http_request(
        &self,
        compiled: &CompiledConfig,
        route: &RouteRule,
        upstream_name: &str,
        upstream_cfg: &UpstreamConfig,
        targets: Vec<&UpstreamTarget>,
        is_tls: bool,
        client_ip: IpAddr,
        req: Request,
    ) -> Response {
        let http = &compiled.config.http;
        let (mut parts, body) = req.into_parts();
        let method = &parts.method;
        let path_and_query = parts
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");

        let target_path: std::borrow::Cow<str> = if route.strip_prefix {
            let stripped = path_and_query
                .strip_prefix(&route.path_prefix)
                .unwrap_or(path_and_query);
            if stripped.starts_with('/') {
                stripped.into()
            } else {
                format!("/{}", stripped).into()
            }
        } else {
            path_and_query.into()
        };

        if let Some(declared) = parts
            .headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
        {
            if declared > http.client_max_body_size {
                return payload_too_large(http.client_max_body_size);
            }
        }

        let request_body = match read_request_body(
            body,
            http.proxy_request_buffer_size,
            http.client_max_body_size,
        )
        .await
        {
            Ok(b) => b,
            Err(status) => {
                if status == StatusCode::PAYLOAD_TOO_LARGE {
                    return payload_too_large(http.client_max_body_size);
                }
                return (status, "400 Bad Request").into_response();
            }
        };

        let next_upstream = route.next_upstream();

        let can_retry = matches!(request_body, RequestBody::Buffered(_))
            && !next_upstream.conditions.is_empty();

        let max_tries = if can_retry {
            next_upstream
                .tries
                .map(|t| t.max(1) as usize)
                .unwrap_or(targets.len())
                .min(targets.len())
        } else {
            1
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
            upstream_cfg.verify_tls,
        );

        let attempt_deadline = next_upstream
            .timeout_ms
            .map(|ms| crate::event::clock::now() + Duration::from_millis(ms));

        let (buffered_body, mut streaming_body) = match request_body {
            RequestBody::Buffered(b) => (Some(b), None),
            RequestBody::Streaming(b) => (None, Some(b)),
        };

        prepare_forwarded_headers(&mut parts.headers, route, client_ip, is_tls);
        let headers = &parts.headers;
        let track_conns = upstream_cfg.algorithm == crate::config::LoadBalanceAlgorithm::LeastConn;

        let mut last_error: Option<String> = None;

        for (attempt, target) in targets.iter().enumerate() {
            if attempt >= max_tries {
                break;
            }
            if let Some(deadline) = attempt_deadline {
                if crate::event::clock::now() >= deadline {
                    last_error = Some("tempo de failover esgotado".to_string());
                    break;
                }
            }

            // Connection counts only matter to least_conn and max_conns; skip the shared
            // counters otherwise.
            let counted = track_conns || target.max_conns.is_some();
            if counted {
                self.pool_manager.inc_conn(&target.url);
            }
            let start_ms = crate::event::clock::mono_ms();

            let outgoing = match (&buffered_body, streaming_body.take()) {
                (Some(bytes), _) if !bytes.is_empty() => OutgoingBody::Buffered(bytes.clone()),
                (_, Some(body)) => OutgoingBody::Streaming(body),
                _ => OutgoingBody::Empty,
            };

            let result = client
                .send(method, &target.url, &target_path, headers, outgoing)
                .await;
            if counted {
                self.pool_manager.dec_conn(&target.url);
            }
            let elapsed_ms = crate::event::clock::mono_ms().saturating_sub(start_ms) as f64;

            match result {
                Ok(upstream_res) => {
                    let status = upstream_res.status();

                    if should_retry_status(&next_upstream.conditions, status.as_u16())
                        && attempt + 1 < max_tries
                    {
                        self.health_checker.mark_failure(
                            upstream_name,
                            &target.url,
                            target.max_fails,
                        );
                        last_error = Some(format!("upstream respondeu {}", status));
                        continue;
                    }

                    self.health_checker
                        .mark_success(upstream_name, &target.url, elapsed_ms);

                    let target_index = upstream_cfg
                        .targets
                        .iter()
                        .position(|t| std::ptr::eq(t, *target))
                        .unwrap_or(0);
                    return build_upstream_response(
                        upstream_res,
                        compiled.target_header(upstream_name, target_index),
                    );
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

        let err_msg = last_error.unwrap_or_else(|| "all upstream targets failed".to_string());
        bad_gateway(&err_msg)
    }
}

pub(super) fn bad_gateway(msg: &str) -> Response {
    Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(format!(
            "502 Bad Gateway - Ferrix Proxy Error: {}",
            msg
        )))
        .unwrap()
}

pub(super) const HOP_BY_HOP: [HeaderName; 9] = [
    header::CONNECTION,
    HeaderName::from_static("keep-alive"),
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    HeaderName::from_static("trailers"),
    header::TRANSFER_ENCODING,
    header::UPGRADE,
];

pub(super) fn strip_hop_by_hop(headers: &mut HeaderMap) {
    // Usually none are present; one pass over the (few) names beats nine hashed removals.
    if !headers.keys().any(|k| HOP_BY_HOP.contains(k)) {
        return;
    }
    for name in HOP_BY_HOP {
        headers.remove(name);
    }
}

const X_REAL_IP: HeaderName = HeaderName::from_static("x-real-ip");
const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");
pub const X_FERRIX_UPSTREAM: HeaderName = HeaderName::from_static("x-ferrix-upstream");

pub(super) fn build_upstream_response(
    upstream_res: Response,
    upstream_header: Option<&HeaderValue>,
) -> Response {
    let (mut parts, body) = upstream_res.into_parts();
    strip_hop_by_hop(&mut parts.headers);
    if let Some(v) = upstream_header {
        parts.headers.insert(X_FERRIX_UPSTREAM, v.clone());
    }
    Response::from_parts(parts, body)
}

/// Turns the client's request headers into the ones sent upstream, in place (the
/// request is consumed by the proxy anyway, so there's no need to copy the map).
pub(super) fn prepare_forwarded_headers(
    headers: &mut HeaderMap,
    route: &RouteRule,
    client_ip: IpAddr,
    is_tls: bool,
) {
    let scheme = scheme_str(is_tls);
    // Room for the X-Real-IP / X-Forwarded-* entries up front, so adding them never
    // rehashes the map.
    headers.reserve(3 + route.proxy_set_headers.len());
    let host = headers.remove(header::HOST);
    strip_hop_by_hop(headers);

    let ip = client_ip.to_string();
    let xff = RealIpResolver::proxy_add_x_forwarded_for(headers, &ip);
    let ip_value = HeaderValue::from_str(&ip).ok();
    let xff_value = HeaderValue::from_str(&xff).ok();

    if let Some(v) = &ip_value {
        headers.insert(X_REAL_IP, v.clone());
    }
    if let Some(v) = &xff_value {
        headers.insert(X_FORWARDED_FOR, v.clone());
    }
    headers.insert(X_FORWARDED_PROTO, HeaderValue::from_static(scheme));

    for (k, v) in &route.proxy_set_headers {
        let Ok(name) = HeaderName::from_bytes(k.as_bytes()) else {
            continue;
        };
        let value = match v.as_str() {
            "$host" => host.clone(),
            "$remote_addr" => ip_value.clone(),
            "$proxy_add_x_forwarded_for" => xff_value.clone(),
            "$scheme" => Some(HeaderValue::from_static(scheme)),
            other => HeaderValue::from_str(other).ok(),
        };
        if let Some(value) = value {
            headers.insert(name, value);
        }
    }
}

pub(super) fn scheme_str(is_tls: bool) -> &'static str {
    if is_tls {
        "https"
    } else {
        "http"
    }
}

async fn read_request_body(
    body: Body,
    buffer_limit: u64,
    max_size: u64,
) -> Result<RequestBody, StatusCode> {
    let mut stream = body.into_data_stream();
    let mut buffered: Vec<Bytes> = Vec::new();
    let mut total: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| StatusCode::BAD_REQUEST)?;
        total += chunk.len() as u64;

        if total > max_size {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }

        buffered.push(chunk);

        if total > buffer_limit {
            let prefix =
                futures_util::stream::iter(buffered.into_iter().map(Ok::<Bytes, std::io::Error>));
            let rest = stream.map(|r| r.map_err(std::io::Error::other));
            let joined = prefix.chain(rest);
            return Ok(RequestBody::Streaming(Body::from_stream(joined)));
        }
    }

    let mut flat = Vec::with_capacity(total as usize);
    for chunk in buffered {
        flat.extend_from_slice(&chunk);
    }
    Ok(RequestBody::Buffered(Bytes::from(flat)))
}

pub(super) fn should_retry_status(conditions: &[NextUpstreamCondition], status: u16) -> bool {
    conditions.iter().any(|c| match c {
        NextUpstreamCondition::Http500 => status == 500,
        NextUpstreamCondition::Http502 => status == 502,
        NextUpstreamCondition::Http503 => status == 503,
        NextUpstreamCondition::Http504 => status == 504,
        NextUpstreamCondition::Http403 => status == 403,
        NextUpstreamCondition::Http404 => status == 404,
        _ => false,
    })
}

pub(super) fn should_retry_error(
    conditions: &[NextUpstreamCondition],
    err: &UpstreamError,
) -> bool {
    if err.is_timeout() {
        return conditions.contains(&NextUpstreamCondition::Timeout)
            || conditions.contains(&NextUpstreamCondition::Error);
    }
    conditions.contains(&NextUpstreamCondition::Error)
}

fn payload_too_large(limit: u64) -> Response {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        format!(
            "413 Payload Too Large - o corpo excede client_max_body_size ({} bytes)",
            limit
        ),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProxyNextUpstreamConfig;

    #[test]
    fn hop_by_hop_headers_are_stripped() {
        let mut h = HeaderMap::new();
        h.insert("Connection", HeaderValue::from_static("keep-alive"));
        h.insert("transfer-encoding", HeaderValue::from_static("chunked"));
        h.insert("UPGRADE", HeaderValue::from_static("websocket"));
        h.insert("content-type", HeaderValue::from_static("text/plain"));
        h.insert("x-real-ip", HeaderValue::from_static("1.2.3.4"));
        strip_hop_by_hop(&mut h);
        assert!(!h.contains_key("connection"));
        assert!(!h.contains_key("transfer-encoding"));
        assert!(!h.contains_key("upgrade"));
        assert!(h.contains_key("content-type"));
        assert!(h.contains_key("x-real-ip"));
    }

    #[test]
    fn default_conditions_retry_502_and_504_but_not_500() {
        let c = ProxyNextUpstreamConfig::default().conditions;
        assert!(should_retry_status(&c, 502));
        assert!(should_retry_status(&c, 504));
        assert!(!should_retry_status(&c, 500));
        assert!(!should_retry_status(&c, 200));
    }

    #[test]
    fn empty_conditions_never_retry() {
        assert!(!should_retry_status(&[], 502));
    }

    #[test]
    fn explicit_conditions_are_honoured() {
        let c = vec![
            NextUpstreamCondition::Http500,
            NextUpstreamCondition::Http503,
        ];
        assert!(should_retry_status(&c, 500));
        assert!(should_retry_status(&c, 503));
        assert!(!should_retry_status(&c, 502));
    }

    #[tokio::test]
    async fn small_body_is_buffered_and_therefore_retryable() {
        let body = Body::from("payload pequeno");
        match read_request_body(body, 1024, 1_000_000).await.unwrap() {
            RequestBody::Buffered(b) => assert_eq!(&b[..], b"payload pequeno"),
            RequestBody::Streaming(_) => panic!("corpo pequeno deveria ser bufferizado"),
        }
    }

    #[tokio::test]
    async fn empty_body_is_buffered_as_empty() {
        match read_request_body(Body::empty(), 1024, 1_000_000)
            .await
            .unwrap()
        {
            RequestBody::Buffered(b) => assert!(b.is_empty()),
            RequestBody::Streaming(_) => panic!("corpo vazio deveria ser bufferizado"),
        }
    }

    #[tokio::test]
    async fn body_above_the_buffer_limit_switches_to_streaming() {
        let big = vec![b'x'; 5000];
        let body = Body::from(big);
        match read_request_body(body, 1024, 1_000_000).await.unwrap() {
            RequestBody::Streaming(_) => {}
            RequestBody::Buffered(_) => panic!("corpo grande deveria ir em streaming"),
        }
    }

    #[tokio::test]
    async fn body_above_the_hard_limit_is_rejected() {
        let big = vec![b'x'; 5000];
        let body = Body::from(big);
        let err = read_request_body(body, 1024, 2000).await.unwrap_err();
        assert_eq!(err, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn streamed_body_preserves_every_byte() {
        let original: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let body = Body::from(original.clone());

        let RequestBody::Streaming(streamed) =
            read_request_body(body, 1024, 1_000_000).await.unwrap()
        else {
            panic!("esperava streaming");
        };

        let collected = axum::body::to_bytes(streamed, usize::MAX).await.unwrap();
        assert_eq!(collected.len(), original.len());
        assert_eq!(&collected[..], &original[..]);
    }
}
