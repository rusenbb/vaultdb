//! Integration tests for `#[note(wikilink)]` / `#[note(backlink)]`.

use std::fs;

use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use vaultdb_orm::{Expr, LinkPredicate, Note, Query, RelationDir, Vault};

#[derive(Debug, Clone, Serialize, Deserialize, Note)]
#[note(folder = "notes", filter = "tags contains type/paper")]
#[allow(dead_code)]
struct Paper {
    #[serde(rename = "_name")]
    title: String,
    tags: Vec<String>,
    #[serde(default, skip)]
    #[note(wikilink)]
    cites: Vec<String>,
    #[serde(default, skip)]
    #[note(backlink)]
    cited_by: Vec<String>,
}

fn fixture() -> (TempDir, Vault) {
    let dir = TempDir::new().unwrap();
    let folder = dir.path().join("notes");
    fs::create_dir_all(&folder).unwrap();
    fs::write(
        folder.join("Root.md"),
        "---\ntags: [type/paper, topic/foundational]\n---\nFoundation.\n",
    )
    .unwrap();
    fs::write(
        folder.join("Builder.md"),
        "---\ntags: [type/paper, topic/applied]\n---\nBuilds on [[Root]].\n",
    )
    .unwrap();
    fs::write(
        folder.join("Sibling.md"),
        "---\ntags: [type/paper, topic/applied]\n---\nAlso uses [[Root]].\n",
    )
    .unwrap();
    let vault = Vault::with_root(dir.path().to_path_buf());
    (dir, vault)
}

#[test]
fn accessor_direction_matches_attribute() {
    assert_eq!(Paper::cites().direction(), RelationDir::Outgoing);
    assert_eq!(Paper::cited_by().direction(), RelationDir::Incoming);
}

#[test]
fn to_target_returns_links_to_target_expr() {
    let e = Paper::cites().to("Root");
    assert!(matches!(e, Expr::LinksTo(LinkPredicate::Target(t)) if t == "Root"));
}

#[test]
fn any_wraps_inner_predicate() {
    let inner = Paper::tags().contains("topic/foundational");
    let e = Paper::cites().any(inner);
    assert!(matches!(e, Expr::LinksTo(LinkPredicate::Where(_))));
}

#[test]
fn link_query_resolves_against_graph() {
    let (_dir, vault) = fixture();

    let hits: Vec<Paper> = Query::<Paper>::new(&vault)
        .filter(Paper::cites().to("Root"))
        .fetch()
        .unwrap();
    let names: Vec<_> = hits.iter().map(|p| p.title.clone()).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"Builder".to_string()));
    assert!(names.contains(&"Sibling".to_string()));
}

#[test]
fn join_via_any_predicate_filters_through_graph() {
    let (_dir, vault) = fixture();

    let hits: Vec<Paper> = Query::<Paper>::new(&vault)
        .filter(Paper::cites().any(Paper::tags().contains("topic/foundational")))
        .fetch()
        .unwrap();
    let names: Vec<_> = hits.iter().map(|p| p.title.clone()).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"Builder".to_string()));
    assert!(names.contains(&"Sibling".to_string()));
}

#[test]
fn backlink_finds_target_of_links() {
    let (_dir, vault) = fixture();

    let hits: Vec<Paper> = Query::<Paper>::new(&vault)
        .filter(Paper::cited_by().to("Builder"))
        .fetch()
        .unwrap();
    let names: Vec<_> = hits.iter().map(|p| p.title.clone()).collect();
    assert_eq!(names, vec!["Root".to_string()]);
}
