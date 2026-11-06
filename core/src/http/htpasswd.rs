use md5::{Digest, Md5};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};
use std::time::SystemTime;

const ITOA64: &[u8] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn to64(out: &mut String, mut v: u32, n: usize) {
    for _ in 0..n {
        out.push(ITOA64[(v & 0x3f) as usize] as char);
        v >>= 6;
    }
}

pub fn apr1(password: &str, salt: &str) -> String {
    let pw = password.as_bytes();
    let salt: String = salt.chars().take(8).collect();
    let sb = salt.as_bytes();
    let mut ctx = Md5::new();
    ctx.update(pw);
    ctx.update(b"$apr1$");
    ctx.update(sb);
    let mut alt = Md5::new();
    alt.update(pw);
    alt.update(sb);
    alt.update(pw);
    let fin = alt.finalize();
    let mut left = pw.len();
    while left > 0 {
        let n = left.min(16);
        ctx.update(&fin[..n]);
        left -= n;
    }
    let mut i = pw.len();
    while i > 0 {
        if i & 1 == 1 {
            ctx.update([0u8]);
        } else {
            ctx.update(&pw[..1]);
        }
        i >>= 1;
    }
    let mut fin = ctx.finalize();
    for i in 0..1000 {
        let mut c = Md5::new();
        if i & 1 == 1 {
            c.update(pw);
        } else {
            c.update(fin);
        }
        if i % 3 != 0 {
            c.update(sb);
        }
        if i % 7 != 0 {
            c.update(pw);
        }
        if i & 1 == 1 {
            c.update(fin);
        } else {
            c.update(pw);
        }
        fin = c.finalize();
    }
    let f = |i: usize| fin[i] as u32;
    let mut out = format!("$apr1${salt}$");
    to64(&mut out, (f(0) << 16) | (f(6) << 8) | f(12), 4);
    to64(&mut out, (f(1) << 16) | (f(7) << 8) | f(13), 4);
    to64(&mut out, (f(2) << 16) | (f(8) << 8) | f(14), 4);
    to64(&mut out, (f(3) << 16) | (f(9) << 8) | f(15), 4);
    to64(&mut out, (f(4) << 16) | (f(10) << 8) | f(5), 4);
    to64(&mut out, f(11), 2);
    out
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn is_hash(stored: &str) -> bool {
    [
        "$2a$", "$2b$", "$2y$", "$apr1$", "$5$", "$6$", "{SHA}", "{SSHA}", "{PLAIN}",
    ]
    .iter()
    .any(|p| stored.starts_with(p))
}

pub fn verify(password: &str, stored: &str) -> bool {
    if let Some(rest) = stored.strip_prefix("$apr1$") {
        let salt = rest.split('$').next().unwrap_or("");
        return ct_eq(apr1(password, salt).as_bytes(), stored.as_bytes());
    }
    if let Some(plain) = stored.strip_prefix("{PLAIN}") {
        return ct_eq(password.as_bytes(), plain.as_bytes());
    }
    #[cfg(feature = "htpasswd")]
    {
        use base64::Engine;
        use sha1::Sha1;
        if stored.starts_with("$2a$") || stored.starts_with("$2b$") || stored.starts_with("$2y$") {
            return bcrypt::verify(password, stored).unwrap_or(false);
        }
        if stored.starts_with("$5$") {
            return sha_crypt::sha256_check(password, stored).is_ok();
        }
        if stored.starts_with("$6$") {
            return sha_crypt::sha512_check(password, stored).is_ok();
        }
        let b64 = base64::engine::general_purpose::STANDARD;
        if let Some(h) = stored.strip_prefix("{SHA}") {
            let d = Sha1::digest(password.as_bytes());
            return ct_eq(b64.encode(d).as_bytes(), h.as_bytes());
        }
        if let Some(h) = stored.strip_prefix("{SSHA}") {
            let Ok(raw) = b64.decode(h) else {
                return false;
            };
            if raw.len() <= 20 {
                return false;
            }
            let (digest, salt) = raw.split_at(20);
            let mut c = Sha1::new();
            c.update(password.as_bytes());
            c.update(salt);
            return ct_eq(&c.finalize(), digest);
        }
    }
    if is_hash(stored) {
        return false;
    }
    ct_eq(password.as_bytes(), stored.as_bytes())
}

struct Loaded {
    modified: Option<SystemTime>,
    users: Arc<HashMap<String, String>>,
}

static FILES: LazyLock<RwLock<HashMap<String, Loaded>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

pub fn parse(text: &str) -> HashMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (u, rest) = l.split_once(':')?;
            let hash = rest.split(':').next().unwrap_or(rest);
            Some((u.to_string(), hash.to_string()))
        })
        .collect()
}

pub fn users(path: &str) -> Option<Arc<HashMap<String, String>>> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    if let Some(l) = FILES.read().ok()?.get(path) {
        if l.modified == modified {
            return Some(l.users.clone());
        }
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(path, error = %e, "cannot read htpasswd file");
            return None;
        }
    };
    let users = Arc::new(parse(&text));
    if let Ok(mut m) = FILES.write() {
        m.insert(
            path.to_string(),
            Loaded {
                modified,
                users: users.clone(),
            },
        );
    }
    Some(users)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_match_openssl_and_apache() {
        assert_eq!(
            apr1("s3cret", "abcd1234"),
            "$apr1$abcd1234$8GMkM2CfYzVxlmcajk1me/"
        );
        assert!(verify("s3cret", "$apr1$abcd1234$8GMkM2CfYzVxlmcajk1me/"));
        assert!(!verify("wrong", "$apr1$abcd1234$8GMkM2CfYzVxlmcajk1me/"));
        assert!(verify("s3cret", "{PLAIN}s3cret"));
        assert!(verify("s3cret", "s3cret"));
        assert!(!verify("s3cre", "s3cret"));
    }

    #[cfg(feature = "htpasswd")]
    #[test]
    fn hashed_formats() {
        assert!(verify("s3cret", "{SHA}/vNB+F2HQ559kaLUZbmHHvZrXpg="));
        assert!(verify(
            "s3cret",
            "$5$saltsalt$i1q2ZQzc.tl/BQ6CHiENAcVDvEY6nJ1OWlWXKh94b1."
        ));
        assert!(verify(
            "s3cret",
            "$6$saltsalt$As4wrv0kZlfch1du9WeH7qhskyLriQWySXrZzynnvi46nFnNxjdpl6ksRegrrKexvhIa/Iny8S8uF3fVWTMuC1"
        ));
        let b = bcrypt::hash("s3cret", 4).unwrap().replace("$2b$", "$2y$");
        assert!(verify("s3cret", &b) && !verify("nope", &b));
    }

    #[test]
    fn file_is_parsed_and_reloaded() {
        let p = std::env::temp_dir().join(format!("ferrix-ht-{}", uuid::Uuid::new_v4()));
        std::fs::write(
            &p,
            "# users\nana:{PLAIN}a1\nbob:$apr1$abcd1234$8GMkM2CfYzVxlmcajk1me/:comment\n",
        )
        .unwrap();
        let path = p.to_str().unwrap();
        let u = users(path).unwrap();
        assert_eq!(u.len(), 2);
        assert!(verify("s3cret", &u["bob"]));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&p, "carl:{PLAIN}x\n").unwrap();
        let t = SystemTime::now() + std::time::Duration::from_secs(5);
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(t).unwrap();
        assert!(users(path).unwrap().contains_key("carl"));
        let _ = std::fs::remove_file(p);
    }
}
