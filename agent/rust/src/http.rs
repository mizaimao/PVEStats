//! Just enough HTTP/1.1 for this agent, as server and as Ollama's client:
//! one request per connection, then close. Every read has a deadline and a
//! size cap, so a client that trickles bytes or never stops cannot hold a
//! thread or grow memory.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Most of a request's head (request line and headers) that is read.
const HEAD_LIMIT: usize = 16 * 1024;

pub struct Request {
    pub method: String,
    /// Without the query string.
    pub path: String,
    /// Names lowercased.
    pub headers: Vec<(String, String)>,
    /// Body bytes that came in with the head.
    pub body_start: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// One read, given whatever is left of `deadline`; None on timeout, error
/// or the other side closing.
fn read_some(stream: &mut TcpStream, buf: &mut [u8], deadline: Instant) -> Option<usize> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return None;
    }
    stream.set_read_timeout(Some(left)).ok()?;
    match stream.read(buf) {
        Ok(0) | Err(_) => None,
        Ok(n) => Some(n),
    }
}

/// Where the head ends and the body starts, looking only from `from` (the
/// last few bytes before what just arrived can complete the blank line).
fn head_end(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    for i in from..buf.len() {
        if buf[i..].starts_with(b"\r\n\r\n") {
            return Some((i, i + 4));
        }
        if buf[i..].starts_with(b"\n\n") {
            return Some((i, i + 2));
        }
    }
    None
}

/// Reads one request's head by `deadline`. None for anything that is not a
/// complete, readable head in time.
pub fn read_request(stream: &mut TcpStream, deadline: Instant) -> Option<Request> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    let (end, body) = loop {
        let n = read_some(stream, &mut chunk, deadline)?;
        let from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(found) = head_end(&buf, from) {
            break found;
        }
        if buf.len() >= HEAD_LIMIT {
            return None;
        }
    };
    parse_head(&buf[..end], buf[body..].to_vec())
}

fn parse_head(head: &[u8], body_start: Vec<u8>) -> Option<Request> {
    let head = std::str::from_utf8(head).ok()?;
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_string();
    let target = first.next()?;
    let path = target.split('?').next().unwrap_or("").to_string();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Some(Request { method, path, headers, body_start })
}

/// The body, `len` bytes of it, by `deadline`.
pub fn read_body(stream: &mut TcpStream, req: &Request, len: usize, deadline: Instant) -> Option<Vec<u8>> {
    let mut body = req.body_start.clone();
    body.truncate(len);
    let mut chunk = [0u8; 1024];
    while body.len() < len {
        let want = (len - body.len()).min(chunk.len());
        let n = read_some(stream, &mut chunk[..want], deadline)?;
        body.extend_from_slice(&chunk[..n]);
    }
    Some(body)
}

/// Sends a JSON reply and closes. No CORS headers, ever.
pub fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        _ => "Error",
    };
    let reply = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let _ = stream.write_all(reply.as_bytes());
}

// ------------------------------------------------------------------ client

/// An http:// URL; Ollama is on loopback, so no TLS.
#[derive(Clone, Debug, PartialEq)]
pub struct Url {
    pub host: String,
    pub port: u16,
    pub path: String,
}

pub fn parse_url(url: &str) -> Option<Url> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        // An IPv6 address: [::1]:11434.
        let (host, after) = rest.split_once(']')?;
        match after.strip_prefix(':') {
            Some(p) => (host, p.parse().ok()?),
            None if after.is_empty() => (host, 80),
            None => return None,
        }
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().ok()?),
            None => (authority, 80),
        }
    };
    if host.is_empty() {
        return None;
    }
    Some(Url { host: host.to_string(), port, path: path.to_string() })
}

/// A body sent with chunked transfer encoding, put back together.
pub fn dechunk(mut data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = data.windows(2).position(|w| w == b"\r\n")?;
        let size_text = std::str::from_utf8(&data[..line_end]).ok()?;
        let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Some(out);
        }
        if data.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

/// One call, all of it (connecting, sending, reading) by `deadline`, at most
/// `cap` bytes of reply. The status and the body.
pub fn call(url: &Url, method: &str, body: Option<&[u8]>, deadline: Instant, cap: usize) -> Result<(u16, Vec<u8>), String> {
    let left = || deadline.saturating_duration_since(Instant::now());
    let addr = (url.host.as_str(), url.port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {}: {e}", url.host))?
        .next()
        .ok_or_else(|| format!("cannot resolve {}", url.host))?;
    if left().is_zero() {
        return Err("timed out".into());
    }
    let mut stream = TcpStream::connect_timeout(&addr, left()).map_err(|e| format!("cannot connect: {e}"))?;
    let mut request = format!(
        "{method} {} HTTP/1.1\r\nHost: {}:{}\r\nUser-Agent: pvestats-agent\r\nAccept: application/json\r\nConnection: close\r\n",
        url.path, url.host, url.port
    );
    if let Some(body) = body {
        request.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    stream.set_write_timeout(Some(left().max(Duration::from_millis(1)))).map_err(|e| e.to_string())?;
    stream.write_all(request.as_bytes()).map_err(|e| format!("cannot send: {e}"))?;
    if let Some(body) = body {
        stream.write_all(body).map_err(|e| format!("cannot send: {e}"))?;
    }

    let mut reply = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if left().is_zero() {
            return Err("timed out".into());
        }
        stream.set_read_timeout(Some(left())).map_err(|e| e.to_string())?;
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                reply.extend_from_slice(&chunk[..n]);
                if reply.len() > cap {
                    return Err("reply too large".into());
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Err("timed out".into())
            }
            Err(e) => return Err(format!("cannot read: {e}")),
        }
    }
    parse_reply(&reply).ok_or_else(|| "unreadable reply".to_string())
}

fn parse_reply(reply: &[u8]) -> Option<(u16, Vec<u8>)> {
    let (end, body_at) = head_end(reply, 0)?;
    let head = std::str::from_utf8(&reply[..end]).ok()?;
    let mut lines = head.lines();
    let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let mut chunked = false;
    let mut length: Option<usize> = None;
    for line in lines {
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        } else if k == "content-length" {
            length = v.parse().ok();
        }
    }
    let raw = &reply[body_at..];
    let body = if chunked {
        dechunk(raw)?
    } else if let Some(n) = length {
        raw.get(..n)?.to_vec()
    } else {
        raw.to_vec()
    };
    Some((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads_and_urls() {
        let r = parse_head(b"POST /unload?x=1 HTTP/1.1\r\nContent-Type: application/json\r\nOrigin: https://x", b"{}".to_vec()).unwrap();
        assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/unload"));
        assert_eq!(r.header("content-type"), Some("application/json"));
        assert_eq!(r.header("origin"), Some("https://x"));
        assert_eq!(head_end(b"GET / HTTP/1.1\r\n\r\nbody", 0), Some((14, 18)));
        assert_eq!(head_end(b"GET / HTTP/1.1\r\n", 0), None);
        assert_eq!(
            parse_url("http://127.0.0.1:11434/api/ps"),
            Some(Url { host: "127.0.0.1".into(), port: 11434, path: "/api/ps".into() })
        );
        assert_eq!(parse_url("http://ollama/api/ps").map(|u| u.port), Some(80));
        assert_eq!(parse_url("https://127.0.0.1/api/ps"), None);
    }

    #[test]
    fn replies_plain_and_chunked() {
        assert_eq!(dechunk(b"4\r\nWiki\r\n5;x=y\r\npedia\r\n0\r\n\r\n"), Some(b"Wikipedia".to_vec()));
        assert_eq!(dechunk(b"9\r\nshort\r\n"), None);
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}extra";
        assert_eq!(parse_reply(plain), Some((200, b"{}".to_vec())));
        let chunked = b"HTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n";
        assert_eq!(parse_reply(chunked), Some((404, b"{}".to_vec())));
    }
}
