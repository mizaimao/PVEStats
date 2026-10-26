//! pvestats-nas: TrueNAS's own counters as JSON over HTTP, for PVEStats'
//! TrueNAS card, in place of asking TrueNAS's API (middlewared), which costs
//! about 25 ms of TrueNAS's CPU a request (measured, nas/README.md).
//!
//! A request reads only files any user may read: /proc/stat, /proc/meminfo
//! and ZFS's arcstats (the ARC, ZFS's cache, which Linux counts as used
//! memory), /proc/uptime, /proc/net/dev, each disk's /sys/block/<disk>/stat,
//! and each pool's state from ZFS's kstat files. What changes slowly comes
//! from a second thread every 30 seconds: which network devices and disks
//! there are, each disk's model and size, each disk's pool (`zpool list -v -H
//! -P`, every five minutes, as it changes only with a disk), each
//! pool's capacity (`zfs get used,available`), which pool holds the system
//! (the one mounted at /), and TrueNAS's LAN address. Each command is killed
//! after 5 seconds, and one that will not die stops any more from starting.
//!
//! Each client keeps one connection and asks over it again and again (HTTP/1.1
//! keep-alive). At most 4 clients at once; a connection with no request for
//! 30 seconds is closed. Answers are at least 100 ms apart across all of
//! them, each request's head is given at most 3 seconds once it has begun,
//! and what a request asks is never looked at. Nothing is logged and nothing
//! written. The server and the command runner are the router agent's
//! (router/agent), as audited there.
//!
//!     pvestats-nas --listen 10.10.10.2:9839
//!     pvestats-nas --once      the JSON once, for trying it by hand

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::Path;
use std::process::{exit, Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// How often the slow-changing things are read again.
const SLOW_EVERY: Duration = Duration::from_secs(30);
/// Which disk is in which pool changes only when a disk does: `zpool` is run
/// every this many rounds (five minutes).
const MEMBERS_EVERY: u32 = 10;
/// Longest a `zpool` or `zfs` run may take before it is killed.
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
/// ZFS's per-pool kstat directories, and its ARC statistics.
const KSTAT: &str = "/proc/spl/kstat/zfs";

/// One disk: its model and size, and the pool it belongs to, if any.
#[derive(Clone, Debug, Default, PartialEq)]
struct DiskInfo {
    name: String,
    model: String,
    size: Option<u64>,
    pool: Option<String>,
}

/// One pool's capacity, from its root dataset: what it holds and what is
/// still free for it, as TrueNAS shows "used of usable".
#[derive(Clone, Debug, Default, PartialEq)]
struct PoolInfo {
    name: String,
    used: Option<u64>,
    available: Option<u64>,
    /// The pool TrueNAS itself runs from.
    boot: bool,
}

/// What the slow thread last found.
#[derive(Clone, Debug, Default)]
struct Slow {
    /// Network devices with a device behind them (a NIC, real or virtio),
    /// each with its driver; bridges, docker0 and veths have none.
    nics: Vec<(String, String)>,
    disks: Vec<DiskInfo>,
    pools: Vec<PoolInfo>,
    ip: Option<String>,
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
            // A line each for the journal, which otherwise shows only the
            // exit status.
            let Ok(addr) = addr.parse::<SocketAddr>() else {
                eprintln!("pvestats-nas: not an address and port: {addr}");
                exit(2);
            };
            let listener = TcpListener::bind(addr).unwrap_or_else(|e| {
                eprintln!("pvestats-nas: cannot listen on {addr}: {e}");
                exit(1);
            });
            let slow = Arc::new(Mutex::new(Slow::default()));
            let theirs = Arc::clone(&slow);
            thread::spawn(move || refresh_forever(&theirs));
            serve(&listener, &slow);
        }
        _ => {
            eprintln!("usage: pvestats-nas --listen ADDR:PORT | --once");
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

/// A file as text, any bytes that are not UTF-8 replaced.
fn read(path: &str) -> Option<String> {
    std::fs::read(path).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Busy and total jiffies over all cores, from /proc/stat's first line:
/// user nice system idle iowait irq softirq steal. And how many cores there
/// are: one cpuN line each.
fn cpu(text: &str) -> Option<(u64, u64, usize)> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let f: Vec<u64> = line.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
    if f.len() < 8 {
        return None;
    }
    let busy = f[0] + f[1] + f[2] + f[5] + f[6] + f[7];
    let cores = text.lines().filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(u8::is_ascii_digit)).count();
    Some((busy, busy + f[3] + f[4], cores))
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

/// The ARC's size in bytes, from ZFS's arcstats ("size 4 12978739664").
fn arc_size(text: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.len() == 3 && f[0] == "size").then(|| f[2].parse().ok()).flatten()
    })
}

/// rx and tx bytes since boot for one interface in /proc/net/dev.
fn net_bytes(text: &str, iface: &str) -> Option<(u64, u64)> {
    text.lines().find_map(|line| {
        let (name, rest) = line.split_once(':')?;
        if name.trim() != iface {
            return None;
        }
        let f: Vec<u64> = rest.split_whitespace().filter_map(|v| v.parse().ok()).collect();
        (f.len() >= 9).then(|| (f[0], f[8]))
    })
}

/// Bytes read and written since boot, from a disk's /sys/block/<disk>/stat:
/// sectors read is the third field and sectors written the seventh, in the
/// kernel's 512-byte sectors whatever the disk's own.
fn disk_bytes(text: &str) -> Option<(u64, u64)> {
    let f: Vec<u64> = text.split_whitespace().filter_map(|v| v.parse().ok()).collect();
    (f.len() >= 7).then(|| (f[2] * 512, f[6] * 512))
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

fn or_null<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or("null".to_string(), |v| v.to_string())
}

fn json(slow: &Slow) -> String {
    let (busy, total, cores) = read("/proc/stat").and_then(|t| cpu(&t)).unwrap_or((0, 0, 0));
    let (mem_total, mem_available) = read("/proc/meminfo").map(|t| memory(&t)).unwrap_or((0, 0));
    let arc = read(&format!("{KSTAT}/arcstats")).and_then(|t| arc_size(&t));
    let uptime = read("/proc/uptime")
        .and_then(|t| t.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
        .filter(|u| u.is_finite())
        .unwrap_or(0.0);
    let dev = read("/proc/net/dev").unwrap_or_default();
    let nics: Vec<String> = slow
        .nics
        .iter()
        .filter_map(|(name, driver)| {
            let (rx, tx) = net_bytes(&dev, name)?;
            Some(format!("{{\"name\":{},\"driver\":{},\"rx\":{rx},\"tx\":{tx}}}", quote(name), quote(driver)))
        })
        .collect();
    let disks: Vec<String> = slow
        .disks
        .iter()
        .map(|d| {
            let (read_b, write_b) = read(&format!("/sys/block/{}/stat", d.name)).and_then(|t| disk_bytes(&t)).unzip();
            format!(
                "{{\"name\":{},\"model\":{},\"size\":{},\"pool\":{},\"read\":{},\"write\":{}}}",
                quote(&d.name),
                quote(&d.model),
                or_null(d.size),
                d.pool.as_deref().map_or("null".to_string(), quote),
                or_null(read_b),
                or_null(write_b),
            )
        })
        .collect();
    let pools: Vec<String> = slow
        .pools
        .iter()
        .map(|p| {
            // The state per request: it is a one-word kstat file, and a pool
            // that degrades should show at once.
            let state = read(&format!("{KSTAT}/{}/state", p.name)).map(|s| s.trim().to_string());
            format!(
                "{{\"name\":{},\"state\":{},\"used\":{},\"available\":{},\"boot\":{}}}",
                quote(&p.name),
                state.as_deref().map_or("null".to_string(), quote),
                or_null(p.used),
                or_null(p.available),
                p.boot,
            )
        })
        .collect();
    format!(
        "{{\"uptime\":{uptime:.2},\"cpus\":{cores},\"cpu\":{{\"busy\":{busy},\"total\":{total}}},\
         \"mem\":{{\"total\":{mem_total},\"available\":{mem_available},\"arc\":{}}},\"ip\":{},\
         \"nics\":[{}],\"disks\":[{}],\"pools\":[{}]}}\n",
        or_null(arc),
        slow.ip.as_deref().map_or("null".to_string(), quote),
        nics.join(","),
        disks.join(",\n"),
        pools.join(","),
    )
}

// ------------------------------------------------------- the slow thread

/// Runs `zpool` and `zfs`, at most one stuck at a time.
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
        // Read as it comes, so a long answer cannot fill the pipe and stall
        // the tool: the reading thread ends when the tool's output does.
        let mut stdout = child.stdout.take()?;
        let reader = thread::spawn(move || {
            let mut out = String::new();
            stdout.read_to_string(&mut out).ok().map(|_| out)
        });
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let out = reader.join().ok().flatten()?;
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
}

/// Each pool's devices from `zpool list -v -H -P -o name`: a pool on a line
/// of its own, its vdevs and devices indented by a tab beneath it. Only
/// devices given as paths (/dev/...) are kept, as (pool, path).
fn pool_devices(out: &str) -> Vec<(String, String)> {
    let mut pool = String::new();
    let mut found = Vec::new();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix('\t') {
            let field = rest.split('\t').next().unwrap_or("").trim();
            if field.starts_with("/dev/") && !pool.is_empty() {
                found.push((pool.clone(), field.to_string()));
            }
        } else if let Some(name) = line.split('\t').next().filter(|n| !n.is_empty()) {
            pool = name.to_string();
        }
    }
    found
}

/// The disk a block device belongs to, from its path in /sys: a partition
/// sits in its disk's directory (.../block/sdf/sdf2), a whole disk directly
/// under block (.../block/sdb).
fn disk_of(sys_path: &Path) -> Option<String> {
    let name = sys_path.file_name()?.to_str()?;
    let parent = sys_path.parent()?;
    if parent.file_name()?.to_str()? == "block" {
        Some(name.to_string())
    } else {
        Some(parent.file_name()?.to_str()?.to_string())
    }
}

/// used and available for each pool's root dataset, from
/// `zfs get -Hp -o name,property,value used,available POOL...`.
fn pool_space(out: &str) -> Vec<(String, &'static str, u64)> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let property = match *f.get(1)? {
                "used" => "used",
                "available" => "available",
                _ => return None,
            };
            Some((f.first()?.to_string(), property, f.get(2)?.trim().parse().ok()?))
        })
        .collect()
}

/// The pool TrueNAS runs from: the one whose dataset is mounted at /.
fn boot_pool(mounts: &str) -> Option<String> {
    mounts.lines().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.len() >= 3 && f[1] == "/" && f[2] == "zfs").then(|| f[0].split('/').next().unwrap_or("").to_string())
    })
}

/// The gateway of the lowest-metric default route that is up, from
/// /proc/net/route, whose addresses are hex in the host's byte order.
fn gateway(route: &str) -> Option<Ipv4Addr> {
    route
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 8 || f[1] != "00000000" || f[7] != "00000000" {
                return None;
            }
            let flags = u32::from_str_radix(f[3], 16).ok()?;
            let metric: u32 = f[6].parse().ok()?;
            let gateway = Ipv4Addr::from(u32::from_str_radix(f[2], 16).ok()?.swap_bytes());
            // RTF_UP and RTF_GATEWAY.
            (flags & 0x3 == 0x3).then_some((metric, gateway))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, gw)| gw)
}

/// The address TrueNAS talks to the LAN from: a UDP socket connected towards
/// the gateway sends nothing, but the kernel picks its source address.
fn address(gateway: Ipv4Addr) -> Option<String> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((gateway, 9)).ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

/// One round of the slow readings: disk and pool membership too when
/// `with_members` (`zpool`, which changes only when a disk does). What a
/// tool would not tell keeps its last value; the tools run without the lock
/// held.
fn refresh(r: &mut Runner, slow: &Mutex<Slow>, with_members: bool) {
    // Network devices with a device behind them, and its driver.
    let mut nics: Vec<(String, String)> = std::fs::read_dir("/sys/class/net")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let device = format!("/sys/class/net/{name}/device");
            Path::new(&device).exists().then(|| {
                let driver = std::fs::read_link(format!("{device}/driver"))
                    .ok()
                    .and_then(|p| p.file_name().and_then(|f| f.to_str()).map(str::to_string))
                    .unwrap_or_default();
                (name, driver)
            })
        })
        .collect();
    nics.sort();

    // Which disk is in which pool, by the device paths zpool gives.
    let listed = if with_members { r.run("zpool", &["list", "-v", "-H", "-P", "-o", "name"]) } else { None };
    let members: Vec<(String, String)> = listed
        .map(|out| pool_devices(&out))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(pool, path)| {
            let device = std::fs::canonicalize(&path).ok()?;
            let name = device.file_name()?.to_str()?.to_string();
            let sys = std::fs::canonicalize(format!("/sys/class/block/{name}")).ok()?;
            Some((disk_of(&sys)?, pool))
        })
        .collect();
    let mut disks: Vec<DiskInfo> = std::fs::read_dir("/sys/block")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| ["sd", "nvme", "vd"].iter().any(|p| n.starts_with(p)))
        .map(|name| DiskInfo {
            model: read(&format!("/sys/block/{name}/device/model")).map(|m| m.trim().to_string()).unwrap_or_default(),
            size: read(&format!("/sys/block/{name}/size")).and_then(|s| s.trim().parse::<u64>().ok()).map(|s| s * 512),
            pool: members.iter().find(|(d, _)| *d == name).map(|(_, p)| p.clone()),
            name,
        })
        .collect();
    disks.sort_by(|a, b| a.name.cmp(&b.name));

    // Each pool's capacity, and which one TrueNAS runs from.
    let names: Vec<String> = std::fs::read_dir(KSTAT)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("state").exists())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    let boot = read("/proc/self/mounts").and_then(|m| boot_pool(&m));
    let mut args = vec!["get", "-Hp", "-o", "name,property,value", "used,available"];
    args.extend(names.iter().map(String::as_str));
    let space = if names.is_empty() { None } else { r.run("zfs", &args).map(|out| pool_space(&out)) };
    let ip = read("/proc/net/route").and_then(|t| gateway(&t)).and_then(address);

    let mut s = lock(slow);
    let old = std::mem::take(&mut s.pools);
    let mut pools: Vec<PoolInfo> = names
        .iter()
        .map(|name| {
            let last = old.iter().find(|p| p.name == *name);
            let get = |property: &str| match &space {
                Some(space) => space.iter().find(|(n, p, _)| n == name && *p == property).map(|(_, _, v)| *v),
                None => last.and_then(|l| if property == "used" { l.used } else { l.available }),
            };
            PoolInfo { name: name.clone(), used: get("used"), available: get("available"), boot: boot.as_deref() == Some(name) }
        })
        .collect();
    pools.sort_by(|a, b| a.name.cmp(&b.name));
    s.pools = pools;
    s.nics = nics;
    // A round without zpool, or a zpool that did not answer, leaves the
    // disks' pools as they were.
    if members.is_empty() && !s.disks.is_empty() {
        for d in &mut disks {
            d.pool = s.disks.iter().find(|o| o.name == d.name).and_then(|o| o.pool.clone());
        }
    }
    s.disks = disks;
    if ip.is_some() {
        s.ip = ip;
    }
}

fn refresh_forever(slow: &Mutex<Slow>) -> ! {
    let mut runner = Runner::default();
    let mut round: u32 = 0;
    loop {
        // Membership every MEMBERS_EVERY rounds, or until it is first known.
        let with_members = round % MEMBERS_EVERY == 0 || lock(slow).disks.iter().all(|d| d.pool.is_none());
        refresh(&mut runner, slow, with_members);
        round = round.wrapping_add(1);
        thread::sleep(SLOW_EVERY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_figures() {
        let stat = "cpu  639392 0 2318210 71164246 5842 0 12350 0 0 0\ncpu0 1 2 3 4 5 6 7 8\nintr 1\n";
        assert_eq!(cpu(stat), Some((2_969_952, 74_140_040, 1)));
        let mem = "MemTotal:       16379288 kB\nMemAvailable:    1937920 kB\n";
        assert_eq!(memory(mem), (16_772_390_912, 1_984_430_080));
        // arcstats as ZFS writes it: a header, then name type data.
        let arc = "19 1 0x01 147 39984 17544596117 324469777461079\nname                            type data\nhits                            4    230980733\nsize                            4    12978739664\n";
        assert_eq!(arc_size(arc), Some(12_978_739_664));
        assert_eq!(arc_size("name type data\n"), None);
    }

    #[test]
    fn counters() {
        let dev = "Inter-| Receive\n face |bytes\n  ens18:99480380496 1 0 0 0 0 0 0 169823787378 2 0 0 0 0 0 0\n";
        assert_eq!(net_bytes(dev, "ens18"), Some((99_480_380_496, 169_823_787_378)));
        assert_eq!(net_bytes(dev, "ens1"), None);
        let stat = "32746521      860 22655667128 140432036   480696      937 15425088  1463754        0 63271792 142934073        0        0        0        0    26746  1038283";
        assert_eq!(disk_bytes(stat), Some((22_655_667_128 * 512, 15_425_088 * 512)));
    }

    #[test]
    fn pools_and_their_disks() {
        // TrueNAS's own answer, 2026-10-26.
        let out = "CompoundZ2FS\n\traidz2-0\n\t/dev/sdb\n\t/dev/sde\n\t/dev/sdd\n\t/dev/sdc\nTimeMachine8\n\t/dev/sdf2\n\
                   freenas-boot\n\t/dev/disk/by-id/scsi-0QEMU_QEMU_HARDDISK_drive-scsi0-part2\n";
        let found = pool_devices(out);
        assert_eq!(found.len(), 6, "{found:?}");
        assert_eq!(found[0], ("CompoundZ2FS".to_string(), "/dev/sdb".to_string()));
        assert_eq!(found[4], ("TimeMachine8".to_string(), "/dev/sdf2".to_string()));
        assert_eq!(found[5].0, "freenas-boot");
        assert_eq!(disk_of(Path::new("/sys/devices/pci0000:00/0000:00:10.0/host0/target0:0:0/0:0:0:0/block/sdf/sdf2")).as_deref(), Some("sdf"));
        assert_eq!(disk_of(Path::new("/sys/devices/pci0000:00/0000:00:10.0/host0/target0:0:0/0:0:0:0/block/sdb")).as_deref(), Some("sdb"));
        let space = "CompoundZ2FS\tused\t21893142802752\nCompoundZ2FS\tavailable\t8969214574272\nTimeMachine8\tused\t5382667722752\n";
        assert_eq!(pool_space(space), [
            ("CompoundZ2FS".to_string(), "used", 21_893_142_802_752),
            ("CompoundZ2FS".to_string(), "available", 8_969_214_574_272),
            ("TimeMachine8".to_string(), "used", 5_382_667_722_752),
        ]);
        let mounts = "freenas-boot/ROOT/25.10.4 / zfs ro,relatime 0 0\nCompoundZ2FS /mnt/CompoundZ2FS zfs rw 0 0\n";
        assert_eq!(boot_pool(mounts).as_deref(), Some("freenas-boot"));
        assert_eq!(boot_pool("/dev/sda2 / ext4 rw 0 0\n"), None);
    }

    #[test]
    fn the_gateway() {
        let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\n\
                     ens18\t00000000\t010A0A0A\t0003\t0\t0\t0\t00000000\n\
                     docker0\t000010AC\t00000000\t0001\t0\t0\t0\t00FFFFFF\n";
        assert_eq!(gateway(route), Some(Ipv4Addr::new(10, 10, 10, 1)));
        assert_eq!(gateway(""), None);
    }

    #[test]
    fn strings_are_escaped() {
        assert_eq!(quote("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000a\"");
    }

    #[test]
    fn heads_and_keeping_the_connection() {
        assert_eq!(head_end(b"GET / HTTP/1.1\r\nHost: r\r\n\r\nGET", 0), Some((23, 27)));
        assert!(keep_alive("GET / HTTP/1.1\r\nHost: r"));
        assert!(!keep_alive("GET / HTTP/1.1\r\nConnection: close"));
        assert!(!keep_alive("POST / HTTP/1.1\r\nContent-Length: 5"));
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

    /// Many requests over one connection, and the answer is whole JSON.
    #[test]
    fn one_connection_serves_request_after_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let slow = Arc::new(Mutex::new(Slow {
            nics: vec![("lo".into(), "x".into())],
            disks: vec![DiskInfo { name: "sdz".into(), model: "M \"1\"".into(), size: Some(512), pool: Some("tank".into()) }],
            pools: vec![PoolInfo { name: "tank".into(), used: Some(1), available: None, boot: false }],
            ip: Some("192.0.2.1".into()),
        }));
        thread::spawn(move || serve(&listener, &slow));
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        stream.write_all(b"GET / HTTP/1.1\r\n\r\nGET / HTTP/1.1\r\nConnection: close\r\n\r\n").unwrap();
        let mut got = String::new();
        let _ = stream.read_to_string(&mut got);
        assert_eq!(got.matches("HTTP/1.1 200 OK").count(), 2, "{got}");
        assert!(got.contains("\"nics\":[{\"name\":\"lo\",\"driver\":\"x\""), "{got}");
        assert!(got.contains("\"model\":\"M \\\"1\\\"\",\"size\":512,\"pool\":\"tank\""), "{got}");
        assert!(got.contains("\"pools\":[{\"name\":\"tank\",\"state\":null,\"used\":1,\"available\":null,\"boot\":false}]"), "{got}");
        assert!(got.contains("\"ip\":\"192.0.2.1\"") && got.ends_with("]}\n"), "{got}");
    }
}
