use std::collections::BTreeMap;

use anyhow::{Context, Result};
use colored::Colorize;

use crate::cli::OutputFormat;
use crate::filter::{WhereClause, WhereExpr, matches_all_with_links, matches_exprs_with_links};
use crate::links::LinkIndex;
use crate::output;
use crate::record::{FieldValue, Record};
use crate::vault::Vault;

const GRAPH_FIELDS: &[&str] = &["_links", "_link_count", "_backlinks", "_backlink_count"];

/// Relational filter parameters.
pub struct RelationalFilters {
    pub links_to: Vec<String>,
    pub linked_from: Vec<String>,
    pub links_to_where: Vec<String>,
    pub linked_from_where: Vec<String>,
}

impl RelationalFilters {
    pub fn is_empty(&self) -> bool {
        self.links_to.is_empty()
            && self.linked_from.is_empty()
            && self.links_to_where.is_empty()
            && self.linked_from_where.is_empty()
    }
}

/// Check if any string references a graph virtual field.
fn needs_graph(
    where_strs: &[String],
    select: &Option<String>,
    sort: Option<&str>,
    relational: &RelationalFilters,
) -> bool {
    if !relational.is_empty() {
        return true;
    }

    let all_strs: Vec<&str> = where_strs
        .iter()
        .map(|s| s.as_str())
        .chain(select.as_deref())
        .chain(sort)
        .collect();

    GRAPH_FIELDS
        .iter()
        .any(|gf| all_strs.iter().any(|s| s.contains(gf)))
}

/// Run the `query` command.
pub fn run_query(
    vault: &Vault,
    folder: &str,
    where_strs: &[String],
    select: &Option<String>,
    sort_field: Option<&str>,
    desc: bool,
    limit: Option<usize>,
    format: &OutputFormat,
    relational: &RelationalFilters,
    recursive: bool,
    verbose: bool,
) -> Result<()> {
    let folder_path = vault.resolve_folder(folder)?;
    let where_exprs = parse_where_clauses(where_strs)?;
    let use_graph = needs_graph(where_strs, select, sort_field, relational);

    // Load records — with content if we need graph fields
    let records = if use_graph {
        vault.load_records_with_content(&folder_path, recursive, verbose)?
    } else {
        vault.load_records(&folder_path, recursive, verbose)?
    };

    // Build link index if needed
    let link_index = if use_graph {
        Some(LinkIndex::build_with_root(&records, Some(&vault.root)))
    } else {
        None
    };

    // Parse relational where expressions
    let links_to_where_exprs: Vec<Vec<WhereExpr>> = relational
        .links_to_where
        .iter()
        .map(|s| Ok(vec![WhereExpr::parse(s)?]))
        .collect::<crate::error::Result<Vec<_>>>()
        .context("parsing --links-to-where")?;

    let linked_from_where_exprs: Vec<Vec<WhereExpr>> = relational
        .linked_from_where
        .iter()
        .map(|s| Ok(vec![WhereExpr::parse(s)?]))
        .collect::<crate::error::Result<Vec<_>>>()
        .context("parsing --linked-from-where")?;

    // Build name->index lookup for relational joins
    let record_by_name: std::collections::BTreeMap<String, usize> = if use_graph {
        records
            .iter()
            .enumerate()
            .map(|(i, r)| (r.virtual_name(), i))
            .collect()
    } else {
        std::collections::BTreeMap::new()
    };

    // Filter records — collect indices of matching records first to avoid borrow conflicts
    let matching_indices: Vec<usize> = (0..records.len())
        .filter(|&i| {
            let r = &records[i];

            // Standard where filters
            if !matches_all_with_links(&where_exprs, r, &vault.root, link_index.as_ref()) {
                return false;
            }

            // Relational filters (all must pass)
            if let Some(idx) = &link_index {
                let name = r.virtual_name();

                // --links-to: this note must link to the specified note
                for target in &relational.links_to {
                    if !idx.has_link_to(&name, target) {
                        return false;
                    }
                }

                // --linked-from: this note must be linked from the specified note
                for source in &relational.linked_from {
                    if !idx.has_link_from(&name, source) {
                        return false;
                    }
                }

                // --links-to-where: this note must link to at least one note matching the condition
                for exprs in &links_to_where_exprs {
                    let outgoing = idx.outgoing_links(&name);
                    let any_match = outgoing.iter().any(|target| {
                        record_by_name.get(*target).is_some_and(|&ti| {
                            matches_exprs_with_links(exprs, &records[ti], &vault.root, Some(idx))
                        })
                    });
                    if !any_match {
                        return false;
                    }
                }

                // --linked-from-where: this note must be linked from at least one note matching the condition
                for exprs in &linked_from_where_exprs {
                    let incoming = idx.incoming_links(&name);
                    let any_match = incoming.iter().any(|source| {
                        record_by_name.get(*source).is_some_and(|&si| {
                            matches_exprs_with_links(exprs, &records[si], &vault.root, Some(idx))
                        })
                    });
                    if !any_match {
                        return false;
                    }
                }
            }

            true
        })
        .collect();

    let mut filtered: Vec<Record> = matching_indices
        .into_iter()
        .map(|i| records[i].clone())
        .collect();

    if let Some(sort_key) = sort_field {
        filtered.sort_by(|a, b| {
            let va = a.get_with_links(sort_key, &vault.root, link_index.as_ref());
            let vb = b.get_with_links(sort_key, &vault.root, link_index.as_ref());
            compare_field_values(va.as_ref(), vb.as_ref())
        });
        if desc {
            filtered.reverse();
        }
    }

    if let Some(n) = limit {
        filtered.truncate(n);
    }

    let select_fields: Vec<String> = select
        .as_ref()
        .map(|s| s.split(',').map(|f| f.trim().to_string()).collect())
        .unwrap_or_default();

    let out = output::format_records_with_links(
        &filtered,
        &select_fields,
        format,
        &vault.root,
        link_index.as_ref(),
    );
    println!("{}", out);

    if verbose {
        eprintln!("{} record(s) matched", filtered.len());
    }

    Ok(())
}

/// Run the `count` command.
pub fn run_count(
    vault: &Vault,
    folder: &str,
    where_strs: &[String],
    recursive: bool,
    verbose: bool,
) -> Result<()> {
    let folder_path = vault.resolve_folder(folder)?;
    let where_exprs = parse_where_clauses(where_strs)?;
    let no_relational = RelationalFilters {
        links_to: vec![],
        linked_from: vec![],
        links_to_where: vec![],
        linked_from_where: vec![],
    };
    let use_graph = needs_graph(where_strs, &None, None, &no_relational);

    let records = if use_graph {
        vault.load_records_with_content(&folder_path, recursive, verbose)?
    } else {
        vault.load_records(&folder_path, recursive, verbose)?
    };

    let link_index = if use_graph {
        Some(LinkIndex::build_with_root(&records, Some(&vault.root)))
    } else {
        None
    };

    let count = records
        .iter()
        .filter(|r| matches_all_with_links(&where_exprs, r, &vault.root, link_index.as_ref()))
        .count();

    println!("{}", count);
    Ok(())
}

/// Run the `fields` command — list all unique frontmatter keys with types and frequencies.
pub fn run_fields(vault: &Vault, folder: &str, recursive: bool, verbose: bool) -> Result<()> {
    let folder_path = vault.resolve_folder(folder)?;
    let records = vault.load_records(&folder_path, recursive, verbose)?;
    let total = records.len();

    // Collect field info: (types seen, count of non-null)
    let mut field_info: BTreeMap<String, FieldInfo> = BTreeMap::new();

    for record in &records {
        for (key, value) in &record.fields {
            let info = field_info.entry(key.clone()).or_default();
            info.total += 1;
            let type_name = value.type_name().to_string();
            if !matches!(value, FieldValue::Null) {
                info.non_null += 1;
            }
            *info.types.entry(type_name).or_insert(0) += 1;
        }
    }

    println!(
        "{:<25} {:<15} {:<10} {}",
        "FIELD".bold(),
        "TYPE(S)".bold(),
        "COUNT".bold(),
        "COVERAGE".bold()
    );
    println!("{}", "─".repeat(65));

    for (key, info) in &field_info {
        let types: Vec<String> = info.types.keys().cloned().collect();
        let type_str = types.join(", ");
        let coverage = if total > 0 {
            format!("{:.0}%", (info.total as f64 / total as f64) * 100.0)
        } else {
            "—".to_string()
        };
        println!(
            "{:<25} {:<15} {:<10} {}",
            key,
            type_str,
            format!("{}/{}", info.non_null, info.total),
            coverage
        );
    }

    println!("\n{} total records in {}", total, folder);
    Ok(())
}

/// Run the `tags` command — list all tags with counts.
pub fn run_tags(vault: &Vault, folder: &str, recursive: bool, verbose: bool) -> Result<()> {
    let folder_path = vault.resolve_folder(folder)?;
    let records = vault.load_records(&folder_path, recursive, verbose)?;

    let mut tag_counts: BTreeMap<String, usize> = BTreeMap::new();

    for record in &records {
        if let Some(FieldValue::List(tags)) = record.fields.get("tags") {
            for tag in tags {
                if let FieldValue::String(s) = tag {
                    *tag_counts.entry(s.clone()).or_insert(0) += 1;
                }
            }
        }
    }

    // Sort by count descending
    let mut sorted: Vec<(String, usize)> = tag_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    println!("{:<40} {}", "TAG".bold(), "COUNT".bold());
    println!("{}", "─".repeat(50));

    for (tag, count) in &sorted {
        println!("{:<40} {}", tag, count);
    }

    println!("\n{} unique tags", sorted.len());
    Ok(())
}

#[derive(Default)]
struct FieldInfo {
    total: usize,
    non_null: usize,
    types: BTreeMap<String, usize>,
}

fn parse_where_clauses(strs: &[String]) -> Result<Vec<WhereClause>> {
    strs.iter()
        .map(|s| WhereClause::parse(s).context(format!("parsing where expression: {}", s)))
        .collect()
}

fn compare_field_values(a: Option<&FieldValue>, b: Option<&FieldValue>) -> std::cmp::Ordering {
    match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(FieldValue::Null), Some(FieldValue::Null)) => std::cmp::Ordering::Equal,
        (Some(FieldValue::Null), _) => std::cmp::Ordering::Less,
        (_, Some(FieldValue::Null)) => std::cmp::Ordering::Greater,
        (Some(a), Some(b)) => {
            // Try numeric comparison
            if let (Some(fa), Some(fb)) = (a.as_float(), b.as_float()) {
                return fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal);
            }
            // Fall back to string
            a.display_value().cmp(&b.display_value())
        }
    }
}
