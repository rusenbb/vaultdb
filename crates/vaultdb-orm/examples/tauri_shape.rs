//! Phase 6 demo — the *shape* of a Tauri integration, without
//! actually pulling Tauri in. Shows:
//!
//! 1. Models with `#[derive(Note, Serialize, Deserialize)]` (add
//!    `specta::Type` in your real Tauri app).
//! 2. Narrow, typed input structs per command (no raw `Query` AST
//!    flowing across the IPC boundary).
//! 3. The plan/execute mutation pattern with server-side plan storage
//!    keyed by an opaque ID.
//!
//! Run with:
//! ```bash
//! cargo run -p vaultdb-orm --example tauri_shape
//! ```
//!
//! In a real Tauri app:
//!
//! ```ignore
//! // Cargo.toml
//! [dependencies]
//! vaultdb-orm = { path = "../vaultdb/crates/vaultdb-orm" }
//! tauri = "2"
//! specta = "2"
//! tauri-specta = "2"
//! serde = { version = "1", features = ["derive"] }
//!
//! // models.rs — derive Type alongside Note
//! #[derive(Serialize, Deserialize, Note, specta::Type)]
//! #[note(folder = "3-Notes", filter = "tags contains type/paper")]
//! pub struct Paper { ... }
//!
//! // commands.rs — each command #[specta::specta]
//! #[tauri::command]
//! #[specta::specta]
//! pub async fn list_papers(
//!     vault: tauri::State<'_, VaultState>,
//!     filter: PaperFilter,
//! ) -> Result<Vec<Paper>, String> { ... }
//!
//! // Frontend (auto-generated TS bindings):
//! //   const papers = await commands.listPapers({ year_min: 2024 });
//! ```

use std::collections::HashMap;
use std::fs;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use vaultdb_orm::{MutationReport, Note, Query, Vault};

/// A `specta::Type` derive would go here too in a real Tauri app.
#[derive(Debug, Clone, Serialize, Deserialize, Note)]
#[note(folder = "3-Notes", filter = "tags contains type/paper")]
pub struct Paper {
    #[serde(rename = "_name")]
    pub title: String,
    pub year: i32,
    pub tags: Vec<String>,
    #[serde(default)]
    pub rating: Option<String>,
}

/// Narrow, typed input — no raw Query AST flowing across IPC.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PaperFilter {
    pub year_min: Option<i32>,
    pub year_max: Option<i32>,
    pub tag: Option<String>,
    pub unrated_only: bool,
}

/// Returned to the frontend instead of the full `MutationReport`
/// (which can be large). The plan itself stays server-side.
#[derive(Debug, Clone, Serialize)]
pub struct MutationPreview {
    pub plan_id: String,
    pub changes: usize,
    pub errors: usize,
    pub sample_paths: Vec<String>,
}

/// Long-lived state held by the Tauri runtime (`tauri::State`).
/// Holds the [`Vault`] plus a plan store keyed by UUID so frontend can
/// preview, then execute, without round-tripping the whole report.
pub struct VaultState {
    pub vault: Vault,
    pub plans: Mutex<HashMap<String, PlanEntry>>,
}

/// One server-side parked plan. In a real app the closure would be
/// `Box<dyn FnOnce(&Vault) -> Result<MutationReport, OrmError> + Send>`
/// so any builder type can be stored, but for this demo we monomorphise.
pub struct PlanEntry {
    pub filter_year_max: i32,
    pub set_rating: String,
}

// ── "Tauri commands" — plain async fns for this demo ──────────────────────

pub async fn list_papers(
    state: &VaultState,
    filter: PaperFilter,
) -> std::result::Result<Vec<Paper>, String> {
    let mut q = Query::<Paper>::new(&state.vault);

    if let Some(y) = filter.year_min {
        q = q.filter(Paper::year().ge(y));
    }
    if let Some(y) = filter.year_max {
        q = q.filter(Paper::year().le(y));
    }
    if let Some(t) = filter.tag {
        q = q.filter(Paper::tags().contains(t));
    }
    if filter.unrated_only {
        q = q.filter(Paper::rating().is_null());
    }

    q.fetch().map_err(|e| e.to_string())
}

pub async fn plan_skim_old_papers(
    state: &VaultState,
    year_max: i32,
) -> std::result::Result<MutationPreview, String> {
    // Run a plan() right now to get an accurate preview …
    let plan = Query::<Paper>::new(&state.vault)
        .filter(Paper::year().lt(year_max))
        .filter(Paper::rating().is_null())
        .update()
        .map_err(|e| e.to_string())?
        .set(Paper::rating(), "skim")
        .plan()
        .map_err(|e| e.to_string())?;

    // … then park the planned operation server-side under an opaque id.
    let plan_id = format!("plan-{}", uuid_lite());
    let preview = MutationPreview {
        plan_id: plan_id.clone(),
        changes: plan.changes.len(),
        errors: plan.errors.len(),
        sample_paths: plan
            .changes
            .iter()
            .take(3)
            .map(|c| c.path.display().to_string())
            .collect(),
    };
    state.plans.lock().unwrap().insert(
        plan_id,
        PlanEntry {
            filter_year_max: year_max,
            set_rating: "skim".into(),
        },
    );
    Ok(preview)
}

pub async fn execute_plan(
    state: &VaultState,
    plan_id: String,
) -> std::result::Result<MutationReport, String> {
    let entry = state
        .plans
        .lock()
        .unwrap()
        .remove(&plan_id)
        .ok_or_else(|| format!("no parked plan with id {plan_id}"))?;

    Query::<Paper>::new(&state.vault)
        .filter(Paper::year().lt(entry.filter_year_max))
        .filter(Paper::rating().is_null())
        .update()
        .map_err(|e| e.to_string())?
        .set(Paper::rating(), entry.set_rating)
        .execute()
        .map_err(|e| e.to_string())
}

fn uuid_lite() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

// ── Demo driver — exercises the commands in sequence ─────────────────────

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = build_demo_vault()?;
    let state = VaultState {
        vault: Vault::with_root(vault_dir.path().to_path_buf()),
        plans: Mutex::new(HashMap::new()),
    };

    println!("--- list_papers({{ unrated_only: true }}) ---");
    let unrated = list_papers(
        &state,
        PaperFilter {
            unrated_only: true,
            ..Default::default()
        },
    )
    .await?;
    for p in &unrated {
        println!("  {} ({}) tags={:?}", p.title, p.year, p.tags);
    }

    println!("\n--- plan_skim_old_papers(2020) → preview ---");
    let preview = plan_skim_old_papers(&state, 2020).await?;
    println!(
        "  plan_id={} changes={} errors={}",
        preview.plan_id, preview.changes, preview.errors
    );
    for p in &preview.sample_paths {
        println!("    - {p}");
    }

    println!("\n--- execute_plan(<plan_id>) ---");
    let report = execute_plan(&state, preview.plan_id.clone()).await?;
    println!("  wrote {} files", report.changes.len());

    println!("\n--- list_papers({{}}) after execute ---");
    for p in list_papers(&state, PaperFilter::default()).await? {
        println!("  {} ({}) rating={:?}", p.title, p.year, p.rating);
    }

    println!("\n--- execute_plan with expired id rejects ---");
    match execute_plan(&state, "plan-nonexistent".into()).await {
        Ok(_) => panic!("expired plan must error"),
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
            "Attention",
            "---\ntags: [type/paper, topic/attention]\nyear: 2017\nrating: deep\n---\nbody\n",
        ),
        (
            "BERT",
            "---\ntags: [type/paper, topic/nlp]\nyear: 2018\n---\nbody\n",
        ),
        (
            "GPT4",
            "---\ntags: [type/paper, topic/llm]\nyear: 2023\nrating: read\n---\nbody\n",
        ),
        (
            "Llama3",
            "---\ntags: [type/paper, topic/llm]\nyear: 2024\n---\nbody\n",
        ),
    ];
    for (name, content) in entries {
        fs::write(folder.join(format!("{name}.md")), content)?;
    }
    Ok(dir)
}
