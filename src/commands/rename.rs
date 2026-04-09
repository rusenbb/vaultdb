use anyhow::Result;
use colored::Colorize;

use crate::links::LinkIndex;
use crate::vault::Vault;

/// Run the `rename` command — rename a note and update all wiki-links across the vault.
pub fn run_rename(
    vault: &Vault,
    old_name: &str,
    new_name: &str,
    folder: &str,
    dry_run: bool,
    verbose: bool,
) -> Result<()> {
    let folder_path = vault.resolve_folder(folder)?;

    // Find the source file
    let old_filename = format!("{}.md", old_name);
    let old_path = folder_path.join(&old_filename);
    if !old_path.exists() {
        anyhow::bail!("file not found: {}", old_path.display());
    }

    let new_filename = format!("{}.md", new_name);
    let new_path = folder_path.join(&new_filename);
    if new_path.exists() {
        anyhow::bail!("target already exists: {}", new_path.display());
    }

    // Load all records to find references and build link index
    let records = vault.load_records_with_content(&vault.root, true, verbose)?;
    let index = LinkIndex::build_with_root(&records, Some(&vault.root));

    // Check for duplicate filenames
    if index.is_ambiguous(old_name) {
        let paths = index.paths_for_name(old_name);
        eprintln!(
            "{} '{}' exists in multiple locations:",
            "warning:".yellow(),
            old_name
        );
        for p in &paths {
            eprintln!("  {}", p);
        }
        eprintln!("only renaming the one in --folder ({})", folder);
    }

    if index.is_ambiguous(new_name) {
        anyhow::bail!(
            "target name '{}' already exists in multiple locations — rename would increase ambiguity",
            new_name
        );
    }

    // Find all files that reference the old name
    let backlinks = index.incoming_links(old_name);

    println!("{} -> {}", old_name.bold(), new_name.bold());
    println!();

    // Rename the file
    println!(
        "  rename: {}",
        old_path
            .strip_prefix(&vault.root)
            .unwrap_or(&old_path)
            .display()
    );

    if !dry_run {
        std::fs::rename(&old_path, &new_path)?;
    }

    // Update wiki-links in all referencing files
    if backlinks.is_empty() {
        println!("  no references to update");
    } else {
        println!("  updating {} reference(s):", backlinks.len());

        // Patterns to search and replace
        // Handle: [[OldName]], [[OldName|alias]], [[OldName#section]], [[OldName#section|alias]]
        let search_patterns = vec![
            format!("[[{}]]", old_name),
            format!("[[{}|", old_name),
            format!("[[{}#", old_name),
        ];
        let replace_with = vec![
            format!("[[{}]]", new_name),
            format!("[[{}|", new_name),
            format!("[[{}#", new_name),
        ];

        for referrer_name in &backlinks {
            // Find the file for this referrer
            let referrer = records.iter().find(|r| r.virtual_name() == *referrer_name);
            let referrer = match referrer {
                Some(r) => r,
                None => continue,
            };

            let content = match &referrer.raw_content {
                Some(c) => c,
                None => continue,
            };

            let mut updated = content.clone();
            let mut changes = 0;
            for (search, replace) in search_patterns.iter().zip(replace_with.iter()) {
                let count = updated.matches(search.as_str()).count();
                if count > 0 {
                    updated = updated.replace(search.as_str(), replace.as_str());
                    changes += count;
                }
            }

            if changes > 0 {
                let rel_path = referrer
                    .path
                    .strip_prefix(&vault.root)
                    .unwrap_or(&referrer.path);
                println!("    {} ({} link(s))", rel_path.display(), changes);

                if !dry_run {
                    std::fs::write(&referrer.path, &updated)?;
                }
            }
        }
    }

    // Also update aliases in the renamed file's own frontmatter if it references itself
    // (rare but possible)

    if dry_run {
        println!("\n{}", format!("(dry-run: no changes written)").yellow());
    } else {
        println!("\n{}", "done".green());
    }

    Ok(())
}
