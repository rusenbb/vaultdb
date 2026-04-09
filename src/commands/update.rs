use anyhow::{Context, Result};
use colored::Colorize;

use crate::error::VaultdbError;
use crate::filter::{WhereClause, matches_all};
use crate::vault::Vault;
use crate::writer::{self, WriteResult};

pub enum UpdateOp {
    Set { field: String, value: String },
    Unset { field: String },
    AddTag { tag: String },
    RemoveTag { tag: String },
}

/// Parse --set arguments ("FIELD=VALUE") into UpdateOps.
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
    recursive: bool,
    verbose: bool,
) -> Result<()> {
    if where_strs.is_empty() {
        return Err(VaultdbError::SafetyRefused {
            reason:
                "update requires at least one --where condition to prevent accidental bulk changes"
                    .into(),
        }
        .into());
    }

    let folder_path = vault.resolve_folder(folder)?;
    let where_clauses: Vec<WhereClause> = where_strs
        .iter()
        .map(|s| WhereClause::parse(s).context(format!("parsing where expression: {}", s)))
        .collect::<Result<Vec<_>>>()?;

    let records = vault.load_records_with_content(&folder_path, recursive, verbose)?;

    let matching: Vec<_> = records
        .into_iter()
        .filter(|r| matches_all(&where_clauses, r, &vault.root))
        .collect();

    if matching.is_empty() {
        println!("No matching records.");
        return Ok(());
    }

    let mut results: Vec<WriteResult> = Vec::new();

    for record in &matching {
        let mut content = record.raw_content.as_ref().unwrap().clone();
        let mut changes = Vec::new();

        for op in ops {
            let (new_content, change) = match op {
                UpdateOp::Set { field, value } => writer::set_field(&content, field, value)
                    .context(format!("setting {} in {}", field, record.virtual_name()))?,
                UpdateOp::Unset { field } => writer::unset_field(&content, field)
                    .context(format!("unsetting {} in {}", field, record.virtual_name()))?,
                UpdateOp::AddTag { tag } => writer::add_tag(&content, tag).context(format!(
                    "adding tag {} in {}",
                    tag,
                    record.virtual_name()
                ))?,
                UpdateOp::RemoveTag { tag } => writer::remove_tag(&content, tag)
                    .context(format!("removing tag {} in {}", tag, record.virtual_name()))?,
            };
            content = new_content;
            changes.push(change);
        }

        results.push(WriteResult {
            path: record.path.clone(),
            original_content: record.raw_content.as_ref().unwrap().clone(),
            modified_content: content,
            changes,
        });
    }

    // Display preview
    for result in &results {
        let rel_path = result
            .path
            .strip_prefix(&vault.root)
            .unwrap_or(&result.path);
        println!("{}", rel_path.display().to_string().bold());
        for change in &result.changes {
            println!("  {}", change);
        }
    }

    if dry_run {
        println!(
            "\n{} (dry-run: no files modified)",
            format!("{} file(s) would be modified", results.len()).yellow()
        );
    } else {
        for result in &results {
            writer::apply(result)?;
        }
        println!(
            "\n{}",
            format!("{} file(s) modified", results.len()).green()
        );
    }

    Ok(())
}
