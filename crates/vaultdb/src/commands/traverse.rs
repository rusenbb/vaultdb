use std::collections::BTreeMap;

use anyhow::{Context, Result};
use colored::Colorize;

use crate::cli::LinkDirection;
use vaultdb_core::links::TraverseDirection;
use vaultdb_core::record::Record;
use vaultdb_core::vault::Vault;
use vaultdb_core::{Direction, Expr, LinkGraph};

/// Run the `traverse` command — BFS from a starting note.
#[allow(clippy::too_many_arguments)]
pub fn run_traverse(
    vault: &Vault,
    name: &str,
    folder: &str,
    depth: usize,
    direction: &LinkDirection,
    where_strs: &[String],
    select: &Option<String>,
    recursive: bool,
    verbose: bool,
) -> Result<()> {
    let folder_path = vault.resolve_folder(folder)?;
    let records = vault
        .load_records_with_content(&folder_path, recursive, verbose)?
        .records;
    let graph = LinkGraph::build_with_root(&records, Some(&vault.root));

    // Parse --where conditions into a single Expr (AND-combined).
    let filter: Option<Expr> = if where_strs.is_empty() {
        None
    } else {
        let exprs: Vec<Expr> = where_strs
            .iter()
            .map(|s| Expr::parse(s).context(format!("parsing where: {}", s)))
            .collect::<Result<Vec<_>>>()?;
        Some(match exprs.len() {
            1 => exprs.into_iter().next().unwrap(),
            _ => Expr::And(exprs),
        })
    };

    // Build a lookup: name -> Record
    let record_map: BTreeMap<String, &Record> =
        records.iter().map(|r| (r.virtual_name(), r)).collect();

    // Convert the public-CLI Direction into the existing internal one used by
    // LinkGraph::traverse (the new `Direction` enum has a From conversion).
    let traverse_dir: TraverseDirection = match direction {
        LinkDirection::Outgoing => Direction::Outgoing.into(),
        LinkDirection::Incoming => Direction::Incoming.into(),
        LinkDirection::Both => Direction::Both.into(),
    };

    let traversal = graph.traverse(name, depth, traverse_dir);

    if traversal.is_empty() || (traversal.len() == 1 && !record_map.contains_key(name)) {
        println!("Note '{}' not found in the link graph.", name);
        return Ok(());
    }

    let select_fields: Vec<String> = select
        .as_ref()
        .map(|s| s.split(',').map(|f| f.trim().to_string()).collect())
        .unwrap_or_default();

    // Group by depth for display
    let mut by_depth: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
    for (note_name, d) in &traversal {
        by_depth.entry(*d).or_default().push(note_name);
    }

    let mut shown = 0;
    let mut filtered_out = 0;

    for (d, names) in &by_depth {
        for note_name in names {
            // Apply where filter if provided
            if let Some(expr) = &filter {
                match record_map.get(*note_name) {
                    Some(record) => {
                        if !vaultdb_core::filter::evaluate_expr(
                            expr,
                            record,
                            &vault.root,
                            Some(&graph),
                        ) {
                            filtered_out += 1;
                            continue;
                        }
                    }
                    None => {
                        filtered_out += 1;
                        continue; // unresolved note, can't check filter
                    }
                }
            }

            let indent = "  ".repeat(*d);
            let prefix = if *d == 0 { "" } else { "→ " };

            let exists = record_map.contains_key(*note_name);
            let name_display = if exists {
                note_name.to_string()
            } else {
                format!("{} {}", note_name, "(unresolved)".dimmed())
            };

            // Extra fields
            let mut extra = String::new();
            if !select_fields.is_empty()
                && let Some(record) = record_map.get(*note_name)
            {
                let parts: Vec<String> = select_fields
                    .iter()
                    .filter_map(|f| {
                        record
                            .get_with_links(f, &vault.root, Some(&graph))
                            .map(|v| format!("{}={}", f, v.display_value()))
                    })
                    .collect();
                if !parts.is_empty() {
                    extra = format!("  {}", parts.join(", ").dimmed());
                }
            }

            let depth_label = format!("[{}]", d).dimmed();
            println!(
                "{}{}{} {}{}",
                indent, prefix, name_display, depth_label, extra
            );
            shown += 1;
        }
    }

    println!(
        "\n{} notes reached (depth {}{})",
        shown,
        depth,
        if filtered_out > 0 {
            format!(", {} filtered out", filtered_out)
        } else {
            String::new()
        }
    );

    Ok(())
}
