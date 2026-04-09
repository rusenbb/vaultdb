use std::collections::BTreeMap;

use anyhow::{Context, Result};
use colored::Colorize;

use crate::frontmatter;
use crate::vault::Vault;
use crate::writer;

/// Run the `create` command — create a new note, optionally from a template.
pub fn run_create(
    vault: &Vault,
    folder: &str,
    name: &str,
    template: Option<&str>,
    set_args: &[String],
    dry_run: bool,
) -> Result<()> {
    let folder_path = vault.resolve_folder(folder)?;
    let filename = format!("{}.md", name);
    let dest = folder_path.join(&filename);

    if dest.exists() {
        anyhow::bail!("file already exists: {}", dest.display());
    }

    // Start with template content or minimal frontmatter
    let mut content = match template {
        Some(tmpl_path) => {
            let tmpl_file = vault.root.join(tmpl_path);
            if !tmpl_file.exists() {
                anyhow::bail!("template not found: {}", tmpl_file.display());
            }
            std::fs::read_to_string(&tmpl_file)
                .context(format!("reading template: {}", tmpl_path))?
        }
        None => format!("---\n---\n\n# {}\n", name),
    };

    // Apply --set overrides
    for s in set_args {
        let (field, value) = s
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--set requires FIELD=VALUE format, got: {}", s))?;
        let field = field.trim();
        let value = value.trim();

        // Check if the file has frontmatter we can modify
        if frontmatter::extract_frontmatter(&content).is_some() {
            let (new_content, _) = writer::set_field(&content, field, value)
                .context(format!("setting field '{}' on new note", field))?;
            content = new_content;
        } else {
            // No frontmatter — wrap content with frontmatter
            content = format!(
                "---\n{}: {}\n---\n{}",
                field,
                writer::quote_value(value),
                content
            );
        }
    }

    let rel_dest = dest.strip_prefix(&vault.root).unwrap_or(&dest);

    if dry_run {
        println!(
            "{}",
            format!("would create: {}", rel_dest.display()).yellow()
        );
        println!("\n{}", content);
    } else {
        // Create parent directory if needed
        if let Some(parent) = dest.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(&dest, &content)?;
        println!("{}", format!("created: {}", rel_dest.display()).green());
    }

    Ok(())
}
