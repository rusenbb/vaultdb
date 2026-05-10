//! Synthetic benchmark — runs a frontmatter-only query and a graph-touching
//! query against a vault of N notes (default 1000), printing wall-clock
//! times. Run with:
//!
//!   cargo run --release --example bench --                   # 1000 notes
//!   cargo run --release --example bench -- 10000             # 10k notes
//!   cargo run --release --example bench -- 50000             # 50k notes
//!
//! The numbers go in the README's perf table — we run this rather than
//! making them up.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use vaultdb_core::{Expr, GraphScope, LinkGraph, Predicate, Query, Value, Vault};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let n: usize = std::env::args()
        .nth(1)
        .as_deref()
        .unwrap_or("1000")
        .parse()
        .expect("note count must be a positive integer");

    let dir = std::env::temp_dir().join(format!("vaultdb-bench-{}", n));
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::create_dir_all(dir.join(".obsidian"))?;
    fs::create_dir_all(dir.join("notes"))?;

    println!("Generating {} notes in {}", n, dir.display());
    let gen_start = Instant::now();
    generate_vault(&dir.join("notes"), n)?;
    let gen_elapsed = gen_start.elapsed();
    println!("  generated in {:.2}s", gen_elapsed.as_secs_f64());

    let vault = Vault::with_root(dir.clone());

    // Warm-up: a no-op load so the page cache is hot for the next runs.
    let _warmup = vault.load_records(&dir.join("notes"), false, false)?;

    // ── Frontmatter query ────────────────────────────────────────────────
    let q = Query {
        folder: "notes".into(),
        filter: Some(Expr::Predicate(Predicate::Equals {
            field: "status".into(),
            value: Value::String("active".into()),
        })),
        select: None,
        sort: None,
        limit: None,
        recursive: false,
    };
    let mut samples = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        let results = vault.query(&q)?;
        samples.push((t.elapsed(), results.len()));
    }
    let (best_fm, hits_fm) = samples
        .iter()
        .min_by_key(|(d, _)| *d)
        .copied()
        .expect("3 samples");
    println!(
        "Frontmatter query (status=active, no graph): {:?} ({} hits)",
        best_fm, hits_fm
    );

    // ── Graph-touching query ─────────────────────────────────────────────
    let q_graph = Query {
        folder: "notes".into(),
        filter: Some(Expr::Predicate(Predicate::Compare {
            field: "_link_count".into(),
            op: vaultdb_core::CompareOp::Gt,
            value: Value::Integer(0),
        })),
        select: None,
        sort: None,
        limit: None,
        recursive: false,
    };
    let mut samples = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        let results = vault.query(&q_graph)?;
        samples.push((t.elapsed(), results.len()));
    }
    let (best_gr, hits_gr) = samples
        .iter()
        .min_by_key(|(d, _)| *d)
        .copied()
        .expect("3 samples");
    println!(
        "Graph query    (_link_count > 0, builds graph): {:?} ({} hits)",
        best_gr, hits_gr
    );

    // ── LinkGraph::All build ────────────────────────────────────────────
    let mut samples = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        let g = vault.link_graph(GraphScope::All)?;
        samples.push((t.elapsed(), g.outgoing_count("note-0")));
    }
    let (best_lg, _) = samples
        .iter()
        .min_by_key(|(d, _)| *d)
        .copied()
        .expect("3 samples");
    println!("link_graph(All): {:?}", best_lg);

    // Cleanup
    fs::remove_dir_all(&dir)?;
    let _ = LinkGraph::default(); // keep type imported on this line
    Ok(())
}

fn generate_vault(folder: &PathBuf, n: usize) -> std::io::Result<()> {
    // Each note has frontmatter (status alternating) and ~3 wikilinks
    // pointing at other notes (some of which exist, some don't, simulating
    // a real vault with unresolved links).
    let names: Vec<String> = (0..n).map(|i| format!("note-{}", i)).collect();
    for (i, name) in names.iter().enumerate() {
        let status = if i % 2 == 0 { "active" } else { "draft" };
        let body = if n > 3 {
            let a = (i + 1) % n;
            let b = (i + 7) % n;
            let c = (i + 113) % n;
            format!("Body. See [[{}]], [[{}]], [[{}]].", names[a], names[b], names[c])
        } else {
            "Body.".to_string()
        };
        let content = format!(
            "---\nstatus: {}\ntags:\n  - bench\n  - kind/note-{}\n---\n{}\n",
            status,
            i % 50,
            body,
        );
        fs::write(folder.join(format!("{}.md", name)), content)?;
    }
    Ok(())
}
