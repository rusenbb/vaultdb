//! Phase 5 demo — graph-flavoured queries via `#[note(wikilink)]` and
//! `#[note(backlink)]`. The struct's `cites` / `cited_by` fields are
//! markers for relation accessors; they don't auto-materialise in v1
//! (frontmatter is still the only data source for them).
//!
//! The interesting filters are the ones that traverse the citation
//! graph in a single query — the killer ORM feature for a vault.
//!
//! Run with:
//! ```bash
//! cargo run -p vaultdb-orm --example paper_graph
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

    /// Outgoing wiki-links. Not populated automatically in v1 —
    /// the field is here as a marker for the relation accessor.
    #[serde(default, skip)]
    #[note(wikilink)]
    cites: Vec<String>,

    /// Notes that wiki-link to this paper.
    #[serde(default, skip)]
    #[note(backlink)]
    cited_by: Vec<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = build_demo_vault()?;
    let vault = Vault::with_root(vault_dir.path().to_path_buf());

    println!("--- papers that cite a specific note ---");
    let cite_aiayn: Vec<Paper> = Query::<Paper>::new(&vault)
        .filter(Paper::cites().to("Attention Is All You Need"))
        .fetch()?;
    for p in &cite_aiayn {
        println!("  {} ({})", p.title, p.year);
    }

    println!("\n--- papers that cite ANY paper tagged topic/attention ---");
    let topic_filter = Paper::tags().contains("topic/attention");
    let cite_attention: Vec<Paper> = Query::<Paper>::new(&vault)
        .filter(Paper::cites().any(topic_filter))
        .fetch()?;
    for p in &cite_attention {
        println!("  {} ({})", p.title, p.year);
    }

    println!("\n--- papers that have ANY paper citing them (backlinks exist) ---");
    let backlinked: Vec<Paper> = Query::<Paper>::new(&vault)
        .filter(Paper::cited_by().any(Paper::tags().contains("type/paper")))
        .fetch()?;
    for p in &backlinked {
        println!("  {} ({})", p.title, p.year);
    }

    println!("\n--- papers from 2024 that cite a paper tagged topic/attention ---");
    let combined: Vec<Paper> = Query::<Paper>::new(&vault)
        .filter(Paper::year().eq(2024) & Paper::cites().any(Paper::tags().contains("topic/attention")))
        .fetch()?;
    for p in &combined {
        println!("  {} ({})", p.title, p.year);
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
            "---\ntags: [type/paper, topic/attention]\nyear: 2017\n---\nFoundational paper.\n",
        ),
        (
            "BERT",
            "---\ntags: [type/paper, topic/nlp]\nyear: 2018\n---\nBuilds on [[Attention Is All You Need]].\n",
        ),
        (
            "GPT-4 Technical Report",
            "---\ntags: [type/paper, topic/llm]\nyear: 2023\n---\nUses [[Attention Is All You Need]] internally.\n",
        ),
        (
            "Llama 3 Herd",
            "---\ntags: [type/paper, topic/llm]\nyear: 2024\n---\nCites [[BERT]] and [[GPT-4 Technical Report]].\n",
        ),
        (
            "Sparse Mixtures",
            "---\ntags: [type/paper, topic/llm]\nyear: 2024\n---\nCites [[Attention Is All You Need]].\n",
        ),
    ];
    for (name, content) in entries {
        fs::write(folder.join(format!("{name}.md")), content)?;
    }
    Ok(dir)
}
