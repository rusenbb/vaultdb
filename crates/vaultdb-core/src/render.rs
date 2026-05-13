//! Export rendering: serialize query results into CSV, JSON, YAML, or XLSX
//! and atomically write them to a vault-scoped path.
//!
//! This module is the single source of truth for what an exported file
//! looks like. The CLI's `--output` flag and the MCP read tools' `export`
//! parameter both delegate here, which keeps the wire shape identical
//! across consumers and lets the pyo3 / wasm bindings get the same
//! renderers for free.
//!
//! The XLSX backend is feature-gated behind `xlsx` (default off) so
//! consumers that don't need spreadsheet output don't pay for
//! `rust_xlsxwriter` and its dependency tree.
//!
//! ## Path safety
//!
//! All exports are written **inside the vault root**. Absolute paths,
//! `..` components, and symlink escapes are rejected. On filename
//! collision, the renderer auto-suffixes `(1)`, `(2)`, ... — there is no
//! overwrite mode, by design.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use crate::error::{Result, VaultdbError};
use crate::links::LinkGraph;
use crate::record::{Record, Value};

// ── Format ─────────────────────────────────────────────────────────────

/// Export output format. CSV carries the delimiter byte so callers can
/// pick comma / semicolon / tab without a separate enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Comma-separated values. The delimiter is configurable: `b','` for
    /// RFC 4180, `b';'` for European-style CSV, `b'\t'` for TSV.
    Csv { delimiter: u8 },
    /// Excel 2007+ `.xlsx`. Available only when vaultdb-core is built
    /// with the `xlsx` feature.
    #[cfg(feature = "xlsx")]
    Xlsx,
    /// Pretty-printed JSON.
    Json,
    /// YAML.
    Yaml,
}

impl Format {
    /// Infer the format from a path's extension.
    ///
    /// - `.csv` → `Csv { delimiter: b',' }`
    /// - `.tsv` → `Csv { delimiter: b'\t' }`
    /// - `.json` → `Json`
    /// - `.yaml` / `.yml` → `Yaml`
    /// - `.xlsx` → `Xlsx` (requires `xlsx` feature)
    /// - `.md` → refused (`.md` is the vault's note format, not an export format)
    /// - everything else → error listing the supported extensions
    pub fn from_path(path: &Path) -> Result<Self> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        match ext.as_deref() {
            Some("csv") => Ok(Format::Csv { delimiter: b',' }),
            Some("tsv") => Ok(Format::Csv { delimiter: b'\t' }),
            Some("json") => Ok(Format::Json),
            Some("yaml") | Some("yml") => Ok(Format::Yaml),
            #[cfg(feature = "xlsx")]
            Some("xlsx") => Ok(Format::Xlsx),
            #[cfg(not(feature = "xlsx"))]
            Some("xlsx") => Err(VaultdbError::SafetyRefused {
                reason:
                    "xlsx export unavailable: vaultdb-core was built without the 'xlsx' feature"
                        .into(),
            }),
            Some("md") => Err(VaultdbError::SafetyRefused {
                reason: format!(
                    "refusing to export to '{}': .md is the vault's note format, not an export format",
                    path.display()
                ),
            }),
            Some(other) => Err(VaultdbError::SafetyRefused {
                reason: format!(
                    "unsupported export extension '.{}'; expected one of: csv, tsv, json, yaml, yml, xlsx",
                    other
                ),
            }),
            None => Err(VaultdbError::SafetyRefused {
                reason: format!(
                    "export path '{}' has no extension; expected one of: csv, tsv, json, yaml, yml, xlsx",
                    path.display()
                ),
            }),
        }
    }

    /// Override the CSV delimiter. No-op on non-CSV formats.
    pub fn with_csv_delimiter(self, delimiter: u8) -> Self {
        match self {
            Format::Csv { .. } => Format::Csv { delimiter },
            other => other,
        }
    }
}

// ── Path safety ────────────────────────────────────────────────────────

/// Resolve a requested export path against the vault root, enforcing the
/// vault sandbox and auto-suffixing on filename collision.
///
/// Rules:
/// - Path must be vault-relative (not absolute).
/// - No `..` components.
/// - Cannot have a `.md` extension (vault's own format).
/// - After joining + canonicalizing the parent, the path must stay under
///   the canonical vault root (catches symlink escapes).
/// - If a file at the target path already exists, the returned path is
///   `<stem> (1).<ext>`, then `<stem> (2).<ext>`, etc. (macOS-style).
///
/// Returns the resolved absolute path where the export should be written.
/// The parent directory is created as a side effect.
pub fn resolve_export_path(vault_root: &Path, requested: &Path) -> Result<PathBuf> {
    if requested.as_os_str().is_empty() {
        return Err(VaultdbError::SafetyRefused {
            reason: "export path is empty".into(),
        });
    }
    if requested.is_absolute() {
        return Err(VaultdbError::SafetyRefused {
            reason: format!(
                "export path must be vault-relative, got absolute path: {}",
                requested.display()
            ),
        });
    }
    for component in requested.components() {
        if matches!(component, Component::ParentDir) {
            return Err(VaultdbError::SafetyRefused {
                reason: format!("export path must not contain '..': {}", requested.display()),
            });
        }
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            return Err(VaultdbError::SafetyRefused {
                reason: format!(
                    "export path must be vault-relative: {}",
                    requested.display()
                ),
            });
        }
    }
    let ext_lower = requested
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    if ext_lower.as_deref() == Some("md") {
        return Err(VaultdbError::SafetyRefused {
            reason: format!(
                "refusing to export to '{}': use a non-.md extension (csv, tsv, json, yaml, xlsx)",
                requested.display()
            ),
        });
    }

    let candidate = vault_root.join(requested);
    let parent = candidate
        .parent()
        .ok_or_else(|| VaultdbError::SafetyRefused {
            reason: format!("export path has no parent: {}", requested.display()),
        })?;
    let file_name = candidate
        .file_name()
        .ok_or_else(|| VaultdbError::SafetyRefused {
            reason: format!("export path has no filename: {}", requested.display()),
        })?
        .to_owned();

    std::fs::create_dir_all(parent)?;

    let canonical_vault = vault_root.canonicalize()?;
    let canonical_parent = parent.canonicalize()?;
    if !canonical_parent.starts_with(&canonical_vault) {
        return Err(VaultdbError::SafetyRefused {
            reason: format!(
                "export path escapes vault root: {} resolved to {}",
                requested.display(),
                canonical_parent.display()
            ),
        });
    }

    let target = canonical_parent.join(&file_name);
    if !target.exists() {
        return Ok(target);
    }

    // Auto-suffix on collision: `Foo.csv` → `Foo (1).csv`, ...
    let stem = candidate
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| VaultdbError::SafetyRefused {
            reason: format!("export path stem is not utf-8: {}", requested.display()),
        })?;
    let ext = candidate.extension().and_then(|s| s.to_str());
    for i in 1..=10_000 {
        let suffixed = match ext {
            Some(e) => canonical_parent.join(format!("{} ({}).{}", stem, i, e)),
            None => canonical_parent.join(format!("{} ({})", stem, i)),
        };
        if !suffixed.exists() {
            return Ok(suffixed);
        }
    }
    Err(VaultdbError::SafetyRefused {
        reason: format!(
            "export path collision could not be resolved after 10000 attempts: {}",
            target.display()
        ),
    })
}

// ── Public exporters ───────────────────────────────────────────────────

/// Export a list of [`Record`]s to a file under the vault root.
///
/// If `select` is `Some(&[..])` and non-empty, those field names define
/// the output columns; otherwise the column set is the union of all
/// frontmatter keys across the records, always prefixed with `_name`.
/// Virtual fields like `_backlink_count` are resolved via `link_index`
/// when present.
///
/// Returns the actual absolute path written (after path resolution and
/// any auto-suffix on filename collision).
pub fn export_records(
    vault_root: &Path,
    requested_path: &Path,
    format: Format,
    records: &[Record],
    select: Option<&[String]>,
    link_index: Option<&LinkGraph>,
) -> Result<PathBuf> {
    let fields = match select {
        Some(s) if !s.is_empty() => s.to_vec(),
        _ => infer_fields(records),
    };
    let bytes = render_records(records, &fields, vault_root, link_index, format)?;
    let resolved = resolve_export_path(vault_root, requested_path)?;
    atomic_write_bytes(&resolved, &bytes)?;
    Ok(resolved)
}

/// Export an arbitrary JSON-serializable value to a file under the vault
/// root.
///
/// JSON and YAML accept any shape. CSV and XLSX only work when the value
/// is tabular — an array of objects, an array of scalars, or a single
/// object — and return an error for anything else.
pub fn export_value(
    vault_root: &Path,
    requested_path: &Path,
    format: Format,
    value: &serde_json::Value,
) -> Result<PathBuf> {
    let bytes = render_value(value, format)?;
    let resolved = resolve_export_path(vault_root, requested_path)?;
    atomic_write_bytes(&resolved, &bytes)?;
    Ok(resolved)
}

// ── Records → bytes ────────────────────────────────────────────────────

/// Render records to the in-memory byte buffer for `format`. Public for
/// consumers that want to stream/inspect bytes without writing to disk.
pub fn render_records(
    records: &[Record],
    fields: &[String],
    vault_root: &Path,
    link_index: Option<&LinkGraph>,
    format: Format,
) -> Result<Vec<u8>> {
    match format {
        Format::Json => render_records_json(records, fields, vault_root, link_index),
        Format::Yaml => render_records_yaml(records, fields, vault_root, link_index),
        Format::Csv { delimiter } => {
            render_records_csv(records, fields, vault_root, link_index, delimiter)
        }
        #[cfg(feature = "xlsx")]
        Format::Xlsx => render_records_xlsx(records, fields, vault_root, link_index),
    }
}

/// Field inference: union of all frontmatter keys across records,
/// always prefixed with `_name` so callers can identify rows.
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

fn render_records_json(
    records: &[Record],
    fields: &[String],
    vault_root: &Path,
    link_index: Option<&LinkGraph>,
) -> Result<Vec<u8>> {
    let items: Vec<serde_json::Value> = records
        .iter()
        .map(|r| {
            let mut map = serde_json::Map::new();
            for f in fields {
                let val = r
                    .get_with_links(f, vault_root, link_index)
                    .unwrap_or(Value::Null);
                map.insert(f.clone(), value_to_json(&val));
            }
            serde_json::Value::Object(map)
        })
        .collect();
    serde_json::to_vec_pretty(&items)
        .map_err(|e| VaultdbError::Internal(format!("json serialize: {}", e)))
}

fn render_records_yaml(
    records: &[Record],
    fields: &[String],
    vault_root: &Path,
    link_index: Option<&LinkGraph>,
) -> Result<Vec<u8>> {
    let mut out = String::new();
    for record in records {
        out.push_str("---\n");
        for f in fields {
            let val = record
                .get_with_links(f, vault_root, link_index)
                .unwrap_or(Value::Null);
            out.push_str(&format!("{}: {}\n", f, val.display_value()));
        }
    }
    Ok(out.into_bytes())
}

fn render_records_csv(
    records: &[Record],
    fields: &[String],
    vault_root: &Path,
    link_index: Option<&LinkGraph>,
    delimiter: u8,
) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut wtr = csv::WriterBuilder::new()
            .delimiter(delimiter)
            .from_writer(&mut buf);
        wtr.write_record(fields)
            .map_err(|e| VaultdbError::Internal(format!("csv header: {}", e)))?;
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
            wtr.write_record(&row)
                .map_err(|e| VaultdbError::Internal(format!("csv row: {}", e)))?;
        }
        wtr.flush()
            .map_err(|e| VaultdbError::Internal(format!("csv flush: {}", e)))?;
    }
    Ok(buf)
}

#[cfg(feature = "xlsx")]
fn render_records_xlsx(
    records: &[Record],
    fields: &[String],
    vault_root: &Path,
    link_index: Option<&LinkGraph>,
) -> Result<Vec<u8>> {
    use rust_xlsxwriter::Workbook;

    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();

    for (col, name) in fields.iter().enumerate() {
        worksheet
            .write(0, col as u16, name.as_str())
            .map_err(|e| VaultdbError::Internal(format!("xlsx header: {}", e)))?;
    }
    for (row, record) in records.iter().enumerate() {
        let row_idx = (row + 1) as u32;
        for (col, field) in fields.iter().enumerate() {
            let col_idx = col as u16;
            let val = record
                .get_with_links(field, vault_root, link_index)
                .unwrap_or(Value::Null);
            write_value_to_xlsx_cell(worksheet, row_idx, col_idx, &val)
                .map_err(|e| VaultdbError::Internal(format!("xlsx cell: {}", e)))?;
        }
    }
    workbook
        .save_to_buffer()
        .map_err(|e| VaultdbError::Internal(format!("xlsx save: {}", e)))
}

#[cfg(feature = "xlsx")]
fn write_value_to_xlsx_cell(
    worksheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    col: u16,
    value: &Value,
) -> std::result::Result<(), rust_xlsxwriter::XlsxError> {
    match value {
        Value::Null => worksheet.write(row, col, "").map(|_| ()),
        Value::String(s) => worksheet.write(row, col, s.as_str()).map(|_| ()),
        Value::Integer(n) => worksheet.write(row, col, *n as f64).map(|_| ()),
        Value::Float(f) => worksheet.write(row, col, *f).map(|_| ()),
        Value::Bool(b) => worksheet.write(row, col, *b).map(|_| ()),
        Value::List(_) | Value::Map(_) => worksheet
            .write(row, col, value.display_value().as_str())
            .map(|_| ()),
    }
}

// ── Generic JSON value → bytes ─────────────────────────────────────────

/// Render an arbitrary JSON value to the format's bytes. JSON / YAML
/// accept any shape; CSV / XLSX only accept tabular shapes (array of
/// objects, array of scalars, single object).
pub fn render_value(value: &serde_json::Value, format: Format) -> Result<Vec<u8>> {
    match format {
        Format::Json => serde_json::to_vec_pretty(value)
            .map_err(|e| VaultdbError::Internal(format!("json serialize: {}", e))),
        Format::Yaml => serde_yaml::to_string(value)
            .map(String::into_bytes)
            .map_err(|e| VaultdbError::Internal(format!("yaml serialize: {}", e))),
        Format::Csv { delimiter } => render_value_csv(value, delimiter),
        #[cfg(feature = "xlsx")]
        Format::Xlsx => render_value_xlsx(value),
    }
}

/// Flatten a JSON value to a (header, rows) tabular shape, or `None` if
/// the value isn't tabular. Used by both the CSV and XLSX backends.
fn json_to_table(value: &serde_json::Value) -> Option<(Vec<String>, Vec<Vec<String>>)> {
    use serde_json::Value as JV;
    match value {
        JV::Array(items) if items.is_empty() => Some((Vec::new(), Vec::new())),
        JV::Array(items) if items.iter().all(|v| v.is_object()) => {
            let mut keys = BTreeSet::new();
            for item in items {
                if let Some(obj) = item.as_object() {
                    for k in obj.keys() {
                        keys.insert(k.clone());
                    }
                }
            }
            let header: Vec<String> = keys.into_iter().collect();
            let rows: Vec<Vec<String>> = items
                .iter()
                .map(|item| {
                    let obj = item.as_object().unwrap();
                    header
                        .iter()
                        .map(|k| obj.get(k).map(json_to_cell_string).unwrap_or_default())
                        .collect()
                })
                .collect();
            Some((header, rows))
        }
        JV::Array(items) if items.iter().all(|v| !v.is_array() && !v.is_object()) => {
            let header = vec!["value".to_string()];
            let rows = items.iter().map(|v| vec![json_to_cell_string(v)]).collect();
            Some((header, rows))
        }
        JV::Object(_) => {
            // Single object → 1-row table over its keys.
            let array = JV::Array(vec![value.clone()]);
            json_to_table(&array)
        }
        _ => None,
    }
}

fn render_value_csv(value: &serde_json::Value, delimiter: u8) -> Result<Vec<u8>> {
    let (header, rows) = json_to_table(value).ok_or_else(|| VaultdbError::SafetyRefused {
        reason: "cannot render this shape as CSV; use json or yaml".into(),
    })?;
    let mut buf = Vec::new();
    {
        let mut wtr = csv::WriterBuilder::new()
            .delimiter(delimiter)
            .from_writer(&mut buf);
        if !header.is_empty() {
            wtr.write_record(&header)
                .map_err(|e| VaultdbError::Internal(format!("csv header: {}", e)))?;
        }
        for row in rows {
            wtr.write_record(&row)
                .map_err(|e| VaultdbError::Internal(format!("csv row: {}", e)))?;
        }
        wtr.flush()
            .map_err(|e| VaultdbError::Internal(format!("csv flush: {}", e)))?;
    }
    Ok(buf)
}

#[cfg(feature = "xlsx")]
fn render_value_xlsx(value: &serde_json::Value) -> Result<Vec<u8>> {
    use rust_xlsxwriter::Workbook;

    let (header, rows) = json_to_table(value).ok_or_else(|| VaultdbError::SafetyRefused {
        reason: "cannot render this shape as XLSX; use json or yaml".into(),
    })?;

    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();
    for (col, name) in header.iter().enumerate() {
        worksheet
            .write(0, col as u16, name.as_str())
            .map_err(|e| VaultdbError::Internal(format!("xlsx header: {}", e)))?;
    }
    for (row_idx, row) in rows.iter().enumerate() {
        let row_num = (row_idx + 1) as u32;
        for (col_idx, cell) in row.iter().enumerate() {
            // Try to write numeric/bool when the cell parses as one; fall
            // back to string. Keeps XLSX cells typed when the underlying
            // JSON was typed, without us re-walking the original Value.
            if let Ok(n) = cell.parse::<i64>() {
                worksheet
                    .write(row_num, col_idx as u16, n as f64)
                    .map_err(|e| VaultdbError::Internal(format!("xlsx cell: {}", e)))?;
            } else if let Ok(f) = cell.parse::<f64>() {
                worksheet
                    .write(row_num, col_idx as u16, f)
                    .map_err(|e| VaultdbError::Internal(format!("xlsx cell: {}", e)))?;
            } else if cell == "true" {
                worksheet
                    .write(row_num, col_idx as u16, true)
                    .map_err(|e| VaultdbError::Internal(format!("xlsx cell: {}", e)))?;
            } else if cell == "false" {
                worksheet
                    .write(row_num, col_idx as u16, false)
                    .map_err(|e| VaultdbError::Internal(format!("xlsx cell: {}", e)))?;
            } else {
                worksheet
                    .write(row_num, col_idx as u16, cell.as_str())
                    .map_err(|e| VaultdbError::Internal(format!("xlsx cell: {}", e)))?;
            }
        }
    }
    workbook
        .save_to_buffer()
        .map_err(|e| VaultdbError::Internal(format!("xlsx save: {}", e)))
}

/// JSON scalar/array/object → flat cell string. Arrays join with `, `.
fn json_to_cell_string(v: &serde_json::Value) -> String {
    use serde_json::Value as JV;
    match v {
        JV::Null => String::new(),
        JV::Bool(b) => b.to_string(),
        JV::Number(n) => n.to_string(),
        JV::String(s) => s.clone(),
        JV::Array(items) => items
            .iter()
            .map(json_to_cell_string)
            .collect::<Vec<_>>()
            .join(", "),
        JV::Object(_) => v.to_string(),
    }
}

/// `Value` → `serde_json::Value`. Same conversion the CLI used inline;
/// lifted here so the renderer is self-contained.
fn value_to_json(val: &Value) -> serde_json::Value {
    match val {
        Value::Null => serde_json::Value::Null,
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Integer(n) => serde_json::json!(n),
        Value::Float(f) => serde_json::json!(f),
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::List(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Map(m) => {
            let obj: serde_json::Map<String, serde_json::Value> = m
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
    }
}

// ── Atomic write ───────────────────────────────────────────────────────

/// Atomic write: temp-file-in-same-dir + rename. Mirrors the shape of
/// `writer::atomic_write` but takes bytes instead of `&str` (XLSX is
/// binary).
fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().ok_or_else(|| {
        VaultdbError::Internal(format!(
            "atomic_write_bytes: no parent dir for {}",
            path.display()
        ))
    })?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    use std::io::Write;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    tmp.persist(path).map_err(|e| VaultdbError::Io(e.error))?;
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn vault() -> TempDir {
        TempDir::new().expect("tempdir")
    }

    fn rec(name: &str, dir: &Path, fields: &[(&str, Value)]) -> Record {
        let path = dir.join(format!("{}.md", name));
        std::fs::write(&path, format!("---\n_name: {}\n---\n", name)).unwrap();
        let mut map = BTreeMap::new();
        for (k, v) in fields {
            map.insert(k.to_string(), v.clone());
        }
        Record {
            path,
            fields: map,
            raw_content: None,
        }
    }

    // Format extension inference

    #[test]
    fn format_csv_default_delimiter_is_comma() {
        let f = Format::from_path(Path::new("foo.csv")).unwrap();
        assert_eq!(f, Format::Csv { delimiter: b',' });
    }

    #[test]
    fn format_tsv_uses_tab_delimiter() {
        let f = Format::from_path(Path::new("foo.tsv")).unwrap();
        assert_eq!(f, Format::Csv { delimiter: b'\t' });
    }

    #[test]
    fn format_json_yaml() {
        assert_eq!(
            Format::from_path(Path::new("a.json")).unwrap(),
            Format::Json
        );
        assert_eq!(
            Format::from_path(Path::new("a.yaml")).unwrap(),
            Format::Yaml
        );
        assert_eq!(Format::from_path(Path::new("a.yml")).unwrap(), Format::Yaml);
    }

    #[test]
    fn format_uppercase_extension() {
        assert_eq!(
            Format::from_path(Path::new("FOO.CSV")).unwrap(),
            Format::Csv { delimiter: b',' }
        );
    }

    #[test]
    fn format_md_extension_refused() {
        let err = Format::from_path(Path::new("foo.md")).unwrap_err();
        assert!(matches!(err, VaultdbError::SafetyRefused { .. }));
    }

    #[test]
    fn format_unknown_extension_refused() {
        let err = Format::from_path(Path::new("foo.pdf")).unwrap_err();
        assert!(matches!(err, VaultdbError::SafetyRefused { .. }));
    }

    #[test]
    fn format_missing_extension_refused() {
        let err = Format::from_path(Path::new("nofile")).unwrap_err();
        assert!(matches!(err, VaultdbError::SafetyRefused { .. }));
    }

    #[test]
    fn with_csv_delimiter_overrides_only_csv() {
        let csv = Format::Csv { delimiter: b',' }.with_csv_delimiter(b';');
        assert_eq!(csv, Format::Csv { delimiter: b';' });
        assert_eq!(Format::Json.with_csv_delimiter(b';'), Format::Json);
    }

    // Path resolution

    #[test]
    fn resolve_basic_path() {
        let v = vault();
        let p = resolve_export_path(v.path(), Path::new("foo.csv")).unwrap();
        // Canonicalized vault path should contain the file.
        assert_eq!(p.file_name().unwrap(), "foo.csv");
        assert!(p.starts_with(v.path().canonicalize().unwrap()));
    }

    #[test]
    fn resolve_creates_parent_dirs() {
        let v = vault();
        let p = resolve_export_path(v.path(), Path::new("a/b/c/foo.csv")).unwrap();
        assert!(p.parent().unwrap().exists());
    }

    #[test]
    fn resolve_rejects_absolute() {
        let v = vault();
        let err = resolve_export_path(v.path(), Path::new("/etc/passwd.csv")).unwrap_err();
        assert!(matches!(err, VaultdbError::SafetyRefused { .. }));
    }

    #[test]
    fn resolve_rejects_parent_dir_escape() {
        let v = vault();
        let err = resolve_export_path(v.path(), Path::new("../escape.csv")).unwrap_err();
        assert!(matches!(err, VaultdbError::SafetyRefused { .. }));
    }

    #[test]
    fn resolve_rejects_deep_parent_dir_escape() {
        let v = vault();
        let err = resolve_export_path(v.path(), Path::new("a/b/../../../escape.csv")).unwrap_err();
        assert!(matches!(err, VaultdbError::SafetyRefused { .. }));
    }

    #[test]
    fn resolve_rejects_md_extension() {
        let v = vault();
        let err = resolve_export_path(v.path(), Path::new("Note.md")).unwrap_err();
        assert!(matches!(err, VaultdbError::SafetyRefused { .. }));
    }

    #[test]
    fn resolve_rejects_empty_path() {
        let v = vault();
        let err = resolve_export_path(v.path(), Path::new("")).unwrap_err();
        assert!(matches!(err, VaultdbError::SafetyRefused { .. }));
    }

    #[test]
    fn resolve_autosuffix_on_collision() {
        let v = vault();
        let first = resolve_export_path(v.path(), Path::new("foo.csv")).unwrap();
        std::fs::write(&first, b"already here").unwrap();

        let second = resolve_export_path(v.path(), Path::new("foo.csv")).unwrap();
        assert_eq!(second.file_name().unwrap(), "foo (1).csv");
        std::fs::write(&second, b"and here").unwrap();

        let third = resolve_export_path(v.path(), Path::new("foo.csv")).unwrap();
        assert_eq!(third.file_name().unwrap(), "foo (2).csv");
    }

    // Record rendering

    #[test]
    fn export_records_json() {
        let v = vault();
        let dir = v.path().join("notes");
        std::fs::create_dir_all(&dir).unwrap();
        let records = vec![
            rec("Alpha", &dir, &[("status", Value::String("active".into()))]),
            rec("Beta", &dir, &[("status", Value::String("draft".into()))]),
        ];
        let path = export_records(
            v.path(),
            Path::new("out.json"),
            Format::Json,
            &records,
            None,
            None,
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["_name"], "Alpha");
        assert_eq!(arr[0]["status"], "active");
    }

    #[test]
    fn export_records_csv_default_comma() {
        let v = vault();
        let dir = v.path().join("notes");
        std::fs::create_dir_all(&dir).unwrap();
        let records = vec![rec("Alpha", &dir, &[("rating", Value::Integer(5))])];
        let path = export_records(
            v.path(),
            Path::new("out.csv"),
            Format::Csv { delimiter: b',' },
            &records,
            None,
            None,
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let mut lines = body.lines();
        let header = lines.next().unwrap();
        assert!(header.contains("_name") && header.contains("rating"));
        assert!(header.contains(','));
        let row = lines.next().unwrap();
        assert!(row.contains("Alpha"));
        assert!(row.contains('5'));
    }

    #[test]
    fn export_records_csv_semicolon_delimiter() {
        let v = vault();
        let dir = v.path().join("notes");
        std::fs::create_dir_all(&dir).unwrap();
        let records = vec![rec("Alpha", &dir, &[("x", Value::Integer(1))])];
        let path = export_records(
            v.path(),
            Path::new("out.csv"),
            Format::Csv { delimiter: b';' },
            &records,
            None,
            None,
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let header = body.lines().next().unwrap();
        assert!(header.contains(';'));
        assert!(!header.contains(','));
    }

    #[test]
    fn export_records_tsv_via_extension() {
        let v = vault();
        let dir = v.path().join("notes");
        std::fs::create_dir_all(&dir).unwrap();
        let records = vec![rec("Alpha", &dir, &[("x", Value::Integer(1))])];
        let fmt = Format::from_path(Path::new("out.tsv")).unwrap();
        let path =
            export_records(v.path(), Path::new("out.tsv"), fmt, &records, None, None).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let header = body.lines().next().unwrap();
        assert!(header.contains('\t'));
    }

    #[test]
    fn export_records_yaml() {
        let v = vault();
        let dir = v.path().join("notes");
        std::fs::create_dir_all(&dir).unwrap();
        let records = vec![rec(
            "Alpha",
            &dir,
            &[("status", Value::String("active".into()))],
        )];
        let path = export_records(
            v.path(),
            Path::new("out.yaml"),
            Format::Yaml,
            &records,
            None,
            None,
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("_name: Alpha"));
        assert!(body.contains("status: active"));
    }

    #[test]
    fn export_records_respects_select() {
        let v = vault();
        let dir = v.path().join("notes");
        std::fs::create_dir_all(&dir).unwrap();
        let records = vec![rec(
            "Alpha",
            &dir,
            &[
                ("status", Value::String("active".into())),
                ("rating", Value::Integer(5)),
            ],
        )];
        let select = vec!["_name".to_string(), "rating".to_string()];
        let path = export_records(
            v.path(),
            Path::new("out.csv"),
            Format::Csv { delimiter: b',' },
            &records,
            Some(&select),
            None,
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let header = body.lines().next().unwrap();
        assert!(header.contains("_name"));
        assert!(header.contains("rating"));
        assert!(!header.contains("status"));
    }

    // JSON value rendering

    #[test]
    fn export_value_json_arbitrary_shape() {
        let v = vault();
        let value = serde_json::json!({
            "path": "/foo/bar",
            "nested": { "x": 1 }
        });
        let path = export_value(v.path(), Path::new("out.json"), Format::Json, &value).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["nested"]["x"], 1);
    }

    #[test]
    fn export_value_csv_array_of_objects() {
        let v = vault();
        let value = serde_json::json!([
            { "name": "Alpha", "depth": 1 },
            { "name": "Beta", "depth": 2 },
        ]);
        let path = export_value(
            v.path(),
            Path::new("hits.csv"),
            Format::Csv { delimiter: b',' },
            &value,
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let header = body.lines().next().unwrap();
        assert!(header.contains("name"));
        assert!(header.contains("depth"));
        assert_eq!(body.lines().count(), 3); // header + 2 rows
    }

    #[test]
    fn export_value_csv_array_of_scalars() {
        let v = vault();
        let value = serde_json::json!(["a", "b", "c"]);
        let path = export_value(
            v.path(),
            Path::new("scalars.csv"),
            Format::Csv { delimiter: b',' },
            &value,
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let mut lines = body.lines();
        assert_eq!(lines.next().unwrap(), "value");
        assert_eq!(lines.next().unwrap(), "a");
    }

    #[test]
    fn export_value_csv_rejects_non_tabular_shape() {
        let v = vault();
        let value = serde_json::json!("just a string");
        let err = export_value(
            v.path(),
            Path::new("bad.csv"),
            Format::Csv { delimiter: b',' },
            &value,
        )
        .unwrap_err();
        assert!(matches!(err, VaultdbError::SafetyRefused { .. }));
    }

    #[test]
    fn export_value_yaml_arbitrary_shape() {
        let v = vault();
        let value = serde_json::json!({ "key": "value" });
        let path = export_value(v.path(), Path::new("out.yaml"), Format::Yaml, &value).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("key: value"));
    }

    // XLSX (feature-gated)

    #[cfg(feature = "xlsx")]
    #[test]
    fn export_records_xlsx_writes_real_workbook() {
        let v = vault();
        let dir = v.path().join("notes");
        std::fs::create_dir_all(&dir).unwrap();
        let records = vec![rec(
            "Alpha",
            &dir,
            &[
                ("rating", Value::Integer(5)),
                ("status", Value::String("active".into())),
            ],
        )];
        let path = export_records(
            v.path(),
            Path::new("out.xlsx"),
            Format::Xlsx,
            &records,
            None,
            None,
        )
        .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        // XLSX is a ZIP container — magic bytes are "PK\x03\x04".
        assert_eq!(&bytes[..4], b"PK\x03\x04");
    }

    #[cfg(feature = "xlsx")]
    #[test]
    fn export_value_xlsx_array_of_objects() {
        let v = vault();
        let value = serde_json::json!([
            { "name": "Alpha", "depth": 1 },
            { "name": "Beta", "depth": 2 },
        ]);
        let path = export_value(v.path(), Path::new("hits.xlsx"), Format::Xlsx, &value).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"PK\x03\x04");
    }

    // Atomic-write semantics

    #[test]
    fn atomic_write_replaces_existing() {
        let v = vault();
        let target: PathBuf = v.path().join("foo.txt");
        std::fs::write(&target, b"old").unwrap();
        atomic_write_bytes(&target, b"new").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
    }
}
