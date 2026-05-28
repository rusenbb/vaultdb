//! Integration tests for typed mutations via `Query::update()`.

use std::fs;

use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use vaultdb_orm::{Note, Query, Vault};

#[derive(Debug, Clone, Serialize, Deserialize, Note)]
#[note(folder = "notes", filter = "tags contains type/paper")]
struct Paper {
    #[serde(rename = "_name")]
    title: String,
    year: i32,
    tags: Vec<String>,
    #[serde(default)]
    rating: Option<String>,
}

fn fixture() -> (TempDir, Vault) {
    let dir = TempDir::new().unwrap();
    let folder = dir.path().join("notes");
    fs::create_dir_all(&folder).unwrap();
    fs::write(
        folder.join("Alpha.md"),
        "---\ntags: [type/paper]\nyear: 2018\n---\nbody\n",
    )
    .unwrap();
    fs::write(
        folder.join("Beta.md"),
        "---\ntags: [type/paper]\nyear: 2024\nrating: read\n---\nbody\n",
    )
    .unwrap();
    fs::write(
        folder.join("Gamma.md"),
        "---\ntags: [type/concept]\nyear: 2024\n---\nbody\n",
    )
    .unwrap();
    let vault = Vault::with_root(dir.path().to_path_buf());
    (dir, vault)
}

#[test]
fn update_requires_user_filter() {
    let (_dir, vault) = fixture();
    let res = Query::<Paper>::new(&vault).update();
    assert!(
        res.is_err(),
        "must reject .update() that relies on discriminator alone"
    );
}

#[test]
fn plan_does_not_modify_disk() {
    let (_dir, vault) = fixture();
    let before = std::fs::read_to_string(_dir.path().join("notes/Alpha.md")).unwrap();
    let plan = Query::<Paper>::new(&vault)
        .filter(Paper::year().lt(2020))
        .update()
        .unwrap()
        .set(Paper::rating(), "skim")
        .plan()
        .unwrap();
    assert_eq!(plan.changes.len(), 1);
    let after = std::fs::read_to_string(_dir.path().join("notes/Alpha.md")).unwrap();
    assert_eq!(before, after, "plan() must not write");
}

#[test]
fn execute_writes_and_skips_records_outside_filter() {
    let (_dir, vault) = fixture();

    let report = Query::<Paper>::new(&vault)
        .filter(Paper::year().lt(2020))
        .update()
        .unwrap()
        .set(Paper::rating(), "skim")
        .execute()
        .unwrap();
    assert_eq!(report.changes.len(), 1);
    assert!(report.errors.is_empty());

    let alpha: Paper = Query::<Paper>::new(&vault)
        .filter(Paper::title().eq("Alpha"))
        .first()
        .unwrap()
        .expect("Alpha must still exist");
    assert_eq!(alpha.rating.as_deref(), Some("skim"));

    let beta: Paper = Query::<Paper>::new(&vault)
        .filter(Paper::title().eq("Beta"))
        .first()
        .unwrap()
        .unwrap();
    assert_eq!(
        beta.rating.as_deref(),
        Some("read"),
        "Beta must be untouched"
    );
}

#[test]
fn body_set_overwrites_via_typed_update() {
    // Smoke test the body surface on the typed Update wrapper —
    // confirms the v1.6.0+ body API is reachable through orm::Update<T>
    // and that the filter still scopes the write (Beta untouched).
    let (dir, vault) = fixture();

    let report = Query::<Paper>::new(&vault)
        .filter(Paper::title().eq("Alpha"))
        .update()
        .unwrap()
        .set_body("Replaced body.\n")
        .execute()
        .unwrap();
    assert_eq!(report.changes.len(), 1);
    assert!(report.errors.is_empty());

    let alpha = fs::read_to_string(dir.path().join("notes/Alpha.md")).unwrap();
    assert!(alpha.ends_with("---\nReplaced body.\n"), "got:\n{}", alpha);
    assert!(!alpha.contains("body\n"), "old body must be gone");

    let beta = fs::read_to_string(dir.path().join("notes/Beta.md")).unwrap();
    assert!(
        beta.ends_with("---\nbody\n"),
        "Beta body untouched: {}",
        beta
    );
}

#[test]
fn body_append_with_blank_line_separator_via_typed_update() {
    // body_separator + append_body together — exercises both new
    // methods through the typed wrapper, and confirms the separator
    // override actually reaches the core builder.
    let (dir, vault) = fixture();

    Query::<Paper>::new(&vault)
        .filter(Paper::title().eq("Alpha"))
        .update()
        .unwrap()
        .body_separator("\n\n")
        .append_body("Notes section.")
        .execute()
        .unwrap();

    let alpha = fs::read_to_string(dir.path().join("notes/Alpha.md")).unwrap();
    assert!(alpha.ends_with("body\n\nNotes section."), "got:\n{}", alpha);
}

#[test]
fn body_clear_via_typed_update_keeps_frontmatter() {
    let (dir, vault) = fixture();

    Query::<Paper>::new(&vault)
        .filter(Paper::title().eq("Alpha"))
        .update()
        .unwrap()
        .clear_body()
        .execute()
        .unwrap();

    let alpha = fs::read_to_string(dir.path().join("notes/Alpha.md")).unwrap();
    assert!(alpha.contains("year: 2018"), "frontmatter preserved");
    assert!(!alpha.contains("body"), "body cleared: {}", alpha);
    assert!(alpha.ends_with("---\n"));
}

#[test]
fn discriminator_protects_other_record_kinds() {
    // Gamma is type/concept, not type/paper. A filter `year = 2024 &
    // discriminator(type/paper)` must not touch it.
    let (dir, vault) = fixture();
    let before = std::fs::read_to_string(dir.path().join("notes/Gamma.md")).unwrap();

    let _ = Query::<Paper>::new(&vault)
        .filter(Paper::year().eq(2024))
        .update()
        .unwrap()
        .set(Paper::rating(), "skim")
        .execute()
        .unwrap();

    let after = std::fs::read_to_string(dir.path().join("notes/Gamma.md")).unwrap();
    assert_eq!(before, after, "Gamma is type/concept, must be untouched");
}
