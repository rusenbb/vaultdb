//! # vaultdb-orm
//!
//! A typed model layer over `vaultdb-core`. Declare a Rust struct that
//! represents a kind of note in your vault, and query it like a database
//! row.
//!
//! ```no_run
//! use serde::{Deserialize, Serialize};
//! use vaultdb_orm::{Note, Query, Vault};
//!
//! #[derive(Serialize, Deserialize, Debug, Note)]
//! #[note(folder = "3-Notes", filter = "tags contains type/paper")]
//! struct Paper {
//!     #[serde(rename = "_name")]
//!     title: String,
//!     year: i32,
//!     tags: Vec<String>,
//! }
//!
//! let vault = Vault::discover(std::path::Path::new(".")).unwrap();
//! let papers: Vec<Paper> = Query::<Paper>::new(&vault).fetch().unwrap();
//! ```
//!
//! `#[derive(Note)]` generates both the `Note` trait impl and one typed
//! `FieldRef` accessor per struct field (`Paper::year()`,
//! `Paper::tags()`), so filter construction is compile-checked. See the
//! `vaultdb-orm-macros` crate for the supported `#[note(...)]` keys.

pub mod create;
pub mod error;
pub mod field;
pub mod note;
pub mod query;
pub mod relation;
pub mod update;
pub mod value;

pub use create::Create;
pub use error::{OrmError, Result};
pub use field::FieldRef;
pub use note::{Note, record_to_json};
pub use query::Query;
pub use relation::{RelationDir, RelationRef};
pub use update::Update;
pub use value::{json_to_value, value_to_json};

/// `#[derive(Note)]` — generates the [`Note`] trait impl from struct
/// attributes. See the crate docs for the supported `#[note(...)]`
/// keys.
pub use vaultdb_orm_macros::Note;

// Re-export the most useful core types so consumers can write
// `use vaultdb_orm::*;` without also importing vaultdb-core for the
// common case.
pub use vaultdb_core::{
    CompareOp, Expr, LinkPredicate, MutationReport, Predicate, SortKey, Value, Vault,
};
