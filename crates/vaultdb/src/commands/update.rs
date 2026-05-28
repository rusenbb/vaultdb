use anyhow::{Context, Result};
use colored::Colorize;

use vaultdb_core::error::VaultdbError;
use vaultdb_core::schema;
use vaultdb_core::vault::Vault;
use vaultdb_core::{Expr, MutationReport, UpdateBuilder, Value};

pub enum UpdateOp {
    Set { field: String, value: String },
    Unset { field: String },
    AddTag { tag: String },
    RemoveTag { tag: String },
    SetBody { text: String },
    AppendBody { text: String },
    ClearBody,
}

/// Translate the common backslash escapes (`\n`, `\r`, `\t`, `\\`) in a
/// user-supplied separator string into their literal bytes. Shell quoting
/// makes literal newlines awkward to pass on the command line, so we
/// accept the escaped form and unescape here. Unknown escapes pass
/// through untouched (e.g. `\x` stays `\x`) — keeps the rule simple.
pub fn unescape_separator(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Parse --set arguments ("FIELD=VALUE") and the other flags into UpdateOps.
pub fn parse_operations(
    set: &[String],
    unset: &[String],
    add_tag: &[String],
    remove_tag: &[String],
    set_body: Option<&str>,
    append_body: &[String],
    clear_body: bool,
) -> Result<Vec<UpdateOp>> {
    let mut ops = Vec::new();

    for s in set {
        let (field, value) = s
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--set requires FIELD=VALUE format, got: {}", s))?;
        ops.push(UpdateOp::Set {
            field: field.trim().to_string(),
            value: value.trim().to_string(),
        });
    }

    for field in unset {
        ops.push(UpdateOp::Unset {
            field: field.trim().to_string(),
        });
    }

    for tag in add_tag {
        ops.push(UpdateOp::AddTag {
            tag: tag.trim().to_string(),
        });
    }

    for tag in remove_tag {
        ops.push(UpdateOp::RemoveTag {
            tag: tag.trim().to_string(),
        });
    }

    if clear_body {
        ops.push(UpdateOp::ClearBody);
    }

    if let Some(text) = set_body {
        ops.push(UpdateOp::SetBody {
            text: text.to_string(),
        });
    }

    for text in append_body {
        ops.push(UpdateOp::AppendBody {
            text: text.to_string(),
        });
    }

    if ops.is_empty() {
        anyhow::bail!(
            "no operations specified. Use --set, --unset, --add-tag, --remove-tag, --set-body, --append-body, or --clear-body"
        );
    }

    Ok(ops)
}

/// Run the `update` command.
#[allow(clippy::too_many_arguments)]
pub fn run_update(
    vault: &Vault,
    folder: &str,
    where_strs: &[String],
    ops: &[UpdateOp],
    body_separator: Option<&str>,
    dry_run: bool,
    recursive: bool,
    _verbose: bool,
) -> Result<()> {
    if where_strs.is_empty() {
        return Err(VaultdbError::SafetyRefused {
            reason:
                "update requires at least one --where condition to prevent accidental bulk changes"
                    .into(),
        }
        .into());
    }

    // Combine all --where conditions with AND.
    let exprs: Vec<Expr> = where_strs
        .iter()
        .map(|s| Expr::parse(s).context(format!("parsing where expression: {}", s)))
        .collect::<Result<Vec<_>>>()?;
    let filter = match exprs.len() {
        1 => exprs.into_iter().next().unwrap(),
        _ => Expr::And(exprs),
    };

    let mut builder = UpdateBuilder::new(folder, filter).recursive(recursive);
    if let Some(sep) = body_separator {
        builder = builder.body_separator(sep);
    }
    for op in ops {
        builder = match op {
            UpdateOp::Set { field, value } => builder.set(field, Value::parse_scalar(value)),
            UpdateOp::Unset { field } => builder.unset(field),
            UpdateOp::AddTag { tag } => builder.add_tag(tag),
            UpdateOp::RemoveTag { tag } => builder.remove_tag(tag),
            UpdateOp::SetBody { text } => builder.set_body(text),
            UpdateOp::AppendBody { text } => builder.append_body(text),
            UpdateOp::ClearBody => builder.clear_body(),
        };
    }

    // Schema enforcement: when a schema file exists, the post-update
    // record must validate against every applicable collection.
    // Malformed schema files fail the command instead of silently
    // skipping enforcement.
    let schema_path = schema::schema_path(&vault.root);
    if schema_path.is_file() {
        let vault_schema = schema::load_schema(&schema_path)
            .context(format!("loading {}", schema_path.display()))?;
        builder = builder.with_vault_schema(vault_schema);
    }

    let report = if dry_run {
        builder.plan(vault)?
    } else {
        builder.execute(vault)?
    };

    print_report(&report, &vault.root, dry_run, "modified");
    Ok(())
}

/// Format a MutationReport with the existing CLI's colored output style.
pub(crate) fn print_report(
    report: &MutationReport,
    vault_root: &std::path::Path,
    dry_run: bool,
    verb_past: &str,
) {
    if report.changes.is_empty() && report.errors.is_empty() {
        println!("No matching records.");
        return;
    }

    for change in &report.changes {
        let rel_path = change.path.strip_prefix(vault_root).unwrap_or(&change.path);
        println!("{}", rel_path.display().to_string().bold());
        for line in change.description.split("; ") {
            if !line.is_empty() {
                println!("  {}", line);
            }
        }
    }

    for err in &report.errors {
        let rel_path = err.path.strip_prefix(vault_root).unwrap_or(&err.path);
        eprintln!("{} {}: {}", "error:".red(), rel_path.display(), err.message);
    }

    if dry_run {
        println!(
            "\n{} (dry-run: no files {})",
            format!("{} file(s) would be {}", report.changes.len(), verb_past).yellow(),
            verb_past
        );
    } else {
        println!(
            "\n{}",
            format!("{} file(s) {}", report.changes.len(), verb_past).green()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::unescape_separator;

    #[test]
    fn unescape_common_sequences() {
        assert_eq!(unescape_separator("\\n"), "\n");
        assert_eq!(unescape_separator("\\n\\n"), "\n\n");
        assert_eq!(unescape_separator("\\t"), "\t");
        assert_eq!(unescape_separator("\\r\\n"), "\r\n");
        assert_eq!(unescape_separator("\\\\"), "\\");
    }

    #[test]
    fn unescape_unknown_escape_passes_through() {
        // `\x` is not a recognised escape — leave it as-is rather than
        // silently dropping the backslash.
        assert_eq!(unescape_separator("\\x"), "\\x");
    }

    #[test]
    fn unescape_plain_string_unchanged() {
        assert_eq!(unescape_separator("---"), "---");
        assert_eq!(unescape_separator(""), "");
    }
}
