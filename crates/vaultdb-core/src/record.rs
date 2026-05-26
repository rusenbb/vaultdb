//! [`Record`] (one parsed `.md` file) and [`Value`] (the typed cell values).
//! Records have virtual fields (`_name`, `_path`, `_modified`, etc.)
//! computed lazily from the path and frontmatter.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A value from YAML frontmatter, preserving type information.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum Value {
    Null,
    String(String),
    Integer(i64),
    Float(f64),
    Bool(bool),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}

// Ergonomic `From` impls so callers building predicates programmatically
// (notably `vaultdb-orm`'s typed field accessors) can write `.eq(2024)`
// or `.contains("topic/ai")` and have it produce the right `Value`
// variant without an explicit constructor.

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::String(v.to_string())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::String(v)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Integer(v as i64)
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Integer(v)
    }
}
impl From<u32> for Value {
    fn from(v: u32) -> Self {
        Value::Integer(v as i64)
    }
}
impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::Float(v as f64)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}
impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(v: Vec<T>) -> Self {
        Value::List(v.into_iter().map(Into::into).collect())
    }
}

/// One parsed .md file = one record.
///
/// Serialization note: `path` serializes as a string. For machine-portable
/// JSON, store records relative to the vault root before round-tripping;
/// absolute paths are host-specific. `raw_content` is skipped when `None`
/// so the wire format stays compact for record listings that don't include
/// body text.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Record {
    /// Absolute path to the .md file.
    pub path: PathBuf,
    /// Parsed frontmatter fields.
    pub fields: BTreeMap<String, Value>,
    /// Raw file content — only loaded for write operations.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub raw_content: Option<String>,
}

impl Record {
    /// Look up a field by name, checking virtual fields first.
    pub fn get(&self, key: &str, vault_root: &Path) -> Option<Value> {
        self.get_with_links(key, vault_root, None)
    }

    /// Look up a field, including graph virtual fields when a link index is provided.
    pub fn get_with_links(
        &self,
        key: &str,
        vault_root: &Path,
        link_index: Option<&crate::links::LinkGraph>,
    ) -> Option<Value> {
        match key {
            "_name" => Some(Value::String(self.virtual_name())),
            "_path" => Some(Value::String(self.virtual_path(vault_root))),
            "_folder" => Some(Value::String(self.virtual_folder())),
            "_modified" => self.virtual_modified().map(Value::String),
            "_created" => self.virtual_created().map(Value::String),
            "_links" | "_link_count" | "_backlinks" | "_backlink_count" => {
                let name = self.virtual_name();
                link_index.and_then(|idx| {
                    idx.virtual_fields(&name)
                        .into_iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, v)| v)
                })
            }
            "_length" => {
                let content = self.load_content();
                Some(Value::Integer(content.len() as i64))
            }
            "_body_length" => {
                let content = self.load_content();
                let body_len = crate::frontmatter::extract_frontmatter(&content)
                    .map(|(_, body_start)| content[body_start..].trim().len())
                    .unwrap_or(content.trim().len());
                Some(Value::Integer(body_len as i64))
            }
            "_body" => {
                // Full body text (everything after the closing `---` of
                // the frontmatter). Used by body-search predicates such as
                // `_body contains "search term"` and `_body matches
                // "^prefix"`. The DSL parser doesn't need to know about
                // this — it's a normal virtual field that flows through
                // the existing `Contains` / `Matches` / `StartsWith` /
                // `EndsWith` predicates.
                let content = self.load_content();
                let body = crate::frontmatter::extract_frontmatter(&content)
                    .map(|(_, body_start)| content[body_start..].to_string())
                    .unwrap_or(content);
                Some(Value::String(body))
            }
            "_body_links" => {
                // Markdown links `[label](url)` in the body, as a list of
                // {label, url} maps. Complements the wiki-link graph
                // (`_links`, which captures `[[Note]]` internal links); this
                // captures external markdown links. Query element-wise with
                // `matches`, e.g. `_body_links matches "cs\\.stanford\\.edu"`.
                let content = self.load_content();
                let body = crate::frontmatter::extract_frontmatter(&content)
                    .map(|(_, body_start)| content[body_start..].to_string())
                    .unwrap_or(content);
                let links: Vec<Value> = crate::links::extract_markdown_links(&body)
                    .into_iter()
                    .map(|(label, url)| {
                        let mut m = BTreeMap::new();
                        m.insert("label".to_string(), Value::String(label));
                        m.insert("url".to_string(), Value::String(url));
                        Value::Map(m)
                    })
                    .collect();
                Some(Value::List(links))
            }
            _ => self.fields.get(key).cloned(),
        }
    }

    /// Get file content — from raw_content if loaded, otherwise read from disk.
    fn load_content(&self) -> String {
        if let Some(ref content) = self.raw_content {
            content.clone()
        } else {
            std::fs::read_to_string(&self.path).unwrap_or_default()
        }
    }

    /// Filename without .md extension, with URL-encoded characters decoded.
    pub fn virtual_name(&self) -> String {
        let raw = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        decode_percent_encoding(&raw)
    }

    /// Relative path from vault root.
    pub fn virtual_path(&self, vault_root: &Path) -> String {
        self.path
            .strip_prefix(vault_root)
            .unwrap_or(&self.path)
            .to_string_lossy()
            .into_owned()
    }

    /// Parent folder name.
    pub fn virtual_folder(&self) -> String {
        self.path
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn virtual_modified(&self) -> Option<String> {
        self.path
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .map(format_system_time)
    }

    fn virtual_created(&self) -> Option<String> {
        self.path
            .metadata()
            .ok()
            .and_then(|m| m.created().ok())
            .map(format_system_time)
    }
}

/// Decode percent-encoded characters in a string (e.g., %20 -> space).
fn decode_percent_encoding(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2
                && let Ok(byte) = u8::from_str_radix(&hex, 16)
            {
                result.push(byte as char);
                continue;
            }
            // Failed to parse — keep the original
            result.push('%');
            result.push_str(&hex);
        } else {
            result.push(c);
        }
    }
    result
}

fn format_system_time(t: SystemTime) -> String {
    let duration = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let secs = duration.as_secs();
    // Simple ISO-ish format without pulling in chrono for now
    let days = secs / 86400;
    let remaining = secs % 86400;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    // Approximate date from epoch days — good enough for sorting and display
    // For proper formatting we'd use chrono, but this avoids the dependency for virtual fields
    let (year, month, day) = epoch_days_to_date(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        year, month, day, hours, minutes
    )
}

/// Today's calendar date in `YYYY-MM-DD` form. Used by
/// `FieldSchema::default_expr = today` and by any consumer that needs
/// a date matching the format vaultdb's virtual-field outputs use.
pub fn today_string() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86400;
    let (y, m, d) = epoch_days_to_date(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Wall-clock now in `YYYY-MM-DD HH:MM` form. Used by
/// `FieldSchema::default_expr = now`. Format matches the existing
/// `_modified` / `_created` virtual fields so values are sortable
/// together.
pub fn now_string() -> String {
    format_system_time(SystemTime::now())
}

/// Seconds since the Unix epoch. Used by
/// `FieldSchema::default_expr = epoch`.
pub fn epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn epoch_days_to_date(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

impl Value {
    /// Try to get a string reference.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Try to interpret as i64 (from Integer, or by parsing a String).
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(n) => Some(*n),
            Value::Float(f) => Some(*f as i64),
            Value::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Try to interpret as f64.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Integer(n) => Some(*n as f64),
            Value::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Check if this value (as a List) contains an item matching the needle string.
    pub fn list_contains(&self, needle: &str) -> bool {
        match self {
            Value::List(items) => items.iter().any(|item| item.display_value() == needle),
            Value::String(s) => s.contains(needle),
            _ => false,
        }
    }

    /// Human-readable type name.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::String(_) => "string",
            Value::Integer(_) => "integer",
            Value::Float(_) => "float",
            Value::Bool(_) => "bool",
            Value::List(_) => "list",
            Value::Map(_) => "map",
        }
    }

    /// Display-friendly string representation.
    pub fn display_value(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            Value::Integer(n) => n.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.display_value()).collect();
                parts.join(", ")
            }
            Value::Map(m) => {
                let parts: Vec<String> = m
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, v.display_value()))
                    .collect();
                parts.join(", ")
            }
        }
    }

    /// Whether this value is null or an empty collection.
    pub fn is_empty(&self) -> bool {
        match self {
            Value::Null => true,
            Value::String(s) => s.is_empty(),
            Value::List(l) => l.is_empty(),
            Value::Map(m) => m.is_empty(),
            _ => false,
        }
    }

    /// Returns the inner bool if this is `Value::Bool`, else `None`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Returns the inner list if this is `Value::List`, else `None`.
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(v) => Some(v),
            _ => None,
        }
    }

    /// Returns the inner map if this is `Value::Map`, else `None`.
    pub fn as_map(&self) -> Option<&std::collections::BTreeMap<String, Value>> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }

    /// Returns true if this value is `Value::Null`.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Best-effort parse of a CLI-style `FIELD=VALUE` scalar string into a
    /// typed `Value`. Tries bool literal (`true` / `false` exact match —
    /// case-sensitive to match YAML), then integer, then float, then falls
    /// back to a string. Shared by every frontend that accepts
    /// loosely-typed user input — CLI `--set`, MCP `plan_update`'s legacy
    /// string-set path, and `CreateBuilder` set-field via the CLI.
    pub fn parse_scalar(s: &str) -> Self {
        match s {
            "true" => return Value::Bool(true),
            "false" => return Value::Bool(false),
            _ => {}
        }
        if let Ok(i) = s.parse::<i64>() {
            return Value::Integer(i);
        }
        if let Ok(f) = s.parse::<f64>() {
            return Value::Float(f);
        }
        Value::String(s.to_string())
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_value())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_name_strips_extension() {
        let record = Record {
            path: PathBuf::from("/vault/3-Notes/TypeScript.md"),
            fields: BTreeMap::new(),
            raw_content: None,
        };
        assert_eq!(record.virtual_name(), "TypeScript");
    }

    #[test]
    fn virtual_name_handles_chinese() {
        let record = Record {
            path: PathBuf::from("/vault/3-Notes/快.md"),
            fields: BTreeMap::new(),
            raw_content: None,
        };
        assert_eq!(record.virtual_name(), "快");
    }

    #[test]
    fn virtual_path_relative_to_root() {
        let record = Record {
            path: PathBuf::from("/vault/3-Notes/TypeScript.md"),
            fields: BTreeMap::new(),
            raw_content: None,
        };
        assert_eq!(
            record.virtual_path(Path::new("/vault")),
            "3-Notes/TypeScript.md"
        );
    }

    #[test]
    fn virtual_folder() {
        let record = Record {
            path: PathBuf::from("/vault/3-Notes/TypeScript.md"),
            fields: BTreeMap::new(),
            raw_content: None,
        };
        assert_eq!(record.virtual_folder(), "3-Notes");
    }

    #[test]
    fn virtual_body_links_extracts_markdown_links() {
        let record = Record {
            path: PathBuf::from("/vault/notes/U.md"),
            fields: BTreeMap::new(),
            raw_content: Some(
                "---\ndb-table: university\n---\n## Admissions Links\n\
                 - [Apply](https://grad.example.edu/apply)\n\
                 - [Tuition](https://example.edu/tuition)\n"
                    .to_string(),
            ),
        };
        let v = record.get("_body_links", Path::new("/vault")).unwrap();
        let Value::List(items) = v else {
            panic!("expected a list");
        };
        assert_eq!(items.len(), 2);
        let Value::Map(first) = &items[0] else {
            panic!("expected a map");
        };
        assert_eq!(first.get("label"), Some(&Value::String("Apply".into())));
        assert_eq!(
            first.get("url"),
            Some(&Value::String("https://grad.example.edu/apply".into()))
        );
    }

    #[test]
    fn field_value_list_contains() {
        let val = Value::List(vec![
            Value::String("type/concept".into()),
            Value::String("topic/chinese".into()),
        ]);
        assert!(val.list_contains("topic/chinese"));
        assert!(!val.list_contains("topic/movies"));
    }

    #[test]
    fn field_value_string_contains_substring() {
        let val = Value::String("hello world".into());
        assert!(val.list_contains("world"));
    }

    #[test]
    fn field_value_type_names() {
        assert_eq!(Value::Null.type_name(), "null");
        assert_eq!(Value::Integer(5).type_name(), "integer");
        assert_eq!(Value::String("x".into()).type_name(), "string");
        assert_eq!(Value::List(vec![]).type_name(), "list");
    }

    #[test]
    fn field_value_numeric_coercion() {
        assert_eq!(Value::Integer(42).as_float(), Some(42.0));
        assert_eq!(Value::Float(3.5).as_integer(), Some(3));
        assert_eq!(Value::String("7".into()).as_integer(), Some(7));
        assert_eq!(Value::String("not a number".into()).as_integer(), None);
    }

    #[test]
    fn display_value_formatting() {
        assert_eq!(Value::Null.display_value(), "");
        assert_eq!(Value::Integer(2019).display_value(), "2019");
        assert_eq!(
            Value::List(vec![Value::String("a".into()), Value::String("b".into()),])
                .display_value(),
            "a, b"
        );
    }

    #[test]
    fn record_serializes_with_path_as_string_and_skips_raw_content() {
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("status".into(), Value::String("active".into()));
        let r = Record {
            path: std::path::PathBuf::from("/v/notes/a.md"),
            fields,
            raw_content: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("/v/notes/a.md"));
        assert!(json.contains("status"));
        assert!(!json.contains("raw_content"));
    }

    #[test]
    fn record_round_trips_through_serde() {
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("k".into(), Value::Integer(1));
        let r = Record {
            path: std::path::PathBuf::from("/v/x.md"),
            fields,
            raw_content: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: Record = serde_json::from_str(&json).unwrap();
        assert_eq!(back.path, r.path);
        assert_eq!(back.fields.get("k"), Some(&Value::Integer(1)));
        assert!(back.raw_content.is_none());
    }

    #[test]
    fn value_helpers_string() {
        let v = Value::String("hi".into());
        assert_eq!(v.as_str(), Some("hi"));
        assert_eq!(v.as_integer(), None);
        assert!(!v.is_null());
    }

    #[test]
    fn value_helpers_integer() {
        let v = Value::Integer(7);
        assert_eq!(v.as_integer(), Some(7));
        assert_eq!(v.as_float(), Some(7.0));
        assert!(!v.is_null());
    }

    #[test]
    fn value_helpers_float() {
        let v = Value::Float(1.5);
        assert_eq!(v.as_float(), Some(1.5));
    }

    #[test]
    fn value_helpers_bool() {
        let v = Value::Bool(true);
        assert_eq!(v.as_bool(), Some(true));
    }

    #[test]
    fn value_helpers_list() {
        let v = Value::List(vec![Value::Integer(1), Value::Integer(2)]);
        assert_eq!(v.as_list().map(|s| s.len()), Some(2));
    }

    #[test]
    fn value_helpers_map() {
        let mut m = std::collections::BTreeMap::new();
        m.insert("k".into(), Value::String("v".into()));
        let v = Value::Map(m);
        assert_eq!(v.as_map().map(|m| m.len()), Some(1));
    }

    #[test]
    fn value_helpers_null() {
        let v = Value::Null;
        assert!(v.is_null());
        assert_eq!(v.as_str(), None);
    }

    #[test]
    fn value_serializes_untagged() {
        let v = Value::List(vec![Value::Integer(1), Value::String("x".into())]);
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, r#"[1,"x"]"#);
    }

    #[test]
    fn value_deserializes_untagged() {
        let v: Value = serde_json::from_str(r#"[1,"x"]"#).unwrap();
        assert_eq!(
            v,
            Value::List(vec![Value::Integer(1), Value::String("x".into())])
        );
    }

    // ── parse_scalar coverage ─────────────────────────────────────────

    #[test]
    fn parse_scalar_bool_literals() {
        assert_eq!(Value::parse_scalar("true"), Value::Bool(true));
        assert_eq!(Value::parse_scalar("false"), Value::Bool(false));
    }

    #[test]
    fn parse_scalar_case_sensitive_for_bool() {
        // YAML's bool literals are lowercase only; mixed-case stays string
        // so that values like "True" or "FALSE" don't accidentally get
        // coerced to bool.
        assert_eq!(Value::parse_scalar("True"), Value::String("True".into()));
        assert_eq!(Value::parse_scalar("FALSE"), Value::String("FALSE".into()));
    }

    #[test]
    fn parse_scalar_integer_then_float_then_string() {
        assert_eq!(Value::parse_scalar("42"), Value::Integer(42));
        assert_eq!(Value::parse_scalar("3.5"), Value::Float(3.5));
        assert_eq!(Value::parse_scalar("hi"), Value::String("hi".into()));
    }
}
