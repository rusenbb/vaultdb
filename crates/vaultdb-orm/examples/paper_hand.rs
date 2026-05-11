//! Hand-written `impl Note for Paper` — the shape `#[derive(Note)]` will
//! generate in Phase 2.
//!
//! Run with:
//! ```bash
//! cargo run -p vaultdb-orm --example paper_hand
//! ```

use std::fs;

use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use vaultdb_core::{Expr, SortKey, Vault};
use vaultdb_orm::{Note, Query};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Paper {
    #[serde(rename = "_name")]
    title: String,
    year: i32,
    tags: Vec<String>,
    #[serde(default)]
    rating: Option<String>,
}

impl Note for Paper {
    const FOLDER: &'static str = "3-Notes";

    fn discriminator() -> Option<Expr> {
        Expr::parse("tags contains type/paper").ok()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = build_demo_vault()?;
    let vault = Vault::with_root(vault_dir.path().to_path_buf());

    println!("--- all papers ---");
    let papers: Vec<Paper> = Query::<Paper>::new(&vault).fetch()?;
    for p in &papers {
        println!("  {} ({}) {:?}", p.title, p.year, p.tags);
    }

    println!("\n--- papers from >= 2023, sorted by year desc ---");
    let recent: Vec<Paper> = Query::<Paper>::new(&vault)
        .filter(Expr::parse("year >= 2023")?)
        .order_by(SortKey {
            field: "year".into(),
            descending: true,
        })
        .fetch()?;
    for p in &recent {
        println!("  {} ({})", p.title, p.year);
    }

    println!("\n--- first unrated paper ---");
    let first: Option<Paper> = Query::<Paper>::new(&vault)
        .filter(Expr::parse("rating missing")?)
        .first()?;
    if let Some(p) = first {
        println!("  {} ({})", p.title, p.year);
    } else {
        println!("  (none)");
    }

    println!("\n--- count of all papers ---");
    let n = Query::<Paper>::new(&vault).count()?;
    println!("  {n}");

    Ok(())
}

/// Build a synthetic vault under a tempdir so the example is
/// self-contained.
fn build_demo_vault() -> std::io::Result<TempDir> {
    let dir = TempDir::new()?;
    let folder = dir.path().join("3-Notes");
    fs::create_dir_all(&folder)?;

    let papers = [
        (
            "Attention Is All You Need",
            r#"---
tags: [type/paper, topic/attention]
year: 2017
rating: deep
---
Transformer paper.
"#,
        ),
        (
            "BERT",
            r#"---
tags: [type/paper, topic/nlp]
year: 2018
---
Bidirectional encoder representations.
"#,
        ),
        (
            "GPT-4 Technical Report",
            r#"---
tags: [type/paper, topic/llm]
year: 2023
rating: read
---
Closed-weights model write-up.
"#,
        ),
        (
            // A non-paper note in the same folder — discriminator should drop it.
            "Transformer Architecture",
            r#"---
tags: [type/concept]
year: 2017
---
Concept note.
"#,
        ),
    ];

    for (name, content) in papers {
        fs::write(folder.join(format!("{name}.md")), content)?;
    }

    Ok(dir)
}
