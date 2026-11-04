//! HAProxy PROXY protocol v1 (text) and v2 (binary): read the real client
//! address that a load balancer puts in front of a connection, and write one
//! towards a backend (nginx `listen ... proxy_protocol` / `proxy_protocol on`).

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const V2_SIG: [u8; 12] = [
    0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A,
];

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("PROXY protocol: {msg}"))
}

/// Parses a complete header at the start of `buf`.
/// `Ok(Some((len, addr)))`: header of `len` bytes, source `addr` (None for
/// LOCAL/UNKNOWN). `Ok(None)`: need more bytes.
pub fn parse(buf: &[u8]) -> io::Result<Option<(usize, Option<SocketAddr>)>> {
    if buf.len() >= 12 && buf[..12] == V2_SIG {
        if buf.len() < 16 {
            return Ok(None);
        }
        let len = 16 + u16::from_be_bytes([buf[14], buf[15]]) as usize;
        if buf.len() < len {
            return Ok(None);
        }
        let cmd = buf[12] & 0x0F;
        let fam = buf[13] >> 4;
        let body = &buf[16..len];
        if cmd == 0 {
            return Ok(Some((len, None))); // LOCAL (health checks)
        }
        let addr = match fam {
            1 if body.len() >= 12 => Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(body[0], body[1], body[2], body[3])),
                u16::from_be_bytes([body[8], body[9]]),
            )),
            2 if body.len() >= 36 => {
                let mut a = [0u8; 16];
                a.copy_from_slice(&body[..16]);
                Some(SocketAddr::new(
                    IpAddr::V6(Ipv6Addr::from(a)),
                    u16::from_be_bytes([body[32], body[33]]),
                ))
            }
            _ => None,
        };
        return Ok(Some((len, addr)));
    }
    if buf.len() < 6 {
        return if b"PROXY "[..buf.len()] == *buf {
            Ok(None)
        } else {
            Err(bad("missing header"))
        };
    }
    if &buf[..6] != b"PROXY " {
        return Err(bad("missing header"));
    }
    let Some(end) = buf.windows(2).position(|w| w == b"\r\n") else {
        return if buf.len() > 107 {
            Err(bad("v1 line too long"))
        } else {
            Ok(None)
        };
    };
    let line = std::str::from_utf8(&buf[..end]).map_err(|_| bad("v1 not text"))?;
    let f: Vec<&str> = line.split(' ').collect();
    let addr = match f.as_slice() {
        ["PROXY", "TCP4" | "TCP6", src, _dst, sport, _dport] => Some(SocketAddr::new(
            src.parse().map_err(|_| bad("bad source address"))?,
            sport.parse().map_err(|_| bad("bad source port"))?,
        )),
        ["PROXY", "UNKNOWN", ..] => None,
        _ => return Err(bad("malformed v1 line")),
    };
    Ok(Some((end + 2, addr)))
}

/// Reads and consumes the header from `tcp` (peeking first, so no payload
/// byte is lost). Returns the client address it announced, if any.
pub async fn read(tcp: &mut TcpStream, timeout: Duration) -> io::Result<Option<SocketAddr>> {
    let fut = async {
        let mut buf = [0u8; 536];
        loop {
            let n = tcp.peek(&mut buf).await?;
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            if let Some((len, addr)) = parse(&buf[..n])? {
                let mut sink = vec![0u8; len];
                tcp.read_exact(&mut sink).await?;
                return Ok(addr);
            }
            if n == buf.len() {
                return Err(bad("header too long"));
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    };
    tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "PROXY protocol header timed out"))?
}

/// v1 header announcing `src` → `dst`.
pub fn v1_header(src: SocketAddr, dst: SocketAddr) -> String {
    let (src, dst) = match (src, dst) {
        (SocketAddr::V4(_), SocketAddr::V6(d)) => {
            (src, SocketAddr::new(d.ip().to_canonical(), d.port()))
        }
        _ => (src, dst),
    };
    match (src, dst) {
        (SocketAddr::V4(s), SocketAddr::V4(d)) => {
            format!(
                "PROXY TCP4 {} {} {} {}\r\n",
                s.ip(),
                d.ip(),
                s.port(),
                d.port()
            )
        }
        (SocketAddr::V6(s), SocketAddr::V6(d)) => {
            format!(
                "PROXY TCP6 {} {} {} {}\r\n",
                s.ip(),
                d.ip(),
                s.port(),
                d.port()
            )
        }
        _ => "PROXY UNKNOWN\r\n".to_string(),
    }
}

pub async fn write(upstream: &mut TcpStream, src: SocketAddr, dst: SocketAddr) -> io::Result<()> {
    upstream.write_all(v1_header(src, dst).as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1() {
        let h = b"PROXY TCP4 203.0.113.7 10.0.0.1 51234 443\r\nGET /";
        let (len, a) = parse(h).unwrap().unwrap();
        assert_eq!(&h[len..], b"GET /");
        assert_eq!(a.unwrap().to_string(), "203.0.113.7:51234");
        assert!(parse(b"PROXY TCP4 1.2").unwrap().is_none());
        assert!(parse(b"GET / HTTP/1.1\r\n").is_err());
        assert_eq!(parse(b"PROXY UNKNOWN\r\n").unwrap(), Some((15, None)));
    }

    #[test]
    fn v2() {
        let mut h = V2_SIG.to_vec();
        h.extend([
            0x21, 0x11, 0, 12, 198, 51, 100, 9, 10, 0, 0, 1, 0x1F, 0x90, 0x01, 0xBB,
        ]);
        h.extend(b"xyz");
        let (len, a) = parse(&h).unwrap().unwrap();
        assert_eq!(len, 28);
        assert_eq!(a.unwrap().to_string(), "198.51.100.9:8080");
        assert!(parse(&h[..20]).unwrap().is_none());
    }

    #[test]
    fn writes_v1() {
        let s: SocketAddr = "1.2.3.4:5".parse().unwrap();
        let d: SocketAddr = "6.7.8.9:10".parse().unwrap();
        assert_eq!(v1_header(s, d), "PROXY TCP4 1.2.3.4 6.7.8.9 5 10\r\n");
    }

    #[tokio::test]
    async fn read_consumes_only_the_header() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let mut c = TcpStream::connect(addr).await.unwrap();
            c.write_all(b"PROXY TCP4 9.9.9.9 1.1.1.1 1000 80\r\nhello")
                .await
                .unwrap();
        });
        let (mut s, _) = l.accept().await.unwrap();
        let a = read(&mut s, Duration::from_secs(1)).await.unwrap();
        assert_eq!(a.unwrap().to_string(), "9.9.9.9:1000");
        let mut rest = String::new();
        s.read_to_string(&mut rest).await.unwrap();
        assert_eq!(rest, "hello");
    }
}
