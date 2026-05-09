use anyhow::Result;
use colored::Colorize;

use vaultdb_core::vault::Vault;
use vaultdb_core::{GraphScope, RenameBuilder};

/// Run the `rename` command — rename a note and update all wiki-links across the vault.
pub fn run_rename(
    vault: &Vault,
    old_name: &str,
    new_name: &str,
    folder: &str,
    dry_run: bool,
    _verbose: bool,
) -> Result<()> {
    // Build the link graph to do ambiguity checks before delegating to the
    // RenameBuilder. The builder also handles target-not-found and target-
    // exists errors itself, but the ambiguity warnings are CLI-side concerns.
    let graph = vault.link_graph(GraphScope::All)?;

    if graph.is_ambiguous(old_name) {
        let paths = graph.paths_for_name(old_name);
        eprintln!(
            "{} '{}' exists in multiple locations:",
            "warning:".yellow(),
            old_name
        );
        for p in &paths {
            eprintln!("  {}", p);
        }
        eprintln!("only renaming the one in --folder ({})", folder);
    }

    if graph.is_ambiguous(new_name) {
        anyhow::bail!(
            "target name '{}' already exists in multiple locations — rename would increase ambiguity",
            new_name
        );
    }

    let plan_report = RenameBuilder::new(folder, old_name, new_name).plan(vault)?;

    if !plan_report.errors.is_empty() {
        let joined = plan_report
            .errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!("{}", joined);
    }

    println!("{} -> {}", old_name.bold(), new_name.bold());
    println!();

    let backlink_count = plan_report.changes.len().saturating_sub(1); // first change is the rename itself
    for (i, change) in plan_report.changes.iter().enumerate() {
        let rel_path = change
            .path
            .strip_prefix(&vault.root)
            .unwrap_or(&change.path);
        if i == 0 {
            println!("  rename: {}", rel_path.display());
        } else {
            println!("    update: {}", rel_path.display());
        }
    }
    if backlink_count == 0 {
        println!("  no references to update");
    }

    if dry_run {
        println!("\n{}", "(dry-run: no changes written)".yellow());
        return Ok(());
    }

    let exec_report = RenameBuilder::new(folder, old_name, new_name).execute(vault)?;
    for err in &exec_report.errors {
        let rel = err.path.strip_prefix(&vault.root).unwrap_or(&err.path);
        eprintln!("{} {}: {}", "error:".red(), rel.display(), err.message);
    }
    println!("\n{}", "done".green());

    Ok(())
}
