//! Phase 4 demo — bulk update via typed `Query::update()`. Runs both
//! plan() (read-only preview) and execute() (writes).
//!
//! Run with:
//! ```bash
//! cargo run -p vaultdb-orm --example paper_mutate
//! ```

use std::fs;

use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use vaultdb_orm::{Note, Query, Vault};

#[derive(Debug, Clone, Serialize, Deserialize, Note)]
#[note(folder = "3-Notes", filter = "tags contains type/paper")]
struct Paper {
    #[serde(rename = "_name")]
    title: String,
    year: i32,
    tags: Vec<String>,
    #[serde(default)]
    rating: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = build_demo_vault()?;
    let vault = Vault::with_root(vault_dir.path().to_path_buf());

    println!("--- before: all papers ---");
    for p in Query::<Paper>::new(&vault).fetch()? {
        println!(
            "  {} ({}) rating={:?} tags={:?}",
            p.title, p.year, p.rating, p.tags
        );
    }

    println!("\n--- plan: tag pre-2020 unrated papers as 'skim' ---");
    let plan = Query::<Paper>::new(&vault)
        .filter(Paper::rating().is_null())
        .filter(Paper::year().lt(2020))
        .update()?
        .set(Paper::rating(), "skim")
        .plan()?;
    println!(
        "  plan would touch {} files; {} errors",
        plan.changes.len(),
        plan.errors.len()
    );
    for c in &plan.changes {
        println!("    - {}", c.path.display());
    }

    println!("\n--- execute the same update ---");
    let report = Query::<Paper>::new(&vault)
        .filter(Paper::rating().is_null())
        .filter(Paper::year().lt(2020))
        .update()?
        .set(Paper::rating(), "skim")
        .execute()?;
    println!("  wrote {} files", report.changes.len());

    println!("\n--- after: all papers ---");
    for p in Query::<Paper>::new(&vault).fetch()? {
        println!("  {} ({}) rating={:?}", p.title, p.year, p.rating);
    }

    println!("\n--- safety: .update() without .filter() fails ---");
    match Query::<Paper>::new(&vault).update() {
        Ok(_) => panic!("update without user filter must error"),
        Err(e) => println!("  rejected: {e}"),
    }

    Ok(())
}

fn build_demo_vault() -> std::io::Result<TempDir> {
    let dir = TempDir::new()?;
    let folder = dir.path().join("3-Notes");
    fs::create_dir_all(&folder)?;
    let entries = [
        (
            "Attention Is All You Need",
            "---\ntags: [type/paper, topic/attention]\nyear: 2017\nrating: deep\n---\nbody\n",
        ),
        (
            "BERT",
            "---\ntags: [type/paper, topic/nlp]\nyear: 2018\n---\nbody\n",
        ),
        (
            "GPT-4 Technical Report",
            "---\ntags: [type/paper, topic/llm]\nyear: 2023\nrating: read\n---\nbody\n",
        ),
        (
            "Llama 3 Herd",
            "---\ntags: [type/paper, topic/llm]\nyear: 2024\n---\nbody\n",
        ),
    ];
    for (name, content) in entries {
        fs::write(folder.join(format!("{name}.md")), content)?;
    }
    Ok(dir)
}
