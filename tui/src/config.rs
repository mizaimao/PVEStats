//! Credentials from .env, card layout and appearance from config.yaml, the
//! same files the app reads. Read once at startup; credentials never leave
//! this process (spec §6).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_yaml::Value as Yaml;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Basic,
    Gpu,
    Nas,
}

#[derive(Clone, Debug)]
pub struct Host {
    pub name: Option<String>,
    pub view: View,
    /// Interface name -> label, overriding the derived role (config.yaml).
    pub nic_labels: BTreeMap<String, String>,
}

pub struct Config {
    pub pve_host: String,
    pub pve_node: Option<String>,
    pub pve_token_id: String,
    pub pve_token_secret: String,
    pub truenas_host: String,
    pub truenas_api_key: String,
    pub agent_url: String,
    pub hosts: BTreeMap<u64, Host>,
    pub theme: String,
    /// Seconds of history kept per source, and so the span of every chart.
    pub window: f64,
    /// Seconds between themes in a tour of all of them, for picking one;
    /// 0 keeps to `theme`. config.yaml, `tui: theme_tour`.
    pub tour: f64,
    /// Minutes past midnight between which the display is switched off,
    /// against burn-in: config.yaml, `tui: screen_off: "03:00-05:00"`.
    pub screen_off: Option<(u32, u32)>,
}

impl Config {
    pub fn load(root: &Path) -> Result<Config, String> {
        let file = read_env(&root.join(".env"));
        // As python-dotenv does by default: a variable already set in the
        // environment wins over the file.
        let var = |key: &str| {
            std::env::var(key)
                .ok()
                .or_else(|| file.get(key).cloned())
                .unwrap_or_default()
                .trim()
                .to_string()
        };

        let path = root.join("config.yaml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let raw: Yaml = serde_yaml::from_str(&text)
            .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;

        // ui.json holds what the app's settings panel changed; it overrides
        // config.yaml's ui block one setting at a time, as in the app.
        let saved: serde_json::Value = std::fs::read_to_string(root.join("ui.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let ui = raw.get("ui");
        let theme = saved
            .get("theme")
            .and_then(|v| v.as_str())
            .or_else(|| ui.and_then(|u| u.get("theme")).and_then(|v| v.as_str()))
            .unwrap_or("ice")
            .to_string();
        let window = saved
            .get("window")
            .and_then(|v| v.as_f64())
            .or_else(|| ui.and_then(|u| u.get("window")).and_then(|v| v.as_f64()))
            .filter(|v| v.is_finite())
            .unwrap_or(180.0)
            .clamp(30.0, 3600.0);

        Ok(Config {
            pve_host: var("PVE_HOST"),
            pve_node: Some(var("PVE_NODE")).filter(|n| !n.is_empty()),
            pve_token_id: var("PVE_TOKEN_ID"),
            pve_token_secret: var("PVE_TOKEN_SECRET"),
            truenas_host: var("TRUENAS_HOST"),
            truenas_api_key: var("TRUENAS_API_KEY"),
            agent_url: var("ML60_AGENT_URL"),
            hosts: hosts(&raw),
            theme,
            window,
            screen_off: raw
                .get("tui")
                .and_then(|t| t.get("screen_off"))
                .and_then(|v| v.as_str())
                .and_then(hours),
            tour: raw
                .get("tui")
                .and_then(|t| t.get("theme_tour"))
                .and_then(|v| v.as_f64())
                .filter(|v| v.is_finite() && *v > 0.0)
                .unwrap_or(0.0),
        })
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

    pub fn pve_base(&self) -> String {
        format!("https://{}:8006/api2/json", self.pve_host)
    }

    pub fn pve_auth_header(&self) -> String {
        format!("PVEAPIToken={}={}", self.pve_token_id, self.pve_token_secret)
    }

    /// Panel set for a guest. Unknown VMIDs default to `basic` (§4.5).
    pub fn view_for(&self, vmid: u64) -> View {
        self.hosts.get(&vmid).map_or(View::Basic, |h| h.view)
    }
}

fn hosts(raw: &Yaml) -> BTreeMap<u64, Host> {
    let mut out = BTreeMap::new();
    // `hosts:` with every entry commented out reads as null, not [].
    let Some(list) = raw.get("hosts").and_then(|h| h.as_sequence()) else {
        return out;
    };
    for entry in list {
        let vmid = match entry.get("vmid") {
            Some(Yaml::Number(n)) => n.as_u64(),
            Some(Yaml::String(s)) => s.trim().parse().ok(),
            _ => None,
        };
        let Some(vmid) = vmid else { continue };
        let view = match entry.get("view").and_then(|v| v.as_str()) {
            Some("gpu") => View::Gpu,
            Some("nas") => View::Nas,
            _ => View::Basic,
        };
        let nic_labels = entry
            .get("nic_labels")
            .and_then(|l| l.as_mapping())
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| Some((k.as_str()?.to_string(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let name = entry.get("name").and_then(|v| v.as_str()).map(str::to_string);
        out.insert(vmid, Host { name, view, nic_labels });
    }
    out
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

/// The repo root: where config.yaml and .env live. `--root` wins; then
/// PVESTATS_ROOT; then the nearest folder above the binary that holds a
/// config.yaml (the binary sits in tui/target/release); then the current one.
/// "03:00-05:00" as minutes past midnight, start and end.
fn hours(text: &str) -> Option<(u32, u32)> {
    let minutes = |t: &str| {
        let (h, m) = t.trim().split_once(':')?;
        let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
        (h < 24 && m < 60).then_some(h * 60 + m)
    };
    let (from, to) = text.split_once('-')?;
    Some((minutes(from)?, minutes(to)?)).filter(|(a, b)| a != b)
}

pub fn find_root(flag: Option<PathBuf>) -> PathBuf {
    if let Some(root) = flag.or_else(|| std::env::var_os("PVESTATS_ROOT").map(PathBuf::from)) {
        return root;
    }
    if let Ok(exe) = std::env::current_exe() {
        let exe = exe.canonicalize().unwrap_or(exe);
        for dir in exe.ancestors().skip(1) {
            if dir.join("config.yaml").is_file() {
                return dir.to_path_buf();
            }
        }
    }
    PathBuf::from(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_lines() {
        let dir = std::env::temp_dir().join(format!("pvestats-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(
            &path,
            "# comment\nA=1\nexport B = two \nC=\"x # y\"\nD='q'\nE=v # note\nF=\n",
        )
        .unwrap();
        let env = read_env(&path);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(env["A"], "1");
        assert_eq!(env["B"], "two");
        assert_eq!(env["C"], "x # y");
        assert_eq!(env["D"], "q");
        assert_eq!(env["E"], "v");
        assert_eq!(env["F"], "");
    }

    #[test]
    fn host_entries() {
        let raw: Yaml = serde_yaml::from_str(
            "hosts:\n  - vmid: 222\n    name: ml60\n    view: gpu\n  - vmid: \"100\"\n    view: nas\n    nic_labels:\n      ens18: virtual (E610)\n  - name: no vmid\n",
        )
        .unwrap();
        let hosts = hosts(&raw);
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[&222].view, View::Gpu);
        assert_eq!(hosts[&100].nic_labels["ens18"], "virtual (E610)");
        let empty: Yaml = serde_yaml::from_str("hosts:\n").unwrap();
        assert!(super::hosts(&empty).is_empty());
    }

    #[test]
    fn hour_windows() {
        assert_eq!(hours("03:00-05:00"), Some((180, 300)));
        assert_eq!(hours("23:30 - 01:00"), Some((1410, 60)));
        assert_eq!(hours("3-5"), None);
        assert_eq!(hours("03:00-03:00"), None);
    }
}
