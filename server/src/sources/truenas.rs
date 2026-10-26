//! TrueNAS over JSON-RPC 2.0 on a WebSocket, as
//! pvestats/sources/truenas.py. Not REST: TrueNAS never wired role-based
//! access control into its REST framework, so a read-only key is refused
//! there regardless of configuration.

use std::collections::{BTreeMap, HashMap};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use serde_json::{json, Map, Value};
use tungstenite::{Message, WebSocket};

use super::{lock, TRUENAS_TIMEOUT};
use crate::config::Config;
use crate::net;
use crate::state::Source;

/// Disks, interfaces and their identifiers change rarely, but they do, and
/// the socket can stay up for weeks.
const DISCOVER_EVERY: i64 = 60; // x TRUENAS_INTERVAL = every 5 minutes
/// Capacity and health move on the scale of hours.
const POOLS_EVERY: i64 = 6; // x TRUENAS_INTERVAL = every 30 s

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

fn list(v: &Value) -> &[Value] {
    v.as_array().map_or(&[][..], Vec::as_slice)
}

fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Holds one authenticated WebSocket open and polls over it; reconnects
/// after any failure except a method error.
pub struct TrueNas {
    config: Arc<Config>,
    tls: Arc<ClientConfig>,
    ws: Option<Socket>,
    counter: u64,
    started: Instant,
    /// Graph name -> identifiers.
    identifiers: HashMap<String, Vec<String>>,
    /// Interface name -> {role, media, addresses}.
    nics: HashMap<String, Map<String, Value>>,
    disk_sizes: HashMap<String, Value>,
    pools: Vec<Value>,
    /// Pool name -> (used, used + available) bytes.
    usable: HashMap<String, (i64, i64)>,
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
            tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => "websocket closed".into(),
            other => other.to_string(),
        })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, Fail> {
        self.counter += 1;
        let id = self.counter;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let Some(ws) = self.ws.as_mut() else { return Err(Fail::Link("not connected".into())) };
        if let Err(e) = ws.send(Message::text(request.to_string())) {
            return Err(self.link_error(e));
        }
        loop {
            let ws = self.ws.as_mut().ok_or(Fail::Link("not connected".into()))?;
            let message = match ws.read() {
                Ok(m) => m,
                Err(e) => return Err(self.link_error(e)),
            };
            let payload: Value = match message {
                Message::Text(t) => serde_json::from_str(t.as_str()).map_err(|e| Fail::Link(format!("bad reply: {e}")))?,
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

    fn close(&mut self) {
        if let Some(mut ws) = self.ws.take() {
            let _ = ws.close(None);
        }
    }

    fn connect(&mut self) -> Result<(), Fail> {
        // A reconnect must re-read the pools, not reuse a stale list.
        self.pool_countdown = 0;
        let host = self.config.truenas_host.clone();
        let (name, port) = match host.rsplit_once(':') {
            Some((name, port)) if port.parse::<u16>().is_ok() => (name.to_string(), port.parse().unwrap_or(443)),
            _ => (host.clone(), 443),
        };
        let started = self.started;
        let link = |e: std::io::Error| Fail::Link(if net::is_timeout(&e) { net::timed_out(started) } else { e.to_string() });
        let addresses: Vec<_> =
            (name.as_str(), port).to_socket_addrs().map_err(|e| Fail::Link(format!("cannot resolve {name}: {e}")))?.collect();
        // Each address in turn, as aiohttp tried them: an IPv6 one that does
        // not answer must not stop the IPv4 one being tried. Bounded like
        // every other call (spec §7): connect, then each read and write.
        let mut tcp = Err(Fail::Link(format!("cannot resolve {name}")));
        for address in &addresses {
            tcp = TcpStream::connect_timeout(address, TRUENAS_TIMEOUT).map_err(link);
            if tcp.is_ok() {
                break;
            }
        }
        let tcp = tcp?;
        tcp.set_read_timeout(Some(TRUENAS_TIMEOUT)).map_err(link)?;
        tcp.set_write_timeout(Some(TRUENAS_TIMEOUT)).map_err(link)?;
        let _ = tcp.set_nodelay(true);
        let server = ServerName::try_from(name.clone()).map_err(|e| Fail::Link(e.to_string()))?;
        let tls = ClientConnection::new(self.tls.clone(), server).map_err(|e| Fail::Link(e.to_string()))?;
        let url = format!("wss://{host}/api/current");
        let ws = match tungstenite::client(url.as_str(), StreamOwned::new(tls, tcp)) {
            Ok((ws, _)) => ws,
            Err(tungstenite::HandshakeError::Failure(e)) => return Err(self.link_error(e)),
            Err(tungstenite::HandshakeError::Interrupted(_)) => return Err(Fail::Link(net::timed_out(self.started))),
        };
        self.ws = Some(ws);
        self.counter = 0;
        // A socket left open after a failed login or a timed-out discovery
        // would stay half set up for good.
        let handshake = self.handshake();
        if handshake.is_err() {
            self.close();
        }
        handshake
    }

    fn handshake(&mut self) -> Result<(), Fail> {
        let key = self.config.truenas_api_key.clone();
        if self.call("auth.login_with_api_key", json!([key]))? != Value::Bool(true) {
            return Err(Fail::Link("api key rejected".into()));
        }
        self.discover()
    }

    /// Finds the disks and interfaces to poll, and what each one is. Each
    /// table is built aside and swapped in whole, so a failed call leaves the
    /// old one.
    fn discover(&mut self) -> Result<(), Fail> {
        self.discover_countdown = DISCOVER_EVERY;
        // Identifiers are discovered, never invented: disk ones are full
        // descriptive strings ("sdb | Type: HDD | Model: … | Serial: …").
        let graphs = self.call("reporting.netdata_graphs", json!([]))?;
        self.identifiers = list(&graphs)
            .iter()
            .filter_map(|g| {
                let name = text(g, "name").filter(|n| *n == "disk" || *n == "interface")?;
                let ids = g.get("identifiers").map(list).unwrap_or(&[]).iter().filter_map(|i| i.as_str().map(str::to_string));
                Some((name.to_string(), ids.collect()))
            })
            .collect();

        // Interface roles, derived rather than hardcoded by name: Proxmox
        // stamps BC:24:11 on the virtio NICs it creates; a passed-through card
        // reports a real media subtype where virtio reports "Unknown Other".
        let mut nics = HashMap::new();
        for iface in list(&self.call("interface.query", json!([]))?) {
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
            let addresses: Vec<Value> = state
                .get("aliases")
                .map(list)
                .unwrap_or(&[])
                .iter()
                .filter(|a| text(a, "type") == Some("INET"))
                .map(|a| a.get("address").cloned().unwrap_or(Value::Null))
                .collect();
            let mut nic = Map::new();
            nic.insert("role".into(), json!(role));
            nic.insert("media".into(), json!(media));
            nic.insert("addresses".into(), Value::Array(addresses));
            nics.insert(text(iface, "name").unwrap_or("").to_string(), nic);
        }
        self.nics = nics;

        // Disk capacities, joined onto the throughput rows by device name.
        self.disk_sizes = list(&self.call("disk.query", json!([]))?)
            .iter()
            .map(|d| (text(d, "name").unwrap_or("").to_string(), d.get("size").cloned().unwrap_or(Value::Null)))
            .collect();
        Ok(())
    }

    pub fn poll(&mut self, source: &Mutex<Source>) -> Result<(), String> {
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

    /// Pool name -> (used, used + available) bytes, from root datasets:
    /// zpool's size counts parity, its root dataset what it can store. A
    /// method error leaves this empty, and the card falls back to raw.
    fn usable_space(&mut self) -> Result<HashMap<String, (i64, i64)>, Fail> {
        let names: Vec<Value> = self.pools.iter().filter_map(|p| p.get("name").filter(|n| !n.is_null()).cloned()).collect();
        if names.is_empty() {
            return Ok(HashMap::new());
        }
        let datasets = match self.call("pool.dataset.query", json!([[["id", "in", names]], {"extra": {"retrieve_children": false}}])) {
            Ok(d) => d,
            Err(Fail::Rpc(_)) => return Ok(HashMap::new()),
            Err(e) => return Err(e),
        };
        let parsed = |d: &Value, k: &str| d.get(k).and_then(|v| v.get("parsed")).and_then(Value::as_i64);
        Ok(list(&datasets)
            .iter()
            .filter_map(|d| {
                let (used, available) = (parsed(d, "used")?, parsed(d, "available")?);
                Some((text(d, "id")?.to_string(), (used, used + available)))
            })
            .collect())
    }

    fn read(&mut self, source: &Mutex<Source>) -> Result<(), Fail> {
        self.discover_countdown -= 1;
        if self.discover_countdown <= 0 {
            match self.discover() {
                // The last discovery's tables still hold; the next try is in
                // DISCOVER_EVERY polls (discover resets the countdown first).
                Err(Fail::Rpc(_)) | Ok(()) => {}
                Err(e) => return Err(e),
            }
        }

        let mut targets = Vec::new();
        for name in ["interface", "disk"] {
            for id in self.identifiers.get(name).map_or(&[][..], Vec::as_slice) {
                targets.push(json!({"name": name, "identifier": id}));
            }
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
        let series = self.call("reporting.netdata_get_data", json!([targets, {"start": now - 10, "end": now}]))?;

        self.pool_countdown -= 1;
        if self.pool_countdown <= 0 || self.pools.is_empty() {
            self.pools = list(&self.call("pool.query", json!([]))?).to_vec();
            self.usable = self.usable_space()?;
            self.pool_countdown = POOLS_EVERY;
        }

        let mut interfaces = Map::new();
        let mut disks = BTreeMap::new();
        for entry in list(&series) {
            let Some(row) = last_sample(entry.get("data").unwrap_or(&Value::Null)) else { continue };
            // "sdb | Type: HDD | …" -> "sdb"
            let label = text(entry, "identifier").unwrap_or("").split('|').next().unwrap_or("").trim().to_string();
            let (a, b) = (row[1].as_f64().unwrap_or(0.0), row[2].as_f64().unwrap_or(0.0));
            if text(entry, "name") == Some("interface") {
                // netdata reports Kilobits/s; normalise to bytes/s.
                let mut nic = Map::new();
                nic.insert("rx_bps".into(), json!(a * 125.0));
                nic.insert("tx_bps".into(), json!(b * 125.0));
                for (k, v) in self.nics.get(&label).cloned().unwrap_or_default() {
                    nic.insert(k, v);
                }
                interfaces.insert(label, Value::Object(nic));
            } else {
                // netdata reports Kibibytes/s; normalise to bytes/s.
                let size = self.disk_sizes.get(&label).cloned().unwrap_or(Value::Null);
                disks.insert(label, json!({"read_bps": a * 1024.0, "write_bps": b * 1024.0, "size": size}));
            }
        }

        let pools: Vec<Value> = self
            .pools
            .iter()
            .rev()
            .map(|p| {
                let name = text(p, "name").unwrap_or("");
                let (used, usable) = self.usable.get(name).map_or((Value::Null, Value::Null), |(u, t)| (json!(u), json!(t)));
                let pass = |k: &str| p.get(k).cloned().unwrap_or(Value::Null);
                json!({
                    "name": pass("name"),
                    "status": pass("status"),
                    "healthy": pass("healthy"),
                    "size": pass("size"),
                    "allocated": pass("allocated"),
                    "free": pass("free"),
                    "used_bytes": used,
                    "usable": usable,
                })
            })
            .collect();

        let mut s = lock(source);
        for (name, nic) in &interfaces {
            s.push(&format!("if.{name}.rx"), nic.get("rx_bps").cloned());
            s.push(&format!("if.{name}.tx"), nic.get("tx_bps").cloned());
        }
        let mut data = Map::new();
        data.insert("interfaces".into(), Value::Object(interfaces));
        // netdata lists identifiers newest-first; sorted, sdb..sdf read in
        // the order the drives are labelled.
        data.insert("disks".into(), Value::Object(disks.into_iter().collect()));
        data.insert("pools".into(), Value::Array(pools));
        s.succeed(data);
        Ok(())
    }
}

/// The most recent [time, a, b] row with both values present.
fn last_sample(data: &Value) -> Option<&[Value]> {
    list(data).iter().rev().filter_map(Value::as_array).find(|row| row.len() >= 3 && !row[1].is_null() && !row[2].is_null()).map(Vec::as_slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latest_complete_sample() {
        let data = json!([[1, 2, 3], [2, 4, 5], [3, null, 1]]);
        assert_eq!(last_sample(&data).map(|r| r[1].clone()), Some(json!(4)));
        assert_eq!(last_sample(&json!([])), None);
    }
}
