//! The web page's HTTP server. Binds 127.0.0.1 only; no authentication
//! (spec §5). Serves the page and the sources' state; credentials stay in
//! this process and never reach the page.
//!
//! One request per connection, then close; at most MAX_CONNECTIONS at once,
//! each read with a deadline and a size cap.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{page, settings, Web};
use crate::net;

/// High enough that a web page holding idle sockets to 127.0.0.1 cannot
/// crowd the dashboard out; each costs a parked thread at most.
const MAX_CONNECTIONS: usize = 256;
/// For the first byte of a request: a connection that says nothing goes.
const FIRST_BYTE: Duration = Duration::from_secs(2);
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);
const HEAD_LIMIT: usize = 16 * 1024;
const BODY_LIMIT: usize = 64 * 1024;
/// An unload waits for any generation in flight; the agent's own ceiling.
const UNLOAD_TIMEOUT: Duration = Duration::from_secs(35);

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// The media type, lowercased, without parameters.
    fn content_type(&self) -> String {
        self.header("content-type").and_then(|v| v.split(';').next()).unwrap_or("").trim().to_ascii_lowercase()
    }
}

/// %-decoding for paths; `+` is a space.
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct Reply {
    status: u16,
    content_type: &'static str,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn json(status: u16, value: &Value) -> Reply {
        Reply { status, content_type: "application/json; charset=utf-8", headers: Vec::new(), body: value.to_string().into_bytes() }
    }

    fn text(status: u16, text: &str) -> Reply {
        Reply { status, content_type: "text/plain; charset=utf-8", headers: Vec::new(), body: text.as_bytes().to_vec() }
    }

    fn no_store(mut self) -> Reply {
        self.headers.push(("Cache-Control", "no-store".into()));
        self
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

pub fn serve(listener: TcpListener, web: Arc<Web>) -> ! {
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if active.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    continue; // closed at once, unread
                }
                active.fetch_add(1, Ordering::SeqCst);
                let (c, a) = (Arc::clone(&web), Arc::clone(&active));
                let spawned = thread::Builder::new().stack_size(512 * 1024).spawn(move || {
                    handle(&c, stream);
                    a.fetch_sub(1, Ordering::SeqCst);
                });
                if spawned.is_err() {
                    active.fetch_sub(1, Ordering::SeqCst);
                }
            }
            // Out of file descriptors, say: wait rather than spin.
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let started = Instant::now();
    let deadline = started + REQUEST_DEADLINE;
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    let read = |stream: &mut TcpStream, chunk: &mut [u8], first: bool| -> Option<usize> {
        let until = if first { started + FIRST_BYTE } else { deadline };
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        stream.set_read_timeout(Some(left)).ok()?;
        match stream.read(chunk) {
            Ok(0) | Err(_) => None,
            Ok(n) => Some(n),
        }
    };
    let (head_len, body_at) = loop {
        let n = read(stream, &mut chunk, buf.is_empty())?;
        let from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf[from..].windows(4).position(|w| w == b"\r\n\r\n") {
            break (from + i, from + i + 4);
        }
        if buf.len() > HEAD_LIMIT {
            return None;
        }
    };
    let head = std::str::from_utf8(&buf[..head_len]).ok()?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_string();
    let target = first.next()?;
    let path = target.split_once('?').map_or(target, |(path, _)| path);
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    if length > BODY_LIMIT {
        return Some(Request { method: "TOO-LARGE".into(), path: String::new(), headers, body: Vec::new() });
    }
    let mut body = buf[body_at..].to_vec();
    body.truncate(length);
    while body.len() < length {
        let want = (length - body.len()).min(chunk.len());
        let n = read(stream, &mut chunk[..want], false)?;
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Request { method, path: path.to_string(), headers, body })
}

fn handle(web: &Web, mut stream: TcpStream) {
    let Some(req) = read_request(&mut stream) else { return };
    let mut reply = if req.method == "TOO-LARGE" {
        Reply::text(413, "request too large")
    } else {
        match own_page_only(web.config.port, &req) {
            Some(refusal) => refusal,
            None => route(web, &req),
        }
    };
    // Never inside another page: a hidden frame could turn two clicks on a
    // lure into Stop and Confirm.
    reply.headers.push(("X-Frame-Options", "DENY".into()));
    reply.headers.push(("Content-Security-Policy", "frame-ancestors 'none'".into()));
    // The page's modules revalidate on every load, so a browser never runs
    // an update against stale scripts.
    if req.path.starts_with("/static/") {
        reply.headers.push(("Cache-Control", "no-cache".into()));
    }
    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reason(reply.status),
        reply.content_type,
        reply.body.len()
    );
    for (k, v) in &reply.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let _ = stream.write_all(out.as_bytes());
    if req.method != "HEAD" {
        let _ = stream.write_all(&reply.body);
    }
}

/// Refuses requests that did not come from this dashboard's own page, as
/// server.py's middleware: Host must name this server; a browser's
/// Sec-Fetch-Site must say same-origin (or a top-level navigation to the
/// page); writes must come from this origin and carry JSON. A request with
/// neither header is a local process, which could reach anything here anyway.
fn own_page_only(port: u16, req: &Request) -> Option<Reply> {
    let mut hosts = vec![format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    if port == 80 {
        // Browsers leave the default port out of Host and Origin.
        hosts.extend(["127.0.0.1".to_string(), "localhost".to_string()]);
    }
    if !req.header("host").is_some_and(|h| hosts.iter().any(|x| x == h)) {
        return Some(Reply::text(403, "unexpected Host header"));
    }
    let opening_page = matches!(req.method.as_str(), "GET" | "HEAD")
        && req.path == "/"
        && req.header("sec-fetch-mode") == Some("navigate")
        && req.header("sec-fetch-dest") == Some("document");
    if let Some(site) = req.header("sec-fetch-site") {
        if site != "same-origin" && site != "none" && !opening_page {
            return Some(Reply::text(403, "cross-site request refused"));
        }
    }
    if req.method == "POST" {
        if let Some(origin) = req.header("origin") {
            if !hosts.iter().any(|h| origin == format!("http://{h}")) {
                return Some(Reply::text(403, "cross-origin request refused"));
            }
        }
        if req.content_type() != "application/json" {
            return Some(Reply::text(415, "expected application/json"));
        }
    }
    None
}

fn route(c: &Web, req: &Request) -> Reply {
    let get = matches!(req.method.as_str(), "GET" | "HEAD");
    let post = req.method == "POST";
    match req.path.as_str() {
        "/" if get => index(c),
        "/api/state" if get => Reply::json(200, &page::snapshot(&c.config, &c.shared)).no_store(),
        "/api/ui" if get => Reply::json(200, &Value::Object(c.settings.current())).no_store(),
        "/api/ui" if post => save_ui(c, req),
        "/api/agent/unload" if post => unload(c, req),
        p if get && p.starts_with("/static/") => static_file(&c.config.root.join("static"), &p["/static/".len()..]),
        "/" | "/api/state" | "/api/ui" | "/api/agent/unload" => Reply::text(405, "405: Method Not Allowed"),
        _ => Reply::text(404, "404: Not Found"),
    }
}

/// The page, with the saved appearance already on its <html> element, so
/// the first paint is right with no client storage (pywebview's private
/// mode wipes it at every launch).
fn index(c: &Web) -> Reply {
    let ui = c.settings.current();
    let theme = ui.get("theme").and_then(Value::as_str).unwrap_or("ice");
    let glow = ui.get("glow").and_then(Value::as_f64).unwrap_or(1.0).clamp(0.0, 3.0);
    let mut attrs = vec![format!("data-theme=\"{}\"", escape(theme)), format!("style=\"--glow-scale: {}\"", python_float(glow))];
    for name in ["scanlines", "grain", "aurora"] {
        if ui.get(name).and_then(Value::as_bool).unwrap_or(false) {
            attrs.push(format!("data-{name}=\"on\""));
        }
    }
    let Ok(page) = std::fs::read_to_string(c.config.root.join("static").join("index.html")) else {
        return Reply::text(500, "index.html is missing");
    };
    let page = page.replacen("<html lang=\"en\">", &format!("<html lang=\"en\" {}>", attrs.join(" ")), 1);
    Reply { status: 200, content_type: "text/html; charset=utf-8", headers: vec![("Cache-Control", "no-store".into())], body: page.into_bytes() }
}

/// As Python prints a float: 1.0, not 1.
fn python_float(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

/// html.escape with quote=True.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

/// Saves appearance settings: the one write the dashboard makes, and it
/// lands on the Mac, never on a homelab disk (spec §3).
fn save_ui(c: &Web, req: &Request) -> Reply {
    let Some(changes) = serde_json::from_slice::<Value>(&req.body).ok().filter(Value::is_object) else {
        return Reply::text(400, "expected a JSON object");
    };
    // Refused before anything is written: a bad window used to be saved and
    // then stop every later launch.
    let clean = settings::clean(&changes);
    let mut bad: Vec<&String> = changes.as_object().map_or(Vec::new(), |m| m.keys().filter(|k| settings::is_key(k) && !clean.contains_key(*k)).collect());
    bad.sort();
    if !bad.is_empty() {
        return Reply::text(400, &format!("invalid value for {}", bad.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
    }
    let saved = match c.settings.save(&changes) {
        Ok(s) => s,
        Err(e) => return Reply::text(500, &format!("could not save: {e}")),
    };
    // The history window re-lengths the ring buffers the charts read from.
    if changes.get("window").is_some() {
        c.set_history_window(saved.get("window").and_then(Value::as_f64).unwrap_or(180.0));
    }
    Reply::json(200, &Value::Object(saved))
}

/// Relays one "stop this model" request to the ml60 agent, which decides
/// whether to honour it; only the model name crosses. Every answer is JSON
/// in the agent's own shape, so the Stop button can read `error`.
fn unload(c: &Web, req: &Request) -> Reply {
    let failed = |status: u16, message: &str| Reply::json(status, &json!({"ok": false, "error": message}));
    if !c.config.agent_configured() {
        return failed(503, "no ml60 agent configured");
    }
    let Some(body) = serde_json::from_slice::<Value>(&req.body).ok().filter(Value::is_object) else {
        return failed(400, "expected a JSON object");
    };
    let name = match body.get("model").and_then(Value::as_str).map(str::trim) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return failed(400, "no model named"),
    };
    let url = format!("{}/unload", c.config.agent_url.trim_end_matches('/'));
    let http = net::http(net::home_ca_tls(), UNLOAD_TIMEOUT);
    let started = Instant::now();
    let (status, response) = match http.post(&url).send_json(json!({"model": name})) {
        Ok(r) => (r.status(), r),
        Err(ureq::Error::Status(code, r)) => (code, r),
        Err(e) => return failed(502, &net::describe(e, started)),
    };
    match response.into_json::<Value>() {
        Ok(payload) => Reply::json(if status == 200 { 200 } else { 502 }, &payload),
        Err(e) => failed(502, &format!("unreadable reply: {e}")),
    }
}

/// A file under static/, never outside it.
fn static_file(dir: &Path, rest: &str) -> Reply {
    let rest = decode(rest);
    let relative = Path::new(&rest);
    if rest.is_empty() || relative.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Reply::text(404, "404: Not Found");
    }
    let path = dir.join(relative);
    let Ok(body) = std::fs::read(&path) else { return Reply::text(404, "404: Not Found") };
    let content_type = match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "" | "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    };
    Reply { status: 200, content_type, headers: Vec::new(), body }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_strings_and_static_paths() {
        assert_eq!(decode("a%20b+c%2"), "a b c%2");
        let dir = std::env::temp_dir().join(format!("pvestats-static-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x.js"), "1").unwrap();
        assert_eq!(static_file(&dir, "x.js").status, 200);
        assert_eq!(static_file(&dir, "x.js").content_type, "text/javascript; charset=utf-8");
        assert_eq!(static_file(&dir, "../Cargo.toml").status, 404);
        assert_eq!(static_file(&dir, "%2e%2e/Cargo.toml").status, 404);
        assert_eq!(static_file(&dir, "/etc/passwd").status, 404);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!((python_float(1.0), python_float(2.5)), ("1.0".to_string(), "2.5".to_string()));
        assert_eq!(escape("a\"<b>"), "a&quot;&lt;b&gt;");
    }
}
