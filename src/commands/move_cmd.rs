use anyhow::{Context, Result};
use colored::Colorize;

use crate::error::VaultdbError;
use crate::filter::{WhereClause, matches_all};
use crate::vault::Vault;

/// Run the `move` command.
pub fn run_move(
    vault: &Vault,
    folder: &str,
    where_strs: &[String],
    target_folder: &str,
    dry_run: bool,
    recursive: bool,
    verbose: bool,
) -> Result<()> {
    if where_strs.is_empty() {
        return Err(VaultdbError::SafetyRefused {
            reason: "move requires at least one --where condition".into(),
        }
        .into());
    }

    let folder_path = vault.resolve_folder(folder)?;
    let target_path = vault.root.join(target_folder);

    let where_clauses: Vec<WhereClause> = where_strs
        .iter()
        .map(|s| WhereClause::parse(s).context(format!("parsing where expression: {}", s)))
        .collect::<Result<Vec<_>>>()?;

    let records = vault.load_records(&folder_path, recursive, verbose)?;
    let matching: Vec<_> = records
        .into_iter()
        .filter(|r| matches_all(&where_clauses, r, &vault.root))
        .collect();

    if matching.is_empty() {
        println!("No matching records.");
        return Ok(());
    }

    // Check for filename conflicts
    for record in &matching {
        let filename = record.path.file_name().unwrap();
        let dest = target_path.join(filename);
        if dest.exists() {
            anyhow::bail!(
                "conflict: {} already exists in {}",
                filename.to_string_lossy(),
                target_folder
            );
        }
    }

    for record in &matching {
        let filename = record.path.file_name().unwrap();
        let dest = target_path.join(filename);
        let rel_source = record
            .path
            .strip_prefix(&vault.root)
            .unwrap_or(&record.path);
        let rel_dest = dest.strip_prefix(&vault.root).unwrap_or(&dest);

        println!("{} -> {}", rel_source.display(), rel_dest.display());

        if !dry_run {
            // Create target directory if it doesn't exist
            if !target_path.exists() {
                std::fs::create_dir_all(&target_path)?;
            }
            std::fs::rename(&record.path, &dest)?;
        }
    }

    if dry_run {
        println!(
            "\n{}",
            format!("{} file(s) would be moved (dry-run)", matching.len()).yellow()
        );
    } else {
        println!(
            "\n{}",
            format!("{} file(s) moved to {}", matching.len(), target_folder).green()
        );
    }

    Ok(())
}
