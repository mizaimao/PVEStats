//! pvestats-agent: ml60's GPU and loaded-model agent for PVEStats (spec
//! §3.2), in place of agent.py and with the same replies, plus "ram" and
//! "host".
//!
//! GET / answers with the GPU (NVML), Ollama's loaded models, ml60's own
//! memory (/proc/meminfo) and its CPU, network, disk, uptime and address
//! (host.rs) as JSON;
//! POST /unload {"model": name} asks Ollama to drop a loaded model, the one
//! write this project makes. Both are served on 127.0.0.1:9836, which Caddy
//! publishes to the LAN with no auth.
//!
//! What a client cannot do to ml60, by design:
//! - Set how often the GPU is read. One thread reads NVML about once a
//!   second, and only while someone has asked in the last 15 s; requests
//!   are served its latest reading. The same goes for Ollama's list, behind
//!   its 30 s cache.
//! - Tie it up. At most 16 connections at once (more are closed at once),
//!   each read with a 2 s deadline and a size cap, one request each.
//! - Unload freely. One unload at a time, only of a model that is loaded,
//!   and not the same model again within 10 s.
//!
//! Nothing is logged per request and nothing is written to disk: two lines
//! to stderr per start.

mod gpu;
mod host;
mod http;
mod ollama;

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::process::exit;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const MAX_CONNECTIONS: usize = 16;
/// For reading a whole request, head and body.
const REQUEST_DEADLINE: Duration = Duration::from_secs(2);
/// {"model": "<name>"} is a few dozen bytes; anything near this is not one.
const MAX_BODY: usize = 4096;
const SAMPLE_EVERY: Duration = Duration::from_secs(1);
/// With no request for this long, the threads stop reading and sleep.
const IDLE_AFTER: Duration = Duration::from_secs(15);
/// A GPU reading older than this is not served without first waiting for a
/// new one: the sampler was idle.
const STALE_AFTER: Duration = Duration::from_millis(2500);
/// The most a request waits for fresh readings; the dashboard allows 2 s.
const FRESH_WAIT: Duration = Duration::from_millis(1200);
const UNLOAD_COOLDOWN: Duration = Duration::from_secs(10);

pub struct Config {
    bind: SocketAddr,
    gpu_index: u32,
    ollama: http::Url,
    generate: http::Url,
    allow_unload: bool,
}

/// The environment, as agent.py read it.
fn config() -> Result<Config, String> {
    let var = |key: &str, default: &str| std::env::var(key).unwrap_or_else(|_| default.to_string());
    let host = var("PVESTATS_AGENT_BIND", "127.0.0.1");
    let port: u16 = var("PVESTATS_AGENT_PORT", "9836").trim().parse().map_err(|_| "PVESTATS_AGENT_PORT is not a port")?;
    let bind = (host.trim(), port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .ok_or("PVESTATS_AGENT_BIND is not an address")?;
    let gpu_index = var("PVESTATS_GPU_INDEX", "0").trim().parse().map_err(|_| "PVESTATS_GPU_INDEX is not a number")?;
    let ollama_url = var("OLLAMA_URL", "http://127.0.0.1:11434/api/ps");
    let generate_url = std::env::var("OLLAMA_GENERATE_URL").unwrap_or_else(|_| match ollama_url.rfind("/api/") {
        Some(i) => format!("{}/api/generate", &ollama_url[..i]),
        None => "http://127.0.0.1:11434/api/generate".into(),
    });
    let ollama = http::parse_url(&ollama_url).ok_or("OLLAMA_URL must be an http:// URL")?;
    let generate = http::parse_url(&generate_url).ok_or("OLLAMA_GENERATE_URL must be an http:// URL")?;
    // Fails closed: only these leave it open, so "false", "off" or an empty
    // value close it.
    let allow = var("PVESTATS_AGENT_ALLOW_UNLOAD", "1").trim().to_ascii_lowercase();
    let allow_unload = matches!(allow.as_str(), "1" | "true" | "yes" | "on");
    Ok(Config { bind, gpu_index, ollama, generate, allow_unload })
}

/// The latest GPU reading.
#[derive(Default)]
struct Reading {
    value: Option<Value>,
    error: Option<String>,
    at: Option<Instant>,
}

pub struct Shared {
    config: Config,
    gpu: Mutex<Reading>,
    gpu_fresh: Condvar,
    models: Mutex<ollama::Models>,
    models_fresh: Condvar,
    /// When someone last asked; the threads wait on `wake` while it is old.
    demand: Mutex<Option<Instant>>,
    wake: Condvar,
    connections: AtomicUsize,
    unloading: AtomicBool,
    /// Models unloaded lately, and when.
    stopped: Mutex<HashMap<String, Instant>>,
    /// When an unload last went to Ollama: at most one a second.
    last_unload: Mutex<Option<Instant>>,
}

impl Shared {
    fn new(config: Config) -> Self {
        Shared {
            config,
            gpu: Mutex::new(Reading::default()),
            gpu_fresh: Condvar::new(),
            models: Mutex::new(ollama::Models::default()),
            models_fresh: Condvar::new(),
            demand: Mutex::new(None),
            wake: Condvar::new(),
            connections: AtomicUsize::new(0),
            unloading: AtomicBool::new(false),
            stopped: Mutex::new(HashMap::new()),
            last_unload: Mutex::new(None),
        }
    }
}

/// A poisoned lock still holds good data.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn wait<'a, T>(cv: &Condvar, guard: MutexGuard<'a, T>, timeout: Duration) -> MutexGuard<'a, T> {
    match cv.wait_timeout(guard, timeout) {
        Ok((g, _)) => g,
        Err(e) => e.into_inner().0,
    }
}

fn main() {
    let config = config().unwrap_or_else(|e| {
        eprintln!("pvestats-agent: {e}");
        exit(2);
    });
    let listener = TcpListener::bind(config.bind).unwrap_or_else(|e| {
        eprintln!("pvestats-agent: cannot listen on {}: {e}", config.bind);
        exit(1);
    });
    eprintln!("pvestats-agent listening on {}", config.bind);
    let shared = Arc::new(Shared::new(config));
    let s = Arc::clone(&shared);
    thread::spawn(move || sample_forever(&s));
    let s = Arc::clone(&shared);
    thread::spawn(move || models_forever(&s));
    serve(&listener, &shared);
}

// --------------------------------------------------------------- threads

/// Returns once someone has asked within IDLE_AFTER; sleeps until then.
fn wait_for_demand(s: &Shared) {
    let mut d = lock(&s.demand);
    while !d.is_some_and(|t| t.elapsed() < IDLE_AFTER) {
        d = s.wake.wait(d).unwrap_or_else(|e| e.into_inner());
    }
}

fn sample_forever(s: &Shared) -> ! {
    // NVML starts here, not before listening: should it hang, the models
    // are still served. Loudly if the GPU is unreachable, so a broken
    // install shows in `systemctl status`, not only in the JSON.
    let mut gpu = gpu::Gpu::default();
    if let Err(e) = gpu.start() {
        eprintln!("warning: NVML unavailable at startup: {e}");
    }
    loop {
        wait_for_demand(s);
        let started = Instant::now();
        let (value, error) = match gpu.read(s.config.gpu_index) {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(e)),
        };
        *lock(&s.gpu) = Reading { value, error, at: Some(Instant::now()) };
        s.gpu_fresh.notify_all();
        thread::sleep(SAMPLE_EVERY.saturating_sub(started.elapsed()));
    }
}

fn models_forever(s: &Shared) -> ! {
    loop {
        wait_for_demand(s);
        if lock(&s.models).due(Instant::now()) {
            // Fetched without the lock held: Ollama may take its full second.
            let outcome = ollama::fetch(&s.config.ollama);
            lock(&s.models).take(outcome, Instant::now());
            s.models_fresh.notify_all();
        }
        // Woken at once by a request or an unload; otherwise a look every
        // 200 ms at whether the cache has run out.
        let d = lock(&s.demand);
        drop(wait(&s.wake, d, Duration::from_millis(200)));
    }
}

// --------------------------------------------------------------- serving

fn serve(listener: &TcpListener, s: &Arc<Shared>) -> ! {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // Over the limit: closed at once, unread.
                if s.connections.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    continue;
                }
                s.connections.fetch_add(1, Ordering::SeqCst);
                let theirs = Arc::clone(s);
                let spawned = thread::Builder::new().stack_size(512 * 1024).spawn(move || {
                    handle(&theirs, stream);
                    theirs.connections.fetch_sub(1, Ordering::SeqCst);
                });
                if spawned.is_err() {
                    s.connections.fetch_sub(1, Ordering::SeqCst);
                }
            }
            // Out of file descriptors, say: wait rather than spin.
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn handle(s: &Shared, mut stream: TcpStream) {
    let deadline = Instant::now() + REQUEST_DEADLINE;
    let Some(req) = http::read_request(&mut stream, deadline) else { return };
    let (status, body) = match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => (200, payload(s)),
        ("POST", "/unload") => unload(s, &mut stream, &req, deadline),
        ("GET" | "POST", _) => (404, json!({"ok": false, "error": "not found"})),
        _ => (501, json!({"ok": false, "error": "not implemented"})),
    };
    http::respond(&mut stream, status, &body.to_string());
}

/// The reply to GET /: the latest readings, waiting up to FRESH_WAIT for
/// new ones when the threads had gone idle or the model cache has run out.
fn payload(s: &Shared) -> Value {
    let asked = Instant::now();
    let until = asked + FRESH_WAIT;
    *lock(&s.demand) = Some(asked);
    s.wake.notify_all();

    let (gpu, error) = {
        let mut g = lock(&s.gpu);
        while !g.at.is_some_and(|t| t.elapsed() < STALE_AFTER) {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            g = wait(&s.gpu_fresh, g, left);
        }
        // A reading that never came (NVML hung, say) is said to be missing,
        // not served as if it were current.
        match g.at.map(|t| t.elapsed()) {
            Some(age) if age < STALE_AFTER => (g.value.clone(), g.error.clone()),
            Some(age) => (None, Some(format!("NVML: no reading for {} s", age.as_secs()))),
            None => (None, Some("NVML: no reading yet".to_string())),
        }
    };
    let models = {
        let mut m = lock(&s.models);
        while m.due(Instant::now()) && !m.fetched.is_some_and(|f| f >= asked) {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            m = wait(&s.models_fresh, m, left);
        }
        m.list.clone()
    };
    let error = match (&gpu, error) {
        (Some(_), _) => Value::Null,
        (None, Some(e)) => Value::String(e),
        (None, None) => Value::String("no GPU reading yet".into()),
    };
    let ram = std::fs::read_to_string("/proc/meminfo").map_or(Value::Null, |t| meminfo(&t));
    let host = host::reading();
    json!({"gpu": gpu, "models": models, "can_unload": s.config.allow_unload, "error": error, "ram": ram, "host": host})
}

/// ml60's own memory, from /proc/meminfo, in bytes: the total; what is
/// available to programs without swapping (free memory plus cache the
/// kernel can drop); and the cache itself (buffers, page cache and
/// reclaimable slab, as `free` counts buff/cache). PVE cannot tell cache
/// from programs and counts both as used: 95% on ml60 when its programs use
/// 12% and model files fill the rest as cache. Null if unreadable.
fn meminfo(text: &str) -> Value {
    let kib = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
            .map(|k| k * 1024)
    };
    match (kib("MemTotal:"), kib("MemAvailable:")) {
        (Some(total), Some(available)) => {
            let cache: u64 = [kib("Buffers:"), kib("Cached:"), kib("SReclaimable:")].iter().flatten().sum();
            json!({"total": total, "available": available, "cache": cache})
        }
        _ => Value::Null,
    }
}

/// POST /unload, its checks in agent.py's order, then three of its own:
/// one at a time, not the same model twice within 10 s, and only a model
/// that is loaded.
fn unload(s: &Shared, stream: &mut TcpStream, req: &http::Request, deadline: Instant) -> (u16, Value) {
    let refuse = |status: u16, error: &str| (status, json!({"ok": false, "error": error}));
    // Any web page can make a browser send a cross-site POST with a
    // text/plain body and no preflight, and Caddy forwards it. Browsers mark
    // those with Origin; the dashboard's relay sends JSON and no Origin.
    if req.header("origin").is_some() {
        return refuse(403, "cross-site request refused");
    }
    let media = req.header("content-type").and_then(|v| v.split(';').next()).map(|v| v.trim().to_ascii_lowercase());
    if media.as_deref() != Some("application/json") {
        return refuse(415, "expected application/json");
    }
    if !s.config.allow_unload {
        return refuse(403, "unload disabled on this agent");
    }
    let length = req.header("content-length").unwrap_or("").trim();
    if req.header("transfer-encoding").is_some() || length.is_empty() || !length.bytes().all(|b| b.is_ascii_digit()) {
        return refuse(411, "Content-Length required");
    }
    let length: usize = match length.parse() {
        Ok(n) if length.len() <= 6 && n <= MAX_BODY => n,
        _ => return refuse(413, "request too large"),
    };
    let Some(body) = http::read_body(stream, req, length, deadline) else {
        return refuse(400, "malformed request");
    };
    let parsed: Option<Value> = if body.is_empty() { Some(json!({})) } else { serde_json::from_slice(&body).ok() };
    let Some(request) = parsed.filter(Value::is_object) else {
        return refuse(400, "malformed request");
    };
    let name = match request.get("model").and_then(Value::as_str).map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => return refuse(400, "no model named"),
    };

    if s.unloading.swap(true, Ordering::SeqCst) {
        return refuse(409, "another unload is in progress");
    }
    let reply = unload_one(s, &name);
    s.unloading.store(false, Ordering::SeqCst);
    reply
}

fn unload_one(s: &Shared, name: &str) -> (u16, Value) {
    let refuse = |status: u16, error: String| (status, json!({"ok": false, "error": error}));
    {
        let mut stopped = lock(&s.stopped);
        stopped.retain(|_, at| at.elapsed() < UNLOAD_COOLDOWN);
        if stopped.contains_key(name) {
            return refuse(429, format!("{name} was stopped moments ago"));
        }
    }
    // Whether it is loaded, from the cached list while that is a few
    // seconds old at most: a loop of requests naming models that are not
    // loaded must not reach Ollama, which logs each call to disk.
    let cached = {
        let m = lock(&s.models);
        m.fetched.filter(|t| t.elapsed() < ollama::RECHECK_AFTER).map(|_| m.list.clone())
    };
    let listed = |list: &[Value]| list.iter().any(|m| m["name"].as_str() == Some(name));
    if cached.as_deref().is_some_and(|list| !listed(list)) {
        return refuse(404, format!("{name} is not loaded"));
    }
    // From here on Ollama is asked: once a second at most.
    {
        let mut last = lock(&s.last_unload);
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return refuse(429, "too many requests; try again in a second".into());
        }
        *last = Some(Instant::now());
    }
    if cached.is_none() {
        match ollama::fetch(&s.config.ollama) {
            Err(e) => return refuse(502, format!("Ollama: {e}")),
            Ok(list) if !listed(&list) => return refuse(404, format!("{name} is not loaded")),
            Ok(_) => {}
        }
    }
    let (status, body) = ollama::unload(&s.config.generate, name);
    // Whatever came of it, the list is fetched again at once and the model
    // watched until it has gone; once it has been stopped, not again for a
    // while.
    let now = Instant::now();
    if status == 200 {
        lock(&s.stopped).insert(name.to_string(), now);
    }
    lock(&s.models).stopped(name, now);
    s.wake.notify_all();
    (status, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::atomic::AtomicUsize;

    /// A stand-in Ollama: /api/ps answers with `ps`, /api/generate with an
    /// unload; it counts the calls to each.
    struct FakeOllama {
        url: String,
        ps_calls: Arc<AtomicUsize>,
        generate_calls: Arc<AtomicUsize>,
    }

    fn fake_ollama(ps: &str) -> FakeOllama {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let ps = Arc::new(Mutex::new(ps.to_string()));
        let (ps_calls, generate_calls) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (p, pc, gc) = (ps, ps_calls.clone(), generate_calls.clone());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let Some(req) = http::read_request(&mut stream, Instant::now() + Duration::from_secs(2)) else { continue };
                if let Some(n) = req.header("content-length").and_then(|v| v.parse().ok()) {
                    let _ = http::read_body(&mut stream, &req, n, Instant::now() + Duration::from_secs(2));
                }
                let body = if req.path == "/api/ps" {
                    pc.fetch_add(1, Ordering::SeqCst);
                    lock(&p).clone()
                } else {
                    gc.fetch_add(1, Ordering::SeqCst);
                    r#"{"model":"qwen3:32b","done":true,"done_reason":"unload"}"#.to_string()
                };
                // Chunked, as Go sends longer replies.
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        FakeOllama { url, ps_calls, generate_calls }
    }

    const PS: &str = r#"{"models":[{"name":"qwen3:32b","model":"qwen3:32b","size":20000,"size_vram":20000,"expires_at":"2318-01-01T00:00:00Z","context_length":8192}]}"#;

    /// The agent, serving on a free port, with a GPU reading that stays
    /// fresh (no NVML on the build machine).
    fn agent(ollama: &FakeOllama, allow_unload: bool) -> String {
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            gpu_index: 0,
            ollama: http::parse_url(&format!("{}/api/ps", ollama.url)).unwrap(),
            generate: http::parse_url(&format!("{}/api/generate", ollama.url)).unwrap(),
            allow_unload,
        };
        let listener = TcpListener::bind(config.bind).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let shared = Arc::new(Shared::new(config));
        let s = Arc::clone(&shared);
        thread::spawn(move || loop {
            *lock(&s.gpu) = Reading { value: Some(json!({"util": 7, "procs": []})), error: None, at: Some(Instant::now()) };
            s.gpu_fresh.notify_all();
            thread::sleep(Duration::from_millis(200));
        });
        let s = Arc::clone(&shared);
        thread::spawn(move || models_forever(&s));
        let s = Arc::clone(&shared);
        thread::spawn(move || serve(&listener, &s));
        addr
    }

    /// Sends raw bytes and returns the status and the JSON body.
    fn ask(addr: &str, raw: &str) -> (u16, Value) {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        stream.write_all(raw.as_bytes()).unwrap();
        let mut reply = Vec::new();
        let _ = stream.read_to_end(&mut reply);
        let text = String::from_utf8_lossy(&reply);
        let status = text.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let body = text.split_once("\r\n\r\n").map_or("", |(_, b)| b);
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }

    fn post(addr: &str, headers: &str, body: &str) -> (u16, Value) {
        ask(addr, &format!("POST /unload HTTP/1.1\r\nHost: x\r\n{headers}Content-Length: {}\r\n\r\n{body}", body.len()))
    }

    const JSON: &str = "Content-Type: application/json\r\n";

    #[test]
    fn get_serves_the_gpu_and_the_models() {
        let ollama = fake_ollama(PS);
        let addr = agent(&ollama, true);
        let (status, body) = ask(&addr, "GET /?x=1 HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(status, 200);
        assert_eq!(body["gpu"]["util"], 7);
        assert_eq!(body["models"][0]["name"], "qwen3:32b");
        assert_eq!(body["models"][0]["context_length"], 8192);
        assert_eq!((body["can_unload"].as_bool(), body["error"].is_null()), (Some(true), true));
        // The build machine's own memory, as ml60's will be.
        let ram = &body["ram"];
        assert!(ram["total"].as_u64().unwrap() >= ram["available"].as_u64().unwrap(), "{ram}");
        assert!(ram["cache"].is_u64(), "{ram}");
        // And its CPU, network, disk and address, in the reply too.
        let host = &body["host"];
        assert!(host["cpu"]["total"].is_u64() && host["disk"]["total"].is_u64() && host["ip"].is_string(), "{host}");
        // Cached: more requests do not call Ollama again.
        for _ in 0..5 {
            ask(&addr, "GET / HTTP/1.1\r\n\r\n");
        }
        assert_eq!(ollama.ps_calls.load(Ordering::SeqCst), 1);
        assert_eq!(ask(&addr, "GET /nope HTTP/1.1\r\n\r\n").0, 404);
        assert_eq!(ask(&addr, "OPTIONS /unload HTTP/1.1\r\n\r\n").0, 501);
    }

    #[test]
    fn an_ollama_reply_of_the_wrong_shape_still_serves_the_gpu() {
        let ollama = fake_ollama("null");
        let addr = agent(&ollama, true);
        let (status, body) = ask(&addr, "GET / HTTP/1.1\r\n\r\n");
        assert_eq!(status, 200);
        assert_eq!(body["models"], json!([]));
        assert_eq!(body["gpu"]["util"], 7);
    }

    #[test]
    fn unload_checks_in_order_then_unloads_once() {
        let ollama = fake_ollama(PS);
        let addr = agent(&ollama, true);
        // The dashboard has polled, so the list is cached.
        ask(&addr, "GET / HTTP/1.1\r\n\r\n");
        let model = r#"{"model":"qwen3:32b"}"#;
        assert_eq!(post(&addr, &format!("Origin: https://evil\r\n{JSON}"), model).0, 403);
        assert_eq!(post(&addr, "Content-Type: text/plain\r\n", model).0, 415);
        assert_eq!(ask(&addr, &format!("POST /unload HTTP/1.1\r\n{JSON}\r\n")).0, 411);
        assert_eq!(ask(&addr, &format!("POST /unload HTTP/1.1\r\n{JSON}Content-Length: ²\r\n\r\n")).0, 411);
        assert_eq!(ask(&addr, &format!("POST /unload HTTP/1.1\r\n{JSON}Content-Length: 99999\r\n\r\n")).0, 413);
        assert_eq!(post(&addr, JSON, "{not json").1["error"], "malformed request");
        assert_eq!(post(&addr, JSON, "[1]").1["error"], "malformed request");
        assert_eq!(post(&addr, JSON, r#"{"model":"  "}"#).1["error"], "no model named");
        // Not loaded, answered from the cached list as often as asked.
        for _ in 0..5 {
            assert_eq!(post(&addr, JSON, r#"{"model":"llama3:8b"}"#).0, 404);
        }
        assert_eq!(ollama.ps_calls.load(Ordering::SeqCst), 1, "the 404s did not reach Ollama");
        assert_eq!(ollama.generate_calls.load(Ordering::SeqCst), 0, "nothing reached Ollama yet");
        let (status, body) = post(&addr, "Content-Type: Application/JSON; charset=utf-8\r\n", model);
        assert_eq!((status, body["ok"].as_bool()), (200, Some(true)), "{body}");
        assert_eq!(post(&addr, JSON, model).0, 429, "not the same model again at once");
        assert_eq!(ollama.generate_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unload_can_be_switched_off() {
        let ollama = fake_ollama(PS);
        let addr = agent(&ollama, false);
        let (status, body) = post(&addr, JSON, r#"{"model":"qwen3:32b"}"#);
        assert_eq!((status, body["error"].as_str()), (403, Some("unload disabled on this agent")));
        assert_eq!(ask(&addr, "GET / HTTP/1.1\r\n\r\n").1["can_unload"], false);
    }

    #[test]
    fn a_body_that_never_comes_is_given_two_seconds() {
        let ollama = fake_ollama(PS);
        let addr = agent(&ollama, true);
        let mut stream = TcpStream::connect(&addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let started = Instant::now();
        stream.write_all(format!("POST /unload HTTP/1.1\r\n{JSON}Content-Length: 40\r\n\r\n{{\"mo").as_bytes()).unwrap();
        let mut reply = String::new();
        let _ = stream.read_to_string(&mut reply);
        assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn connections_past_the_limit_are_closed_at_once() {
        let ollama = fake_ollama(PS);
        let addr = agent(&ollama, true);
        // Sixteen that say nothing hold every place for up to 2 s each.
        let idle: Vec<TcpStream> = (0..MAX_CONNECTIONS).map(|_| TcpStream::connect(&addr).unwrap()).collect();
        thread::sleep(Duration::from_millis(300));
        let mut extra = TcpStream::connect(&addr).unwrap();
        extra.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let _ = extra.write_all(b"GET / HTTP/1.1\r\n\r\n");
        let mut buf = Vec::new();
        let _ = extra.read_to_end(&mut buf);
        assert!(buf.is_empty(), "the seventeenth got an answer");
        drop(idle);
        // Once theirs time out, the agent answers again.
        thread::sleep(Duration::from_millis(2500));
        assert_eq!(ask(&addr, "GET / HTTP/1.1\r\n\r\n").0, 200);
    }
}

#[cfg(test)]
mod meminfo_tests {
    use super::*;

    #[test]
    fn memory_as_free_counts_it() {
        // ml60's own figures on 2026-10-26: 64 GiB, 8 GB used by programs,
        // 58.5 GB of cache. SwapCached and KReclaimable come first here,
        // unlike the real file, so a key matched anywhere in a line would
        // read them in place of Cached and SReclaimable.
        let text = "MemTotal:       65782392 kB\nMemFree:         1355540 kB\nMemAvailable:   57908632 kB\n\
                    Buffers:          157140 kB\nSwapCached:       1234 kB\nCached:         55369100 kB\n\
                    KReclaimable:    1702124 kB\nSReclaimable:    1636588 kB\n";
        let ram = meminfo(text);
        assert_eq!(ram["total"], 65_782_392u64 * 1024);
        assert_eq!(ram["available"], 57_908_632u64 * 1024);
        assert_eq!(ram["cache"], (157_140u64 + 55_369_100 + 1_636_588) * 1024);
        assert_eq!(meminfo("MemTotal: 1 kB\n"), Value::Null);
    }
}
