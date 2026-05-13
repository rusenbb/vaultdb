//! Integration tests for `#[derive(Note)]`. These belong outside `src/`
//! because the derive macro is a separate crate and we want to exercise
//! the full compile pipeline.

use serde::{Deserialize, Serialize};
use vaultdb_orm::{Expr, Note, Query, Vault};

#[derive(Debug, Serialize, Deserialize, Note)]
#[note(folder = "notes")]
struct Plain {
    #[serde(rename = "_name")]
    name: String,
}

#[derive(Debug, Serialize, Deserialize, Note)]
#[note(folder = "papers", filter = "tags contains type/paper")]
struct Filtered {
    #[serde(rename = "_name")]
    title: String,
    year: i32,
}

// Phase 6 — discriminator-only model (folder defaults to "").
#[derive(Debug, Serialize, Deserialize, Note)]
#[note(discriminator = "tags contains eduport-type/person")]
struct Person {
    #[serde(rename = "_name")]
    name: String,
    role: String,
    #[allow(dead_code)]
    university: String,
}

// Phase 6 — collection-named model.
#[derive(Debug, Serialize, Deserialize, Note)]
#[note(folder = "Notes/movie", collection = "movies")]
struct Movie {
    #[serde(rename = "_name")]
    title: String,
    director: String,
    year: i32,
}

#[test]
fn derive_emits_folder_const() {
    assert_eq!(<Plain as Note>::FOLDER, "notes");
    assert_eq!(<Filtered as Note>::FOLDER, "papers");
}

#[test]
fn derive_without_filter_has_no_discriminator() {
    assert!(<Plain as Note>::discriminator().is_none());
}

#[test]
fn derive_with_filter_parses_at_runtime() {
    let disc = <Filtered as Note>::discriminator().expect("expected discriminator");
    let manual = Expr::parse("tags contains type/paper").unwrap();
    assert_eq!(disc, manual);
}

#[test]
fn derive_with_discriminator_only_defaults_folder_to_empty() {
    assert_eq!(<Person as Note>::FOLDER, "");
    let disc = <Person as Note>::discriminator().expect("expected discriminator");
    let manual = Expr::parse("tags contains eduport-type/person").unwrap();
    assert_eq!(disc, manual);
}

#[test]
fn collection_attribute_defaults_to_none() {
    assert!(<Plain as Note>::collection().is_none());
    assert!(<Filtered as Note>::collection().is_none());
    assert!(<Person as Note>::collection().is_none());
}

#[test]
fn collection_attribute_sets_collection_name() {
    assert_eq!(<Movie as Note>::collection(), Some("movies"));
}

#[test]
fn field_names_lists_struct_fields_minus_virtuals() {
    // _name maps to a virtual field — excluded.
    let names = <Movie as Note>::field_names();
    assert!(names.contains(&"director"));
    assert!(names.contains(&"year"));
    assert!(!names.iter().any(|n| n.starts_with('_')));
}

#[test]
fn query_can_be_constructed_from_derived_type() {
    // We don't actually run it against a vault here — just make sure the
    // typed query constructor accepts the derived type.
    use std::path::PathBuf;
    let vault = Vault::with_root(PathBuf::from("/nonexistent"));
    let _q = Query::<Filtered>::new(&vault);
}

#[test]
fn field_accessor_returns_fieldref_with_struct_field_key() {
    // No #[serde(rename)] → frontmatter key matches struct field name.
    assert_eq!(Filtered::year().name(), "year");
}

#[test]
fn field_accessor_honours_serde_rename() {
    // #[serde(rename = "_name")] on `title` → FieldRef carries "_name".
    assert_eq!(Filtered::title().name(), "_name");
    assert_eq!(Plain::name().name(), "_name");
}

#[test]
fn field_accessor_builds_typed_expr_via_operators() {
    use vaultdb_orm::{Expr, Predicate, Value};
    let filter = Filtered::year().ge(2024) & Filtered::title().contains("BERT");
    match filter {
        Expr::And(parts) => {
            assert_eq!(parts.len(), 2);
            assert!(matches!(
                &parts[1],
                Expr::Predicate(Predicate::Contains { field, value })
                    if field == "_name" && *value == Value::String("BERT".into())
            ));
        }
        other => panic!("expected And, got {:?}", other),
    }
}
