use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Response, StatusCode};
use axum::response::IntoResponse;
use std::path::{Component, Path, PathBuf};

use crate::config::DavConfig;

pub fn handles(cfg: &DavConfig, method: &Method) -> bool {
    cfg.methods
        .iter()
        .any(|m| m.eq_ignore_ascii_case(method.as_str()))
}

fn safe_join(root: &Path, url_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(url_path);
    let mut out = root.to_path_buf();
    for c in Path::new(decoded.trim_start_matches('/')).components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn inside_root(root: &Path, p: &Path) -> bool {
    let (Ok(root), Some(parent)) = (root.canonicalize(), p.parent()) else {
        return false;
    };
    let mut probe = parent.to_path_buf();
    while !probe.exists() {
        if !probe.pop() {
            return false;
        }
    }
    probe.canonicalize().is_ok_and(|c| c.starts_with(&root))
}

fn status(code: StatusCode) -> Response<Body> {
    (code, "").into_response()
}

fn destination(headers: &HeaderMap) -> Option<String> {
    let d = headers.get("destination")?.to_str().ok()?;
    let path = match d.find("://") {
        Some(i) => &d[i + 3..][d[i + 3..].find('/').unwrap_or(d.len() - i - 3)..],
        None => d,
    };
    Some(path.split('?').next().unwrap_or("/").to_string())
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let t = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &t)?;
        } else {
            std::fs::copy(e.path(), t)?;
        }
    }
    Ok(())
}

pub async fn handle(
    cfg: &DavConfig,
    root: &str,
    url_path: &str,
    method: &Method,
    headers: &HeaderMap,
    body: Body,
    max_body: u64,
) -> Response<Body> {
    let root = PathBuf::from(root);
    let Some(target) = safe_join(&root, url_path) else {
        return status(StatusCode::FORBIDDEN);
    };
    if !inside_root(&root, &target) {
        return status(StatusCode::FORBIDDEN);
    }
    match method.as_str() {
        "PUT" => {
            if url_path.ends_with('/') {
                return status(StatusCode::CONFLICT);
            }
            let Some(parent) = target.parent() else {
                return status(StatusCode::CONFLICT);
            };
            if !parent.is_dir() {
                if !cfg.create_full_put_path {
                    return status(StatusCode::CONFLICT);
                }
                if std::fs::create_dir_all(parent).is_err() {
                    return status(StatusCode::INTERNAL_SERVER_ERROR);
                }
            }
            let existed = target.exists();
            let bytes = match axum::body::to_bytes(body, max_body as usize).await {
                Ok(b) => b,
                Err(_) => return status(StatusCode::PAYLOAD_TOO_LARGE),
            };
            let tmp = target.with_extension(format!("ferrix-put-{}", uuid::Uuid::new_v4()));
            if tokio::fs::write(&tmp, &bytes).await.is_err()
                || tokio::fs::rename(&tmp, &target).await.is_err()
            {
                let _ = tokio::fs::remove_file(&tmp).await;
                return status(StatusCode::INTERNAL_SERVER_ERROR);
            }
            #[cfg(unix)]
            if let Some(mode) = cfg.access {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode));
            }
            status(if existed {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::CREATED
            })
        }
        "DELETE" => {
            let r = if target.is_dir() {
                if !url_path.ends_with('/') {
                    return status(StatusCode::CONFLICT);
                }
                std::fs::remove_dir_all(&target)
            } else {
                std::fs::remove_file(&target)
            };
            match r {
                Ok(()) => status(StatusCode::NO_CONTENT),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => status(StatusCode::NOT_FOUND),
                Err(_) => status(StatusCode::INTERNAL_SERVER_ERROR),
            }
        }
        "MKCOL" => {
            if !url_path.ends_with('/') {
                return status(StatusCode::CONFLICT);
            }
            if target.exists() {
                return status(StatusCode::METHOD_NOT_ALLOWED);
            }
            match std::fs::create_dir(&target) {
                Ok(()) => status(StatusCode::CREATED),
                Err(_) => status(StatusCode::CONFLICT),
            }
        }
        "COPY" | "MOVE" => {
            let Some(dest_path) = destination(headers) else {
                return status(StatusCode::BAD_REQUEST);
            };
            let Some(dest) = safe_join(&root, &dest_path) else {
                return status(StatusCode::FORBIDDEN);
            };
            if !inside_root(&root, &dest) {
                return status(StatusCode::FORBIDDEN);
            }
            if !target.exists() {
                return status(StatusCode::NOT_FOUND);
            }
            let overwrite = headers
                .get("overwrite")
                .and_then(|v| v.to_str().ok())
                .is_none_or(|v| !v.eq_ignore_ascii_case("F"));
            let existed = dest.exists();
            if existed && !overwrite {
                return status(StatusCode::PRECONDITION_FAILED);
            }
            if existed {
                let _ = if dest.is_dir() {
                    std::fs::remove_dir_all(&dest)
                } else {
                    std::fs::remove_file(&dest)
                };
            }
            if let Some(p) = dest.parent() {
                if !p.is_dir() {
                    return status(StatusCode::CONFLICT);
                }
            }
            let r = if method == "MOVE" {
                std::fs::rename(&target, &dest)
            } else if target.is_dir() {
                copy_dir(&target, &dest)
            } else {
                std::fs::copy(&target, &dest).map(|_| ())
            };
            match r {
                Ok(()) => status(if existed {
                    StatusCode::NO_CONTENT
                } else {
                    StatusCode::CREATED
                }),
                Err(_) => status(StatusCode::INTERNAL_SERVER_ERROR),
            }
        }
        _ => {
            let mut r = status(StatusCode::METHOD_NOT_ALLOWED);
            if let Ok(v) = cfg.methods.join(", ").parse() {
                r.headers_mut().insert(header::ALLOW, v);
            }
            r
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DavConfig {
        DavConfig {
            methods: ["PUT", "DELETE", "MKCOL", "COPY", "MOVE"]
                .map(String::from)
                .to_vec(),
            create_full_put_path: true,
            access: None,
        }
    }

    #[tokio::test]
    async fn put_copy_move_delete() {
        let root = std::env::temp_dir().join(format!("ferrix-dav-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let r = root.to_str().unwrap();
        let h = HeaderMap::new();
        let c = cfg();
        let put = |p: &'static str| handle(&c, r, p, &Method::PUT, &h, Body::from("x"), 1024);
        assert_eq!(put("/a/b.txt").await.status(), 201);
        assert_eq!(put("/a/b.txt").await.status(), 204);
        assert_eq!(put("/../evil").await.status(), 403);
        let mut hd = HeaderMap::new();
        hd.insert("destination", "http://h/a/c.txt".parse().unwrap());
        let copy: Method = "COPY".parse().unwrap();
        assert_eq!(
            handle(&cfg(), r, "/a/b.txt", &copy, &hd, Body::empty(), 0)
                .await
                .status(),
            201
        );
        let mv: Method = "MOVE".parse().unwrap();
        hd.insert("destination", "/a/d.txt".parse().unwrap());
        assert_eq!(
            handle(&cfg(), r, "/a/c.txt", &mv, &hd, Body::empty(), 0)
                .await
                .status(),
            201
        );
        assert!(root.join("a/d.txt").is_file() && !root.join("a/c.txt").exists());
        let mk: Method = "MKCOL".parse().unwrap();
        assert_eq!(
            handle(&cfg(), r, "/dir/", &mk, &h, Body::empty(), 0)
                .await
                .status(),
            201
        );
        assert_eq!(
            handle(&cfg(), r, "/a/d.txt", &Method::DELETE, &h, Body::empty(), 0)
                .await
                .status(),
            204
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
