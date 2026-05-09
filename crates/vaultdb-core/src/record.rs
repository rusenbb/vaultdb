use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A value from YAML frontmatter, preserving type information.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Null,
    String(String),
    Integer(i64),
    Float(f64),
    Bool(bool),
    List(Vec<FieldValue>),
    Map(BTreeMap<String, FieldValue>),
}

/// One parsed .md file = one record.
#[derive(Debug, Clone)]
pub struct Record {
    /// Absolute path to the .md file.
    pub path: PathBuf,
    /// Parsed frontmatter fields.
    pub fields: BTreeMap<String, FieldValue>,
    /// Raw file content — only loaded for write operations.
    pub raw_content: Option<String>,
}

impl Record {
    /// Look up a field by name, checking virtual fields first.
    pub fn get(&self, key: &str, vault_root: &Path) -> Option<FieldValue> {
        self.get_with_links(key, vault_root, None)
    }

    /// Look up a field, including graph virtual fields when a link index is provided.
    pub fn get_with_links(
        &self,
        key: &str,
        vault_root: &Path,
        link_index: Option<&crate::links::LinkIndex>,
    ) -> Option<FieldValue> {
        match key {
            "_name" => Some(FieldValue::String(self.virtual_name())),
            "_path" => Some(FieldValue::String(self.virtual_path(vault_root))),
            "_folder" => Some(FieldValue::String(self.virtual_folder())),
            "_modified" => self.virtual_modified().map(FieldValue::String),
            "_created" => self.virtual_created().map(FieldValue::String),
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
                Some(FieldValue::Integer(content.len() as i64))
            }
            "_body_length" => {
                let content = self.load_content();
                let body_len = crate::frontmatter::extract_frontmatter(&content)
                    .map(|(_, body_start)| content[body_start..].trim().len())
                    .unwrap_or(content.trim().len());
                Some(FieldValue::Integer(body_len as i64))
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
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                    continue;
                }
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

impl FieldValue {
    /// Try to get a string reference.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            FieldValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// Try to interpret as i64 (from Integer, or by parsing a String).
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            FieldValue::Integer(n) => Some(*n),
            FieldValue::Float(f) => Some(*f as i64),
            FieldValue::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Try to interpret as f64.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            FieldValue::Float(f) => Some(*f),
            FieldValue::Integer(n) => Some(*n as f64),
            FieldValue::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Check if this value (as a List) contains an item matching the needle string.
    pub fn list_contains(&self, needle: &str) -> bool {
        match self {
            FieldValue::List(items) => items.iter().any(|item| item.display_value() == needle),
            FieldValue::String(s) => s.contains(needle),
            _ => false,
        }
    }

    /// Human-readable type name.
    pub fn type_name(&self) -> &'static str {
        match self {
            FieldValue::Null => "null",
            FieldValue::String(_) => "string",
            FieldValue::Integer(_) => "integer",
            FieldValue::Float(_) => "float",
            FieldValue::Bool(_) => "bool",
            FieldValue::List(_) => "list",
            FieldValue::Map(_) => "map",
        }
    }

    /// Display-friendly string representation.
    pub fn display_value(&self) -> String {
        match self {
            FieldValue::Null => String::new(),
            FieldValue::String(s) => s.clone(),
            FieldValue::Integer(n) => n.to_string(),
            FieldValue::Float(f) => f.to_string(),
            FieldValue::Bool(b) => b.to_string(),
            FieldValue::List(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.display_value()).collect();
                parts.join(", ")
            }
            FieldValue::Map(m) => {
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
            FieldValue::Null => true,
            FieldValue::String(s) => s.is_empty(),
            FieldValue::List(l) => l.is_empty(),
            FieldValue::Map(m) => m.is_empty(),
            _ => false,
        }
    }
}

impl std::fmt::Display for FieldValue {
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
    fn field_value_list_contains() {
        let val = FieldValue::List(vec![
            FieldValue::String("type/concept".into()),
            FieldValue::String("topic/chinese".into()),
        ]);
        assert!(val.list_contains("topic/chinese"));
        assert!(!val.list_contains("topic/movies"));
    }

    #[test]
    fn field_value_string_contains_substring() {
        let val = FieldValue::String("hello world".into());
        assert!(val.list_contains("world"));
    }

    #[test]
    fn field_value_type_names() {
        assert_eq!(FieldValue::Null.type_name(), "null");
        assert_eq!(FieldValue::Integer(5).type_name(), "integer");
        assert_eq!(FieldValue::String("x".into()).type_name(), "string");
        assert_eq!(FieldValue::List(vec![]).type_name(), "list");
    }

    #[test]
    fn field_value_numeric_coercion() {
        assert_eq!(FieldValue::Integer(42).as_float(), Some(42.0));
        assert_eq!(FieldValue::Float(3.14).as_integer(), Some(3));
        assert_eq!(FieldValue::String("7".into()).as_integer(), Some(7));
        assert_eq!(FieldValue::String("not a number".into()).as_integer(), None);
    }

    #[test]
    fn display_value_formatting() {
        assert_eq!(FieldValue::Null.display_value(), "");
        assert_eq!(FieldValue::Integer(2019).display_value(), "2019");
        assert_eq!(
            FieldValue::List(vec![
                FieldValue::String("a".into()),
                FieldValue::String("b".into()),
            ])
            .display_value(),
            "a, b"
        );
    }
}
