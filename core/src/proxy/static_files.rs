use axum::body::Body;
use axum::http::{header, Response, StatusCode};
use std::path::PathBuf;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tracing::warn;

pub struct StaticFileServer;

impl StaticFileServer {
    pub async fn serve_file(
        root_dir: &str,
        path: &str,
        try_files: Option<&str>,
    ) -> Result<Response<Body>, StatusCode> {
        let clean_path = path.trim_start_matches('/');
        let base_root = PathBuf::from(root_dir);

        // Security: Ensure path doesn't escape base_root
        let mut target_file: Option<PathBuf> = None;

        // Try direct file first
        let candidate = base_root.join(clean_path);
        if candidate.is_file() {
            target_file = Some(candidate);
        } else if candidate.is_dir() {
            let index_html = candidate.join("index.html");
            if index_html.is_file() {
                target_file = Some(index_html);
            }
        }

        // Try_files fallback handling (e.g. "$uri $uri/ /index.html")
        if target_file.is_none() {
            if let Some(tf) = try_files {
                for token in tf.split_whitespace() {
                    if token == "$uri" {
                        let p = base_root.join(clean_path);
                        if p.is_file() {
                            target_file = Some(p);
                            break;
                        }
                    } else if token == "$uri/" {
                        let p = base_root.join(clean_path).join("index.html");
                        if p.is_file() {
                            target_file = Some(p);
                            break;
                        }
                    } else {
                        // Explicit fallback path like /index.html
                        let fallback_clean = token.trim_start_matches('/');
                        let p = base_root.join(fallback_clean);
                        if p.is_file() {
                            target_file = Some(p);
                            break;
                        }
                    }
                }
            }
        }

        let file_path = match target_file {
            Some(p) => p,
            None => {
                return Err(StatusCode::NOT_FOUND);
            }
        };

        // Double check canonical path stays within root
        if let (Ok(canon_file), Ok(canon_root)) =
            (file_path.canonicalize(), base_root.canonicalize())
        {
            if !canon_file.starts_with(&canon_root) {
                warn!(file = %file_path.display(), root = %base_root.display(), "Directory traversal attempt blocked");
                return Err(StatusCode::FORBIDDEN);
            }
        }

        let mut file = match File::open(&file_path).await {
            Ok(f) => f,
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        };

        let metadata = match file.metadata().await {
            Ok(m) => m,
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        };

        let len = metadata.len();
        let mime = mime_guess::from_path(&file_path)
            .first_or_octet_stream()
            .to_string();

        let mut contents = Vec::with_capacity(len as usize);
        if file.read_to_end(&mut contents).await.is_err() {
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }

        let body = Body::from(contents);
        let response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .header(header::CONTENT_LENGTH, len.to_string())
            .header(header::CACHE_CONTROL, "public, max-age=3600")
            .body(body)
            .unwrap();

        Ok(response)
    }
}
