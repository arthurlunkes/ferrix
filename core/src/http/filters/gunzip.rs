//! nginx `gunzip on`: a gzip-encoded response is decompressed for clients
//! that do not accept gzip (the backend or a pre-compressed file sent gzip).

use axum::body::Body;
use axum::http::{header, HeaderMap, Response};
use futures_util::TryStreamExt;
use tokio_util::io::{ReaderStream, StreamReader};

pub fn client_accepts_gzip(h: &HeaderMap) -> bool {
    h.get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|e| {
            let mut it = e.trim().split(';');
            let name = it.next().unwrap_or("").trim();
            let q0 = it.any(|p| p.trim().replace(' ', "") == "q=0");
            (name.eq_ignore_ascii_case("gzip") || name == "*") && !q0
        })
}

pub fn apply(resp: Response<Body>) -> Response<Body> {
    let gz = resp
        .headers()
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("gzip"));
    if !gz {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    parts.headers.remove(header::CONTENT_ENCODING);
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.remove(header::ETAG);
    parts.extensions.remove::<crate::http::server::SendFile>();
    let reader = StreamReader::new(body.into_data_stream().map_err(std::io::Error::other));
    let decoder = async_compression::tokio::bufread::GzipDecoder::new(reader);
    Response::from_parts(parts, Body::from_stream(ReaderStream::new(decoder)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn decompresses_for_clients_without_gzip() {
        let mut enc = async_compression::tokio::write::GzipEncoder::new(Vec::new());
        enc.write_all(b"hello world").await.unwrap();
        enc.shutdown().await.unwrap();
        let resp = Response::builder()
            .header(header::CONTENT_ENCODING, "gzip")
            .body(Body::from(enc.into_inner()))
            .unwrap();
        let out = apply(resp);
        assert!(out.headers().get(header::CONTENT_ENCODING).is_none());
        let b = axum::body::to_bytes(out.into_body(), 1024).await.unwrap();
        assert_eq!(&b[..], b"hello world");

        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT_ENCODING, "br, gzip;q=0.5".parse().unwrap());
        assert!(client_accepts_gzip(&h));
        h.insert(header::ACCEPT_ENCODING, "gzip;q=0, br".parse().unwrap());
        assert!(!client_accepts_gzip(&h));
    }
}
