use anyhow::{Context, Result};
use colored::Colorize;

use vaultdb_core::frontmatter;
use vaultdb_core::record::Value;
use vaultdb_core::schema;
use vaultdb_core::vault::Vault;
use vaultdb_core::writer;

const SCHEMA_FILENAME: &str = "vaultdb-schema.yaml";

/// Run the `create` command — create a new note, optionally from a template.
pub fn run_create(
    vault: &Vault,
    folder: &str,
    name: &str,
    template: Option<&str>,
    set_args: &[String],
    dry_run: bool,
) -> Result<()> {
    let folder_path = vault.resolve_folder(folder)?;
    let filename = format!("{}.md", name);
    let dest = folder_path.join(&filename);

    if dest.exists() {
        anyhow::bail!("file already exists: {}", dest.display());
    }

    // Start with template content or minimal frontmatter
    let mut content = match template {
        Some(tmpl_path) => {
            let tmpl_file = vault.root.join(tmpl_path);
            if !tmpl_file.exists() {
                anyhow::bail!("template not found: {}", tmpl_file.display());
            }
            std::fs::read_to_string(&tmpl_file)
                .context(format!("reading template: {}", tmpl_path))?
        }
        None => format!("---\n---\n\n# {}\n", name),
    };

    // Apply --set overrides
    for s in set_args {
        let (field, value) = s
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--set requires FIELD=VALUE format, got: {}", s))?;
        let field = field.trim();
        let value = value.trim();

        // Check if the file has frontmatter we can modify
        if frontmatter::extract_frontmatter(&content).is_some() {
            let (new_content, _) = writer::set_field(&content, field, value)
                .context(format!("setting field '{}' on new note", field))?;
            content = new_content;
        } else {
            // No frontmatter — wrap content with frontmatter
            content = format!(
                "---\n{}: {}\n---\n{}",
                field,
                writer::quote_value(value),
                content
            );
        }
    }

    content = apply_schema_defaults(vault, folder, content)
        .context("applying schema defaults from vaultdb-schema.yaml")?;

    let rel_dest = dest.strip_prefix(&vault.root).unwrap_or(&dest);

    if dry_run {
        println!(
            "{}",
            format!("would create: {}", rel_dest.display()).yellow()
        );
        println!("\n{}", content);
    } else {
        // Create parent directory if needed
        if let Some(parent) = dest.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &content)?;
        println!("{}", format!("created: {}", rel_dest.display()).green());
    }

    Ok(())
}

fn apply_schema_defaults(vault: &Vault, folder: &str, mut content: String) -> Result<String> {
    let schema_path = vault.root.join(SCHEMA_FILENAME);
    if !schema_path.is_file() {
        return Ok(content);
    }

    let vault_schema = schema::load_schema(&schema_path)
        .with_context(|| format!("loading {}", schema_path.display()))?;

    let collection = match best_collection_for_folder(&vault_schema, folder) {
        Some(c) => c,
        None => return Ok(content),
    };

    let mut fields = extract_fields(&content)?;

    // If the template has no frontmatter at all, only wrap it when we actually
    // need to write defaults.
    let missing_defaults: Vec<(&String, Value)> = collection
        .fields
        .iter()
        .filter_map(|(field_name, field_schema)| {
            let already_set = fields
                .get(field_name)
                .is_some_and(|v| !matches!(v, Value::Null));
            if already_set {
                return None;
            }

            if let Some(v) = field_schema.default.clone() {
                return Some(Ok((field_name, v)));
            }
            if let Some(expr) = field_schema.default_expr.as_deref() {
                return Some(eval_default_expr(expr).map(|v| (field_name, v)));
            }
            None
        })
        .collect::<Result<Vec<_>>>()?;

    if !missing_defaults.is_empty() && frontmatter::extract_frontmatter(&content).is_none() {
        content = wrap_with_empty_frontmatter(content);
        fields = extract_fields(&content)?;
    }

    for (field_name, value) in missing_defaults {
        if fields
            .get(field_name)
            .is_some_and(|v| !matches!(v, Value::Null))
        {
            continue;
        }
        content = upsert_frontmatter_value(&content, field_name, &value)?;
        fields.insert(field_name.clone(), value);
    }

    let mut required_fields: std::collections::BTreeSet<String> =
        collection.required.iter().cloned().collect();
    for (field_name, field_schema) in &collection.fields {
        if field_schema.required.unwrap_or(false) {
            required_fields.insert(field_name.clone());
        }
    }

    let missing_required: Vec<String> = required_fields
        .into_iter()
        .filter(|f| matches!(fields.get(f), None | Some(Value::Null)))
        .collect();

    if !missing_required.is_empty() {
        anyhow::bail!(
            "schema-required field(s) missing for folder '{}': {}",
            folder,
            missing_required.join(", ")
        );
    }

    Ok(content)
}

fn best_collection_for_folder<'a>(
    vault_schema: &'a schema::VaultSchema,
    folder: &str,
) -> Option<&'a schema::CollectionSchema> {
    vault_schema
        .collections
        .values()
        .filter(|c| c.folder == folder || folder.starts_with(&format!("{}/", c.folder)))
        .max_by_key(|c| c.folder.len())
}

fn extract_fields(content: &str) -> Result<std::collections::BTreeMap<String, Value>> {
    let Some((fm_text, _)) = frontmatter::extract_frontmatter(content) else {
        return Ok(std::collections::BTreeMap::new());
    };
    frontmatter::parse_frontmatter(fm_text).context("parsing frontmatter")
}

fn wrap_with_empty_frontmatter(content: String) -> String {
    // Keep a blank line between the frontmatter and the original content so
    // templates that start with headings don't get jammed against `---`.
    format!("---\n---\n\n{}", content)
}

fn upsert_frontmatter_value(content: &str, key: &str, value: &Value) -> Result<String> {
    if frontmatter::extract_frontmatter(content).is_none() {
        anyhow::bail!("cannot write schema defaults: note template has no frontmatter");
    }

    let existing = extract_fields(content)?;
    let mut next = content.to_string();
    if existing.contains_key(key) {
        let (updated, _) = writer::unset_field(&next, key).context("removing existing field")?;
        next = updated;
    }

    insert_frontmatter_field(&next, key, value)
}

fn insert_frontmatter_field(content: &str, key: &str, value: &Value) -> Result<String> {
    // We intentionally keep this line-based to avoid rewriting template
    // frontmatter formatting unnecessarily.
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() || lines[0].trim() != "---" {
        anyhow::bail!("cannot apply schema defaults: missing frontmatter delimiters");
    }

    let close_idx = lines[1..]
        .iter()
        .position(|l| l.trim() == "---")
        .map(|i| i + 1)
        .ok_or_else(|| anyhow::anyhow!("cannot apply schema defaults: missing closing '---'"))?;

    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 8);
    for (idx, line) in lines.iter().enumerate() {
        if idx == close_idx {
            for new_line in render_field_yaml_lines(key, value)? {
                out.push(new_line);
            }
        }
        out.push((*line).to_string());
    }

    Ok(out.join("\n"))
}

fn render_field_yaml_lines(key: &str, value: &Value) -> Result<Vec<String>> {
    match value {
        Value::Null => Ok(vec![format!("{}:", key)]),
        Value::String(s) => Ok(vec![format!("{}: {}", key, render_yaml_string(s))]),
        Value::Integer(i) => Ok(vec![format!("{}: {}", key, i)]),
        Value::Float(f) => Ok(vec![format!("{}: {}", key, f)]),
        Value::Bool(b) => Ok(vec![format!("{}: {}", key, b)]),
        Value::List(_) | Value::Map(_) => {
            let fragment = serde_yaml::to_string(value)
                .map_err(|e| anyhow::anyhow!("serializing default value for {}: {}", key, e))?;
            let fragment = strip_yaml_document_start(&fragment);
            let mut lines = Vec::new();
            lines.push(format!("{}:", key));
            for line in fragment.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                lines.push(format!("  {}", line));
            }
            Ok(lines)
        }
        _ => anyhow::bail!("unsupported default value type for field '{}'", key),
    }
}

fn strip_yaml_document_start(s: &str) -> &str {
    s.strip_prefix("---\n")
        .or_else(|| s.strip_prefix("---\r\n"))
        .unwrap_or(s)
}

fn yaml_quote_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn render_yaml_string(value: &str) -> String {
    // Prefer readable plain scalars when they're unambiguous, but quote when
    // YAML might reinterpret the token.
    let lower = value.to_ascii_lowercase();
    let ambiguous = matches!(lower.as_str(), "null" | "~" | "true" | "false")
        || value.parse::<i64>().is_ok()
        || value.parse::<f64>().is_ok()
        || looks_like_date_or_time(value);

    if ambiguous {
        yaml_quote_string(value)
    } else {
        writer::quote_value(value)
    }
}

fn looks_like_date_or_time(value: &str) -> bool {
    // Avoid emitting plain scalars like `2026-05-12` that might be parsed as a timestamp.
    let bytes = value.as_bytes();
    matches!(
        bytes,
        [y0, y1, y2, y3, b'-', m0, m1, b'-', d0, d1]
            if y0.is_ascii_digit()
                && y1.is_ascii_digit()
                && y2.is_ascii_digit()
                && y3.is_ascii_digit()
                && m0.is_ascii_digit()
                && m1.is_ascii_digit()
                && d0.is_ascii_digit()
                && d1.is_ascii_digit()
    ) || matches!(
        bytes,
        [y0, y1, y2, y3, b'-', m0, m1, b'-', d0, d1, b' ', h0, h1, b':', n0, n1]
            if y0.is_ascii_digit()
                && y1.is_ascii_digit()
                && y2.is_ascii_digit()
                && y3.is_ascii_digit()
                && m0.is_ascii_digit()
                && m1.is_ascii_digit()
                && d0.is_ascii_digit()
                && d1.is_ascii_digit()
                && h0.is_ascii_digit()
                && h1.is_ascii_digit()
                && n0.is_ascii_digit()
                && n1.is_ascii_digit()
    )
}

fn eval_default_expr(expr: &str) -> Result<Value> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();

    match expr {
        "epoch" => Ok(Value::Integer(secs as i64)),
        "today" => {
            let days = secs / 86_400;
            let (year, month, day) = epoch_days_to_date(days);
            Ok(Value::String(format!(
                "{:04}-{:02}-{:02}",
                year, month, day
            )))
        }
        "now" => {
            let days = secs / 86_400;
            let remaining = secs % 86_400;
            let hours = remaining / 3_600;
            let minutes = (remaining % 3_600) / 60;
            let (year, month, day) = epoch_days_to_date(days);
            Ok(Value::String(format!(
                "{:04}-{:02}-{:02} {:02}:{:02}",
                year, month, day, hours, minutes
            )))
        }
        other => anyhow::bail!("unknown default_expr '{}'", other),
    }
}

fn epoch_days_to_date(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
