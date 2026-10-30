//! The web page's appearance settings: config.yaml's `ui:` block, overridden
//! by ui.json, which the page's settings panel writes through POST /api/ui.
//! The terminal reads the theme from the same file.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{json, Map, Value};

/// Appearance settings and their defaults.
fn defaults() -> Map<String, Value> {
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
pub fn clean(values: &Value) -> Map<String, Value> {
    let mut clean = Map::new();
    let Some(values) = values.as_object() else { return clean };
    for (key, default) in defaults() {
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

/// Whether a key is one of the appearance settings.
pub fn is_key(key: &str) -> bool {
    defaults().contains_key(key)
}

fn merge(base: &Map<String, Value>, over: &Map<String, Value>) -> Map<String, Value> {
    let mut out = base.clone();
    for (k, v) in over {
        out.insert(k.clone(), v.clone());
    }
    out
}

pub struct Settings {
    file: PathBuf,
    /// The defaults with config.yaml's block over them.
    base: Map<String, Value>,
    saving: Mutex<()>,
}

impl Settings {
    pub fn new(root: PathBuf, yaml: &Value) -> Self {
        Settings { file: root.join("ui.json"), base: merge(&defaults(), &clean(yaml)), saving: Mutex::new(()) }
    }

    /// Just what the settings panel has changed, as stored in ui.json.
    fn saved(&self) -> Map<String, Value> {
        std::fs::read_to_string(&self.file)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .map(|v| clean(&v))
            .unwrap_or_default()
    }

    pub fn current(&self) -> Map<String, Value> {
        merge(&self.base, &self.saved())
    }

    /// Saves settings, keeping unknown keys and bad values out, written to a
    /// temporary file and moved into place so a crash cannot leave half a
    /// file. ui.json holds only what was changed here.
    pub fn save(&self, changes: &Value) -> std::io::Result<Map<String, Value>> {
        let _one_at_a_time = self.saving.lock().unwrap_or_else(|e| e.into_inner());
        let saved = merge(&self.saved(), &clean(changes));
        let temporary = self.file.with_file_name(format!(".ui.{}.tmp", std::process::id()));
        let text = serde_json::to_string_pretty(&Value::Object(saved.clone())).unwrap_or_else(|_| "{}".into());
        if let Err(e) = std::fs::write(&temporary, text).and_then(|_| std::fs::rename(&temporary, &self.file)) {
            let _ = std::fs::remove_file(&temporary);
            return Err(e);
        }
        Ok(merge(&self.base, &saved))
    }

    /// Seconds of history the charts span.
    pub fn window(&self) -> f64 {
        self.current().get("window").and_then(Value::as_f64).unwrap_or(180.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_of_the_wrong_type_are_dropped() {
        let kept = clean(&json!({"theme": "neon", "glow": "x", "window": 300, "grain": 1, "aurora": true, "other": 1}));
        assert_eq!(Value::Object(kept), json!({"theme": "neon", "window": 300, "aurora": true}));
        assert!(clean(&json!([1])).is_empty());
    }

    #[test]
    fn saved_over_config_over_defaults() {
        let dir = std::env::temp_dir().join(format!("pvestats-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = Settings::new(dir.clone(), &json!({"theme": "neon", "grain": true}));
        assert_eq!(settings.current()["theme"], "neon");
        let after = settings.save(&json!({"theme": "amber", "window": 600, "nope": 1})).unwrap();
        assert_eq!((after["theme"].as_str(), after["grain"].as_bool()), (Some("amber"), Some(true)));
        assert_eq!(settings.window(), 600.0);
        let file: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("ui.json")).unwrap()).unwrap();
        assert_eq!(file, json!({"theme": "amber", "window": 600}));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
