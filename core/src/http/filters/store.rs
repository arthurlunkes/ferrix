use axum::body::Body;
use axum::http::Response;
use std::path::{Component, Path, PathBuf};

const MAX_STORED: usize = 64 << 20;

pub fn target(dir: &str, uri_path: &str) -> Option<PathBuf> {
    let decoded = crate::http::uri_path::decode(uri_path).ok()?;
    let mut rel = PathBuf::new();
    for c in Path::new(decoded.trim_start_matches('/')).components() {
        match c {
            Component::Normal(p) => rel.push(p),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if decoded.ends_with('/') || rel.as_os_str().is_empty() {
        rel.push("index.html");
    }
    Some(Path::new(dir).join(rel))
}

pub async fn apply(resp: Response<Body>, dir: &str, uri_path: &str) -> Response<Body> {
    let Some(path) = target(dir, uri_path) else {
        return resp;
    };
    let (parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_STORED).await else {
        return Response::from_parts(parts, Body::empty());
    };
    let data = bytes.clone();
    tokio::task::spawn_blocking(move || {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, &data).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    });
    Response::from_parts(parts, Body::from(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_path_stays_inside_the_directory() {
        assert_eq!(target("/c", "/a/b.css"), Some(PathBuf::from("/c/a/b.css")));
        assert_eq!(target("/c", "/a/"), Some(PathBuf::from("/c/a/index.html")));
        assert_eq!(target("/c", "/../etc/passwd"), None);
    }
}
