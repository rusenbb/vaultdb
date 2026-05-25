use anyhow::{Context, Result};
use colored::Colorize;

use vaultdb_core::error::VaultdbError;
use vaultdb_core::vault::Vault;
use vaultdb_core::{DeleteBuilder, Expr, GraphScope};

/// Build a single combined filter `Expr` from one or more `--where` strings.
fn build_filter(where_strs: &[String]) -> Result<Expr> {
    let exprs: Vec<Expr> = where_strs
        .iter()
        .map(|s| Expr::parse(s).context(format!("parsing where expression: {}", s)))
        .collect::<Result<Vec<_>>>()?;
    Ok(match exprs.len() {
        1 => exprs.into_iter().next().unwrap(),
        _ => Expr::And(exprs),
    })
}

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
    _verbose: bool,
) -> Result<()> {
    if where_strs.is_empty() {
        return Err(VaultdbError::SafetyRefused {
            reason: "delete requires at least one --where condition".into(),
        }
        .into());
    }

    let filter = build_filter(where_strs)?;
    let plan_report = DeleteBuilder::new(folder, filter.clone())
        .permanent(force)
        .recursive(recursive)
        .plan(vault)?;

    if plan_report.changes.is_empty() {
        println!("No matching records.");
        return Ok(());
    }

    // Whole-vault link graph for dangling-link warnings.
    let graph = vault.link_graph(GraphScope::All)?;

    let action_word = if force { "delete" } else { "trash" };
    let mut total_dangling = 0;

    for change in &plan_report.changes {
        let rel_path = change
            .path
            .strip_prefix(&vault.root)
            .unwrap_or(&change.path);
        let name = change
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let backlinks = graph.incoming_links(name);

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
            format!(
                "{} file(s) would be deleted (dry-run)",
                plan_report.changes.len()
            )
        } else {
            format!(
                "{} file(s) would be trashed (dry-run)",
                plan_report.changes.len()
            )
        };
        println!("\n{}", dry_msg.yellow());
        return Ok(());
    }

    // Actually apply.
    let exec_report = DeleteBuilder::new(folder, filter)
        .permanent(force)
        .recursive(recursive)
        .execute(vault)?;

    for err in &exec_report.errors {
        let rel = err.path.strip_prefix(&vault.root).unwrap_or(&err.path);
        eprintln!("{} {}: {}", "error:".red(), rel.display(), err.message);
    }

    let success_count = exec_report.changes.len() - exec_report.errors.len();
    let msg = if force {
        format!("{} file(s) permanently deleted", success_count)
    } else {
        format!("{} file(s) moved to .trash/", success_count)
    };
    println!("\n{}", msg.green());

    Ok(())
}
