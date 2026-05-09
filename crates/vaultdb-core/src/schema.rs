use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Result, VaultdbError};
use crate::record::FieldValue;

/// Top-level schema file structure.
#[derive(Debug, Serialize, Deserialize)]
pub struct VaultSchema {
    pub collections: BTreeMap<String, CollectionSchema>,
}

/// Schema for a single collection (a folder + optional filter).
#[derive(Debug, Serialize, Deserialize)]
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
#[derive(Debug, Serialize, Deserialize)]
pub struct FieldSchema {
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(rename = "enum")]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<serde_yaml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

/// Load schema from a file.
pub fn load_schema(path: &Path) -> Result<VaultSchema> {
    let content = std::fs::read_to_string(path).map_err(|_| {
        VaultdbError::SchemaError(format!("cannot read schema file: {}", path.display()))
    })?;
    let schema: VaultSchema = serde_yaml::from_str(&content)?;
    Ok(schema)
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
    fields: &BTreeMap<String, FieldValue>,
    schema: &CollectionSchema,
) -> Vec<Violation> {
    let mut violations = Vec::new();

    // Check required fields
    for req in &schema.required {
        match fields.get(req) {
            None | Some(FieldValue::Null) => {
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
            Some(v) if !matches!(v, FieldValue::Null) => v,
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
                serde_yaml::Value::String(s) => s == &display,
                serde_yaml::Value::Number(n) => n.to_string() == display,
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
                            .map(yaml_value_display)
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
    }

    violations
}

fn yaml_value_display(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Null => "null".to_string(),
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
        _ => true, // unknown type — don't enforce
    }
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

            if !matches!(
                value,
                FieldValue::Null | FieldValue::List(_) | FieldValue::Map(_)
            ) {
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
                        // Try to parse as number
                        if let Ok(n) = v.parse::<i64>() {
                            serde_yaml::Value::Number(serde_yaml::Number::from(n))
                        } else {
                            serde_yaml::Value::String(v)
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
                required: None,
            },
        );
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
    use crate::record::{FieldValue, Record};
    use std::path::PathBuf;

    fn make_record(fields: Vec<(&str, FieldValue)>) -> Record {
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

        let record = make_record(vec![("tags", FieldValue::String("x".into()))]);
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
                required: None,
            },
        );

        let schema = CollectionSchema {
            description: None,
            folder: "notes".into(),
            filter: vec![],
            required: vec![],
            fields,
        };

        let record = make_record(vec![("year", FieldValue::String("not a number".into()))]);
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
                    serde_yaml::Value::String("to-watch".into()),
                    serde_yaml::Value::String("watched".into()),
                ],
                min: None,
                max: None,
                required: None,
            },
        );

        let schema = CollectionSchema {
            description: None,
            folder: "notes".into(),
            filter: vec![],
            required: vec![],
            fields,
        };

        let record = make_record(vec![("status", FieldValue::String("invalid".into()))]);
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
                required: None,
            },
        );

        let schema = CollectionSchema {
            description: None,
            folder: "notes".into(),
            filter: vec![],
            required: vec![],
            fields,
        };

        let record = make_record(vec![("rating", FieldValue::Integer(15))]);
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
                enum_values: vec![serde_yaml::Value::String("to-watch".into())],
                min: None,
                max: None,
                required: None,
            },
        );

        let schema = CollectionSchema {
            description: None,
            folder: "notes".into(),
            filter: vec![],
            required: vec!["status".into()],
            fields,
        };

        let record = make_record(vec![("status", FieldValue::String("to-watch".into()))]);
        let violations = validate_record("test.md", &record.fields, &schema);
        assert!(violations.is_empty());
    }

    #[test]
    fn infer_schema_basic() {
        let records = vec![
            make_record(vec![
                ("status", FieldValue::String("active".into())),
                ("year", FieldValue::Integer(2020)),
            ]),
            make_record(vec![
                ("status", FieldValue::String("draft".into())),
                ("year", FieldValue::Integer(2021)),
            ]),
        ];

        let schema = infer_schema("notes", &records);
        assert_eq!(schema.fields.get("status").unwrap().field_type, "string");
        assert_eq!(schema.fields.get("year").unwrap().field_type, "integer");
        assert!(schema.required.contains(&"status".to_string()));
        assert!(schema.required.contains(&"year".to_string()));
    }
}
