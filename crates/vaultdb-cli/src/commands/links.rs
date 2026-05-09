use anyhow::Result;
use colored::Colorize;

use crate::cli::LinkDirection;
use crate::links::LinkIndex;
use crate::vault::Vault;

/// Run the `links` command — show outgoing/incoming links for a note.
pub fn run_links(
    vault: &Vault,
    name: &str,
    folder: &str,
    direction: &LinkDirection,
    recursive: bool,
    verbose: bool,
) -> Result<()> {
    let folder_path = vault.resolve_folder(folder)?;
    let records = vault.load_records_with_content(&folder_path, recursive, verbose)?;
    let index = LinkIndex::build_with_root(&records, Some(&vault.root));

    println!("{}", name.bold());

    match direction {
        LinkDirection::Outgoing | LinkDirection::Both => {
            let outgoing = index.outgoing_links(name);
            println!("\n{} ({}):", "Outgoing links".underline(), outgoing.len());
            if outgoing.is_empty() {
                println!("  (none)");
            } else {
                for link in &outgoing {
                    // Check if the target exists as a record
                    let exists = records.iter().any(|r| r.virtual_name() == *link);
                    if exists {
                        println!("  -> {}", link);
                    } else {
                        println!("  -> {} {}", link, "(unresolved)".dimmed());
                    }
                }
            }
        }
        _ => {}
    }

    match direction {
        LinkDirection::Incoming | LinkDirection::Both => {
            let incoming = index.incoming_links(name);
            println!("\n{} ({}):", "Backlinks".underline(), incoming.len());
            if incoming.is_empty() {
                println!("  (none)");
            } else {
                for link in &incoming {
                    println!("  <- {}", link);
                }
            }
        }
        _ => {}
    }

    println!();
    Ok(())
}
