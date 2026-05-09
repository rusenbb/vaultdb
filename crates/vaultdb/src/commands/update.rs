use anyhow::{Context, Result};
use colored::Colorize;

use vaultdb_core::error::VaultdbError;
use vaultdb_core::vault::Vault;
use vaultdb_core::{Expr, MutationReport, UpdateBuilder, Value};

pub enum UpdateOp {
    Set { field: String, value: String },
    Unset { field: String },
    AddTag { tag: String },
    RemoveTag { tag: String },
}

/// Parse --set arguments ("FIELD=VALUE") and the other flags into UpdateOps.
pub fn parse_operations(
    set: &[String],
    unset: &[String],
    add_tag: &[String],
    remove_tag: &[String],
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

    if ops.is_empty() {
        anyhow::bail!("no operations specified. Use --set, --unset, --add-tag, or --remove-tag");
    }

    Ok(ops)
}

/// Run the `update` command.
pub fn run_update(
    vault: &Vault,
    folder: &str,
    where_strs: &[String],
    ops: &[UpdateOp],
    dry_run: bool,
    _recursive: bool,
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

    let mut builder = UpdateBuilder::new(folder, filter);
    for op in ops {
        builder = match op {
            UpdateOp::Set { field, value } => builder.set(field, parse_set_value(value)),
            UpdateOp::Unset { field } => builder.unset(field),
            UpdateOp::AddTag { tag } => builder.add_tag(tag),
            UpdateOp::RemoveTag { tag } => builder.remove_tag(tag),
        };
    }

    let report = if dry_run {
        builder.plan(vault)?
    } else {
        builder.execute(vault)?
    };

    print_report(&report, &vault.root, dry_run, "modified");
    Ok(())
}

/// Best-effort parse of a CLI `--set` value into a `Value`. Tries integer, then
/// float, then falls back to a string so the existing CLI's behaviour
/// (typed values when they look numeric) is preserved.
fn parse_set_value(s: &str) -> Value {
    if let Ok(i) = s.parse::<i64>() {
        return Value::Integer(i);
    }
    if let Ok(f) = s.parse::<f64>() {
        return Value::Float(f);
    }
    Value::String(s.to_string())
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
