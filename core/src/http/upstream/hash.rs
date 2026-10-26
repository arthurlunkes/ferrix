//! Ketama-style consistent hashing for the `consistent_hash` balancing algorithm.

use axum::http::{header, request::Parts};
use std::net::IpAddr;

use crate::config::UpstreamConfig;

const POINTS_PER_WEIGHT: usize = 160;

/// FNV-1a: stable across processes and Rust versions, so every Ferrix instance behind
/// a load balancer maps a key to the same server.
pub fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashKey {
    RemoteAddr,
    RequestUri,
    Uri,
    Host,
    Header(String),
    Cookie(String),
    Arg(String),
}

impl HashKey {
    pub fn parse(spec: Option<&str>) -> Self {
        let spec = spec.unwrap_or("$remote_addr").trim();
        match spec {
            "$remote_addr" | "$binary_remote_addr" => HashKey::RemoteAddr,
            "$request_uri" => HashKey::RequestUri,
            "$uri" => HashKey::Uri,
            "$host" | "$http_host" => HashKey::Host,
            _ => {
                if let Some(h) = spec.strip_prefix("$http_") {
                    HashKey::Header(h.replace('_', "-").to_ascii_lowercase())
                } else if let Some(c) = spec.strip_prefix("$cookie_") {
                    HashKey::Cookie(c.to_string())
                } else if let Some(a) = spec.strip_prefix("$arg_") {
                    HashKey::Arg(a.to_string())
                } else {
                    HashKey::RemoteAddr
                }
            }
        }
    }

    pub fn hash(&self, parts: &Parts, client_ip: IpAddr) -> u64 {
        let bytes: &[u8] = match self {
            HashKey::RemoteAddr => {
                return match client_ip {
                    IpAddr::V4(v4) => fnv1a(&v4.octets()),
                    IpAddr::V6(v6) => fnv1a(&v6.octets()),
                }
            }
            HashKey::RequestUri => parts
                .uri
                .path_and_query()
                .map(|p| p.as_str().as_bytes())
                .unwrap_or(b"/"),
            HashKey::Uri => parts.uri.path().as_bytes(),
            HashKey::Host => parts
                .headers
                .get(header::HOST)
                .map(|v| v.as_bytes())
                .unwrap_or(b""),
            HashKey::Header(name) => parts
                .headers
                .get(name.as_str())
                .map(|v| v.as_bytes())
                .unwrap_or(b""),
            HashKey::Cookie(name) => parts
                .headers
                .get_all(header::COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|v| v.split(';'))
                .filter_map(|kv| kv.trim().split_once('='))
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_bytes())
                .unwrap_or(b""),
            HashKey::Arg(name) => parts
                .uri
                .query()
                .unwrap_or("")
                .split('&')
                .filter_map(|kv| kv.split_once('='))
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_bytes())
                .unwrap_or(b""),
        };
        fnv1a(bytes)
    }
}

#[derive(Debug, Clone)]
pub struct HashRing {
    pub key: HashKey,
    // (point, index into upstream.targets), sorted by point.
    points: Vec<(u64, usize)>,
    targets: usize,
}

impl HashRing {
    pub fn build(upstream: &UpstreamConfig) -> Self {
        let mut points = Vec::new();
        for (i, t) in upstream.targets.iter().enumerate() {
            if t.backup {
                continue;
            }
            for p in 0..POINTS_PER_WEIGHT * t.weight.max(1) as usize {
                points.push((fnv1a(format!("{}-{}", t.url, p).as_bytes()), i));
            }
        }
        points.sort_unstable();
        Self {
            key: HashKey::parse(upstream.hash_key.as_deref()),
            points,
            targets: upstream.targets.len(),
        }
    }

    /// Target indices in failover order for `hash`: the owner of the key first, then the
    /// next distinct servers clockwise on the ring.
    pub fn order(&self, hash: u64) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.targets);
        if self.points.is_empty() {
            return out;
        }
        let start = self.points.partition_point(|(p, _)| *p < hash);
        for k in 0..self.points.len() {
            let (_, idx) = self.points[(start + k) % self.points.len()];
            if !out.contains(&idx) {
                out.push(idx);
                if out.len() == self.targets {
                    break;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LoadBalanceAlgorithm, UpstreamTarget};

    fn upstream(urls: &[&str]) -> UpstreamConfig {
        UpstreamConfig {
            name: "u".into(),
            algorithm: LoadBalanceAlgorithm::ConsistentHash,
            targets: urls
                .iter()
                .map(|u| UpstreamTarget {
                    url: u.to_string(),
                    weight: 1,
                    max_fails: 3,
                    fail_timeout_secs: 10,
                    backup: false,
                    down: false,
                    max_conns: None,
                })
                .collect(),
            health_check: None,
            verify_tls: true,
            hash_key: Some("$request_uri".into()),
        }
    }

    #[test]
    fn same_key_same_server_and_every_server_is_listed() {
        let ring = HashRing::build(&upstream(&["a", "b", "c"]));
        let h = fnv1a(b"/x");
        assert_eq!(ring.order(h), ring.order(h));
        let mut all = ring.order(h);
        all.sort();
        assert_eq!(all, vec![0, 1, 2]);
    }

    #[test]
    fn removing_a_server_only_moves_its_keys() {
        let three = HashRing::build(&upstream(&["a", "b", "c"]));
        let two = HashRing::build(&upstream(&["a", "b"]));
        let mut moved = 0;
        for i in 0..2000 {
            let h = fnv1a(format!("/k{}", i).as_bytes());
            let before = three.order(h)[0];
            let after = two.order(h)[0];
            if before != 2 && before != after {
                moved += 1;
            }
        }
        assert_eq!(moved, 0, "keys owned by surviving servers must not move");
    }

    #[test]
    fn key_specs_parse() {
        assert_eq!(HashKey::parse(None), HashKey::RemoteAddr);
        assert_eq!(
            HashKey::parse(Some("$http_x_user")),
            HashKey::Header("x-user".into())
        );
        assert_eq!(
            HashKey::parse(Some("$cookie_sid")),
            HashKey::Cookie("sid".into())
        );
        assert_eq!(HashKey::parse(Some("$arg_id")), HashKey::Arg("id".into()));
    }
}
