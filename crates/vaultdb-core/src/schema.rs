//! Schema inference and validation. `infer_schema` walks records to discover
//! field types and cardinalities; `validate_record` checks a record against a
//! schema; `schema_to_yaml` renders a schema to YAML for persistence.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, VaultdbError};
use crate::record::Value;

/// Canonical filename for the persisted schema, relative to the vault root.
/// CLI and MCP both load `<vault>/vaultdb-schema.yaml` via this constant.
pub const SCHEMA_FILENAME: &str = "vaultdb-schema.yaml";

/// Resolve the schema file path for a vault root.
pub fn schema_path(vault_root: &Path) -> PathBuf {
    vault_root.join(SCHEMA_FILENAME)
}

/// Top-level schema file structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSchema {
    pub collections: BTreeMap<String, CollectionSchema>,
}

impl VaultSchema {
    /// Collections whose `folder` matches `folder` exactly, or is a path
    /// under it (e.g. when `folder = "Notes"`, also matches collections
    /// declared with `folder: Notes/movie`). Used by `schema show` /
    /// `schema validate` to scope queries to a folder, and by the MCP
    /// `schema_show` tool's optional folder filter.
    pub fn collections_for_folder<'a>(
        &'a self,
        folder: &str,
    ) -> Vec<(&'a String, &'a CollectionSchema)> {
        let prefix = format!("{}/", folder);
        self.collections
            .iter()
            .filter(|(_, c)| c.folder == folder || c.folder.starts_with(&prefix))
            .collect()
    }
}

/// Schema for a single collection (a folder + optional filter).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionSchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub folder: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filter: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, FieldSchema>,
}

/// Schema for a single field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSchema {
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(rename = "enum")]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Static default applied when a record is created without an explicit
    /// value for this field. Validated against `field_type` and `enum_values`
    /// at schema load time, so bad defaults fail loudly rather than silently
    /// landing in user files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// Dynamic default — one of the closed enum values `today`, `now`,
    /// `epoch`. Resolved at the moment a record is created, not at schema
    /// load. Mutually exclusive with `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_expr: Option<String>,
}

/// Recognised values for `FieldSchema::default_expr`. Any other value is
/// rejected at schema load time. Resolution happens elsewhere (in
/// `CreateBuilder`, Phase 3) — this enum is just the contract.
pub const DEFAULT_EXPRS: &[&str] = &["today", "now", "epoch"];

/// Load schema from a file.
///
/// Errors are mapped to `VaultdbError::SchemaError` with a human-readable
/// reason — the underlying YAML parser is an implementation detail and is
/// deliberately not exposed in the public error type, so consumers don't
/// transitively depend on whichever YAML crate vaultdb chooses today.
///
/// After parsing, every field's `default` and `default_expr` is validated:
/// - `default_expr` must be one of [`DEFAULT_EXPRS`].
/// - `default` literals must be compatible with `field_type`.
/// - `default` literals must satisfy `enum_values` when both are set.
/// - `default` and `default_expr` are mutually exclusive.
pub fn load_schema(path: &Path) -> Result<VaultSchema> {
    let content = std::fs::read_to_string(path).map_err(|_| {
        VaultdbError::SchemaError(format!("cannot read schema file: {}", path.display()))
    })?;
    let parsed: VaultSchema = serde_yaml::from_str(&content)
        .map_err(|e| VaultdbError::SchemaError(format!("parsing {}: {}", path.display(), e)))?;
    validate_schema_defaults(&parsed)?;
    Ok(parsed)
}

/// Walk every field in every collection and check that any declared
/// defaults are well-formed. Exposed publicly so consumers that build a
/// `VaultSchema` in code (not by loading a file) can run the same check.
pub fn validate_schema_defaults(schema: &VaultSchema) -> Result<()> {
    for (col_name, col) in &schema.collections {
        for (field_name, field) in &col.fields {
            validate_field_defaults(col_name, field_name, field)?;
        }
    }
    Ok(())
}

fn validate_field_defaults(col: &str, field: &str, schema: &FieldSchema) -> Result<()> {
    if schema.default.is_some() && schema.default_expr.is_some() {
        return Err(VaultdbError::SchemaError(format!(
            "collection '{}', field '{}': `default` and `default_expr` are mutually exclusive",
            col, field
        )));
    }

    if let Some(expr) = &schema.default_expr
        && !DEFAULT_EXPRS.contains(&expr.as_str())
    {
        return Err(VaultdbError::SchemaError(format!(
            "collection '{}', field '{}': default_expr '{}' is not recognised (expected one of {:?})",
            col, field, expr, DEFAULT_EXPRS
        )));
    }

    if let Some(val) = &schema.default {
        // Type compatibility check. Reuses `type_matches` so the rules
        // stay aligned with what `validate_record` enforces at runtime.
        let actual = val.type_name();
        if !type_matches(actual, &schema.field_type) {
            return Err(VaultdbError::SchemaError(format!(
                "collection '{}', field '{}': default has type '{}', incompatible with field type '{}'",
                col, field, actual, schema.field_type
            )));
        }

        // Format check for constrained string types. A bad `default:
        // 2024-99-99` should fail at schema load, not when a user
        // creates a note.
        if let Value::String(s) = val {
            let format_ok = match schema.field_type.as_str() {
                "wikilink" => is_valid_wikilink(s),
                "date" => is_valid_date(s),
                "url" => is_valid_url(s),
                _ => true,
            };
            if !format_ok {
                return Err(VaultdbError::SchemaError(format!(
                    "collection '{}', field '{}': default '{}' is not a valid {}",
                    col, field, s, schema.field_type
                )));
            }
        }

        // Enum compatibility.
        if !schema.enum_values.is_empty() {
            let display = val.display_value();
            let matches_enum = schema.enum_values.iter().any(|e| match e {
                Value::String(s) => s == &display,
                Value::Integer(i) => i.to_string() == display,
                Value::Float(f) => f.to_string() == display,
                Value::Bool(b) => b.to_string() == display,
                _ => false,
            });
            if !matches_enum {
                return Err(VaultdbError::SchemaError(format!(
                    "collection '{}', field '{}': default '{}' is not in `enum` values",
                    col, field, display
                )));
            }
        }
    }

    Ok(())
}

/// Serialize a schema to YAML string.
pub fn schema_to_yaml(schema: &VaultSchema) -> Result<String> {
    serde_yaml::to_string(schema)
        .map_err(|e| VaultdbError::SchemaError(format!("rendering schema as YAML: {}", e)))
}

/// A single validation violation.
#[derive(Debug)]
pub struct Violation {
    pub file: String,
    pub field: String,
    pub message: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {} — {}", self.file, self.field, self.message)
    }
}

/// Validate a record's fields against a collection schema.
pub fn validate_record(
    filename: &str,
    fields: &BTreeMap<String, Value>,
    schema: &CollectionSchema,
) -> Vec<Violation> {
    let mut violations = Vec::new();

    // Check required fields
    for req in &schema.required {
        match fields.get(req) {
            None | Some(Value::Null) => {
                violations.push(Violation {
                    file: filename.to_string(),
                    field: req.clone(),
                    message: "required field is missing or null".into(),
                });
            }
            _ => {}
        }
    }

    // Check field constraints
    for (field_name, field_schema) in &schema.fields {
        let value = match fields.get(field_name) {
            Some(v) if !matches!(v, Value::Null) => v,
            _ => continue, // skip absent/null fields (required check handles those)
        };

        // Type check
        let actual_type = value.type_name();
        let expected_type = &field_schema.field_type;
        if !type_matches(actual_type, expected_type) {
            violations.push(Violation {
                file: filename.to_string(),
                field: field_name.clone(),
                message: format!("expected type '{}', got '{}'", expected_type, actual_type),
            });
        }

        // Enum check
        if !field_schema.enum_values.is_empty() {
            let display = value.display_value();
            let matches_enum = field_schema.enum_values.iter().any(|e| match e {
                Value::String(s) => s == &display,
                Value::Integer(i) => i.to_string() == display,
                Value::Float(f) => f.to_string() == display,
                Value::Bool(b) => b.to_string() == display,
                _ => false,
            });
            if !matches_enum {
                violations.push(Violation {
                    file: filename.to_string(),
                    field: field_name.clone(),
                    message: format!(
                        "value '{}' not in allowed values: {:?}",
                        display,
                        field_schema
                            .enum_values
                            .iter()
                            .map(value_display)
                            .collect::<Vec<_>>()
                    ),
                });
            }
        }

        // Min/max check for numeric fields
        if let Some(min) = field_schema.min
            && let Some(num) = value.as_float()
            && num < min
        {
            violations.push(Violation {
                file: filename.to_string(),
                field: field_name.clone(),
                message: format!("value {} is below minimum {}", num, min),
            });
        }
        if let Some(max) = field_schema.max
            && let Some(num) = value.as_float()
            && num > max
        {
            violations.push(Violation {
                file: filename.to_string(),
                field: field_name.clone(),
                message: format!("value {} exceeds maximum {}", num, max),
            });
        }

        // Format checks for the "string-shaped but constrained" types.
        // These don't introduce new Value variants — on disk they're
        // still YAML strings — but validate_record refuses values that
        // don't match the expected format.
        if let Value::String(s) = value {
            match expected_type.as_str() {
                "wikilink" => {
                    if !is_valid_wikilink(s) {
                        violations.push(Violation {
                            file: filename.to_string(),
                            field: field_name.clone(),
                            message: format!(
                                "value '{}' is not a valid wikilink; expected [[name]], [[name|alias]], [[name#section]], or [[name#section|alias]]",
                                s
                            ),
                        });
                    }
                }
                "date" => {
                    if !is_valid_date(s) {
                        violations.push(Violation {
                            file: filename.to_string(),
                            field: field_name.clone(),
                            message: format!(
                                "value '{}' is not a valid date; expected YYYY-MM-DD",
                                s
                            ),
                        });
                    }
                }
                "url" => {
                    if !is_valid_url(s) {
                        violations.push(Violation {
                            file: filename.to_string(),
                            field: field_name.clone(),
                            message: format!("value '{}' is not a valid URL", s),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    violations
}

fn value_display(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        other => format!("{:?}", other),
    }
}

fn type_matches(actual: &str, expected: &str) -> bool {
    match expected {
        "string" => actual == "string",
        "integer" => actual == "integer",
        "float" => actual == "float" || actual == "integer",
        "number" => actual == "integer" || actual == "float",
        "bool" => actual == "bool",
        "list" => actual == "list",
        "map" => actual == "map",
        // Constrained string types: stored as YAML strings on disk,
        // distinguished from plain string by a format check applied
        // after the type check in `validate_record`.
        "wikilink" | "date" | "url" => actual == "string",
        _ => true, // unknown type — don't enforce
    }
}

/// True if `s` is a syntactically valid wikilink: `[[target]]`, with
/// optional `|alias` and/or `#section`. Target must be non-empty and
/// must not contain `]]`. Does NOT verify the target exists in the
/// vault — that requires a `LinkGraph` and is out of scope for v1.
pub fn is_valid_wikilink(s: &str) -> bool {
    let inner = match s.strip_prefix("[[").and_then(|x| x.strip_suffix("]]")) {
        Some(i) => i,
        None => return false,
    };
    // Brackets are the outer delimiters only — anything inside that
    // contains `[` or `]` is malformed (e.g. `[[a][b]]`).
    if inner.is_empty() || inner.contains('[') || inner.contains(']') {
        return false;
    }
    // Target is everything up to the first `|` or `#`.
    let target_end = inner.find(['|', '#']).unwrap_or(inner.len());
    !inner[..target_end].trim().is_empty()
}

/// True if `s` parses as a calendar date in `YYYY-MM-DD` form, with the
/// month and day in valid ranges. Does NOT do leap-year validation —
/// `2024-02-30` passes today; a stricter check can come in Phase 8 if
/// the false-negative cost ever materialises.
pub fn is_valid_date(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return false;
    }
    if parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return false;
    }
    let year = parts[0].parse::<u32>();
    let month = parts[1].parse::<u32>();
    let day = parts[2].parse::<u32>();
    match (year, month, day) {
        (Ok(_), Ok(m), Ok(d)) => (1..=12).contains(&m) && (1..=31).contains(&d),
        _ => false,
    }
}

/// True if `s` parses as an absolute URL (i.e. has a scheme like
/// `https`, `http`, `mailto`, `file`, …). Relative URLs are rejected
/// on purpose — a vault field declared `type: url` should be navigable
/// on its own.
pub fn is_valid_url(s: &str) -> bool {
    url::Url::parse(s).is_ok()
}

/// Infer a schema from a set of records.
pub fn infer_schema(folder_name: &str, records: &[crate::record::Record]) -> CollectionSchema {
    let mut field_types: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut field_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut field_count: BTreeMap<String, usize> = BTreeMap::new();
    let total = records.len();

    for record in records {
        for (key, value) in &record.fields {
            let type_name = value.type_name().to_string();
            *field_types
                .entry(key.clone())
                .or_default()
                .entry(type_name)
                .or_insert(0) += 1;
            *field_count.entry(key.clone()).or_insert(0) += 1;

            if !matches!(value, Value::Null | Value::List(_) | Value::Map(_)) {
                field_values
                    .entry(key.clone())
                    .or_default()
                    .push(value.display_value());
            }
        }
    }

    let mut fields = BTreeMap::new();
    let mut required = Vec::new();

    for (key, types) in &field_types {
        // Determine the dominant type
        let dominant_type = types
            .iter()
            .filter(|(t, _)| *t != "null")
            .max_by_key(|(_, count)| *count)
            .map(|(t, _)| t.clone())
            .unwrap_or_else(|| "string".to_string());

        // Check if field is present in all records with non-null values
        let non_null_count = types
            .iter()
            .filter(|(t, _)| *t != "null")
            .map(|(_, c)| c)
            .sum::<usize>();

        if non_null_count == total && total > 0 {
            required.push(key.clone());
        }

        // Infer enum if there are few unique values
        let enum_values = if let Some(values) = field_values.get(key) {
            let mut unique: Vec<String> = values.clone();
            unique.sort();
            unique.dedup();
            if unique.len() <= 10 && unique.len() < values.len() / 2 {
                unique
                    .into_iter()
                    .map(|v| {
                        // Try to parse as integer
                        if let Ok(n) = v.parse::<i64>() {
                            Value::Integer(n)
                        } else {
                            Value::String(v)
                        }
                    })
                    .collect()
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        fields.insert(
            key.clone(),
            FieldSchema {
                field_type: dominant_type,
                enum_values,
                min: None,
                max: None,
                default: None,
                default_expr: None,
            },
        );
        // `required` is tracked at the collection level (above); kept out
        // of `FieldSchema` deliberately so there's a single source of truth.
    }

    CollectionSchema {
        description: Some(format!("Auto-inferred schema for {}", folder_name)),
        folder: folder_name.to_string(),
        filter: vec![],
        required,
        fields,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Record, Value};
    use std::path::PathBuf;

    fn make_record(fields: Vec<(&str, Value)>) -> Record {
        let mut map = BTreeMap::new();
        for (k, v) in fields {
            map.insert(k.to_string(), v);
        }
        Record {
            path: PathBuf::from("/vault/notes/test.md"),
            fields: map,
            raw_content: None,
        }
    }

    #[test]
    fn validate_required_field_missing() {
        let schema = CollectionSchema {
            description: None,
            folder: "notes".into(),
            filter: vec![],
            required: vec!["status".into()],
            fields: BTreeMap::new(),
        };

        let record = make_record(vec![("tags", Value::String("x".into()))]);
        let violations = validate_record("test.md", &record.fields, &schema);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("required"));
    }

    #[test]
    fn validate_type_mismatch() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "year".into(),
            FieldSchema {
                field_type: "integer".into(),
                enum_values: vec![],
                min: None,
                max: None,
                default: None,
                default_expr: None,
            },
        );

        let schema = CollectionSchema {
            description: None,
            folder: "notes".into(),
            filter: vec![],
            required: vec![],
            fields,
        };

        let record = make_record(vec![("year", Value::String("not a number".into()))]);
        let violations = validate_record("test.md", &record.fields, &schema);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("type"));
    }

    #[test]
    fn validate_enum_violation() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "status".into(),
            FieldSchema {
                field_type: "string".into(),
                enum_values: vec![
                    Value::String("to-watch".into()),
                    Value::String("watched".into()),
                ],
                min: None,
                max: None,
                default: None,
                default_expr: None,
            },
        );

        let schema = CollectionSchema {
            description: None,
            folder: "notes".into(),
            filter: vec![],
            required: vec![],
            fields,
        };

        let record = make_record(vec![("status", Value::String("invalid".into()))]);
        let violations = validate_record("test.md", &record.fields, &schema);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("not in allowed"));
    }

    #[test]
    fn validate_min_max() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "rating".into(),
            FieldSchema {
                field_type: "number".into(),
                enum_values: vec![],
                min: Some(1.0),
                max: Some(10.0),
                default: None,
                default_expr: None,
            },
        );

        let schema = CollectionSchema {
            description: None,
            folder: "notes".into(),
            filter: vec![],
            required: vec![],
            fields,
        };

        let record = make_record(vec![("rating", Value::Integer(15))]);
        let violations = validate_record("test.md", &record.fields, &schema);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("exceeds maximum"));
    }

    #[test]
    fn validate_passes_clean_record() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "status".into(),
            FieldSchema {
                field_type: "string".into(),
                enum_values: vec![Value::String("to-watch".into())],
                min: None,
                max: None,
                default: None,
                default_expr: None,
            },
        );

        let schema = CollectionSchema {
            description: None,
            folder: "notes".into(),
            filter: vec![],
            required: vec!["status".into()],
            fields,
        };

        let record = make_record(vec![("status", Value::String("to-watch".into()))]);
        let violations = validate_record("test.md", &record.fields, &schema);
        assert!(violations.is_empty());
    }

    #[test]
    fn infer_schema_basic() {
        let records = vec![
            make_record(vec![
                ("status", Value::String("active".into())),
                ("year", Value::Integer(2020)),
            ]),
            make_record(vec![
                ("status", Value::String("draft".into())),
                ("year", Value::Integer(2021)),
            ]),
        ];

        let schema = infer_schema("notes", &records);
        assert_eq!(schema.fields.get("status").unwrap().field_type, "string");
        assert_eq!(schema.fields.get("year").unwrap().field_type, "integer");
        assert!(schema.required.contains(&"status".to_string()));
        assert!(schema.required.contains(&"year".to_string()));
    }

    // ── Phase 1: wikilink / date / url type validation ────────────────

    fn schema_with_field(name: &str, field_type: &str) -> CollectionSchema {
        let mut fields = BTreeMap::new();
        fields.insert(
            name.into(),
            FieldSchema {
                field_type: field_type.into(),
                enum_values: vec![],
                min: None,
                max: None,
                default: None,
                default_expr: None,
            },
        );
        CollectionSchema {
            description: None,
            folder: "notes".into(),
            filter: vec![],
            required: vec![],
            fields,
        }
    }

    #[test]
    fn wikilink_accepts_plain() {
        assert!(is_valid_wikilink("[[name]]"));
        assert!(is_valid_wikilink("[[kyoto-university-kyoto-yoshida-KG9l]]"));
    }

    #[test]
    fn wikilink_accepts_alias_and_section() {
        assert!(is_valid_wikilink("[[name|alias]]"));
        assert!(is_valid_wikilink("[[name#section]]"));
        assert!(is_valid_wikilink("[[name#section|alias]]"));
    }

    #[test]
    fn wikilink_rejects_malformed() {
        assert!(!is_valid_wikilink("name"));
        assert!(!is_valid_wikilink("[name]"));
        assert!(!is_valid_wikilink("[[]]"));
        assert!(!is_valid_wikilink("[[  ]]"));
        assert!(!is_valid_wikilink("[[a][b]]"));
    }

    #[test]
    fn validate_wikilink_field_catches_bad_value() {
        let schema = schema_with_field("university", "wikilink");
        let record = make_record(vec![("university", Value::String("kyoto".into()))]);
        let violations = validate_record("p.md", &record.fields, &schema);
        assert_eq!(violations.len(), 1, "{:?}", violations);
        assert!(violations[0].message.contains("wikilink"));
    }

    #[test]
    fn validate_wikilink_field_passes_good_value() {
        let schema = schema_with_field("university", "wikilink");
        let record = make_record(vec![(
            "university",
            Value::String("[[kyoto-university-KG9l]]".into()),
        )]);
        let violations = validate_record("p.md", &record.fields, &schema);
        assert!(violations.is_empty(), "{:?}", violations);
    }

    #[test]
    fn date_accepts_iso_calendar() {
        assert!(is_valid_date("2024-05-13"));
        assert!(is_valid_date("1999-01-01"));
        assert!(is_valid_date("2030-12-31"));
    }

    #[test]
    fn date_rejects_garbage_and_wrong_components() {
        assert!(!is_valid_date("not-a-date"));
        assert!(!is_valid_date("2024/05/13"));
        assert!(!is_valid_date("2024-13-01")); // bad month
        assert!(!is_valid_date("2024-00-15")); // bad month
        assert!(!is_valid_date("2024-05-32")); // bad day
        assert!(!is_valid_date("24-05-13")); // 2-digit year
        assert!(!is_valid_date("2024-5-13")); // 1-digit month
    }

    #[test]
    fn validate_date_field_catches_bad_value() {
        let schema = schema_with_field("due", "date");
        let record = make_record(vec![("due", Value::String("not-a-date".into()))]);
        let violations = validate_record("t.md", &record.fields, &schema);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("date"));
    }

    #[test]
    fn validate_date_field_passes_good_value() {
        let schema = schema_with_field("due", "date");
        let record = make_record(vec![("due", Value::String("2024-05-13".into()))]);
        let violations = validate_record("t.md", &record.fields, &schema);
        assert!(violations.is_empty());
    }

    #[test]
    fn url_accepts_common_schemes() {
        assert!(is_valid_url("https://example.com"));
        assert!(is_valid_url("http://example.com/path?q=1"));
        assert!(is_valid_url("mailto:a@b.com"));
        assert!(is_valid_url("file:///tmp/x"));
    }

    #[test]
    fn url_rejects_garbage_and_relative() {
        assert!(!is_valid_url("not a url"));
        assert!(!is_valid_url("/relative/path"));
        assert!(!is_valid_url("example.com")); // no scheme
    }

    #[test]
    fn validate_url_field_catches_bad_value() {
        let schema = schema_with_field("homepage", "url");
        let record = make_record(vec![("homepage", Value::String("example.com".into()))]);
        let violations = validate_record("p.md", &record.fields, &schema);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("URL"));
    }

    #[test]
    fn validate_url_field_passes_good_value() {
        let schema = schema_with_field("homepage", "url");
        let record = make_record(vec![(
            "homepage",
            Value::String("https://example.com".into()),
        )]);
        let violations = validate_record("p.md", &record.fields, &schema);
        assert!(violations.is_empty());
    }

    #[test]
    fn constrained_type_still_requires_string_actual() {
        // A wikilink-typed field receiving an integer should fail the
        // type check, not just the wikilink-format check.
        let schema = schema_with_field("university", "wikilink");
        let record = make_record(vec![("university", Value::Integer(42))]);
        let violations = validate_record("p.md", &record.fields, &schema);
        assert!(violations.iter().any(|v| v.message.contains("type")));
    }

    // ── Phase 2: default / default_expr validation at load time ────────

    fn schema_with_defaulted_field(
        name: &str,
        field_type: &str,
        default: Option<Value>,
        default_expr: Option<String>,
        enum_values: Vec<Value>,
    ) -> VaultSchema {
        let mut fields = BTreeMap::new();
        fields.insert(
            name.into(),
            FieldSchema {
                field_type: field_type.into(),
                enum_values,
                min: None,
                max: None,
                default,
                default_expr,
            },
        );
        VaultSchema {
            collections: BTreeMap::from([(
                "movies".to_string(),
                CollectionSchema {
                    description: None,
                    folder: "Notes/movie".into(),
                    filter: vec![],
                    required: vec![],
                    fields,
                },
            )]),
        }
    }

    #[test]
    fn default_literal_matching_type_passes() {
        let s = schema_with_defaulted_field(
            "year",
            "integer",
            Some(Value::Integer(2024)),
            None,
            vec![],
        );
        assert!(validate_schema_defaults(&s).is_ok());
    }

    #[test]
    fn default_literal_wrong_type_rejected() {
        let s = schema_with_defaulted_field(
            "year",
            "integer",
            Some(Value::String("nope".into())),
            None,
            vec![],
        );
        let err = validate_schema_defaults(&s).unwrap_err().to_string();
        assert!(err.contains("incompatible"), "got: {}", err);
        assert!(err.contains("year"));
    }

    #[test]
    fn default_literal_outside_enum_rejected() {
        let s = schema_with_defaulted_field(
            "status",
            "string",
            Some(Value::String("invalid".into())),
            None,
            vec![
                Value::String("to-watch".into()),
                Value::String("watched".into()),
            ],
        );
        let err = validate_schema_defaults(&s).unwrap_err().to_string();
        assert!(err.contains("enum"), "got: {}", err);
    }

    #[test]
    fn default_literal_inside_enum_passes() {
        let s = schema_with_defaulted_field(
            "status",
            "string",
            Some(Value::String("to-watch".into())),
            None,
            vec![
                Value::String("to-watch".into()),
                Value::String("watched".into()),
            ],
        );
        assert!(validate_schema_defaults(&s).is_ok());
    }

    #[test]
    fn default_expr_known_keyword_passes() {
        for expr in DEFAULT_EXPRS {
            let s =
                schema_with_defaulted_field("due", "date", None, Some(expr.to_string()), vec![]);
            assert!(
                validate_schema_defaults(&s).is_ok(),
                "default_expr '{}' should be valid",
                expr
            );
        }
    }

    #[test]
    fn default_expr_unknown_keyword_rejected() {
        let s = schema_with_defaulted_field("due", "date", None, Some("tomorrow".into()), vec![]);
        let err = validate_schema_defaults(&s).unwrap_err().to_string();
        assert!(err.contains("default_expr"), "got: {}", err);
        assert!(err.contains("tomorrow"));
    }

    #[test]
    fn default_and_default_expr_mutually_exclusive() {
        let s = schema_with_defaulted_field(
            "due",
            "date",
            Some(Value::String("2024-05-13".into())),
            Some("today".into()),
            vec![],
        );
        let err = validate_schema_defaults(&s).unwrap_err().to_string();
        assert!(err.contains("mutually exclusive"), "got: {}", err);
    }

    #[test]
    fn default_for_wikilink_must_be_well_formed() {
        let s = schema_with_defaulted_field(
            "university",
            "wikilink",
            Some(Value::String("kyoto".into())),
            None,
            vec![],
        );
        let err = validate_schema_defaults(&s).unwrap_err().to_string();
        assert!(err.contains("wikilink"), "got: {}", err);

        // And the right shape passes.
        let s = schema_with_defaulted_field(
            "university",
            "wikilink",
            Some(Value::String("[[kyoto-university-KG9l]]".into())),
            None,
            vec![],
        );
        assert!(validate_schema_defaults(&s).is_ok());
    }

    #[test]
    fn default_for_date_must_be_well_formed() {
        let s = schema_with_defaulted_field(
            "due",
            "date",
            Some(Value::String("2024-99-99".into())),
            None,
            vec![],
        );
        let err = validate_schema_defaults(&s).unwrap_err().to_string();
        assert!(err.contains("date"), "got: {}", err);
    }

    #[test]
    fn load_schema_runs_default_validation() {
        // End-to-end: write a YAML with a bad default to disk, load it,
        // and verify we get a SchemaError pointing at the field.
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            r#"
collections:
  movies:
    folder: Notes/movie
    fields:
      year:
        type: integer
        default: "not an integer"
"#
        )
        .unwrap();
        let err = load_schema(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("year"), "got: {}", err);
        assert!(err.contains("incompatible"), "got: {}", err);
    }
}
