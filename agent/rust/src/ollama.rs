//! Ollama: its loaded-model list, cached and kept fresh by one thread, and
//! the one write this project makes, unloading a model.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::http::{self, Url};

/// The dashboard allows 2 s for the whole request, so Ollama gets 1 s of it.
pub const LIST_TIMEOUT: Duration = Duration::from_secs(1);
/// An unload waits for any generation in flight to finish.
pub const UNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// Most of a reply from Ollama that is read.
const REPLY_CAP: usize = 1 << 20;
/// The list changes only when a model loads or unloads, and Ollama logs a
/// line on ml60's disk per request: at the dashboard's 1 Hz a 30 s cache
/// turns 86,400 calls a day into 2,880.
pub const CACHE_FOR: Duration = Duration::from_secs(30);
/// Shorter while the answer is about to change: a stopped model still
/// listed (a busy one unloads when its request ends), or Ollama down.
pub const RECHECK_AFTER: Duration = Duration::from_secs(5);
/// How long a stopped model is watched for at the short pace.
pub const WATCH_FOR: Duration = Duration::from_secs(120);

/// The models as the dashboard reads them, from a reply to /api/ps; None
/// for a reply that is not that shape, which counts as Ollama being down.
pub fn parse_ps(body: &[u8]) -> Option<Vec<Value>> {
    let reply: Value = serde_json::from_slice(body).ok()?;
    let models = reply.as_object()?.get("models")?.as_array()?;
    Some(
        models
            .iter()
            .filter_map(Value::as_object)
            .map(|m| {
                let name = m.get("name").filter(|n| n.as_str().is_some_and(|s| !s.is_empty()));
                let pass = |key: &str| m.get(key).cloned().unwrap_or(Value::Null);
                json!({
                    "name": name.or_else(|| m.get("model")).cloned().unwrap_or(Value::Null),
                    "size": pass("size"),
                    "size_vram": pass("size_vram"),
                    "expires_at": pass("expires_at"),
                    "context_length": pass("context_length"),
                })
            })
            .collect(),
    )
}

/// Ollama's loaded models, now.
pub fn fetch(url: &Url) -> Result<Vec<Value>, String> {
    let (status, body) = http::call(url, "GET", None, Instant::now() + LIST_TIMEOUT, REPLY_CAP)?;
    if status != 200 {
        return Err(format!("Ollama answered {status}"));
    }
    parse_ps(&body).ok_or_else(|| "Ollama's model list is not the expected shape".to_string())
}

/// The cached list and what decides when it is fetched again.
#[derive(Default)]
pub struct Models {
    pub list: Vec<Value>,
    /// None: fetch at the next chance.
    pub expires: Option<Instant>,
    /// When the last fetch finished, whatever came of it.
    pub fetched: Option<Instant>,
    /// Models stopped from the dashboard, watched until gone.
    stopping: HashMap<String, Instant>,
}

impl Models {
    pub fn due(&self, now: Instant) -> bool {
        self.expires.map_or(true, |e| e <= now)
    }

    /// Takes a fetch's outcome, as the Python agent did: a list is kept 30 s,
    /// or 5 s while a stopped model is still on it; a failure counts as an
    /// empty list, rechecked after 5 s.
    pub fn take(&mut self, outcome: Result<Vec<Value>, String>, now: Instant) {
        self.fetched = Some(now);
        match outcome {
            Ok(list) => {
                let listed: Vec<&str> = list.iter().filter_map(|m| m["name"].as_str()).collect();
                self.stopping.retain(|name, until| listed.contains(&name.as_str()) && *until > now);
                let keep = if self.stopping.is_empty() { CACHE_FOR } else { RECHECK_AFTER };
                self.list = list;
                self.expires = Some(now + keep);
            }
            Err(_) => {
                self.list = Vec::new();
                self.expires = Some(now + RECHECK_AFTER);
            }
        }
    }

    /// After an unload, whatever came of it: fetch again at once, and watch
    /// the model until it has gone.
    pub fn stopped(&mut self, name: &str, now: Instant) {
        self.stopping.insert(name.to_string(), now + WATCH_FOR);
        self.expires = None;
    }
}

/// The reply to a POST /unload once the request itself has passed its
/// checks: the status and the body. Asks Ollama to drop `name` from memory
/// with `keep_alive: 0` on an empty generate request, which is what
/// `ollama stop` does; it comes back with done_reason "unload".
pub fn unload(generate: &Url, name: &str) -> (u16, Value) {
    let body = json!({"model": name, "keep_alive": 0, "stream": false}).to_string();
    let outcome = http::call(generate, "POST", Some(body.as_bytes()), Instant::now() + UNLOAD_TIMEOUT, REPLY_CAP);
    match outcome {
        Err(e) => (502, json!({"ok": false, "error": format!("Ollama: {e}")})),
        Ok((status, body)) => {
            // One JSON object, or the last line of a stream of them.
            let result: Value = std::str::from_utf8(&body)
                .ok()
                .and_then(|text| text.lines().rev().find(|l| !l.trim().is_empty()))
                .and_then(|line| serde_json::from_str(line).ok())
                .unwrap_or_else(|| Value::Object(Map::new()));
            if status != 200 {
                let said = result.get("error").and_then(Value::as_str).unwrap_or("no reason given");
                return (502, json!({"ok": false, "error": format!("Ollama answered {status}: {said}")}));
            }
            let ok = result.get("done_reason").and_then(Value::as_str) == Some("unload");
            let mut reply = json!({"ok": ok, "result": result});
            if !ok {
                reply["error"] = json!("Ollama did not confirm the unload");
            }
            (200, reply)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_replies_of_the_wrong_shape_count_as_down() {
        for bad in ["null", "[]", "{\"models\":null}", "{\"models\":{}}", "not json", "\"x\""] {
            assert_eq!(parse_ps(bad.as_bytes()), None, "{bad}");
        }
        assert_eq!(parse_ps(b"{\"models\":[]}"), Some(vec![]));
        // Entries that are not objects are skipped, not fatal.
        let list = parse_ps(br#"{"models":[null, 3, {"model":"qwen3:32b","size":10,"size_vram":8,"expires_at":"2026-10-26T01:00:00Z"}]}"#).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["name"], "qwen3:32b");
        assert_eq!(list[0]["size_vram"], 8);
        assert_eq!(list[0]["context_length"], Value::Null);
        let named = parse_ps(br#"{"models":[{"name":"a:1","model":"b:2"},{"name":"","model":"c:3"}]}"#).unwrap();
        assert_eq!((named[0]["name"].as_str(), named[1]["name"].as_str()), (Some("a:1"), Some("c:3")));
    }

    #[test]
    fn the_cache_keeps_30_s_and_5_s_while_watching() {
        let now = Instant::now();
        let mut m = Models::default();
        assert!(m.due(now));
        let list = || Ok(parse_ps(br#"{"models":[{"name":"a"}]}"#).unwrap());
        m.take(list(), now);
        assert_eq!(m.expires, Some(now + CACHE_FOR));
        m.stopped("a", now);
        assert!(m.due(now));
        m.take(list(), now);
        assert_eq!(m.expires, Some(now + RECHECK_AFTER), "still listed: watched");
        m.take(Ok(vec![]), now);
        assert_eq!(m.expires, Some(now + CACHE_FOR), "gone: back to the long cache");
        m.take(Err("down".into()), now);
        assert_eq!((m.list.len(), m.expires), (0, Some(now + RECHECK_AFTER)));
    }
}
