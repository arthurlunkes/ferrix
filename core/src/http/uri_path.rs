use axum::http::StatusCode;

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn decode(raw: &str) -> Result<String, StatusCode> {
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let (Some(h), Some(l)) = (
                b.get(i + 1).copied().and_then(hex),
                b.get(i + 2).copied().and_then(hex),
            ) else {
                return Err(StatusCode::BAD_REQUEST);
            };
            let v = h * 16 + l;
            if v == 0 {
                return Err(StatusCode::BAD_REQUEST);
            }
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| StatusCode::BAD_REQUEST)
}

pub fn normalize(raw: &str) -> Result<String, StatusCode> {
    let decoded = if raw.contains('%') {
        decode(raw)?
    } else {
        raw.to_string()
    };
    if !decoded.contains("/.") {
        return Ok(decoded);
    }
    let trailing = decoded.ends_with('/') || decoded.ends_with("/.") || decoded.ends_with("/..");
    let mut segs: Vec<&str> = Vec::new();
    for seg in decoded.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if segs.pop().is_none() {
                    return Err(StatusCode::BAD_REQUEST);
                }
            }
            s => segs.push(s),
        }
    }
    let mut out = String::with_capacity(decoded.len());
    for s in &segs {
        out.push('/');
        out.push_str(s);
    }
    if out.is_empty() || trailing {
        out.push('/');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nginx_like() {
        assert_eq!(normalize("/%68ello.txt").unwrap(), "/hello.txt");
        assert_eq!(normalize("/meu%20arquivo.pdf").unwrap(), "/meu arquivo.pdf");
        assert_eq!(normalize("/sub/../hello.txt").unwrap(), "/hello.txt");
        assert_eq!(normalize("/a/./b/").unwrap(), "/a/b/");
        assert_eq!(normalize("/a/b/..").unwrap(), "/a/");
        assert_eq!(
            normalize("/%2e%2e/%2e%2e/etc/passwd"),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(normalize("/../x"), Err(StatusCode::BAD_REQUEST));
        assert_eq!(normalize("/a%00b"), Err(StatusCode::BAD_REQUEST));
        assert_eq!(normalize("/a%zz"), Err(StatusCode::BAD_REQUEST));
        assert_eq!(normalize("/plain").unwrap(), "/plain");
    }
}
