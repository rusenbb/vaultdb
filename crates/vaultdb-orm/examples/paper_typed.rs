//! Phase 3 demo — the same vault as `paper_derive`, but every filter is
//! built from typed field accessors. No `Expr::parse(...)` calls in
//! user code.
//!
//! Run with:
//! ```bash
//! cargo run -p vaultdb-orm --example paper_typed
//! ```

use std::fs;

use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use vaultdb_orm::{Note, Query, SortKey, Vault};

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

    println!("--- papers from >= 2023 with topic/llm, sorted by year desc ---");
    let hits: Vec<Paper> = Query::<Paper>::new(&vault)
        .filter(Paper::year().ge(2023) & Paper::tags().contains("topic/llm"))
        .order_by(SortKey {
            field: "year".into(),
            descending: true,
        })
        .fetch()?;
    for p in &hits {
        println!("  {} ({})", p.title, p.year);
    }

    println!("\n--- unrated OR pre-2020 ---");
    let mix: Vec<Paper> = Query::<Paper>::new(&vault)
        .filter(Paper::rating().is_null() | Paper::year().lt(2020))
        .fetch()?;
    for p in &mix {
        println!("  {} ({}) rating={:?}", p.title, p.year, p.rating);
    }

    println!("\n--- NOT (year == 2018) ---");
    let not_2018: Vec<Paper> = Query::<Paper>::new(&vault)
        .filter(!Paper::year().eq(2018))
        .fetch()?;
    for p in &not_2018 {
        println!("  {} ({})", p.title, p.year);
    }

    println!("\n--- title starts with 'B' ---");
    let starts_b: Vec<Paper> = Query::<Paper>::new(&vault)
        .filter(Paper::title().starts_with("B"))
        .fetch()?;
    for p in &starts_b {
        println!("  {}", p.title);
    }

    // Compile-time check: field accessor names match struct field names,
    // and the FieldRef carries the resolved frontmatter key.
    assert_eq!(Paper::title().name(), "_name");
    assert_eq!(Paper::year().name(), "year");
    assert_eq!(Paper::tags().name(), "tags");

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
