//! pvestats-pve: a Proxmox VE host's own counters as JSON over HTTP, read
//! straight from the kernel, in place of asking Proxmox's API (pveproxy)
//! for them.
//!
//! A request reads only files any user may read. For the host:
//! /proc/stat, /proc/meminfo, /proc/loadavg and /proc/uptime. For each
//! running VM, from its cgroup (/sys/fs/cgroup/qemu.slice/<vmid>.scope):
//! its CPU time and the host memory it holds; its QEMU process's start; and
//! the counters of its network devices (tap<vmid>i<n>). Nothing is run,
//! nothing logged and nothing written, and no VM is asked anything: QEMU's
//! control socket, which Proxmox itself uses for backups and migrations, is
//! left alone. So there are no disk counters for VMs (those come from that
//! socket), and a VM's memory is what it holds of the host's, not what its
//! guest uses.
//!
//! Each client keeps one connection and asks over it again and again (HTTP/1.1
//! keep-alive). At most 4 clients at once; a connection with no request for
//! 30 seconds is closed. Answers are at least 100 ms apart across all of
//! them, each request's head is given at most 3 seconds once it has begun,
//! and what a request asks is never looked at. The server is the router
//! agent's (router/agent), as audited there.
//!
//!     pvestats-pve --listen 10.10.10.100:9838
//!     pvestats-pve --once      the JSON once, for trying it by hand

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::process::exit;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

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
/// Where systemd puts each running VM's processes on Proxmox VE.
const VMS: &str = "/sys/fs/cgroup/qemu.slice";
/// Kernel clock ticks a second, for a process's start: USER_HZ, which is 100
/// on every architecture Proxmox runs on.
const TICKS: f64 = 100.0;

/// A poisoned lock still holds good data.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--once" => print!("{}", json()),
        [flag, addr] if flag == "--listen" => {
            // A line each for the journal, which otherwise shows only the
            // exit status.
            let Ok(addr) = addr.parse::<SocketAddr>() else {
                eprintln!("pvestats-pve: not an address and port: {addr}");
                exit(2);
            };
            let listener = TcpListener::bind(addr).unwrap_or_else(|e| {
                eprintln!("pvestats-pve: cannot listen on {addr}: {e}");
                exit(1);
            });
            serve(&listener);
        }
        _ => {
            eprintln!("usage: pvestats-pve --listen ADDR:PORT | --once");
            exit(2);
        }
    }
}

// ------------------------------------------------------------------ serving

fn serve(listener: &TcpListener) -> ! {
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
                let (last, gone) = (Arc::clone(&last), Arc::clone(&active));
                let spawned = thread::Builder::new().stack_size(256 * 1024).spawn(move || {
                    client(stream, &last);
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
fn client(mut stream: TcpStream, last: &Mutex<Option<Instant>>) {
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
        let body = json();
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

fn number(path: &str) -> Option<u64> {
    read(path)?.trim().parse().ok()
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

/// The 1, 5 and 15 minute load averages, as written ("0.52 0.58 0.59").
fn load(text: &str) -> Option<[&str; 3]> {
    // Finite numbers only: Rust would also read "inf" and "NaN", which are
    // not JSON. The kernel writes neither; this keeps the answer JSON anyway.
    let mut f = text.split_whitespace().filter(|v| v.parse::<f64>().is_ok_and(f64::is_finite));
    Some([f.next()?, f.next()?, f.next()?])
}

/// The CPU time a cgroup has used, in microseconds, from its cpu.stat.
fn usage_usec(text: &str) -> Option<u64> {
    text.lines().find_map(|l| l.strip_prefix("usage_usec ")).and_then(|v| v.trim().parse().ok())
}

/// When a process started, in seconds since the host booted: the 22nd field
/// of /proc/PID/stat, counted after the name, which may hold spaces.
fn started(stat: &str) -> Option<f64> {
    let after = &stat[stat.rfind(')')? + 1..];
    let ticks: u64 = after.split_whitespace().nth(19)?.parse().ok()?;
    Some(ticks as f64 / TICKS)
}

/// The VM a network device belongs to: Proxmox names a VM's devices
/// tap<vmid>i<n>.
fn tap_vmid(name: &str) -> Option<u64> {
    let (vmid, n) = name.strip_prefix("tap")?.split_once('i')?;
    n.parse::<u32>().ok()?;
    vmid.parse().ok()
}

/// One running VM, as the host sees it.
#[derive(Debug, Default, PartialEq)]
struct Guest {
    vmid: u64,
    /// CPU time of every process in the VM's scope (QEMU, its vhost
    /// workers, swtpm if any), in microseconds. Proxmox divides its figure
    /// by the VM's vCPUs, which are in its config: the client has them.
    cpu_usec: Option<u64>,
    /// Everything charged to the VM's cgroup: guest memory QEMU has touched,
    /// page cache, the kernel's own pages for it, and its helpers. Not
    /// swapped-out pages, nor guest memory on hugepages.
    mem: Option<u64>,
    /// Bytes to the VM and from it, as Proxmox counts netin and netout: what
    /// the host sends into its tap devices, and what comes out of them. A
    /// device removed (a NIC unplugged) takes its bytes with it, so a client
    /// takes a fall as a reset, as with Proxmox's own counters.
    netin: u64,
    netout: u64,
    /// Seconds since its QEMU process started.
    uptime: Option<f64>,
}

/// Every running VM, by vmid: those with a cgroup scope.
fn guests(host_uptime: f64) -> Vec<Guest> {
    let Ok(dir) = std::fs::read_dir(VMS) else { return Vec::new() };
    let mut out: Vec<Guest> = dir
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".scope")?.parse::<u64>().ok())
        .map(|vmid| {
            let scope = format!("{VMS}/{vmid}.scope");
            // The QEMU process, not a helper that joined the scope before
            // it (swtpm for a VM with a TPM, virtiofsd): the one named kvm.
            let pid = read(&format!("{scope}/cgroup.procs")).and_then(|t| {
                t.lines()
                    .filter_map(|l| l.trim().parse::<u32>().ok())
                    .find(|p| read(&format!("/proc/{p}/comm")).is_some_and(|c| c.trim() == "kvm"))
            });
            Guest {
                vmid,
                cpu_usec: read(&format!("{scope}/cpu.stat")).and_then(|t| usage_usec(&t)),
                mem: number(&format!("{scope}/memory.current")),
                uptime: pid
                    .and_then(|p| read(&format!("/proc/{p}/stat")))
                    .and_then(|s| started(&s))
                    .map(|s| (host_uptime - s).max(0.0)),
                ..Guest::default()
            }
        })
        .collect();
    out.sort_by_key(|g| g.vmid);
    // Each tap device's counters to its VM. The host transmits what the VM
    // receives.
    for entry in std::fs::read_dir("/sys/class/net").into_iter().flatten().flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
        let Some(vmid) = tap_vmid(&name) else { continue };
        let Some(g) = out.iter_mut().find(|g| g.vmid == vmid) else { continue };
        g.netin += number(&format!("/sys/class/net/{name}/statistics/tx_bytes")).unwrap_or(0);
        g.netout += number(&format!("/sys/class/net/{name}/statistics/rx_bytes")).unwrap_or(0);
    }
    out
}

fn or_null<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or("null".to_string(), |v| v.to_string())
}

fn json() -> String {
    let (busy, total, cores) = read("/proc/stat").and_then(|t| cpu(&t)).unwrap_or((0, 0, 0));
    let (mem_total, mem_available) = read("/proc/meminfo").map(|t| memory(&t)).unwrap_or((0, 0));
    let loadavg = read("/proc/loadavg").unwrap_or_default();
    let load = load(&loadavg).map_or("null".to_string(), |l| format!("[{}]", l.join(",")));
    let uptime = read("/proc/uptime")
        .and_then(|t| t.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
        .filter(|u| u.is_finite())
        .unwrap_or(0.0);
    // A VM a line, so a person or a line tool can read them.
    let guests: Vec<String> = guests(uptime)
        .iter()
        .map(|g| {
            format!(
                "\n{{\"vmid\":{},\"cpu_usec\":{},\"mem\":{},\"netin\":{},\"netout\":{},\"uptime\":{}}}",
                g.vmid,
                or_null(g.cpu_usec),
                or_null(g.mem),
                g.netin,
                g.netout,
                or_null(g.uptime.map(|u| format!("{u:.2}"))),
            )
        })
        .collect();
    format!(
        "{{\"uptime\":{uptime:.2},\"cpus\":{cores},\"cpu\":{{\"busy\":{busy},\"total\":{total}}},\
         \"mem\":{{\"total\":{mem_total},\"available\":{mem_available}}},\"load\":{load},\
         \"guests\":[{}]}}\n",
        guests.join(","),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_figures() {
        let stat = "cpu  639392 0 2318210 71164246 5842 0 12350 0 0 0\ncpu0 1 2 3 4 5 6 7 8\ncpu1 1 2 3 4 5 6 7 8\nintr 1 2\n";
        assert_eq!(cpu(stat), Some((2_969_952, 74_140_040, 2)));
        assert_eq!(cpu("intr 1 2 3\n"), None);
        let mem = "MemTotal:        2047960 kB\nMemFree:          536096 kB\nMemAvailable:     871020 kB\n";
        assert_eq!(memory(mem), (2_097_111_040, 891_924_480));
        assert_eq!(load("0.52 0.58 0.59 2/1203 49012\n"), Some(["0.52", "0.58", "0.59"]));
        assert_eq!(load(""), None);
        assert_eq!(load("inf 0.5 NaN 0.7 0.8\n"), Some(["0.5", "0.7", "0.8"]));
    }

    #[test]
    fn vm_figures() {
        assert_eq!(usage_usec("usage_usec 81239847\nuser_usec 1\nsystem_usec 2\n"), Some(81_239_847));
        assert_eq!(usage_usec("user_usec 1\n"), None);
        // A name with spaces and a parenthesis, as a process may have: the
        // start is counted from the last ')'.
        let stat = "4242 (kvm (x) y) S 1 4242 4242 0 -1 4194560 1 2 3 4 5 6 7 8 20 0 40 0 123456 100 200";
        assert_eq!(started(stat), Some(1234.56));
        assert_eq!(tap_vmid("tap100i0"), Some(100));
        assert_eq!(tap_vmid("tap222i12"), Some(222));
        assert_eq!(tap_vmid("fwpr103p0"), None);
        assert_eq!(tap_vmid("tapx"), None);
        assert_eq!(tap_vmid("tap100ix"), None);
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
        thread::spawn(move || serve(&listener));
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
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
        assert_eq!(text.matches("HTTP/1.1 200 OK").count(), 3, "{text}");
        assert!(text.contains("Connection: keep-alive") && text.ends_with("]}\n"));
        let held: Vec<TcpStream> = (0..MAX_CLIENTS).map(|_| TcpStream::connect(addr).unwrap()).collect();
        thread::sleep(Duration::from_millis(200));
        let mut fifth = TcpStream::connect(addr).unwrap();
        fifth.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let _ = fifth.write_all(b"GET / HTTP/1.1\r\n\r\n");
        assert_eq!(fifth.read(&mut chunk).unwrap_or(0), 0);
        drop(held);
    }

    #[test]
    fn the_answer_is_json() {
        // The build machine has no qemu.slice: the guests are an empty list.
        let text = json();
        assert!(text.starts_with("{\"uptime\":"), "{text}");
        assert!(text.contains("\"cpu\":{\"busy\":") && text.contains("\"load\":["), "{text}");
        assert!(text.ends_with("]}\n"), "{text}");
    }
}
