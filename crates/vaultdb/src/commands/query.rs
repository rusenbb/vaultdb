use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;

use crate::cli::{CsvDelimiter, OutputFormat};
use crate::output;
use vaultdb_core::links::LinkGraph;
use vaultdb_core::record::{Record, Value};
use vaultdb_core::render;
use vaultdb_core::vault::Vault;
use vaultdb_core::{Expr, LinkPredicate};

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

/// Combine multiple `--where` strings (AND-ed) and the relational filters into
/// a single optional `Expr`. Returns `None` when there are no constraints.
fn build_filter(where_strs: &[String], relational: &RelationalFilters) -> Result<Option<Expr>> {
    let mut clauses: Vec<Expr> = Vec::new();

    for s in where_strs {
        clauses.push(Expr::parse(s).context(format!("parsing where expression: {}", s))?);
    }
    for target in &relational.links_to {
        clauses.push(Expr::LinksTo(LinkPredicate::Target(target.clone())));
    }
    for target in &relational.linked_from {
        clauses.push(Expr::LinkedFrom(LinkPredicate::Target(target.clone())));
    }
    for s in &relational.links_to_where {
        let inner = Expr::parse(s).context(format!("parsing --links-to-where: {}", s))?;
        clauses.push(Expr::LinksTo(LinkPredicate::Where(Box::new(inner))));
    }
    for s in &relational.linked_from_where {
        let inner = Expr::parse(s).context(format!("parsing --linked-from-where: {}", s))?;
        clauses.push(Expr::LinkedFrom(LinkPredicate::Where(Box::new(inner))));
    }

    Ok(match clauses.len() {
        0 => None,
        1 => Some(clauses.into_iter().next().unwrap()),
        _ => Some(Expr::And(clauses)),
    })
}

/// True if the query needs a `LinkGraph` to be built. The library's
/// `expr_uses_links` covers the filter (it knows about both `LinksTo`/
/// `LinkedFrom` variants and graph-virtual-field predicates); this layer
/// adds the CLI-only concerns: select / sort flags can reference graph
/// virtual fields too, and `--links-to` / `--linked-from` always need one.
fn needs_graph(
    select: &Option<String>,
    sort: Option<&str>,
    relational: &RelationalFilters,
    filter: Option<&Expr>,
) -> bool {
    if !relational.is_empty() {
        return true;
    }
    if filter.is_some_and(vaultdb_core::filter::expr_uses_links) {
        return true;
    }
    let projection: Vec<&str> = select.as_deref().into_iter().chain(sort).collect();
    vaultdb_core::filter::GRAPH_VIRTUAL_FIELDS
        .iter()
        .any(|gf| projection.iter().any(|s| s.contains(gf)))
}

/// Run the `query` command.
#[allow(clippy::too_many_arguments)]
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
    output_path: Option<&Path>,
    csv_delimiter: CsvDelimiter,
) -> Result<()> {
    let folder_path = vault.resolve_folder(folder)?;
    let filter = build_filter(where_strs, relational)?;
    let use_graph = needs_graph(select, sort_field, relational, filter.as_ref());

    // Load records — with content if we need graph fields
    let records = if use_graph {
        vault
            .load_records_with_content(&folder_path, recursive, verbose)?
            .records
    } else {
        vault
            .load_records(&folder_path, recursive, verbose)?
            .records
    };

    // Build link index if needed
    let link_index = if use_graph {
        Some(LinkGraph::build_with_root(&records, Some(&vault.root)))
    } else {
        None
    };

    // Filter
    let mut filtered: Vec<Record> = if let Some(expr) = filter.as_ref() {
        records
            .into_iter()
            .filter(|r| {
                vaultdb_core::filter::evaluate_expr(expr, r, &vault.root, link_index.as_ref())
            })
            .collect()
    } else {
        records
    };

    // Sort
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

    // Limit
    if let Some(n) = limit {
        filtered.truncate(n);
    }

    // Output
    let select_fields: Vec<String> = select
        .as_ref()
        .map(|s| s.split(',').map(|f| f.trim().to_string()).collect())
        .unwrap_or_default();

    // `--output <path>` short-circuits stdout: we render via the core
    // renderer to a file under the vault, print the resolved path, and
    // skip the table/JSON/etc. dump that would normally go to stdout.
    if let Some(out_path) = output_path {
        let fmt = render::Format::from_path(out_path)
            .context("inferring export format from --output extension")?
            .with_csv_delimiter(csv_delimiter.as_byte());
        let select_opt: Option<&[String]> = if select_fields.is_empty() {
            None
        } else {
            Some(&select_fields)
        };
        let written = render::export_records(
            &vault.root,
            out_path,
            fmt,
            &filtered,
            select_opt,
            link_index.as_ref(),
        )
        .context("writing export file")?;
        println!("{}", written.display());
        if verbose {
            eprintln!("{} record(s) exported", filtered.len());
        }
        return Ok(());
    }

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
    let no_relational = RelationalFilters {
        links_to: vec![],
        linked_from: vec![],
        links_to_where: vec![],
        linked_from_where: vec![],
    };
    let filter = build_filter(where_strs, &no_relational)?;
    let use_graph = needs_graph(&None, None, &no_relational, filter.as_ref());

    let records = if use_graph {
        vault
            .load_records_with_content(&folder_path, recursive, verbose)?
            .records
    } else {
        vault
            .load_records(&folder_path, recursive, verbose)?
            .records
    };

    let link_index = if use_graph {
        Some(LinkGraph::build_with_root(&records, Some(&vault.root)))
    } else {
        None
    };

    let count = if let Some(expr) = filter.as_ref() {
        records
            .iter()
            .filter(|r| {
                vaultdb_core::filter::evaluate_expr(expr, r, &vault.root, link_index.as_ref())
            })
            .count()
    } else {
        records.len()
    };

    println!("{}", count);
    Ok(())
}

/// Run the `fields` command — list all unique frontmatter keys with types and frequencies.
pub fn run_fields(vault: &Vault, folder: &str, recursive: bool, verbose: bool) -> Result<()> {
    let folder_path = vault.resolve_folder(folder)?;
    let records = vault
        .load_records(&folder_path, recursive, verbose)?
        .records;
    let total = records.len();

    // Collect field info: (types seen, count of non-null)
    let mut field_info: BTreeMap<String, FieldInfo> = BTreeMap::new();

    for record in &records {
        for (key, value) in &record.fields {
            let info = field_info.entry(key.clone()).or_default();
            info.total += 1;
            let type_name = value.type_name().to_string();
            if !matches!(value, Value::Null) {
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
    let records = vault
        .load_records(&folder_path, recursive, verbose)?
        .records;

    let mut tag_counts: BTreeMap<String, usize> = BTreeMap::new();

    for record in &records {
        if let Some(Value::List(tags)) = record.fields.get("tags") {
            for tag in tags {
                if let Value::String(s) = tag {
                    *tag_counts.entry(s.clone()).or_insert(0) += 1;
                }
            }
        }
    }

    // Sort by count descending
    let mut sorted: Vec<(String, usize)> = tag_counts.into_iter().collect();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.1));

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

fn compare_field_values(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(Value::Null), Some(Value::Null)) => std::cmp::Ordering::Equal,
        (Some(Value::Null), _) => std::cmp::Ordering::Less,
        (_, Some(Value::Null)) => std::cmp::Ordering::Greater,
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
