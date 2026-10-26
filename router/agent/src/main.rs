//! pvestats-router: an Asuswrt-Merlin router's own counters as JSON over
//! HTTP, for the PVEStats router card. One small process in place of socat
//! and a shell script, which started some 35 programs per request.
//!
//! A request reads only kernel files: /proc/net/dev, /proc/stat,
//! /proc/meminfo, /proc/uptime, the SoC's thermal zone and each wired port's
//! link. What changes slowly (the interface layout, the WAN's state and
//! address, the devices on each band) comes from the firmware's `nvram` and
//! `wl`, run by a second thread every 30 seconds. Each run is killed after 5
//! seconds, and one that will not die stops any more from starting, so a
//! hung driver cannot pile processes up; the counts then just grow old.
//!
//! Each client keeps one connection and asks over it again and again (HTTP/1.1
//! keep-alive): a dashboard polling every second is one entry in the router's
//! connection table, not a new connection a second, each remembered for two
//! minutes after it closes. At most 4 clients at once; a connection with no
//! request for 30 seconds is closed. Answers are at least 100 ms apart across
//! all of them, each request's head is given at most 3 seconds once it has
//! begun, and what a request asks is never looked at. Nothing is logged and
//! nothing written. Should the LAN address change, it exits, for its keeper
//! to start it again on the new one.
//!
//!     pvestats-router --listen 10.10.10.1:9837
//!     pvestats-router --once      the JSON once, for trying it by hand

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::process::{exit, Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// How often the slow-changing things are asked for again.
const SLOW_EVERY: Duration = Duration::from_secs(30);
/// The interface layout changes only when the router is reconfigured: it is
/// read again every this many rounds (five minutes).
const LAYOUT_EVERY: u32 = 10;
/// Longest an `nvram` or `wl` run may take before it is killed.
const RUN_LIMIT: Duration = Duration::from_secs(5);
/// Longest a request's head may take once it has begun, and an answer.
const REQUEST_LIMIT: Duration = Duration::from_secs(3);
/// Most of a request's head that is read.
const HEAD_LIMIT: usize = 8192;
/// Answers are at least this far apart, so a client asking in a tight loop
/// gets ten a second, not as many as a core can make.
const MIN_GAP: Duration = Duration::from_millis(100);
/// Clients served at once, each on the connection it keeps.
const MAX_CLIENTS: usize = 4;
/// A kept connection with no request for this long is closed.
const IDLE_LIMIT: Duration = Duration::from_secs(30);

/// One radio and the networks it carries (wl0 with wl0.1 and so on).
#[derive(Clone, Debug, Default, PartialEq)]
struct Radio {
    /// "2.4 GHz", "5 GHz", "6 GHz-1".
    name: String,
    networks: Vec<String>,
    on: bool,
}

/// Interface names and the like: what the firmware is set up with.
#[derive(Clone, Debug, Default, PartialEq)]
struct Layout {
    model: String,
    wan_if: String,
    /// The physical port the WAN rides on, when it is one of the LAN ports
    /// (a 10G port set as WAN carries the WAN as a VLAN): every download
    /// crosses it, so the dashboard leaves it out of the wired traffic.
    wan_port: Option<String>,
    /// The LAN bridge (br0), whose neighbours and learned addresses give
    /// the wired devices.
    bridge: String,
    /// The wired LAN ports, in the order the router lists them.
    ports: Vec<String>,
    radios: Vec<Radio>,
}

/// What the thread that runs `nvram` and `wl` last found.
#[derive(Clone, Default)]
struct Slow {
    layout: Layout,
    wan_up: bool,
    wan_ip: String,
    /// Devices per radio, in `layout.radios` order, each with when it was
    /// counted; None until it first is. Each radio keeps its own, so one
    /// that will not answer does not hold back the others.
    devices: Vec<Option<(u32, Instant)>>,
    /// Devices on the wired side; None until first counted.
    wired: Option<u32>,
    /// Each wired port's link speed in Mbit/s, in `layout.ports` order.
    /// Read here, not per request: the kernel takes its network-wide lock
    /// to read it, and it changes only when a link renegotiates.
    speeds: Vec<u32>,
}

/// A poisoned lock still holds good data.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--once" => {
            let slow = Mutex::new(Slow::default());
            refresh(&mut Runner::default(), &slow, true);
            print!("{}", json(&lock(&slow).clone()));
        }
        [flag, addr] if flag == "--listen" => {
            let Ok(addr) = addr.parse::<SocketAddr>() else { exit(2) };
            let Ok(listener) = TcpListener::bind(addr) else { exit(1) };
            let slow = Arc::new(Mutex::new(Slow::default()));
            let theirs = Arc::clone(&slow);
            thread::spawn(move || refresh_forever(&theirs, addr));
            serve(&listener, &slow);
        }
        _ => {
            eprintln!("usage: pvestats-router --listen ADDR:PORT | --once");
            exit(2);
        }
    }
}

// ------------------------------------------------------------------ serving

fn serve(listener: &TcpListener, slow: &Arc<Mutex<Slow>>) -> ! {
    let active = Arc::new(AtomicUsize::new(0));
    let last: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // Past the limit: closed at once, unread.
                if active.load(Ordering::SeqCst) >= MAX_CLIENTS {
                    continue;
                }
                active.fetch_add(1, Ordering::SeqCst);
                let (slow, last, gone) = (Arc::clone(slow), Arc::clone(&last), Arc::clone(&active));
                let spawned = thread::Builder::new().stack_size(256 * 1024).spawn(move || {
                    client(stream, &slow, &last);
                    gone.fetch_sub(1, Ordering::SeqCst);
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

/// One client's connection, answered request after request until it is
/// idle for IDLE_LIMIT, asks to close, or sends anything unexpected. What a
/// request asks is never looked at; every one gets the same answer.
fn client(mut stream: TcpStream, slow: &Mutex<Slow>, last: &Mutex<Option<Instant>>) {
    let mut buf: Vec<u8> = Vec::new();
    while let Some(head) = next_head(&mut stream, &mut buf) {
        let keep = keep_alive(&head);
        // Answers at least MIN_GAP apart, across all clients.
        {
            let mut last = lock(last);
            if let Some(t) = *last {
                thread::sleep(MIN_GAP.saturating_sub(t.elapsed()));
            }
            *last = Some(Instant::now());
        }
        let snapshot = lock(slow).clone();
        let body = json(&snapshot);
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: {}\r\n\r\n{body}",
            body.len(),
            if keep { "keep-alive" } else { "close" }
        );
        if stream.set_write_timeout(Some(REQUEST_LIMIT)).is_err() || stream.write_all(reply.as_bytes()).is_err() {
            return;
        }
        if !keep {
            let _ = stream.shutdown(Shutdown::Write);
            return;
        }
    }
}

/// Where a request's head ends, and where what follows it starts: at its
/// blank line, however the client ends its lines. Looks only from `from`.
fn head_end(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    (from..buf.len()).find_map(|i| {
        if buf[i..].starts_with(b"\r\n\r\n") {
            Some((i, i + 4))
        } else if buf[i..].starts_with(b"\n\n") {
            Some((i, i + 2))
        } else {
            None
        }
    })
}

/// The next request's head on the connection: up to IDLE_LIMIT for one to
/// begin, then REQUEST_LIMIT for the rest of it, at most HEAD_LIMIT bytes.
/// Bytes after it (a pipelined request) stay in `buf` for the next call.
fn next_head(stream: &mut TcpStream, buf: &mut Vec<u8>) -> Option<String> {
    let idle_until = Instant::now() + IDLE_LIMIT;
    let mut deadline = (!buf.is_empty()).then(|| Instant::now() + REQUEST_LIMIT);
    let mut scanned: usize = 0;
    let mut chunk = [0u8; 2048];
    loop {
        // Only what arrived since the last look, and the three bytes before
        // it, can complete the blank line: a byte at a time costs no rescans.
        if let Some((end, next)) = head_end(buf, scanned.saturating_sub(3)) {
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            buf.drain(..next);
            return Some(head);
        }
        scanned = buf.len();
        if buf.len() >= HEAD_LIMIT {
            return None;
        }
        let left = deadline.unwrap_or(idle_until).saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        stream.set_read_timeout(Some(left)).ok()?;
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                deadline.get_or_insert(Instant::now() + REQUEST_LIMIT);
            }
        }
    }
}

/// Whether to keep the connection after answering: HTTP/1.1 keeps it unless
/// asked to close, HTTP/1.0 only when asked to keep it. A request with a
/// body is answered and the connection closed, since bodies are not read.
fn keep_alive(head: &str) -> bool {
    let mut lines = head.lines();
    let http11 = lines.next().is_some_and(|l| l.trim_end().ends_with("HTTP/1.1"));
    let mut connection = String::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim().to_ascii_lowercase());
        match name.as_str() {
            "connection" => connection = value,
            "transfer-encoding" => return false,
            "content-length" if value != "0" => return false,
            _ => {}
        }
    }
    if http11 {
        !connection.contains("close")
    } else {
        connection.contains("keep-alive")
    }
}

// --------------------------------------------------------------- the answer

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// rx and tx bytes since boot for every interface in /proc/net/dev. A big
/// first number can follow the colon with no space between them.
fn net_dev(text: &str) -> HashMap<String, (u64, u64)> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let Some((name, rest)) = line.split_once(':') else { continue };
        let fields: Vec<u64> = rest.split_whitespace().filter_map(|f| f.parse().ok()).collect();
        if fields.len() >= 9 {
            out.insert(name.trim().to_string(), (fields[0], fields[8]));
        }
    }
    out
}

/// Busy and total jiffies over all cores, from /proc/stat's first line:
/// user nice system idle iowait irq softirq steal.
fn cpu(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let f: Vec<u64> = line.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
    if f.len() < 8 {
        return None;
    }
    let busy = f[0] + f[1] + f[2] + f[5] + f[6] + f[7];
    Some((busy, busy + f[3] + f[4]))
}

/// Total and available memory in bytes.
fn memory(text: &str) -> (u64, u64) {
    let kib = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    (kib("MemTotal:"), kib("MemAvailable:"))
}

/// A JSON string, quoted.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json(slow: &Slow) -> String {
    let dev = read("/proc/net/dev").map(|t| net_dev(&t)).unwrap_or_default();
    let bytes = |names: &[String]| {
        names.iter().filter_map(|n| dev.get(n)).fold((0u64, 0u64), |(r, t), (a, b)| (r + a, t + b))
    };
    let (busy, total) = read("/proc/stat").and_then(|t| cpu(&t)).unwrap_or((0, 0));
    let (mem_total, mem_available) = read("/proc/meminfo").map(|t| memory(&t)).unwrap_or((0, 0));
    let uptime = read("/proc/uptime")
        .and_then(|t| t.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
        .unwrap_or(0.0);
    let temp = read("/sys/class/thermal/thermal_zone0/temp")
        .and_then(|t| t.trim().parse::<f64>().ok())
        .map_or("null".to_string(), |m| format!("{:.1}", m / 1000.0));

    let l = &slow.layout;
    let (wan_rx, wan_tx) = bytes(std::slice::from_ref(&l.wan_if));
    let ports: Vec<String> = l
        .ports
        .iter()
        .enumerate()
        .map(|(i, p)| {
            // operstate is a plain read; the speed comes from the slow thread.
            let up = read(&format!("/sys/class/net/{p}/operstate")).is_some_and(|s| s.trim() == "up");
            let mbps = if up { slow.speeds.get(i).copied().unwrap_or(0) } else { 0 };
            let (rx, tx) = bytes(std::slice::from_ref(p));
            let wan = l.wan_port.as_deref() == Some(p.as_str());
            format!("{{\"name\":{},\"up\":{up},\"mbps\":{mbps},\"wan\":{wan},\"rx\":{rx},\"tx\":{tx}}}", quote(p))
        })
        .collect();
    let bands: Vec<String> = l
        .radios
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let (rx, tx) = bytes(&r.networks);
            // null for a radio never yet counted: unknown, not none.
            let devices = slow.devices.get(i).copied().flatten().map_or("null".to_string(), |(n, _)| n.to_string());
            format!(
                "{{\"name\":{},\"on\":{},\"devices\":{devices},\"rx\":{rx},\"tx\":{tx}}}",
                quote(&r.name),
                r.on
            )
        })
        .collect();
    // The oldest radio's count: how stale the devices can be.
    let age = slow
        .devices
        .iter()
        .flatten()
        .map(|(_, at)| at.elapsed().as_secs())
        .max()
        .map_or("null".to_string(), |s| s.to_string());
    format!(
        "{{\"model\":{},\"uptime\":{uptime:.2},\"cpu\":{{\"busy\":{busy},\"total\":{total}}},\
         \"mem\":{{\"total\":{mem_total},\"available\":{mem_available}}},\"temp\":{{\"cpu\":{temp}}},\
         \"wan\":{{\"up\":{},\"ip\":{},\"rx\":{wan_rx},\"tx\":{wan_tx}}},\
         \"ports\":[{}],\"wired_devices\":{},\"bands\":[{}],\"devices_age\":{age}}}\n",
        quote(&l.model),
        slow.wan_up,
        quote(&slow.wan_ip),
        ports.join(","),
        slow.wired.map_or("null".to_string(), |n| n.to_string()),
        bands.join(","),
    )
}

// ------------------------------------------------------- nvram and wl

/// Runs the firmware's tools, at most one stuck at a time.
#[derive(Default)]
struct Runner {
    /// A run that would not die when killed. Until it goes, nothing else is
    /// started.
    stuck: Option<Child>,
}

impl Runner {
    /// A tool's output, or None if it failed, could not start, or ran over
    /// RUN_LIMIT and was killed.
    fn run(&mut self, program: &str, args: &[&str]) -> Option<String> {
        self.run_within(program, args, RUN_LIMIT)
    }

    fn run_within(&mut self, program: &str, args: &[&str], limit: Duration) -> Option<String> {
        if let Some(child) = &mut self.stuck {
            match child.try_wait() {
                Ok(None) => return None,
                _ => self.stuck = None,
            }
        }
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    // Its output is a few lines, well inside the pipe's buffer,
                    // so it is all there once the tool has exited.
                    let mut out = String::new();
                    child.stdout.take()?.read_to_string(&mut out).ok()?;
                    return status.success().then_some(out);
                }
                Ok(None) if started.elapsed() < limit => thread::sleep(Duration::from_millis(5)),
                _ => {
                    let _ = child.kill();
                    thread::sleep(Duration::from_millis(50));
                    if !matches!(child.try_wait(), Ok(Some(_))) {
                        self.stuck = Some(child);
                    }
                    return None;
                }
            }
        }
    }

    fn nvram(&mut self, key: &str) -> Option<String> {
        self.run("nvram", &["get", key]).map(|v| v.trim().to_string())
    }

    /// The devices on one network, by address: `wl` lists each as
    /// "assoclist <MAC>".
    fn devices(&mut self, network: &str) -> Option<Vec<String>> {
        self.run("wl", &["-i", network, "assoclist"]).map(|out| assoc_macs(&out))
    }
}

fn assoc_macs(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.strip_prefix("assoclist"))
        .map(|m| m.trim().to_ascii_lowercase())
        .filter(|m| !m.is_empty())
        .collect()
}

/// Complete entries for `bridge` in /proc/net/arp, by address: the devices
/// the router has heard from on its LAN lately.
fn neighbours(arp: &str, bridge: &str) -> HashSet<String> {
    arp.lines()
        .skip(1)
        .filter_map(|line| {
            // IP address, HW type, Flags, HW address, Mask, Device
            let f: Vec<&str> = line.split_whitespace().collect();
            let complete = f.get(2).and_then(|fl| u32::from_str_radix(fl.trim_start_matches("0x"), 16).ok()).is_some_and(|fl| fl & 0x2 != 0);
            (f.len() >= 6 && f[5] == bridge && complete && f[3] != "00:00:00:00:00:00").then(|| f[3].to_ascii_lowercase())
        })
        .collect()
}

/// The addresses in a bridge's forwarding table (/sys/class/net/BR/brforward,
/// 16-byte entries: address, port, is-local, ageing, high port byte), with
/// the port each was learned on; the bridge's own addresses left out.
fn fdb_entries(table: &[u8]) -> Vec<(String, u16)> {
    table
        .chunks_exact(16)
        .filter(|e| e[7] == 0)
        .map(|e| {
            let mac = e[..6].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":");
            (mac, u16::from(e[6]) | u16::from(e[12]) << 8)
        })
        .collect()
}

/// Devices on the wired side: those the router has heard from on its LAN
/// bridge (its neighbour table, and the addresses the bridge learned on
/// wired ports) less those on Wi-Fi (in a radio's client list, or learned on
/// a Wi-Fi port) and those beyond the port the internet comes in on. File
/// reads only. None when the bridge cannot be read.
fn wired_devices(bridge: &str, wan_port: Option<&str>, wireless: &HashSet<String>) -> Option<u32> {
    let base = format!("/sys/class/net/{bridge}");
    let ports: HashMap<u16, String> = std::fs::read_dir(format!("{base}/brif"))
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let number = read(&format!("{base}/brif/{name}/port_no"))?;
            Some((u16::from_str_radix(number.trim().trim_start_matches("0x"), 16).ok()?, name))
        })
        .collect();
    let table = std::fs::read(format!("{base}/brforward")).ok()?;
    let mut wired = read("/proc/net/arp").map(|arp| neighbours(&arp, bridge)).unwrap_or_default();
    let mut elsewhere: HashSet<String> = wireless.clone();
    for (mac, port) in fdb_entries(&table) {
        match ports.get(&port) {
            Some(name) if name.starts_with("eth") && Some(name.as_str()) != wan_port => {
                wired.insert(mac);
            }
            Some(_) => {
                elsewhere.insert(mac);
            }
            None => {}
        }
    }
    wired.retain(|m| !elsewhere.contains(m));
    Some(wired.len() as u32)
}

/// A radio's name from nvram's band number: 2 is 2.4 GHz, 1 is 5 GHz, 4 is
/// 6 GHz. Two radios on 6 GHz are numbered.
fn band_name(nband: &str, sixes: &mut u32, radio: &str) -> String {
    match nband {
        "2" => "2.4 GHz".into(),
        "1" => "5 GHz".into(),
        "4" => {
            *sixes += 1;
            format!("6 GHz-{sixes}")
        }
        _ => radio.to_string(),
    }
}

/// The interface layout, or None if `nvram` would not answer.
fn layout(r: &mut Runner) -> Option<Layout> {
    let model = r.nvram("productid")?;
    let wan_if = r.nvram("wan0_ifname")?;
    let ports = r.nvram("lan_ifnames")?.split_whitespace().filter(|n| n.starts_with("eth")).map(String::from).collect();
    let mut radios = Vec::new();
    let mut sixes = 0;
    for u in 0..6 {
        let radio = r.nvram(&format!("wl{u}_ifname"))?;
        if radio.is_empty() {
            continue;
        }
        let name = band_name(&r.nvram(&format!("wl{u}_nband"))?, &mut sixes, &radio);
        let mut networks = vec![radio];
        networks.extend(r.nvram(&format!("wl{u}_vifs"))?.split_whitespace().map(String::from));
        let on = r.nvram(&format!("wl{u}_radio"))? == "1";
        radios.push(Radio { name, networks, on });
    }
    let wan_port = lower_port(&wan_if);
    let bridge = Some(r.nvram("lan_ifname")?).filter(|b| !b.is_empty()).unwrap_or_else(|| "br0".into());
    Some(Layout { model, wan_if, wan_port, bridge, ports, radios })
}

/// The device a VLAN interface rides on, from the kernel's own link in
/// /sys (`lower_eth1` for vlan4094 on eth1); None for a plain interface.
fn lower_port(interface: &str) -> Option<String> {
    let dir = std::fs::read_dir(format!("/sys/class/net/{interface}")).ok()?;
    dir.flatten().find_map(|e| e.file_name().to_str()?.strip_prefix("lower_").map(str::to_string))
}

fn refresh_forever(slow: &Mutex<Slow>, bound: SocketAddr) -> ! {
    let mut runner = Runner::default();
    let mut round: u32 = 0;
    loop {
        let layout_round = round % LAYOUT_EVERY == 0 || lock(slow).layout.model.is_empty();
        // The LAN address changed without a reboot: exit, and the keeper
        // starts the agent again on the new one.
        if layout_round {
            if let Some(ip) = runner.nvram("lan_ipaddr").and_then(|ip| ip.parse::<std::net::IpAddr>().ok()) {
                if ip != bound.ip() {
                    exit(0);
                }
            }
        }
        refresh(&mut runner, slow, layout_round);
        round = round.wrapping_add(1);
        thread::sleep(SLOW_EVERY);
    }
}

/// A port's link speed in Mbit/s, 0 when down or unknown.
fn speed(port: &str) -> u32 {
    read(&format!("/sys/class/net/{port}/speed"))
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|s| *s > 0)
        .map_or(0, |s| s.min(u32::MAX as i64) as u32)
}

/// One round: the WAN's state and address, the ports' speeds and the
/// devices on each band, and the layout too when asked. What a tool would
/// not tell keeps its last value. The tools run without the lock held.
fn refresh(r: &mut Runner, slow: &Mutex<Slow>, with_layout: bool) {
    let layout = if with_layout { layout(r) } else { None };
    let current = layout.clone().unwrap_or_else(|| lock(slow).layout.clone());
    let wan_up = r.nvram("wan0_state_t").map(|s| s == "2");
    let wan_ip = r.nvram("wan0_ipaddr");
    let speeds: Vec<u32> = current.ports.iter().map(|p| speed(p)).collect();
    // Each radio on its own: a radio that is off has no devices and is not
    // asked; one whose `wl` fails keeps its last count. A device is counted
    // once per radio, however many of its networks list it.
    let mut wireless: HashSet<String> = HashSet::new();
    let counts: Vec<Option<u32>> = current
        .radios
        .iter()
        .map(|radio| {
            if !radio.on {
                return Some(0);
            }
            let mut macs = HashSet::new();
            for network in &radio.networks {
                macs.extend(r.devices(network)?);
            }
            wireless.extend(macs.iter().cloned());
            Some(macs.len() as u32)
        })
        .collect();
    let wired = wired_devices(&current.bridge, current.wan_port.as_deref(), &wireless);

    let mut s = lock(slow);
    if let Some(layout) = layout {
        if layout != s.layout {
            s.devices.clear();
        }
        s.layout = layout;
    }
    let radios = s.layout.radios.len();
    s.devices.resize(radios, None);
    let now = Instant::now();
    for (slot, count) in s.devices.iter_mut().zip(counts) {
        if let Some(n) = count {
            *slot = Some((n, now));
        }
    }
    s.speeds = speeds;
    if wired.is_some() {
        s.wired = wired;
    }
    if let Some(up) = wan_up {
        s.wan_up = up;
    }
    if let Some(ip) = wan_ip {
        s.wan_ip = ip;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_dev_with_and_without_a_space_after_the_colon() {
        let text = "Inter-|   Receive                                                |  Transmit\n \
                    face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n  \
                    eth0:99480380496 1 0 0 0 0 0 0 169823787378 2 0 0 0 0 0 0\n   \
                    wl0.1: 3661879706 1 0 0 0 0 0 0 9161310041 2 0 0 0 0 0 0\n";
        let dev = net_dev(text);
        assert_eq!(dev.get("eth0"), Some(&(99_480_380_496, 169_823_787_378)));
        assert_eq!(dev.get("wl0.1"), Some(&(3_661_879_706, 9_161_310_041)));
        assert_eq!(dev.len(), 2);
    }

    #[test]
    fn cpu_and_memory() {
        assert_eq!(cpu("cpu  639392 0 2318210 71164246 5842 0 12350 0 0 0\ncpu0 1 2 3 4 5 6 7 8\n"), Some((2_969_952, 74_140_040)));
        assert_eq!(cpu("intr 1 2 3\n"), None);
        let mem = "MemTotal:        2047960 kB\nMemFree:          536096 kB\nMemAvailable:     871020 kB\n";
        assert_eq!(memory(mem), (2_097_111_040, 891_924_480));
    }

    #[test]
    fn bands_and_devices() {
        let mut sixes = 0;
        let names: Vec<String> =
            [("1", "wl0"), ("4", "wl1"), ("4", "wl2"), ("2", "wl3")].iter().map(|(b, r)| band_name(b, &mut sixes, r)).collect();
        assert_eq!(names, ["5 GHz", "6 GHz-1", "6 GHz-2", "2.4 GHz"]);
        assert_eq!(assoc_macs("assoclist AA:BB:CC:DD:EE:01\nassoclist AA:BB:CC:DD:EE:02\n"), ["aa:bb:cc:dd:ee:01", "aa:bb:cc:dd:ee:02"]);
        assert!(assoc_macs("").is_empty());
    }

    #[test]
    fn wired_devices_from_the_neighbour_and_bridge_tables() {
        let arp = "IP address       HW type     Flags       HW address            Mask     Device\n\
                   10.10.10.4       0x1         0x2         bc:24:11:00:00:04     *        br0\n\
                   10.10.10.9       0x1         0x0         00:00:00:00:00:00     *        br0\n\
                   10.10.10.50      0x1         0x2         AA:BB:CC:00:00:50     *        br0\n\
                   192.168.8.1      0x1         0x2         11:22:33:44:55:66     *        vlan4094\n";
        let seen = neighbours(arp, "br0");
        assert_eq!(seen.len(), 2, "complete entries on the bridge only: {seen:?}");
        assert!(seen.contains("aa:bb:cc:00:00:50"));
        // One remote address on port 1, the bridge's own on port 2 (left out),
        // one remote on port 0x105 (high byte 1).
        let mut table = vec![0u8; 48];
        table[..6].copy_from_slice(&[0xbc, 0x24, 0x11, 0, 0, 7]);
        table[6] = 1;
        table[16..22].copy_from_slice(&[0xcc, 0x28, 0xaa, 0xfb, 0x50, 0x20]);
        table[22] = 2;
        table[23] = 1;
        table[32..38].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        table[38] = 5;
        table[44] = 1;
        assert_eq!(fdb_entries(&table), [("bc:24:11:00:00:07".to_string(), 1), ("01:02:03:04:05:06".to_string(), 0x105)]);
    }

    #[test]
    fn strings_are_escaped() {
        assert_eq!(quote("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000a\"");
    }

    #[test]
    fn heads_and_keeping_the_connection() {
        assert_eq!(head_end(b"GET / HTTP/1.1\r\nHost: r\r\n\r\nGET", 0), Some((23, 27)));
        assert_eq!(head_end(b"GET / HTTP/1.0\n\n", 0), Some((14, 16)));
        assert_eq!(head_end(b"GET / HTTP/1.1\r\nHost: r\r\n", 0), None);
        assert!(keep_alive("GET / HTTP/1.1\r\nHost: r"));
        assert!(!keep_alive("GET / HTTP/1.1\r\nConnection: close"));
        assert!(!keep_alive("GET / HTTP/1.0"));
        assert!(keep_alive("GET / HTTP/1.0\r\nConnection: Keep-Alive"));
        assert!(!keep_alive("POST / HTTP/1.1\r\nContent-Length: 5"), "a body is not read, so the connection goes");
    }

    /// Many requests over one connection, pipelined ones too, and a client
    /// past the limit closed at once.
    #[test]
    fn one_connection_serves_request_after_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let slow = Arc::new(Mutex::new(Slow::default()));
        thread::spawn(move || serve(&listener, &slow));
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut answers = 0;
        let mut got = Vec::new();
        stream.write_all(b"GET / HTTP/1.1\r\nHost: r\r\n\r\nGET / HTTP/1.1\r\nHost: r\r\n\r\n").unwrap();
        stream.write_all(b"GET / HTTP/1.1\r\nHost: r\r\nConnection: close\r\n\r\n").unwrap();
        let mut chunk = [0u8; 4096];
        while let Ok(n) = stream.read(&mut chunk) {
            if n == 0 {
                break;
            }
            got.extend_from_slice(&chunk[..n]);
        }
        let text = String::from_utf8_lossy(&got);
        answers += text.matches("HTTP/1.1 200 OK").count();
        assert_eq!(answers, 3, "{text}");
        assert!(text.contains("Connection: keep-alive") && text.ends_with("}\n"));
        // Four clients holding their connections; a fifth is closed unread.
        let held: Vec<TcpStream> = (0..MAX_CLIENTS).map(|_| TcpStream::connect(addr).unwrap()).collect();
        thread::sleep(Duration::from_millis(200));
        let mut fifth = TcpStream::connect(addr).unwrap();
        fifth.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let _ = fifth.write_all(b"GET / HTTP/1.1\r\n\r\n");
        assert_eq!(fifth.read(&mut chunk).unwrap_or(0), 0);
        drop(held);
    }

    #[test]
    fn a_run_over_its_limit_is_killed() {
        let mut r = Runner::default();
        let started = Instant::now();
        assert_eq!(r.run_within("sleep", &["10"], Duration::from_millis(200)), None);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(r.stuck.is_none(), "a killed sleep is reaped, not kept");
        assert_eq!(r.run_within("echo", &["hi"], Duration::from_secs(2)).as_deref(), Some("hi\n"));
        assert_eq!(r.run_within("no-such-tool-here", &[], Duration::from_secs(2)), None);
    }

    #[test]
    fn the_answer_is_json() {
        let slow = Slow {
            layout: Layout {
                model: "GT-BE98_PRO".into(),
                wan_if: "lo".into(),
                wan_port: Some("eth-none".into()),
                bridge: "br0".into(),
                ports: vec!["eth-none".into()],
                radios: vec![Radio { name: "5 GHz".into(), networks: vec!["wl0".into(), "wl0.1".into()], on: true }],
            },
            wan_up: true,
            wan_ip: "192.0.2.1".into(),
            devices: vec![Some((3, Instant::now()))],
            wired: Some(11),
            speeds: vec![10_000],
        };
        let text = json(&slow);
        assert!(text.starts_with("{\"model\":\"GT-BE98_PRO\","));
        assert!(text.contains("\"bands\":[{\"name\":\"5 GHz\",\"on\":true,\"devices\":3,"));
        assert!(text.contains("\"ports\":[{\"name\":\"eth-none\",\"up\":false,\"mbps\":0,\"wan\":true,\"rx\":0,\"tx\":0}],\"wired_devices\":11,"));
        assert!(text.ends_with("\"devices_age\":0}\n"));
    }
}
