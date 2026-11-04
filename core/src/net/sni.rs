//! Server Name from a TLS ClientHello, for nginx `ssl_preread` routing.

/// Returns `Ok(Some(name))`, `Ok(None)` (complete hello without SNI / not TLS),
/// or `Err(())` when more bytes are needed.
#[allow(clippy::result_unit_err)]
pub fn parse(buf: &[u8]) -> Result<Option<String>, ()> {
    if buf.is_empty() {
        return Err(());
    }
    if buf[0] != 0x16 {
        return Ok(None);
    }
    if buf.len() < 5 {
        return Err(());
    }
    let rec_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    if buf.len() < 5 + rec_len {
        return Err(());
    }
    Ok(parse_hello(&buf[5..5 + rec_len]))
}

fn parse_hello(h: &[u8]) -> Option<String> {
    let mut r = Reader { b: h, p: 0 };
    if r.u8()? != 1 {
        return None;
    }
    r.skip(3 + 2 + 32)?; // length, version, random
    let sid = r.u8()? as usize;
    r.skip(sid)?;
    let cs = r.u16()? as usize;
    r.skip(cs)?;
    let comp = r.u8()? as usize;
    r.skip(comp)?;
    let ext_total = r.u16()? as usize;
    let end = r.p + ext_total;
    while r.p + 4 <= end {
        let ty = r.u16()?;
        let len = r.u16()? as usize;
        if ty == 0 {
            let mut s = Reader {
                b: r.take(len)?,
                p: 0,
            };
            s.u16()?;
            while let Some(kind) = s.u8() {
                let n = s.u16()? as usize;
                let name = s.take(n)?;
                if kind == 0 {
                    return std::str::from_utf8(name)
                        .ok()
                        .map(|s| s.to_ascii_lowercase());
                }
            }
            return None;
        }
        r.skip(len)?;
    }
    None
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.p..self.p + n)?;
        self.p += n;
        Some(s)
    }
    fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_be_bytes([s[0], s[1]]))
    }
}

/// Picks the value for `name`: exact, then `*.suffix`, then `default`.
pub fn lookup<'a>(
    map: &'a std::collections::HashMap<String, String>,
    name: Option<&str>,
) -> Option<&'a String> {
    if let Some(n) = name {
        if let Some(v) = map.get(n) {
            return Some(v);
        }
        let mut rest = n;
        while let Some(i) = rest.find('.') {
            rest = &rest[i + 1..];
            if let Some(v) = map.get(&format!("*.{rest}")) {
                return Some(v);
            }
        }
    }
    map.get("default")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(name: &str) -> Vec<u8> {
        let mut sni = vec![];
        sni.extend((name.len() as u16 + 3).to_be_bytes());
        sni.push(0);
        sni.extend((name.len() as u16).to_be_bytes());
        sni.extend(name.as_bytes());
        let mut ext = vec![0, 0];
        ext.extend((sni.len() as u16).to_be_bytes());
        ext.extend(sni);
        let mut body = vec![3, 3];
        body.extend([0u8; 32]);
        body.push(0);
        body.extend([0, 2, 0x13, 0x01]);
        body.extend([1, 0]);
        body.extend((ext.len() as u16).to_be_bytes());
        body.extend(ext);
        let mut hs = vec![1, 0];
        hs.extend((body.len() as u16).to_be_bytes());
        hs.extend(body);
        let mut rec = vec![0x16, 3, 1];
        rec.extend((hs.len() as u16).to_be_bytes());
        rec.extend(hs);
        rec
    }

    #[test]
    fn extracts_sni() {
        let h = hello("API.Example.com");
        assert_eq!(parse(&h), Ok(Some("api.example.com".into())));
        assert_eq!(parse(&h[..10]), Err(()));
        assert_eq!(parse(b"GET / HTTP/1.1"), Ok(None));
    }

    #[test]
    fn lookup_order() {
        let m: std::collections::HashMap<String, String> =
            [("a.ex.com", "A"), ("*.ex.com", "W"), ("default", "D")]
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect();
        assert_eq!(lookup(&m, Some("a.ex.com")).unwrap(), "A");
        assert_eq!(lookup(&m, Some("b.c.ex.com")).unwrap(), "W");
        assert_eq!(lookup(&m, Some("x.org")).unwrap(), "D");
        assert_eq!(lookup(&m, None).unwrap(), "D");
    }
}
