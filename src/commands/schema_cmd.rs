use anyhow::{Context, Result};
use colored::Colorize;

use crate::filter::{WhereClause, matches_all};
use crate::schema::{self, VaultSchema};
use crate::vault::Vault;

const SCHEMA_FILENAME: &str = "vaultdb-schema.yaml";

/// Run `schema show` — display schema for a folder.
pub fn run_show(vault: &Vault, folder: &str) -> Result<()> {
    let schema = load_vault_schema(vault)?;
    let matching = find_collections_for_folder(&schema, folder);

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
                            serde_yaml::Value::String(s) => s.clone(),
                            serde_yaml::Value::Number(n) => n.to_string(),
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
    let matching = find_collections_for_folder(&schema, folder);

    if matching.is_empty() {
        println!("No schema defined for folder '{}'", folder);
        return Ok(());
    }

    let folder_path = vault.resolve_folder(folder)?;
    let records = vault.load_records(&folder_path, recursive, verbose)?;
    let mut total_violations = 0;

    for (name, collection) in matching {
        println!("Validating collection: {}", name.bold());

        // Apply collection filter
        let filter_clauses: Vec<WhereClause> = collection
            .filter
            .iter()
            .map(|s| WhereClause::parse(s))
            .collect::<crate::error::Result<Vec<_>>>()
            .context("parsing collection filter")?;

        let filtered: Vec<_> = records
            .iter()
            .filter(|r| filter_clauses.is_empty() || matches_all(&filter_clauses, r, &vault.root))
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
pub fn run_init(vault: &Vault, folder: &str, recursive: bool, verbose: bool) -> Result<()> {
    let folder_path = vault.resolve_folder(folder)?;
    let records = vault.load_records(&folder_path, recursive, verbose)?;

    if records.is_empty() {
        println!("No records found in '{}'", folder);
        return Ok(());
    }

    let collection = schema::infer_schema(folder, &records);
    let schema = VaultSchema {
        collections: std::collections::BTreeMap::from([(folder.to_string(), collection)]),
    };

    let yaml = serde_yaml::to_string(&schema)?;
    println!("{}", yaml);

    let schema_path = vault.root.join(SCHEMA_FILENAME);
    println!(
        "{}",
        format!("To save, write this to: {}", schema_path.display()).dimmed()
    );

    Ok(())
}

fn load_vault_schema(vault: &Vault) -> Result<VaultSchema> {
    let schema_path = vault.root.join(SCHEMA_FILENAME);
    schema::load_schema(&schema_path).context(format!("loading {}", schema_path.display()))
}

fn find_collections_for_folder<'a>(
    schema: &'a VaultSchema,
    folder: &str,
) -> Vec<(&'a String, &'a schema::CollectionSchema)> {
    schema
        .collections
        .iter()
        .filter(|(_, c)| c.folder == folder || c.folder.starts_with(&format!("{}/", folder)))
        .collect()
}
