//! TrueNAS over JSON-RPC 2.0 on a WebSocket. Not REST: TrueNAS never wired
//! role-based access control into its REST framework, so a read-only key is
//! refused there regardless of configuration.

use std::collections::{BTreeMap, HashMap};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use serde_json::{json, Value};
use tungstenite::{Message, WebSocket};

use super::{lock, Disk, NasData, Nic, Pool, TRUENAS_TIMEOUT};
use crate::config::Config;
use crate::json::{list, num, text, truthy};
use crate::net;
use crate::state::Source;

/// Disks, interfaces and their identifiers change rarely (a disk swapped, a
/// NIC added), but they do change, and the socket can stay up for weeks.
const DISCOVER_EVERY: i64 = 60; // x TRUENAS_INTERVAL = every 5 minutes

/// Capacity and health move on the scale of hours.
const POOLS_EVERY: i64 = 6; // x TRUENAS_INTERVAL = every 30s

type Socket = WebSocket<StreamOwned<ClientConnection, TcpStream>>;

enum Fail {
    /// TrueNAS answered, with an error: the connection itself is fine.
    Rpc(String),
    /// A timeout or a dropped connection: the socket may be no good.
    Link(String),
}

impl From<Fail> for String {
    fn from(f: Fail) -> String {
        match f {
            Fail::Rpc(s) | Fail::Link(s) => s,
        }
    }
}

struct NicInfo {
    role: String,
    media: String,
}

/// Holds one authenticated WebSocket open and polls over it. Reconnects
/// after any failure except a method error.
pub struct TrueNas {
    config: Arc<Config>,
    tls: Arc<ClientConfig>,
    ws: Option<Socket>,
    counter: u64,
    /// When the current poll started, for "timed out after Ns".
    started: Instant,
    identifiers: HashMap<String, Vec<String>>,
    nics: HashMap<String, NicInfo>,
    disk_sizes: HashMap<String, f64>,
    pools: Vec<Value>,
    /// Pool name -> (used, used + available) bytes.
    usable: HashMap<String, (f64, f64)>,
    pool_countdown: i64,
    discover_countdown: i64,
}

impl TrueNas {
    pub fn new(config: Arc<Config>) -> Self {
        TrueNas {
            config,
            // Verification off: the NAS certificate is self-signed.
            tls: net::self_signed_tls(),
            ws: None,
            counter: 0,
            started: Instant::now(),
            identifiers: HashMap::new(),
            nics: HashMap::new(),
            disk_sizes: HashMap::new(),
            pools: Vec::new(),
            usable: HashMap::new(),
            pool_countdown: 0,
            discover_countdown: 0,
        }
    }

    fn link_error(&self, err: tungstenite::Error) -> Fail {
        Fail::Link(match err {
            tungstenite::Error::Io(e) if net::is_timeout(&e) => net::timed_out(self.started),
            tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
                "websocket closed".into()
            }
            other => other.to_string(),
        })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, Fail> {
        self.counter += 1;
        let id = self.counter;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let Some(ws) = self.ws.as_mut() else {
            return Err(Fail::Link("not connected".into()));
        };
        let sent = ws.send(Message::text(request.to_string()));
        if let Err(e) = sent {
            return Err(self.link_error(e));
        }
        loop {
            let ws = self.ws.as_mut().ok_or(Fail::Link("not connected".into()))?;
            let message = match ws.read() {
                Ok(m) => m,
                Err(e) => return Err(self.link_error(e)),
            };
            let payload: Value = match message {
                Message::Text(t) => {
                    serde_json::from_str(t.as_str()).map_err(|e| Fail::Link(format!("bad reply: {e}")))?
                }
                // Pings are answered by tungstenite itself.
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
                Message::Binary(_) => return Err(Fail::Link("websocket closed (binary)".into())),
                Message::Close(_) => return Err(Fail::Link("websocket closed".into())),
            };
            if payload.get("id").and_then(Value::as_u64) != Some(id) {
                continue; // a server notification; not ours
            }
            if let Some(error) = payload.get("error") {
                let mut text = error.to_string();
                text.truncate(text.char_indices().nth(200).map_or(text.len(), |(i, _)| i));
                return Err(Fail::Rpc(text));
            }
            return Ok(payload.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn connect(&mut self) -> Result<(), Fail> {
        // A reconnect must re-read the pools, not reuse a stale list.
        self.pool_countdown = 0;
        let host = self.config.truenas_host.clone();
        let (name, port) = match host.rsplit_once(':') {
            Some((name, port)) if port.parse::<u16>().is_ok() => (name.to_string(), port.parse().unwrap()),
            _ => (host.clone(), 443),
        };
        let link = |e: std::io::Error| {
            Fail::Link(if net::is_timeout(&e) { net::timed_out(self.started) } else { e.to_string() })
        };
        let address = (name.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| Fail::Link(format!("cannot resolve {name}: {e}")))?
            .next()
            .ok_or_else(|| Fail::Link(format!("cannot resolve {name}")))?;
        // Bounded like every other call (spec §7): connect, and then each
        // read and write.
        let tcp = TcpStream::connect_timeout(&address, TRUENAS_TIMEOUT).map_err(link)?;
        tcp.set_read_timeout(Some(TRUENAS_TIMEOUT)).map_err(link)?;
        tcp.set_write_timeout(Some(TRUENAS_TIMEOUT)).map_err(link)?;
        let _ = tcp.set_nodelay(true);
        let server = ServerName::try_from(name.clone()).map_err(|e| Fail::Link(e.to_string()))?;
        let tls = ClientConnection::new(self.tls.clone(), server).map_err(|e| Fail::Link(e.to_string()))?;
        let url = format!("wss://{host}/api/current");
        let ws = match tungstenite::client(url.as_str(), StreamOwned::new(tls, tcp)) {
            Ok((ws, _)) => ws,
            Err(tungstenite::HandshakeError::Failure(e)) => return Err(self.link_error(e)),
            Err(tungstenite::HandshakeError::Interrupted(_)) => {
                return Err(Fail::Link(net::timed_out(self.started)))
            }
        };
        self.ws = Some(ws);
        self.counter = 0;
        // poll() reconnects only when there is no socket, so one left open
        // after a failed login or a timed-out discovery would stay half set
        // up for good: no interfaces, no disks.
        let handshake = self.handshake();
        if handshake.is_err() {
            self.close();
        }
        handshake
    }

    /// Log in and discover what to poll, on a freshly opened socket.
    fn handshake(&mut self) -> Result<(), Fail> {
        let key = self.config.truenas_api_key.clone();
        if self.call("auth.login_with_api_key", json!([key]))? != Value::Bool(true) {
            return Err(Fail::Link("api key rejected".into()));
        }
        self.discover()
    }

    /// Find the disks and interfaces to poll, and what each one is.
    fn discover(&mut self) -> Result<(), Fail> {
        self.discover_countdown = DISCOVER_EVERY;
        // Identifiers are discovered, never invented. Disk identifiers are
        // full descriptive strings ("sdb | Type: HDD | Model: … | Serial: …");
        // a bare device name returns no series and no error.
        let graphs = self.call("reporting.netdata_graphs", json!([]))?;
        self.identifiers = graphs
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|g| {
                let name = text(g, "name").filter(|n| *n == "disk" || *n == "interface")?;
                let ids = list(g, "identifiers").iter().filter_map(|i| i.as_str().map(str::to_string));
                Some((name.to_string(), ids.collect()))
            })
            .collect();

        // Interface roles, derived rather than hardcoded by name. Proxmox
        // stamps BC:24:11 on the virtio NICs it creates; a passed-through card
        // reports a real media subtype where virtio reports "Unknown Other".
        let mut nics = HashMap::new();
        for iface in self.call("interface.query", json!([]))?.as_array().into_iter().flatten() {
            let state = iface.get("state").cloned().unwrap_or_default();
            let mac = text(&state, "hardware_link_address").unwrap_or("").to_uppercase();
            let media = text(&state, "active_media_subtype").unwrap_or("").to_string();
            let role = if mac.starts_with("BC:24:11") {
                "virtual"
            } else if !media.is_empty() && !media.eq_ignore_ascii_case("unknown other") {
                "physical"
            } else {
                "unknown"
            };
            if let Some(name) = text(iface, "name") {
                nics.insert(name.to_string(), NicInfo { role: role.into(), media });
            }
        }
        self.nics = nics;

        // Disk capacities, joined onto the throughput rows by device name.
        self.disk_sizes = self
            .call("disk.query", json!([]))?
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| Some((text(d, "name")?.to_string(), num(d, "size")?)))
            .collect();
        Ok(())
    }

    pub fn poll(&mut self, source: &Mutex<Source<NasData>>) -> Result<(), String> {
        self.started = Instant::now();
        if self.ws.is_none() {
            self.connect()?;
        }
        match self.read(source) {
            Ok(()) => Ok(()),
            // A method error is left alone: reconnecting would not help, and
            // each login is an audit entry on the NAS.
            Err(Fail::Rpc(e)) => Err(e),
            Err(Fail::Link(e)) => {
                self.close();
                Err(e)
            }
        }
    }

    fn read(&mut self, source: &Mutex<Source<NasData>>) -> Result<(), Fail> {
        self.discover_countdown -= 1;
        if self.discover_countdown <= 0 {
            match self.discover() {
                // The tables from the last discovery still hold; the next
                // try is in DISCOVER_EVERY polls.
                Ok(()) | Err(Fail::Rpc(_)) => {}
                Err(link) => return Err(link),
            }
        }

        let mut targets = Vec::new();
        for name in ["interface", "disk"] {
            for id in self.identifiers.get(name).into_iter().flatten() {
                targets.push(json!({"name": name, "identifier": id}));
            }
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
        let series = self.call(
            "reporting.netdata_get_data",
            json!([targets, {"start": now - 10, "end": now}]),
        )?;

        self.pool_countdown -= 1;
        if self.pool_countdown <= 0 || self.pools.is_empty() {
            let pools = self.call("pool.query", json!([]))?;
            self.pools = pools.as_array().cloned().unwrap_or_default();
            self.usable = self.usable_space()?;
            self.pool_countdown = POOLS_EVERY;
        }

        let mut interfaces: Vec<(String, Nic)> = Vec::new();
        let mut disks = BTreeMap::new();
        for entry in series.as_array().into_iter().flatten() {
            let Some((a, b)) = last_sample(entry.get("data")) else { continue };
            // "sdb | Type: HDD | …" -> "sdb"
            let identifier = text(entry, "identifier").unwrap_or("");
            let label = identifier.split('|').next().unwrap_or("").trim().to_string();
            if text(entry, "name") == Some("interface") {
                let info = self.nics.get(&label);
                // netdata reports kilobits/s; normalise to bytes/sec.
                let nic = Nic {
                    rx_bps: a * 125.0,
                    tx_bps: b * 125.0,
                    role: info.map(|i| i.role.clone()),
                    media: info.map(|i| i.media.clone()).unwrap_or_default(),
                };
                match interfaces.iter_mut().find(|(n, _)| *n == label) {
                    Some(slot) => slot.1 = nic,
                    None => interfaces.push((label, nic)),
                }
            } else {
                // netdata reports kibibytes/s; normalise to bytes/sec.
                let size = self.disk_sizes.get(&label).copied();
                disks.insert(label, Disk { read_bps: a * 1024.0, write_bps: b * 1024.0, size });
            }
        }

        let pools = self
            .pools
            .iter()
            .rev()
            .map(|p| {
                let name = text(p, "name").unwrap_or("?").to_string();
                let usable = self.usable.get(&name);
                Pool {
                    status: text(p, "status").map(str::to_string),
                    healthy: truthy(p.get("healthy")),
                    size: num(p, "size"),
                    allocated: num(p, "allocated"),
                    used_bytes: usable.map(|u| u.0),
                    usable: usable.map(|u| u.1),
                    name,
                }
            })
            .collect();

        let mut source = lock(source);
        for (name, nic) in &interfaces {
            source.push(&format!("if.{name}.rx"), Some(nic.rx_bps));
            source.push(&format!("if.{name}.tx"), Some(nic.tx_bps));
        }
        source.succeed(NasData { interfaces, disks: disks.into_iter().collect(), pools });
        Ok(())
    }

    /// Pool name -> (used, used + available) bytes, from root datasets.
    ///
    /// zpool's size counts parity, so a RAIDZ2 pool's "size" is about twice
    /// what it can store (CompoundZ2FS: 58.2 TiB raw, 28.1 TiB usable); its
    /// root dataset's used + available is what TrueNAS's Storage page shows.
    /// A method error leaves this empty, and the card falls back to raw.
    fn usable_space(&mut self) -> Result<HashMap<String, (f64, f64)>, Fail> {
        let names: Vec<String> = self.pools.iter().filter_map(|p| text(p, "name").map(str::to_string)).collect();
        if names.is_empty() {
            return Ok(HashMap::new());
        }
        let datasets = match self.call(
            "pool.dataset.query",
            json!([[["id", "in", names]], {"extra": {"retrieve_children": false}}]),
        ) {
            Ok(d) => d,
            Err(Fail::Rpc(_)) => return Ok(HashMap::new()),
            Err(link) => return Err(link),
        };
        let parsed = |d: &Value, key: &str| d.get(key)?.get("parsed")?.as_u64().map(|v| v as f64);
        Ok(datasets
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| {
                let (used, available) = (parsed(d, "used")?, parsed(d, "available")?);
                Some((text(d, "id")?.to_string(), (used, used + available)))
            })
            .collect())
    }

    fn close(&mut self) {
        if let Some(mut ws) = self.ws.take() {
            let _ = ws.close(None);
            let _ = ws.flush();
        }
    }
}

/// The values of the most recent [time, a, b] row that has both.
fn last_sample(data: Option<&Value>) -> Option<(f64, f64)> {
    data?.as_array()?.iter().rev().find_map(|row| {
        let row = row.as_array()?;
        if row.len() < 3 {
            return None;
        }
        Some((row[1].as_f64()?, row[2].as_f64()?))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_complete_row() {
        let data = json!([[1, 1.0, 2.0], [2, 3.0, 4.0], [3, null, 5.0]]);
        assert_eq!(last_sample(Some(&data)), Some((3.0, 4.0)));
        assert_eq!(last_sample(Some(&json!([]))), None);
        assert_eq!(last_sample(None), None);
    }
}
