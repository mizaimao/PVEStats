//! Credentials from .env, card layout from config.yaml, appearance from
//! config.yaml and ui.json, as pvestats/config.py.
//!
//! Credentials are read once at startup and never leave the collector: they
//! are not in the served page and not in /api/state (spec §6).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use serde_yaml::Value as Yaml;

/// Appearance settings and their defaults, in the Python server's order.
fn ui_defaults() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("theme".into(), json!("ice"));
    m.insert("glow".into(), json!(1.0));
    // Seconds of history kept per source, and so the span of every chart.
    m.insert("window".into(), json!(180));
    // Textural overlays, each independently switchable.
    m.insert("scanlines".into(), json!(false));
    m.insert("grain".into(), json!(false));
    m.insert("aurora".into(), json!(false));
    m
}

/// The known appearance settings whose values have the default's type. A
/// value of the wrong type is dropped, so the default stands: config.yaml
/// and ui.json are edited by hand, and /api/ui takes what a local client
/// sends. Not an object at all means nothing usable.
pub fn clean_ui(values: &Value) -> Map<String, Value> {
    let mut clean = Map::new();
    let Some(values) = values.as_object() else { return clean };
    for (key, default) in ui_defaults() {
        let Some(value) = values.get(&key) else { continue };
        let ok = match default {
            Value::Bool(_) => value.is_boolean(),
            // Finite numbers only: NaN or infinity cannot size a buffer.
            Value::Number(_) => value.as_f64().is_some_and(f64::is_finite),
            Value::String(_) => value.is_string(),
            _ => false,
        };
        if ok {
            clean.insert(key, value.clone());
        }
    }
    clean
}

fn merge(base: &Map<String, Value>, over: &Map<String, Value>) -> Map<String, Value> {
    let mut out = base.clone();
    for (k, v) in over {
        out.insert(k.clone(), v.clone());
    }
    out
}

pub struct Config {
    pub root: PathBuf,
    pub pve_host: String,
    pub pve_node: Option<String>,
    pve_token_id: String,
    pve_token_secret: String,
    pub truenas_host: String,
    pub truenas_api_key: String,
    pub agent_url: String,
    pub port: u16,
    /// By VMID, each entry as written in config.yaml.
    pub hosts: Vec<(u64, Map<String, Value>)>,
    ui_defaults: Map<String, Value>,
}

/// KEY=VALUE lines, as python-dotenv reads them: `export ` prefixes, quoted
/// values, and `#` comments, whole-line or after an unquoted value.
fn read_env(path: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else { return out };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim();
        let value = match value.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let inner = &value[1..];
                inner.find(q).map_or(inner, |end| &inner[..end])
            }
            _ => value.find(" #").map_or(value, |i| value[..i].trim_end()),
        };
        out.insert(key.trim().to_string(), value.to_string());
    }
    out
}

fn yaml_to_json(y: &Yaml) -> Value {
    serde_json::to_value(y).unwrap_or(Value::Null)
}

impl Config {
    pub fn load(root: &Path) -> Result<Config, String> {
        let env = read_env(&root.join(".env"));
        // As load_dotenv: a variable already set in the environment wins.
        let var = |key: &str| std::env::var(key).ok().or_else(|| env.get(key).cloned()).unwrap_or_default().trim().to_string();
        let port = match var("DASHBOARD_PORT").as_str() {
            "" => 8080,
            p => p.parse().map_err(|_| format!("DASHBOARD_PORT is not a port: {p}"))?,
        };
        let text = std::fs::read_to_string(root.join("config.yaml")).map_err(|e| format!("config.yaml: {e}"))?;
        let raw: Yaml = serde_yaml::from_str(&text).map_err(|e| format!("config.yaml: {e}"))?;
        let raw = yaml_to_json(&raw);
        let mut hosts = Vec::new();
        for h in raw.get("hosts").and_then(Value::as_array).cloned().unwrap_or_default() {
            let Some(entry) = h.as_object() else { continue };
            let vmid = match entry.get("vmid") {
                Some(Value::Number(n)) => n.as_u64(),
                Some(Value::String(s)) => s.trim().parse().ok(),
                _ => None,
            };
            if let Some(vmid) = vmid {
                hosts.retain(|(v, _)| *v != vmid);
                hosts.push((vmid, entry.clone()));
            }
        }
        let ui_defaults = merge(&ui_defaults(), &clean_ui(raw.get("ui").unwrap_or(&Value::Null)));
        Ok(Config {
            root: root.to_path_buf(),
            pve_host: var("PVE_HOST"),
            pve_node: Some(var("PVE_NODE")).filter(|n| !n.is_empty()),
            pve_token_id: var("PVE_TOKEN_ID"),
            pve_token_secret: var("PVE_TOKEN_SECRET"),
            truenas_host: var("TRUENAS_HOST"),
            truenas_api_key: var("TRUENAS_API_KEY"),
            agent_url: var("ML60_AGENT_URL"),
            port,
            hosts,
            ui_defaults,
        })
    }

    fn ui_state(&self) -> PathBuf {
        self.root.join("ui.json")
    }

    /// Just what the settings panel has changed, as stored in ui.json.
    fn saved_ui(&self) -> Map<String, Value> {
        std::fs::read_to_string(self.ui_state())
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .map(|v| clean_ui(&v))
            .unwrap_or_default()
    }

    /// Appearance settings: config.yaml's, overridden by ui.json.
    pub fn ui(&self) -> Map<String, Value> {
        merge(&self.ui_defaults, &self.saved_ui())
    }

    /// Saves appearance settings, keeping unknown keys and bad values out,
    /// written to a temporary file and moved into place so a crash cannot
    /// leave half a file. ui.json holds only what was changed here.
    pub fn save_ui(&self, changes: &Value) -> std::io::Result<Map<String, Value>> {
        let saved = merge(&self.saved_ui(), &clean_ui(changes));
        let temporary = self.root.join(format!(".ui.{}.tmp", std::process::id()));
        let text = serde_json::to_string_pretty(&Value::Object(saved.clone())).unwrap_or_else(|_| "{}".into());
        if let Err(e) = std::fs::write(&temporary, text).and_then(|_| std::fs::rename(&temporary, self.ui_state())) {
            let _ = std::fs::remove_file(&temporary);
            return Err(e);
        }
        Ok(merge(&self.ui_defaults, &saved))
    }

    /// Whether a key is one of the appearance settings.
    pub fn is_ui_key(key: &str) -> bool {
        ui_defaults().contains_key(key)
    }

    pub fn pve_auth_header(&self) -> String {
        format!("PVEAPIToken={}={}", self.pve_token_id, self.pve_token_secret)
    }

    pub fn pve_base(&self) -> String {
        format!("https://{}:8006/api2/json", self.pve_host)
    }

    pub fn host(&self, vmid: u64) -> Option<&Map<String, Value>> {
        self.hosts.iter().find(|(v, _)| *v == vmid).map(|(_, h)| h)
    }

    /// Panel set for a guest. Unknown VMIDs default to `basic` (§4.5).
    pub fn view_for(&self, vmid: u64) -> Value {
        self.host(vmid).and_then(|h| h.get("view")).cloned().unwrap_or_else(|| json!("basic"))
    }

    pub fn pve_configured(&self) -> bool {
        !self.pve_host.is_empty() && !self.pve_token_id.is_empty() && !self.pve_token_secret.is_empty()
    }

    pub fn truenas_configured(&self) -> bool {
        !self.truenas_host.is_empty() && !self.truenas_api_key.is_empty()
    }

    pub fn agent_configured(&self) -> bool {
        !self.agent_url.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_values_of_the_wrong_type_are_dropped() {
        let clean = clean_ui(&json!({"theme": "neon", "glow": "x", "window": 300, "grain": 1, "aurora": true, "other": 1}));
        assert_eq!(Value::Object(clean), json!({"theme": "neon", "window": 300, "aurora": true}));
        assert!(clean_ui(&json!([1])).is_empty());
    }

    #[test]
    fn env_lines() {
        let dir = std::env::temp_dir().join(format!("pvestats-server-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(&path, "# comment\nA=1\nexport B = two \nC=\"x # y\"\nE=v # note\nF=\n").unwrap();
        let env = read_env(&path);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!((env["A"].as_str(), env["B"].as_str(), env["C"].as_str()), ("1", "two", "x # y"));
        assert_eq!((env["E"].as_str(), env["F"].as_str()), ("v", ""));
    }
}
