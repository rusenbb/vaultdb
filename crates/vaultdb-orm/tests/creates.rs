//! Integration tests for typed creates via `Create::<T>`.

use std::fs;

use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use vaultdb_core::schema::{CollectionSchema, FieldSchema};
use vaultdb_orm::{Create, Note, Value, Vault};

#[derive(Debug, Clone, Serialize, Deserialize, Note)]
#[note(folder = "Notes/movie", filter = "tags contains type/movie")]
#[allow(dead_code)]
struct Movie {
    #[serde(rename = "_name")]
    title: String,
    director: String,
    year: i32,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

fn vault() -> (TempDir, Vault) {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join(".obsidian")).unwrap();
    let vault = Vault::with_root(dir.path().to_path_buf());
    (dir, vault)
}

fn movie_schema() -> CollectionSchema {
    let mut fields = std::collections::BTreeMap::new();
    fields.insert(
        "db-table".into(),
        FieldSchema {
            field_type: "string".into(),
            enum_values: vec![Value::String("movie".into())],
            min: None,
            max: None,
            default: Some(Value::String("movie".into())),
            default_expr: None,
        },
    );
    fields.insert(
        "status".into(),
        FieldSchema {
            field_type: "string".into(),
            enum_values: vec![],
            min: None,
            max: None,
            default: Some(Value::String("to-watch".into())),
            default_expr: None,
        },
    );
    fields.insert(
        "director".into(),
        FieldSchema {
            field_type: "string".into(),
            enum_values: vec![],
            min: None,
            max: None,
            default: None,
            default_expr: None,
        },
    );
    fields.insert(
        "year".into(),
        FieldSchema {
            field_type: "integer".into(),
            enum_values: vec![],
            min: None,
            max: None,
            default: None,
            default_expr: None,
        },
    );
    CollectionSchema {
        description: None,
        folder: "Notes/movie".into(),
        filter: vec![],
        required: vec!["director".into(), "year".into()],
        fields,
    }
}

#[test]
fn typed_create_writes_under_t_folder() {
    let (dir, vault) = vault();
    Create::<Movie>::new(&vault, "Dune")
        .set(Movie::director(), "Denis Villeneuve")
        .set(Movie::year(), 2021_i64)
        .execute()
        .unwrap();
    let written = dir.path().join("Notes/movie/Dune.md");
    assert!(written.is_file(), "{:?}", written);
    let content = fs::read_to_string(&written).unwrap();
    assert!(content.contains("director: Denis Villeneuve"));
    assert!(content.contains("year: 2021"));
}

#[test]
fn typed_create_applies_schema_defaults() {
    let (dir, vault) = vault();
    let report = Create::<Movie>::new(&vault, "Dune")
        .with_schema(movie_schema())
        .set(Movie::director(), "Denis Villeneuve")
        .set(Movie::year(), 2021_i64)
        .execute()
        .unwrap();
    assert_eq!(report.errors.len(), 0);
    let content = fs::read_to_string(dir.path().join("Notes/movie/Dune.md")).unwrap();
    assert!(content.contains("db-table: movie"));
    assert!(content.contains("status: to-watch"));
    assert!(content.contains("director: Denis Villeneuve"));
}

#[test]
fn typed_create_rejects_missing_required() {
    let (dir, vault) = vault();
    let report = Create::<Movie>::new(&vault, "Bare")
        .with_schema(movie_schema())
        .execute()
        .unwrap();
    assert!(!report.errors.is_empty());
    // File must NOT exist.
    assert!(!dir.path().join("Notes/movie/Bare.md").exists());
}

#[test]
fn typed_create_set_raw_for_unmodeled_field() {
    let (dir, vault) = vault();
    Create::<Movie>::new(&vault, "Dune")
        .set(Movie::director(), "DV")
        .set(Movie::year(), 2021_i64)
        .set_raw("imdb_id", "tt1160419")
        .execute()
        .unwrap();
    let content = fs::read_to_string(dir.path().join("Notes/movie/Dune.md")).unwrap();
    assert!(content.contains("imdb_id: tt1160419"));
}

#[test]
fn typed_create_plan_returns_content_without_writing() {
    let (dir, vault) = vault();
    let (report, content) = Create::<Movie>::new(&vault, "Dune")
        .with_schema(movie_schema())
        .set(Movie::director(), "DV")
        .set(Movie::year(), 2021_i64)
        .plan_with_content()
        .unwrap();
    assert_eq!(report.errors.len(), 0);
    assert!(!dir.path().join("Notes/movie/Dune.md").exists());
    assert!(content.unwrap().contains("status: to-watch"));
}

// Phase 6: when `#[note(collection = "...")]` is set AND the vault has
// a vaultdb-schema.yaml declaring that collection, Create<T> attaches
// it automatically. No explicit .with_schema() call required.
#[derive(Debug, Clone, Serialize, Deserialize, Note)]
#[note(folder = "Notes/movie", collection = "movies")]
#[allow(dead_code)]
struct AutoMovie {
    #[serde(rename = "_name")]
    title: String,
    director: String,
    year: i32,
}

#[test]
fn create_auto_resolves_schema_when_collection_is_set() {
    let (dir, vault) = vault();
    // Write a schema YAML that defines defaults for the movies collection.
    fs::write(
        dir.path().join("vaultdb-schema.yaml"),
        r#"
collections:
  movies:
    folder: Notes/movie
    required: [director, year]
    fields:
      db-table:
        type: string
        enum: [movie]
        default: movie
      status:
        type: string
        default: to-watch
      director: { type: string }
      year: { type: integer }
"#,
    )
    .unwrap();

    Create::<AutoMovie>::new(&vault, "Dune")
        .set(AutoMovie::director(), "DV")
        .set(AutoMovie::year(), 2021_i64)
        .execute()
        .unwrap();
    let content = fs::read_to_string(dir.path().join("Notes/movie/Dune.md")).unwrap();
    // The defaults declared in YAML applied — no explicit .with_schema() needed.
    assert!(content.contains("db-table: movie"));
    assert!(content.contains("status: to-watch"));
}
