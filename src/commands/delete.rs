use anyhow::{Context, Result};
use colored::Colorize;

use crate::error::VaultdbError;
use crate::filter::{WhereClause, matches_all};
use crate::links::LinkIndex;
use crate::vault::Vault;

/// Run the `delete` command.
/// By default, moves files to `.trash/` in the vault root.
/// With `--force`, permanently deletes them.
/// Warns about dangling links that will result from deletion.
pub fn run_delete(
    vault: &Vault,
    folder: &str,
    where_strs: &[String],
    force: bool,
    dry_run: bool,
    recursive: bool,
    verbose: bool,
) -> Result<()> {
    if where_strs.is_empty() {
        return Err(VaultdbError::SafetyRefused {
            reason: "delete requires at least one --where condition".into(),
        }
        .into());
    }

    let folder_path = vault.resolve_folder(folder)?;

    let where_clauses: Vec<WhereClause> = where_strs
        .iter()
        .map(|s| WhereClause::parse(s).context(format!("parsing where expression: {}", s)))
        .collect::<Result<Vec<_>>>()?;

    // Load with content so we can build the link index for dangling link warnings
    let records = vault.load_records_with_content(&folder_path, recursive, verbose)?;
    let all_records = vault.load_records_with_content(&vault.root, true, verbose)?;
    let index = LinkIndex::build_with_root(&all_records, Some(&vault.root));

    let matching: Vec<_> = records
        .into_iter()
        .filter(|r| matches_all(&where_clauses, r, &vault.root))
        .collect();

    if matching.is_empty() {
        println!("No matching records.");
        return Ok(());
    }

    let trash_dir = vault.root.join(".trash");
    let action_word = if force { "delete" } else { "trash" };

    let mut total_dangling = 0;

    for record in &matching {
        let rel_path = record
            .path
            .strip_prefix(&vault.root)
            .unwrap_or(&record.path);
        let name = record.virtual_name();

        // Check for incoming links that will become dangling
        let backlinks = index.incoming_links(&name);

        println!("{}: {}", action_word, rel_path.display());
        if !backlinks.is_empty() {
            println!(
                "  {} {} referencing note(s) will have dangling links:",
                "!".yellow(),
                backlinks.len()
            );
            for bl in &backlinks {
                println!("    <- {}", bl);
            }
            total_dangling += backlinks.len();
        }

        if !dry_run {
            if force {
                std::fs::remove_file(&record.path)?;
            } else {
                if !trash_dir.exists() {
                    std::fs::create_dir_all(&trash_dir)?;
                }
                let filename = record.path.file_name().unwrap();
                let mut dest = trash_dir.join(filename);
                // Avoid overwriting previously trashed files with the same name
                if dest.exists() {
                    let stem = dest.file_stem().unwrap().to_string_lossy().to_string();
                    let ext = dest.extension().map(|e| e.to_string_lossy().to_string());
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    dest = match ext {
                        Some(e) => trash_dir.join(format!("{}.{}.{}", stem, ts, e)),
                        None => trash_dir.join(format!("{}.{}", stem, ts)),
                    };
                }
                std::fs::rename(&record.path, &dest)?;
            }
        }
    }

    if total_dangling > 0 {
        println!(
            "\n{}",
            format!(
                "warning: {} link(s) across other notes will become dangling",
                total_dangling
            )
            .yellow()
        );
    }

    if dry_run {
        let dry_msg = if force {
            format!("{} file(s) would be deleted (dry-run)", matching.len())
        } else {
            format!("{} file(s) would be trashed (dry-run)", matching.len())
        };
        println!("\n{}", dry_msg.yellow());
    } else {
        let msg = if force {
            format!("{} file(s) permanently deleted", matching.len())
        } else {
            format!("{} file(s) moved to .trash/", matching.len())
        };
        println!("\n{}", msg.green());
    }

    Ok(())
}
