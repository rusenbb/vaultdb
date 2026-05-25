use anyhow::{Context, Result};
use colored::Colorize;

use vaultdb_core::error::VaultdbError;
use vaultdb_core::vault::Vault;
use vaultdb_core::{Expr, MoveBuilder};

/// Run the `move` command.
pub fn run_move(
    vault: &Vault,
    folder: &str,
    where_strs: &[String],
    target_folder: &str,
    dry_run: bool,
    recursive: bool,
    _verbose: bool,
) -> Result<()> {
    if where_strs.is_empty() {
        return Err(VaultdbError::SafetyRefused {
            reason: "move requires at least one --where condition".into(),
        }
        .into());
    }

    let exprs: Vec<Expr> = where_strs
        .iter()
        .map(|s| Expr::parse(s).context(format!("parsing where expression: {}", s)))
        .collect::<Result<Vec<_>>>()?;
    let filter = match exprs.len() {
        1 => exprs.into_iter().next().unwrap(),
        _ => Expr::And(exprs),
    };

    let plan_report = MoveBuilder::new(folder, target_folder.to_string(), filter.clone())
        .recursive(recursive)
        .plan(vault)?;

    if plan_report.changes.is_empty() && plan_report.errors.is_empty() {
        println!("No matching records.");
        return Ok(());
    }

    // If the plan reported any conflicts, abort early (mirrors the existing
    // CLI's fail-fast behaviour for filename conflicts at the destination).
    if !plan_report.errors.is_empty() {
        let joined = plan_report
            .errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!("{}", joined);
    }

    for change in &plan_report.changes {
        let rel_source = change
            .path
            .strip_prefix(&vault.root)
            .unwrap_or(&change.path);
        let dest = vault.root.join(target_folder).join(
            change
                .path
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("file")),
        );
        let rel_dest = dest.strip_prefix(&vault.root).unwrap_or(&dest);
        println!("{} -> {}", rel_source.display(), rel_dest.display());
    }

    if dry_run {
        println!(
            "\n{}",
            format!(
                "{} file(s) would be moved (dry-run)",
                plan_report.changes.len()
            )
            .yellow()
        );
        return Ok(());
    }

    let exec_report = MoveBuilder::new(folder, target_folder.to_string(), filter)
        .recursive(recursive)
        .execute(vault)?;
    for err in &exec_report.errors {
        let rel = err.path.strip_prefix(&vault.root).unwrap_or(&err.path);
        eprintln!("{} {}: {}", "error:".red(), rel.display(), err.message);
    }
    let success_count = exec_report.changes.len() - exec_report.errors.len();
    println!(
        "\n{}",
        format!("{} file(s) moved to {}", success_count, target_folder).green()
    );

    Ok(())
}
