use anyhow::{Context, Result};
use colored::Colorize;

use vaultdb_core::Expr;
use vaultdb_core::schema::{self, VaultSchema};
use vaultdb_core::vault::Vault;

/// Run `schema show` — display schema for a folder.
pub fn run_show(vault: &Vault, folder: &str) -> Result<()> {
    let schema = load_vault_schema(vault)?;
    let matching = schema.collections_for_folder(folder);

    if matching.is_empty() {
        println!("No schema defined for folder '{}'", folder);
        println!("Run `vaultdb schema init {}` to generate one.", folder);
        return Ok(());
    }

    for (name, collection) in matching {
        println!("{}", name.bold());
        if let Some(desc) = &collection.description {
            println!("  {}", desc);
        }
        println!("  folder: {}", collection.folder);
        if !collection.filter.is_empty() {
            println!("  filter: {:?}", collection.filter);
        }
        if !collection.required.is_empty() {
            println!("  required: {:?}", collection.required);
        }
        if !collection.fields.is_empty() {
            println!("  fields:");
            for (field_name, field_schema) in &collection.fields {
                let mut desc = format!("    {}: {}", field_name, field_schema.field_type);
                if !field_schema.enum_values.is_empty() {
                    let vals: Vec<String> = field_schema
                        .enum_values
                        .iter()
                        .map(|v| match v {
                            vaultdb_core::Value::String(s) => s.clone(),
                            vaultdb_core::Value::Integer(i) => i.to_string(),
                            vaultdb_core::Value::Float(f) => f.to_string(),
                            vaultdb_core::Value::Bool(b) => b.to_string(),
                            vaultdb_core::Value::Null => "null".to_string(),
                            other => format!("{:?}", other),
                        })
                        .collect();
                    desc.push_str(&format!(" [{}]", vals.join(", ")));
                }
                if let Some(min) = field_schema.min {
                    desc.push_str(&format!(" min={}", min));
                }
                if let Some(max) = field_schema.max {
                    desc.push_str(&format!(" max={}", max));
                }
                println!("{}", desc);
            }
        }
        println!();
    }

    Ok(())
}

/// Run `schema validate` — check records against their schema.
pub fn run_validate(vault: &Vault, folder: &str, recursive: bool, verbose: bool) -> Result<()> {
    let schema = load_vault_schema(vault)?;
    let matching = schema.collections_for_folder(folder);

    if matching.is_empty() {
        println!("No schema defined for folder '{}'", folder);
        return Ok(());
    }

    let folder_path = vault.resolve_folder(folder)?;
    let records = vault
        .load_records(&folder_path, recursive, verbose)?
        .records;
    let mut total_violations = 0;

    for (name, collection) in matching {
        println!("Validating collection: {}", name.bold());

        // Apply collection filter — multiple filter strings are AND-ed
        let filter_exprs: Vec<Expr> = collection
            .filter
            .iter()
            .map(|s| Expr::parse(s))
            .collect::<vaultdb_core::error::Result<Vec<_>>>()
            .context("parsing collection filter")?;
        let combined_filter: Option<Expr> = match filter_exprs.len() {
            0 => None,
            1 => Some(filter_exprs.into_iter().next().unwrap()),
            _ => Some(Expr::And(filter_exprs)),
        };

        let filtered: Vec<_> = records
            .iter()
            .filter(|r| {
                combined_filter.as_ref().is_none_or(|expr| {
                    vaultdb_core::filter::evaluate_expr(expr, r, &vault.root, None)
                })
            })
            .collect();

        let mut violations_count = 0;
        for record in &filtered {
            let filename = record.virtual_name();
            let violations = schema::validate_record(&filename, &record.fields, collection);

            for v in &violations {
                println!("  {} {}", "!".red(), v);
                violations_count += 1;
            }
        }

        if violations_count == 0 {
            println!("  {} {} records, all valid", "✓".green(), filtered.len());
        } else {
            println!(
                "\n  {} violations in {} records",
                violations_count,
                filtered.len()
            );
        }
        total_violations += violations_count;
        println!();
    }

    if total_violations > 0 {
        println!(
            "{}",
            format!("{} total violation(s)", total_violations).red()
        );
    } else {
        println!("{}", "All validations passed".green());
    }

    Ok(())
}

/// Run `schema init` — infer schema from existing data.
///
/// With `write = false` (the historical behaviour), the inferred YAML is
/// printed to stdout for review. With `write = true`, the YAML is merged
/// into `<vault>/vaultdb-schema.yaml` (existing collections at the same
/// folder are replaced; other collections are preserved).
pub fn run_init(
    vault: &Vault,
    folder: &str,
    recursive: bool,
    verbose: bool,
    write: bool,
) -> Result<()> {
    let folder_path = vault.resolve_folder(folder)?;
    let records = vault
        .load_records(&folder_path, recursive, verbose)?
        .records;

    if records.is_empty() {
        println!("No records found in '{}'", folder);
        return Ok(());
    }

    let inferred = schema::infer_schema(folder, &records);
    let schema_path = schema::schema_path(&vault.root);

    if write {
        // Merge into the existing schema file if one exists, otherwise
        // start fresh. Replacing an existing collection at the same key
        // is the obvious behaviour for `schema init` — the alternative
        // (refuse to overwrite) makes the command useless on second run.
        let mut full = if schema_path.exists() {
            schema::load_schema(&schema_path)
                .context(format!("loading existing {}", schema_path.display()))?
        } else {
            VaultSchema {
                collections: std::collections::BTreeMap::new(),
            }
        };
        full.collections.insert(folder.to_string(), inferred);

        let yaml = schema::schema_to_yaml(&full)?;
        std::fs::write(&schema_path, &yaml)
            .context(format!("writing {}", schema_path.display()))?;
        println!(
            "{}",
            format!("wrote schema for '{}' to {}", folder, schema_path.display()).green()
        );
    } else {
        let preview = VaultSchema {
            collections: std::collections::BTreeMap::from([(folder.to_string(), inferred)]),
        };
        let yaml = schema::schema_to_yaml(&preview)?;
        println!("{}", yaml);
        println!(
            "{}",
            format!(
                "Preview only. Re-run with --write to save to {}",
                schema_path.display()
            )
            .dimmed()
        );
    }

    Ok(())
}

fn load_vault_schema(vault: &Vault) -> Result<VaultSchema> {
    let schema_path = schema::schema_path(&vault.root);
    schema::load_schema(&schema_path).context(format!("loading {}", schema_path.display()))
}
