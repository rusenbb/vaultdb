use std::collections::BTreeMap;

use anyhow::{Context, Result};
use colored::Colorize;

use crate::cli::LinkDirection;
use crate::filter::{WhereClause, matches_all_with_links};
use crate::links::{LinkIndex, TraverseDirection};
use crate::record::Record;
use crate::vault::Vault;

/// Run the `traverse` command — BFS from a starting note.
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
    let records = vault.load_records_with_content(&folder_path, recursive, verbose)?;
    let index = LinkIndex::build_with_root(&records, Some(&vault.root));

    let where_clauses: Vec<WhereClause> = where_strs
        .iter()
        .map(|s| WhereClause::parse(s).context(format!("parsing where: {}", s)))
        .collect::<Result<Vec<_>>>()?;

    // Build a lookup: name -> Record
    let record_map: BTreeMap<String, &Record> =
        records.iter().map(|r| (r.virtual_name(), r)).collect();

    let traverse_dir = match direction {
        LinkDirection::Outgoing => TraverseDirection::Outgoing,
        LinkDirection::Incoming => TraverseDirection::Incoming,
        LinkDirection::Both => TraverseDirection::Both,
    };

    let traversal = index.traverse(name, depth, traverse_dir);

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
            if !where_clauses.is_empty() {
                if let Some(record) = record_map.get(*note_name) {
                    if !matches_all_with_links(&where_clauses, record, &vault.root, Some(&index)) {
                        filtered_out += 1;
                        continue;
                    }
                } else {
                    filtered_out += 1;
                    continue; // unresolved note, can't check filter
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
            if !select_fields.is_empty() {
                if let Some(record) = record_map.get(*note_name) {
                    let parts: Vec<String> = select_fields
                        .iter()
                        .filter_map(|f| {
                            record
                                .get_with_links(f, &vault.root, Some(&index))
                                .map(|v| format!("{}={}", f, v.display_value()))
                        })
                        .collect();
                    if !parts.is_empty() {
                        extra = format!("  {}", parts.join(", ").dimmed());
                    }
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
