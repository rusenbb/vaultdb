//! stdout rendering for the CLI's read commands.
//!
//! The `Table` format (default, comfy-table pretty-printer) is local —
//! it's only useful when writing to a terminal, not as a file format.
//! Every other format delegates to [`vaultdb_core::render`], which is
//! also what `--output` and the MCP `export` parameter use. One renderer,
//! three call sites.

use std::collections::BTreeSet;
use std::path::Path;

use comfy_table::{ContentArrangement, Table};

use crate::cli::OutputFormat;
use vaultdb_core::links::LinkGraph;
use vaultdb_core::record::{Record, Value};
use vaultdb_core::render;

/// Format records for stdout display.
///
/// Table uses comfy-table here (truncates long cells for terminal
/// width); the other formats delegate to `render::render_records` so
/// the bytes match exactly what `--output foo.{json,csv,yaml}` would
/// have written.
pub fn format_records_with_links(
    records: &[Record],
    select: &[String],
    format: &OutputFormat,
    vault_root: &Path,
    link_index: Option<&LinkGraph>,
) -> String {
    let fields = if select.is_empty() {
        infer_fields(records)
    } else {
        select.to_vec()
    };

    match format {
        OutputFormat::Table => format_table(records, &fields, vault_root, link_index),
        OutputFormat::Json => bytes_to_string(render::render_records(
            records,
            &fields,
            vault_root,
            link_index,
            render::Format::Json,
        )),
        OutputFormat::Yaml => bytes_to_string(render::render_records(
            records,
            &fields,
            vault_root,
            link_index,
            render::Format::Yaml,
        )),
        OutputFormat::Csv => bytes_to_string(render::render_records(
            records,
            &fields,
            vault_root,
            link_index,
            render::Format::Csv { delimiter: b',' },
        )),
    }
}

fn bytes_to_string(result: vaultdb_core::Result<Vec<u8>>) -> String {
    match result {
        Ok(bytes) => String::from_utf8(bytes).unwrap_or_default(),
        Err(e) => format!("render error: {}", e),
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
        if key != "_name" {
            fields.push(key);
        }
    }
    fields
}

fn format_table(
    records: &[Record],
    fields: &[String],
    vault_root: &Path,
    link_index: Option<&LinkGraph>,
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

/// Truncate a display value for table cells.
fn truncate_display(val: &Value, max_len: usize) -> String {
    let s = val.display_value();
    if s.chars().count() > max_len {
        let truncated: String = s.chars().take(max_len - 3).collect();
        format!("{}...", truncated)
    } else {
        s
    }
}
