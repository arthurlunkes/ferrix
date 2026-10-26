//! Response compression (gzip, brotli, zstd), negotiated from `Accept-Encoding`.

use async_compression::tokio::bufread::{BrotliEncoder, GzipEncoder, ZstdEncoder};
use async_compression::Level;
use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, HeaderValue, Response, StatusCode};
use futures_util::TryStreamExt;
use tokio_util::io::{ReaderStream, StreamReader};

use crate::config::{CompressionAlgorithm, CompressionConfig, CompressionLevel};

/// Picks the first algorithm from the configured preference order that the client
/// accepts with a non-zero q-value.
pub fn negotiate(headers: &HeaderMap, cfg: &CompressionConfig) -> Option<CompressionAlgorithm> {
    let accept = headers
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>();
    if accept.is_empty() {
        return None;
    }
    let q_of = |name: &str| -> Option<f32> {
        let mut wildcard = None;
        for item in accept.iter().flat_map(|v| v.split(',')) {
            let mut parts = item.split(';');
            let coding = parts.next().unwrap_or("").trim();
            let q = parts
                .filter_map(|p| p.trim().strip_prefix("q="))
                .find_map(|q| q.trim().parse::<f32>().ok())
                .unwrap_or(1.0);
            if coding.eq_ignore_ascii_case(name) {
                return Some(q);
            }
            if coding == "*" {
                wildcard = Some(q);
            }
        }
        wildcard
    };
    cfg.algorithms
        .iter()
        .copied()
        .find(|a| q_of(token(*a)).is_some_and(|q| q > 0.0))
}

pub fn token(algo: CompressionAlgorithm) -> &'static str {
    match algo {
        CompressionAlgorithm::Zstd => "zstd",
        CompressionAlgorithm::Br => "br",
        CompressionAlgorithm::Gzip => "gzip",
    }
}

pub fn type_matches(content_type: Option<&HeaderValue>, cfg: &CompressionConfig) -> bool {
    let Some(ct) = content_type.and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let mime = ct.split(';').next().unwrap_or("").trim();
    cfg.types.iter().any(|t| match t.strip_suffix('*') {
        Some(prefix) => {
            mime.len() >= prefix.len() && mime[..prefix.len()].eq_ignore_ascii_case(prefix)
        }
        None => mime.eq_ignore_ascii_case(t),
    })
}

fn known_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

pub fn should_compress<B>(resp: &Response<B>, cfg: &CompressionConfig) -> bool {
    let status = resp.status();
    if status.is_informational()
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED
        || status == StatusCode::PARTIAL_CONTENT
        || status == StatusCode::SWITCHING_PROTOCOLS
    {
        return false;
    }
    let h = resp.headers();
    if h.contains_key(header::CONTENT_ENCODING) || h.contains_key(header::CONTENT_RANGE) {
        return false;
    }
    if h.get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("no-transform"))
    {
        return false;
    }
    if known_length(h).is_some_and(|len| len < cfg.min_length) {
        return false;
    }
    type_matches(h.get(header::CONTENT_TYPE), cfg)
}

fn level(algo: CompressionAlgorithm, level: CompressionLevel) -> Level {
    match level {
        CompressionLevel::Fastest => Level::Fastest,
        CompressionLevel::Best => Level::Best,
        // Brotli's library default (11) is far too slow for on-the-fly compression.
        CompressionLevel::Default => Level::Precise(match algo {
            CompressionAlgorithm::Gzip => 6,
            CompressionAlgorithm::Br => 4,
            CompressionAlgorithm::Zstd => 3,
        }),
    }
}

/// Marks a response as encoded with `algo`: sets `Content-Encoding`, adds
/// `Vary: Accept-Encoding`, weakens a strong ETag and drops range support.
pub fn mark_encoded(headers: &mut HeaderMap, algo: CompressionAlgorithm) {
    headers.insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static(token(algo)),
    );
    headers.remove(header::ACCEPT_RANGES);
    headers.append(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    if let Some(etag) = headers.get(header::ETAG).and_then(|v| v.to_str().ok()) {
        if !etag.starts_with("W/") {
            if let Ok(weak) = HeaderValue::from_str(&format!("W/{}", etag)) {
                headers.insert(header::ETAG, weak);
            }
        }
    }
}

pub fn compress(
    resp: Response<Body>,
    algo: CompressionAlgorithm,
    lvl: CompressionLevel,
) -> Response<Body> {
    let (mut parts, body) = resp.into_parts();
    // The body is being replaced, so a zero-copy hint for the raw file no longer applies.
    parts.extensions.remove::<crate::http::server::SendFile>();
    parts.headers.remove(header::CONTENT_LENGTH);
    mark_encoded(&mut parts.headers, algo);

    let reader = StreamReader::new(body.into_data_stream().map_err(std::io::Error::other));
    let lvl = level(algo, lvl);
    let body = match algo {
        CompressionAlgorithm::Gzip => {
            Body::from_stream(ReaderStream::new(GzipEncoder::with_quality(reader, lvl)))
        }
        CompressionAlgorithm::Br => {
            Body::from_stream(ReaderStream::new(BrotliEncoder::with_quality(reader, lvl)))
        }
        CompressionAlgorithm::Zstd => {
            Body::from_stream(ReaderStream::new(ZstdEncoder::with_quality(reader, lvl)))
        }
    };
    Response::from_parts(parts, body)
}

/// One-shot compression for bodies held in memory (static file cache).
pub async fn compress_bytes(
    data: Bytes,
    algo: CompressionAlgorithm,
    lvl: CompressionLevel,
) -> std::io::Result<Bytes> {
    use tokio::io::AsyncReadExt;
    let lvl = level(algo, lvl);
    let reader = std::io::Cursor::new(data);
    let reader = tokio::io::BufReader::new(reader);
    let mut out = Vec::new();
    match algo {
        CompressionAlgorithm::Gzip => {
            GzipEncoder::with_quality(reader, lvl)
                .read_to_end(&mut out)
                .await?
        }
        CompressionAlgorithm::Br => {
            BrotliEncoder::with_quality(reader, lvl)
                .read_to_end(&mut out)
                .await?
        }
        CompressionAlgorithm::Zstd => {
            ZstdEncoder::with_quality(reader, lvl)
                .read_to_end(&mut out)
                .await?
        }
    };
    Ok(Bytes::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(accept: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_str(accept).unwrap(),
        );
        h
    }

    #[test]
    fn negotiation_follows_server_preference_and_q_values() {
        let cfg = CompressionConfig::default();
        assert_eq!(
            negotiate(&headers("gzip, br"), &cfg),
            Some(CompressionAlgorithm::Br)
        );
        assert_eq!(
            negotiate(&headers("gzip"), &cfg),
            Some(CompressionAlgorithm::Gzip)
        );
        assert_eq!(
            negotiate(&headers("br;q=0, gzip"), &cfg),
            Some(CompressionAlgorithm::Gzip)
        );
        assert_eq!(negotiate(&headers("identity"), &cfg), None);
        assert_eq!(
            negotiate(&headers("*"), &cfg),
            Some(CompressionAlgorithm::Zstd)
        );
        assert_eq!(negotiate(&HeaderMap::new(), &cfg), None);
    }

    #[test]
    fn only_compressible_types_and_sizes() {
        let cfg = CompressionConfig::default();
        let resp = |ct: &str, len: Option<u64>| {
            let mut r = Response::new(());
            r.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_str(ct).unwrap());
            if let Some(l) = len {
                r.headers_mut()
                    .insert(header::CONTENT_LENGTH, HeaderValue::from(l));
            }
            r
        };
        assert!(should_compress(
            &resp("text/html; charset=utf-8", Some(5000)),
            &cfg
        ));
        assert!(should_compress(&resp("application/json", None), &cfg));
        assert!(!should_compress(&resp("image/png", Some(5000)), &cfg));
        assert!(!should_compress(&resp("text/plain", Some(10)), &cfg));
    }

    #[tokio::test]
    async fn gzip_round_trip() {
        use http_body_util::BodyExt;
        let text = "hello ".repeat(1000);
        let mut resp = Response::new(Body::from(text.clone()));
        resp.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        resp.headers_mut()
            .insert(header::ETAG, HeaderValue::from_static("\"abc\""));
        let out = compress(resp, CompressionAlgorithm::Gzip, CompressionLevel::Default);
        assert_eq!(out.headers()[header::CONTENT_ENCODING], "gzip");
        assert_eq!(out.headers()[header::ETAG], "W/\"abc\"");
        assert!(out.headers().get(header::CONTENT_LENGTH).is_none());
        let bytes = out.into_body().collect().await.unwrap().to_bytes();
        assert!(bytes.len() < text.len() / 10);

        use tokio::io::AsyncReadExt;
        let mut decoded = Vec::new();
        async_compression::tokio::bufread::GzipDecoder::new(tokio::io::BufReader::new(&bytes[..]))
            .read_to_end(&mut decoded)
            .await
            .unwrap();
        assert_eq!(decoded, text.as_bytes());
    }
}
