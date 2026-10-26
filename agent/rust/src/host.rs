//! ml60's own CPU, network, disk, uptime and address, read from inside it,
//! so the dashboard's ml60 card needs nothing from Proxmox.
//!
//! Each request reads kernel files only: CPU, uptime and the network
//! counters. Disk usage and the address change slowly and are read by a
//! thread of their own every 10 seconds (`slow_forever`): a statvfs that
//! hangs (a dying disk, a stuck FUSE mount) then holds up only that thread
//! and those two figures, never a request. The address comes from a UDP
//! socket that is never sent on: connecting it towards the default gateway
//! makes the kernel pick the address ml60 talks from. Counters are totals;
//! the client makes rates.

use std::ffi::CString;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

/// How often disk usage and the address are read again.
const SLOW_EVERY: Duration = Duration::from_secs(10);

/// A file as text, any bytes that are not UTF-8 replaced: a mount point or
/// interface name in another encoding must not blank the whole file.
fn read(path: &str) -> Option<String> {
    std::fs::read(path).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Busy and total jiffies over all cores, from /proc/stat's first line
/// (user nice system idle iowait irq softirq steal; guest time is already
/// inside user and nice), and how many cores there are: one cpuN line each.
pub fn cpu(text: &str) -> Option<(u64, u64, usize)> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let f: Vec<u64> = line.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
    if f.len() < 8 {
        return None;
    }
    let busy = f[0] + f[1] + f[2] + f[5] + f[6] + f[7];
    let cores = text.lines().filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(u8::is_ascii_digit)).count();
    Some((busy, busy + f[3] + f[4], cores))
}

/// The interface the default route leaves by, and its gateway, from
/// /proc/net/route: the lowest-metric route to 0.0.0.0/0 that is up.
/// Addresses there are hex in the host's byte order (little-endian here).
pub fn default_route(text: &str) -> Option<(String, Ipv4Addr)> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            // Iface Destination Gateway Flags RefCnt Use Metric Mask ...
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 8 || f[1] != "00000000" || f[7] != "00000000" {
                return None;
            }
            let flags = u32::from_str_radix(f[3], 16).ok()?;
            let gateway = u32::from_str_radix(f[2], 16).ok()?;
            let metric: u32 = f[6].parse().ok()?;
            // RTF_UP and RTF_GATEWAY.
            (flags & 0x3 == 0x3).then(|| (metric, f[0].to_string(), Ipv4Addr::from(gateway.swap_bytes())))
        })
        .min_by_key(|(metric, ..)| *metric)
        .map(|(_, iface, gateway)| (iface, gateway))
}

/// Bytes received and sent by one interface, from /proc/net/dev. A big
/// first number can follow the colon with no space between them.
pub fn net_bytes(text: &str, iface: &str) -> Option<(u64, u64)> {
    text.lines().find_map(|line| {
        let (name, rest) = line.split_once(':')?;
        if name.trim() != iface {
            return None;
        }
        let f: Vec<u64> = rest.split_whitespace().filter_map(|v| v.parse().ok()).collect();
        (f.len() >= 9).then(|| (f[0], f[8]))
    })
}

/// The mount points of filesystems on a block device (/dev/...), one per
/// device, from /proc/self/mounts: no pseudo filesystems, no network shares
/// (ml60 mounts the NAS's). Spaces and the like come escaped as \040.
pub fn local_mounts(text: &str) -> Vec<String> {
    let mut seen: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 3 || !f[0].starts_with("/dev/") || seen.contains(&f[0]) {
            continue;
        }
        seen.push(f[0]);
        out.push(unescape(f[1]));
    }
    out
}

/// A mount point as the kernel writes it, with \ooo octal escapes.
fn unescape(path: &str) -> String {
    let b = path.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Total and used bytes of the filesystem at `path`, as `df`'s Use% and
/// Proxmox's guest agent (QEMU 9.1 on) count them: used is blocks less free
/// ones, and total is used plus what an ordinary user may still fill, so the
/// blocks ext4 keeps back for root (5% by default) are in neither. Or None.
fn usage(path: &str) -> Option<(u64, u64)> {
    let path = CString::new(path).ok()?;
    // SAFETY: statvfs fills the struct it is given and reads only the path,
    // a NUL-terminated string that outlives the call.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let size = u64::from(s.f_frsize);
    let used = u64::from(s.f_blocks).saturating_sub(u64::from(s.f_bfree)).saturating_mul(size);
    let total = used.saturating_add(u64::from(s.f_bavail).saturating_mul(size));
    (total > 0).then_some((total, used))
}

/// ml60's address on the LAN: the one the kernel would send from towards
/// the gateway. Connecting a UDP socket sends nothing.
fn address(gateway: Ipv4Addr) -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((gateway, 9)).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

/// Disk usage and the address, as last read; null until first read, or for
/// what cannot be.
#[derive(Clone, Debug, Default)]
pub struct Slow {
    pub disk: Value,
    pub ip: Value,
}

/// Disk usage and the address, read now.
pub fn slow() -> Slow {
    // The largest local filesystem, as the dashboard took the largest that
    // Proxmox's guest agent listed.
    let disk = read("/proc/self/mounts")
        .map(|t| local_mounts(&t))
        .unwrap_or_default()
        .iter()
        .filter_map(|m| usage(m))
        .max_by_key(|(total, _)| *total)
        .map_or(Value::Null, |(total, used)| json!({"total": total, "used": used}));
    let ip = read("/proc/net/route")
        .and_then(|t| default_route(&t))
        .and_then(|(_, gateway)| address(gateway))
        .map_or(Value::Null, |ip| Value::String(ip.to_string()));
    Slow { disk, ip }
}

/// Reads disk usage and the address into `slot` every SLOW_EVERY, for ever.
pub fn slow_forever(slot: &Mutex<Slow>) -> ! {
    loop {
        let fresh = slow();
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = fresh;
        thread::sleep(SLOW_EVERY);
    }
}

/// The reading served as "host": CPU, uptime and network read now, disk and
/// address as `slow` last found them; null for whatever cannot be read.
pub fn reading(slow: &Slow) -> Value {
    let (busy, total, cpus) = read("/proc/stat").and_then(|t| cpu(&t)).map_or((None, None, None), |(b, t, n)| (Some(b), Some(t), Some(n)));
    let uptime = read("/proc/uptime")
        .and_then(|t| t.split_whitespace().next()?.parse::<f64>().ok())
        .filter(|u| u.is_finite());
    let net = read("/proc/net/route").and_then(|t| default_route(&t)).and_then(|(iface, _)| {
        let (rx, tx) = read("/proc/net/dev").and_then(|t| net_bytes(&t, &iface))?;
        Some(json!({"iface": iface, "rx": rx, "tx": tx}))
    });
    json!({
        "cpu": {"busy": busy, "total": total},
        "cpus": cpus,
        "uptime": uptime.map(|u| (u * 100.0).round() / 100.0),
        "net": net,
        "disk": slow.disk,
        "ip": slow.ip,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_counts() {
        let stat = "cpu  639392 0 2318210 71164246 5842 0 12350 0 0 0\ncpu0 1 2 3 4 5 6 7 8\ncpu1 1 2 3 4 5 6 7 8\nintr 1\n";
        assert_eq!(cpu(stat), Some((2_969_952, 74_140_040, 2)));
        assert_eq!(cpu("intr 1 2 3\n"), None);
    }

    #[test]
    fn the_default_route() {
        // ml60's shape: the default via 10.10.10.1 on ens18, Docker's bridge
        // and the LAN as plain routes. 0101A8C0 is 192.168.1.1, a second
        // default with a worse metric.
        let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                     ens18\t00000000\t010A0A0A\t0003\t0\t0\t0\t00000000\t0\t0\t0\n\
                     wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
                     docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0\n\
                     ens18\t000A0A0A\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";
        assert_eq!(default_route(route), Some(("ens18".to_string(), Ipv4Addr::new(10, 10, 10, 1))));
        // A default route that is down does not count.
        let down = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
                    ens18\t00000000\t010A0A0A\t0002\t0\t0\t0\t00000000\n";
        assert_eq!(default_route(down), None);
        assert_eq!(default_route(""), None);
    }

    #[test]
    fn interface_bytes() {
        let dev = "Inter-|   Receive |  Transmit\n face |bytes packets|bytes\n  \
                   ens18:99480380496 1 0 0 0 0 0 0 169823787378 2 0 0 0 0 0 0\n \
                   docker0: 5 1 0 0 0 0 0 0 7 2 0 0 0 0 0 0\n";
        assert_eq!(net_bytes(dev, "ens18"), Some((99_480_380_496, 169_823_787_378)));
        assert_eq!(net_bytes(dev, "docker0"), Some((5, 7)));
        assert_eq!(net_bytes(dev, "ens1"), None, "a name is matched whole");
    }

    #[test]
    fn local_filesystems() {
        // ml60's mounts: the NVMe's root and EFI partitions, pseudo
        // filesystems, a NAS share, and the root bound a second time.
        let mounts = "/dev/nvme0n1p2 / ext4 rw,relatime 0 0\n\
                      proc /proc proc rw 0 0\n\
                      tmpfs /run tmpfs rw 0 0\n\
                      /dev/nvme0n1p1 /boot/efi vfat rw 0 0\n\
                      10.10.10.2:/mnt/WorkFiles /mnt/work nfs4 rw 0 0\n\
                      /dev/nvme0n1p2 /var/lib/docker ext4 rw 0 0\n\
                      /dev/sdb1 /mnt/my\\040disk ext4 rw 0 0\n";
        assert_eq!(local_mounts(mounts), ["/", "/boot/efi", "/mnt/my disk"]);
        assert_eq!(unescape("a\\134b\\011c"), "a\\b\tc");
        assert_eq!(unescape("trailing\\04"), "trailing\\04", "a cut escape is left as it is");
    }

    #[test]
    fn the_reading_is_whole() {
        // The build machine's own figures: every field present and the
        // shapes right, whatever the values.
        let r = reading(&slow());
        assert!(r["cpu"]["busy"].as_u64().unwrap() <= r["cpu"]["total"].as_u64().unwrap(), "{r}");
        assert!(r["cpus"].as_u64().unwrap() >= 1, "{r}");
        assert!(r["uptime"].as_f64().unwrap() > 0.0, "{r}");
        let disk = &r["disk"];
        assert!(disk["used"].as_u64().unwrap() <= disk["total"].as_u64().unwrap(), "{r}");
        assert!(r["net"]["iface"].is_string() && r["ip"].is_string(), "{r}");
    }
}
