//! WASM bindings for `vaultdb-core`.
//!
//! ## Scope
//!
//! `Vault` walks the filesystem, which doesn't exist on
//! `wasm32-unknown-unknown`. This binding deliberately exposes
//! only the parser-side primitives that are useful in the browser:
//!
//! - parse a single markdown file's frontmatter into a structured
//!   record
//! - parse a where-DSL string into an executable predicate
//! - evaluate a predicate against a record
//! - extract `[[wikilinks]]` from a body
//!
//! Callers fetch markdown over HTTP (or read it from IndexedDB)
//! and feed it in as a string. They keep their own ID/path
//! mapping; vaultdb-wasm doesn't pretend to know about the file
//! system.
//!
//! ## Why not a full Vault on top of WebAssembly?
//!
//! `Vault` was designed around `walkdir`, an `fs2`-based vault
//! lock, atomic-rename via tempfile, and the rename journal.
//! None of those translate to a browser sandbox. A future
//! `BrowserVault` shim could implement the same trait surface
//! against IndexedDB, but that's a separate project — it would
//! have its own crash-recovery story and persistence strategy.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use vaultdb_core::filter::evaluate_expr;
use vaultdb_core::links::extract_links;
use vaultdb_core::{Expr, Record, Value};
use wasm_bindgen::prelude::*;

/// One parsed markdown file. JS receives a plain object; the
/// `path` is whatever string the caller passed in (used as a
/// stable identity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedRecord {
    pub path: String,
    /// Frontmatter as a JSON-shaped object (string-keyed).
    pub fields: serde_json::Value,
    /// The body text after the closing `---`. `null` when the
    /// caller passed a `null` body.
    pub body: Option<String>,
}

/// Parse a markdown file with YAML frontmatter into a
/// `ParsedRecord`. `path` is opaque to the binding — it just
/// flows through as the result's `path` field, so the JS caller
/// can use it as an identity key.
///
/// Returns the parsed record on success, or throws a JS Error
/// carrying the underlying parser message.
#[wasm_bindgen(js_name = parseRecord)]
pub fn parse_record(path: &str, raw: &str) -> Result<JsValue, JsValue> {
    let (frontmatter_yaml, body) = split_frontmatter(raw)
        .ok_or_else(|| JsValue::from_str("missing or malformed `---` frontmatter delimiters"))?;
    let fields: serde_yaml::Value = serde_yaml::from_str(frontmatter_yaml)
        .map_err(|e| JsValue::from_str(&format!("frontmatter parse: {e}")))?;
    let json: serde_json::Value = serde_json::to_value(&fields)
        .map_err(|e| JsValue::from_str(&format!("yaml→json: {e}")))?;
    let record = ParsedRecord {
        path: path.to_string(),
        fields: json,
        body: Some(body.to_string()),
    };
    serde_wasm_bindgen::to_value(&record)
        .map_err(|e| JsValue::from_str(&format!("serialise to JS: {e}")))
}

/// Parse a where-DSL string and return a handle (just the
/// canonical string form for now — opaque to the JS side). This
/// validates the syntax up front so the caller catches a bad
/// query before iterating over records.
#[wasm_bindgen(js_name = parseWhereDsl)]
pub fn parse_where_dsl(input: &str) -> Result<JsValue, JsValue> {
    let expr = Expr::parse(input).map_err(|e| JsValue::from_str(&e.to_string()))?;
    let canonical = format!("{expr:?}");
    Ok(JsValue::from_str(&canonical))
}

/// Evaluate a where-DSL string against a single record (as
/// produced by `parseRecord`). Returns `true` / `false` to JS.
///
/// Re-parses the predicate each call. For batch usage you'd
/// hoist the parse out of the loop on the JS side; we keep the
/// API stateless on purpose so it composes well with React's
/// strict-mode double-rendering and similar idioms.
#[wasm_bindgen(js_name = evaluateWhere)]
pub fn evaluate_where(record: JsValue, where_clause: &str) -> Result<bool, JsValue> {
    let parsed: ParsedRecord = serde_wasm_bindgen::from_value(record)
        .map_err(|e| JsValue::from_str(&format!("record shape: {e}")))?;
    let expr = Expr::parse(where_clause).map_err(|e| JsValue::from_str(&e.to_string()))?;

    // Convert the JSON fields into vaultdb's Value tower for the
    // evaluator. Same path the in-tree parser uses, just from JSON
    // rather than YAML.
    let mut fields = std::collections::BTreeMap::new();
    if let serde_json::Value::Object(map) = parsed.fields {
        for (k, v) in map {
            fields.insert(k, json_to_vaultdb_value(v));
        }
    }
    let record = Record {
        path: PathBuf::from(&parsed.path),
        fields,
        raw_content: parsed.body,
    };
    // No vault root in the browser; we pass an empty path. The
    // evaluator only uses the root for graph-virtual fields
    // (`_links`, `_backlinks`), which require a LinkGraph anyway —
    // we pass `None`.
    Ok(evaluate_expr(&expr, &record, &PathBuf::new(), None))
}

/// Extract every `[[wikilink]]` target from a markdown body.
/// Strips fenced and inline code blocks first so links inside
/// code samples don't pollute the result. JS receives a sorted
/// array of unique target strings.
#[wasm_bindgen(js_name = extractLinks)]
pub fn extract_links_wasm(body: &str) -> Result<JsValue, JsValue> {
    let links: Vec<String> = extract_links(body).into_iter().collect();
    serde_wasm_bindgen::to_value(&links)
        .map_err(|e| JsValue::from_str(&format!("serialise links: {e}")))
}

/// Returns the version string baked into the wasm module at
/// build time. Useful for "reload your app, the wasm changed"
/// banners.
#[wasm_bindgen(js_name = version)]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// ── helpers ────────────────────────────────────────────────────────

fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let trimmed = raw.strip_prefix("---\n")?;
    let close = trimmed.find("\n---\n")?;
    Some((&trimmed[..close], &trimmed[close + "\n---\n".len()..]))
}

fn json_to_vaultdb_value(v: serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::String(n.to_string())
            }
        }
        serde_json::Value::String(s) => Value::String(s),
        serde_json::Value::Array(arr) => {
            Value::List(arr.into_iter().map(json_to_vaultdb_value).collect())
        }
        serde_json::Value::Object(obj) => {
            let mut m = std::collections::BTreeMap::new();
            for (k, v) in obj {
                m.insert(k, json_to_vaultdb_value(v));
            }
            Value::Map(m)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_frontmatter_basic() {
        let input = "---\nname: X\n---\nbody\n";
        let (yaml, body) = split_frontmatter(input).unwrap();
        assert_eq!(yaml, "name: X");
        assert_eq!(body, "body\n");
    }

    #[test]
    fn split_returns_none_when_delimiters_missing() {
        assert!(split_frontmatter("no frontmatter here").is_none());
    }

    #[test]
    fn json_to_vaultdb_value_round_trip() {
        let input = serde_json::json!({"a": 1, "b": "two", "c": [true, null]});
            let value = json_to_vaultdb_value(input);
        match value {
            Value::Map(m) => {
                assert!(matches!(m.get("a"), Some(Value::Integer(1))));
                assert!(matches!(m.get("b"), Some(Value::String(s)) if s == "two"));
            }
            _ => panic!("expected Map"),
        }
    }
}
