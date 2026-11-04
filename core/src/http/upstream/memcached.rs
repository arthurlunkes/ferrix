use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const MAX_VALUE: usize = 16 << 20;

pub enum Reply {
    Hit { flags: u32, data: Vec<u8> },
    Miss,
}

fn parse(buf: &[u8], key: &str) -> Result<Option<Reply>, String> {
    if buf.starts_with(b"END\r\n") {
        return Ok(Some(Reply::Miss));
    }
    let Some(nl) = buf.windows(2).position(|w| w == b"\r\n") else {
        return Ok(None);
    };
    let line = std::str::from_utf8(&buf[..nl]).map_err(|_| "bad memcached reply")?;
    let f: Vec<&str> = line.split(' ').collect();
    match f.as_slice() {
        ["VALUE", k, flags, len, ..] if *k == key => {
            let len: usize = len.parse().map_err(|_| "bad length")?;
            if len > MAX_VALUE {
                return Err("value too large".into());
            }
            let start = nl + 2;
            if buf.len() < start + len + 2 {
                return Ok(None);
            }
            Ok(Some(Reply::Hit {
                flags: flags.parse().unwrap_or(0),
                data: buf[start..start + len].to_vec(),
            }))
        }
        ["SERVER_ERROR", ..] | ["CLIENT_ERROR", ..] | ["ERROR"] => Err(line.to_string()),
        _ => Err(format!("unexpected memcached reply: {line}")),
    }
}

pub fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 250 && key.bytes().all(|b| b > 32 && b != 127)
}

pub async fn get(
    addr: &str,
    key: &str,
    connect: Duration,
    read: Duration,
) -> Result<Reply, String> {
    if !valid_key(key) {
        return Err("invalid memcached key".into());
    }
    let mut s = tokio::time::timeout(connect, TcpStream::connect(addr))
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let exchange = async {
        s.write_all(format!("get {key}\r\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let mut buf = Vec::with_capacity(4096);
        let mut chunk = [0u8; 16384];
        loop {
            let n = s.read(&mut chunk).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("memcached closed the connection".to_string());
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(r) = parse(&buf, key)? {
                return Ok(r);
            }
        }
    };
    tokio::time::timeout(read, exchange)
        .await
        .map_err(|_| "memcached timed out".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_replies() {
        assert!(matches!(parse(b"END\r\n", "k"), Ok(Some(Reply::Miss))));
        assert!(parse(b"VALUE k 0 5\r\nhel", "k").unwrap().is_none());
        match parse(b"VALUE k 3 5\r\nhello\r\nEND\r\n", "k")
            .unwrap()
            .unwrap()
        {
            Reply::Hit { flags, data } => assert_eq!((flags, &data[..]), (3, &b"hello"[..])),
            Reply::Miss => panic!(),
        }
        assert!(parse(b"SERVER_ERROR out of memory\r\n", "k").is_err());
        assert!(!valid_key("a b") && valid_key("/page?x=1"));
    }

    #[tokio::test]
    async fn round_trip() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0u8; 64];
            let n = s.read(&mut b).await.unwrap();
            assert_eq!(&b[..n], b"get /home\r\n");
            s.write_all(b"VALUE /home 0 2\r\nhi\r\nEND\r\n")
                .await
                .unwrap();
        });
        let r = get(
            &addr,
            "/home",
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(matches!(r, Reply::Hit { ref data, .. } if data == b"hi"));
    }
}
