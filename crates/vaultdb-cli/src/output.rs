use std::collections::BTreeSet;
use std::path::Path;

use comfy_table::{ContentArrangement, Table};

use crate::cli::OutputFormat;
use crate::links::LinkIndex;
use crate::record::{FieldValue, Record};

/// Format records for display.
pub fn format_records(
    records: &[Record],
    select: &[String],
    format: &OutputFormat,
    vault_root: &Path,
) -> String {
    format_records_with_links(records, select, format, vault_root, None)
}

/// Format records with optional link index for graph virtual fields.
pub fn format_records_with_links(
    records: &[Record],
    select: &[String],
    format: &OutputFormat,
    vault_root: &Path,
    link_index: Option<&LinkIndex>,
) -> String {
    let fields = if select.is_empty() {
        infer_fields(records)
    } else {
        select.to_vec()
    };

    match format {
        OutputFormat::Table => format_table(records, &fields, vault_root, link_index),
        OutputFormat::Json => format_json(records, &fields, vault_root, link_index),
        OutputFormat::Yaml => format_yaml(records, &fields, vault_root, link_index),
        OutputFormat::Csv => format_csv(records, &fields, vault_root, link_index),
    }
}

/// Infer which fields to display by collecting all non-null fields across records.
/// Always starts with _name.
fn infer_fields(records: &[Record]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    for record in records {
        for key in record.fields.keys() {
            seen.insert(key.clone());
        }
    }
    let mut fields = vec!["_name".to_string()];
    for key in seen {
        fields.push(key);
    }
    fields
}

fn format_table(
    records: &[Record],
    fields: &[String],
    vault_root: &Path,
    link_index: Option<&LinkIndex>,
) -> String {
    let mut table = Table::new();
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(fields);

    for record in records {
        let row: Vec<String> = fields
            .iter()
            .map(|f| {
                record
                    .get_with_links(f, vault_root, link_index)
                    .map(|v| truncate_display(&v, 60))
                    .unwrap_or_default()
            })
            .collect();
        table.add_row(row);
    }

    table.to_string()
}

fn format_json(
    records: &[Record],
    fields: &[String],
    vault_root: &Path,
    link_index: Option<&LinkIndex>,
) -> String {
    let items: Vec<serde_json::Value> = records
        .iter()
        .map(|r| {
            let mut map = serde_json::Map::new();
            for f in fields {
                let val = r
                    .get_with_links(f, vault_root, link_index)
                    .unwrap_or(FieldValue::Null);
                map.insert(f.clone(), field_value_to_json(&val));
            }
            serde_json::Value::Object(map)
        })
        .collect();

    serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string())
}

fn format_yaml(
    records: &[Record],
    fields: &[String],
    vault_root: &Path,
    link_index: Option<&LinkIndex>,
) -> String {
    let mut output = String::new();
    for record in records {
        output.push_str("---\n");
        for f in fields {
            let val = record
                .get_with_links(f, vault_root, link_index)
                .unwrap_or(FieldValue::Null);
            output.push_str(&format!("{}: {}\n", f, val.display_value()));
        }
    }
    output
}

fn format_csv(
    records: &[Record],
    fields: &[String],
    vault_root: &Path,
    link_index: Option<&LinkIndex>,
) -> String {
    let mut buf = Vec::new();
    {
        let mut wtr = csv::Writer::from_writer(&mut buf);
        wtr.write_record(fields).ok();
        for record in records {
            let row: Vec<String> = fields
                .iter()
                .map(|f| {
                    record
                        .get_with_links(f, vault_root, link_index)
                        .map(|v| v.display_value())
                        .unwrap_or_default()
                })
                .collect();
            wtr.write_record(&row).ok();
        }
        wtr.flush().ok();
    }
    String::from_utf8(buf).unwrap_or_default()
}

fn field_value_to_json(val: &FieldValue) -> serde_json::Value {
    match val {
        FieldValue::Null => serde_json::Value::Null,
        FieldValue::String(s) => serde_json::Value::String(s.clone()),
        FieldValue::Integer(n) => serde_json::json!(n),
        FieldValue::Float(f) => serde_json::json!(f),
        FieldValue::Bool(b) => serde_json::Value::Bool(*b),
        FieldValue::List(items) => {
            serde_json::Value::Array(items.iter().map(field_value_to_json).collect())
        }
        FieldValue::Map(m) => {
            let obj: serde_json::Map<String, serde_json::Value> = m
                .iter()
                .map(|(k, v)| (k.clone(), field_value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
    }
}

/// Truncate a display value for table cells.
fn truncate_display(val: &FieldValue, max_len: usize) -> String {
    let s = val.display_value();
    if s.chars().count() > max_len {
        let truncated: String = s.chars().take(max_len - 3).collect();
        format!("{}...", truncated)
    } else {
        s
    }
}
