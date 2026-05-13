//! # vaultdb-core
//!
//! A markdown-as-database engine. Treats folders of `.md` files with YAML
//! frontmatter as queryable structured data, with `[[wikilinks]]` forming a
//! first-class link graph. Both the tabular (frontmatter) and graph (link)
//! shapes are equally first-class — you can filter records by frontmatter,
//! by graph predicates ("links to anything tagged X"), or by any combination.
//!
//! ## Quick start
//!
//! ```no_run
//! use vaultdb_core::{Expr, Predicate, Query, Value, Vault};
//!
//! let vault = Vault::discover(std::path::Path::new(".")).unwrap();
//! let q = Query {
//!     folder: "notes".into(),
//!     filter: Some(Expr::Predicate(Predicate::Equals {
//!         field: "status".into(),
//!         value: Value::String("active".into()),
//!     })),
//!     select: None,
//!     sort: None,
//!     limit: Some(10),
//!     recursive: false,
//! };
//! let records = vault.query(&q).unwrap();
//! ```
//!
//! ## Public API surface
//!
//! - [`Vault`]: entry point. Discover, load, query, build the link graph.
//! - [`Record`] and [`Value`]: the row + cell types.
//! - [`Expr`], [`Predicate`], [`LinkPredicate`], [`Query`]: the query AST.
//! - [`LinkGraph`], [`GraphScope`], [`Direction`], [`UnresolvedLink`]: the link
//!   graph and its traversal/scoping types.
//! - [`UpdateBuilder`], [`DeleteBuilder`], [`MoveBuilder`], [`RenameBuilder`]:
//!   typed mutation API. Each has [`UpdateBuilder::plan`]-style preview and
//!   `execute` commit methods.
//! - [`LoadResult`], [`ParseError`]: parse-diagnostic surfacing for
//!   [`Vault::load_records`].
//! - [`VaultdbError`], [`Result`]: error types.
//!
//! ## Design philosophy
//!
//! No daemon, no cache, no state files. Every read traverses the filesystem
//! fresh. Stateful concerns (file watchers, full-text indexes, typed schemas)
//! belong in consumers, not in vaultdb-core.

pub mod dsl;
pub mod error;
pub mod filter;
pub mod frontmatter;
pub mod journal;
pub mod links;
pub mod lock;
pub mod mutation;
pub mod query;
pub mod record;
pub mod render;
pub mod schema;
pub mod vault;
pub mod writer;

pub use error::{ParseError, Result, VaultdbError};
pub use links::{Direction, GraphScope, LinkGraph, UnresolvedLink};
pub use mutation::{
    CreateBuilder, DeleteBuilder, MoveBuilder, MutationError, MutationReport, PlannedChange,
    RenameBuilder, UpdateBuilder,
};
pub use query::{CompareOp, Expr, LinkPredicate, Predicate, Query, SortKey};
pub use record::{Record, Value};
pub use render::{Format as ExportFormat, export_records, export_value, resolve_export_path};
pub use vault::{LoadResult, QueryIter, Vault};
pub use writer::WriteOptions;
