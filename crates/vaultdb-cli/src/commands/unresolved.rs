use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use colored::Colorize;

use crate::links::{LinkIndex, TraverseDirection};
use crate::vault::Vault;

/// Run the `unresolved` command — find all wiki-links pointing to non-existent files.
/// Optionally scoped to notes within N hops of a starting note.
pub fn run_unresolved(
    vault: &Vault,
    folder: &str,
    from: Option<&str>,
    depth: usize,
    recursive: bool,
    verbose: bool,
) -> Result<()> {
    let folder_path = vault.resolve_folder(folder)?;
    let records = vault.load_records_with_content(&folder_path, recursive, verbose)?;
    let index = LinkIndex::build_with_root(&records, Some(&vault.root));

    // Collect all known note names
    let known_names: BTreeSet<String> = records.iter().map(|r| r.virtual_name()).collect();

    // If --from is specified, limit scope to notes within --depth hops
    let scope: BTreeSet<String> = match from {
        Some(start) => index
            .traverse(start, depth, TraverseDirection::Outgoing)
            .into_iter()
            .map(|(name, _)| name)
            .collect(),
        None => known_names.clone(),
    };

    // Find unresolved link targets from notes in scope
    let mut unresolved: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for record in &records {
        let source = record.virtual_name();
        if !scope.contains(&source) {
            continue;
        }

        let outgoing = index.outgoing_links(&source);
        for target in outgoing {
            if !known_names.contains(target) {
                unresolved
                    .entry(target.to_string())
                    .or_default()
                    .push(source.clone());
            }
        }
    }

    if unresolved.is_empty() {
        let scope_msg = match from {
            Some(start) => format!(" within {} hops of {}", depth, start),
            None => String::new(),
        };
        println!("No unresolved links{}.", scope_msg);
        return Ok(());
    }

    // Sort by number of references
    let mut sorted: Vec<(String, Vec<String>)> = unresolved.into_iter().collect();
    sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    if let Some(start) = from {
        println!(
            "Unresolved links within {} hops of {}:\n",
            depth,
            start.bold()
        );
    }

    println!(
        "{:<45} {}",
        "UNRESOLVED LINK".bold(),
        "REFERENCED FROM".bold()
    );
    println!("{}", "─".repeat(80));

    for (target, sources) in &sorted {
        println!("{:<45} {} note(s)", target, sources.len());
        if verbose {
            for source in sources {
                println!("  <- {}", source);
            }
        }
    }

    println!(
        "\n{} unresolved link(s) across {} source note(s)",
        sorted.len(),
        sorted
            .iter()
            .flat_map(|(_, s)| s)
            .collect::<BTreeSet<_>>()
            .len()
    );

    Ok(())
}
