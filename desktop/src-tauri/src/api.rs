//! A minimal HTTP/1.1 client for talking to the local server. The manager only ever calls the server it started on
//! 127.0.0.1, so plain HTTP is enough and no HTTP library (or TLS stack) has to be linked in; the admin key never
//! leaves this process except on that loopback connection.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

pub struct Response {
    pub status: u16,
    pub body: String,
}

pub fn request(port: u16, method: &str, path: &str, key: Option<&str>, body: Option<&str>, timeout: Duration) -> Result<Response, String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|e| format!("cannot reach the server on port {port}: {e}"))?;
    s.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nAccept: application/json\r\n");
    if let Some(k) = key {
        req.push_str(&format!("Authorization: Bearer {k}\r\n"));
    }
    match body {
        Some(b) => req.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}", b.len())),
        None => req.push_str("Content-Length: 0\r\n\r\n"),
    }
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).map_err(|e| format!("the server did not answer in time: {e}"))?;
    parse(&raw)
}

pub fn parse(raw: &[u8]) -> Result<Response, String> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("the server sent an incomplete answer")?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status: u16 = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|c| c.parse().ok()).ok_or("the server sent no status")?;
    let rest = &raw[split + 4..];
    let chunked = head.lines().any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.to_ascii_lowercase().contains("chunked"));
    let body = if chunked { dechunk(rest)? } else { rest.to_vec() };
    Ok(Response { status, body: String::from_utf8_lossy(&body).into_owned() })
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let eol = b.windows(2).position(|w| w == b"\r\n").ok_or("bad chunked body")?;
        let size = usize::from_str_radix(String::from_utf8_lossy(&b[..eol]).split(';').next().unwrap_or("").trim(), 16).map_err(|_| "bad chunk size")?;
        b = &b[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size + 2 {
            return Err("truncated chunked body".into());
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

/// True when something answers `GET /api/v1/healthz` with 200 and `ok`.
pub fn healthy(port: u16) -> bool {
    request(port, "GET", "/api/v1/healthz", None, None, Duration::from_secs(2)).map(|r| r.status == 200 && r.body.trim() == "ok").unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_chunked_bodies_are_read() {
        let plain = parse(b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\n{}").unwrap();
        assert_eq!((plain.status, plain.body.as_str()), (201, "{}"));
        let chunked = parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n").unwrap();
        assert_eq!(chunked.body, "abcde");
        assert!(parse(b"garbage").is_err());
        assert!(parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n").is_err());
    }
}
