use anyhow::{Context, Result};
use colored::Colorize;

use vaultdb_core::schema::{self, VaultSchema};
use vaultdb_core::vault::Vault;
use vaultdb_core::{CreateBuilder, Value};

/// Run the `create` command — create a new note, optionally from a template
/// and a schema collection.
///
/// When `<vault>/vaultdb-schema.yaml` exists and contains a collection whose
/// `folder` matches `folder` exactly, that collection's `default:` /
/// `default_expr:` fields auto-fill anything not supplied via `--set`, and
/// `required:` is enforced before the file is written.
pub fn run_create(
    vault: &Vault,
    folder: &str,
    name: &str,
    template: Option<&str>,
    set_args: &[String],
    dry_run: bool,
) -> Result<()> {
    let mut builder = CreateBuilder::new(folder, name);

    if let Some(t) = template {
        builder = builder.template(t);
    }

    for s in set_args {
        let (field, value) = s
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--set requires FIELD=VALUE format, got: {}", s))?;
        builder = builder.set(field.trim(), Value::parse_scalar(value.trim()));
    }

    // Schema enforcement: when the file exists we attach the full
    // vault schema so every applicable collection (folder ancestors +
    // filter matches) participates — defaults layered, post-state
    // strictly validated. A malformed schema file is a hard error
    // rather than a silent downgrade to "no schema."
    let schema_path = schema::schema_path(&vault.root);
    if schema_path.is_file() {
        let vault_schema: VaultSchema = schema::load_schema(&schema_path)
            .context(format!("loading {}", schema_path.display()))?;
        builder = builder.with_vault_schema(vault_schema);
    }

    if dry_run {
        let (report, content) = builder.plan_with_content(vault)?;
        if !report.errors.is_empty() {
            for err in &report.errors {
                let rel_path = err.path.strip_prefix(&vault.root).unwrap_or(&err.path);
                eprintln!("{} {}: {}", "error:".red(), rel_path.display(), err.message);
            }
            anyhow::bail!("create plan failed");
        }
        for change in &report.changes {
            let rel_path = change
                .path
                .strip_prefix(&vault.root)
                .unwrap_or(&change.path);
            println!(
                "{}",
                format!("would create: {}", rel_path.display()).yellow()
            );
            println!("  {}", change.description);
        }
        if let Some(c) = content {
            println!("\n{}", c);
        }
    } else {
        let report = builder.execute(vault)?;
        if !report.errors.is_empty() {
            for err in &report.errors {
                let rel_path = err.path.strip_prefix(&vault.root).unwrap_or(&err.path);
                eprintln!("{} {}: {}", "error:".red(), rel_path.display(), err.message);
            }
            anyhow::bail!("create failed");
        }
        for change in &report.changes {
            let rel_path = change
                .path
                .strip_prefix(&vault.root)
                .unwrap_or(&change.path);
            println!("{}", format!("created: {}", rel_path.display()).green());
            println!("  {}", change.description);
        }
    }

    Ok(())
}
