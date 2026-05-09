//! vaultdb-core — library engine for vaultdb.

pub mod error;
pub mod filter;
pub mod frontmatter;
pub mod links;
pub mod mutation;
pub mod query;
pub mod record;
pub mod schema;
pub mod vault;
pub mod writer;

pub use record::{Record, Value};
pub use error::ParseError;
pub use vault::{LoadResult, Vault};
pub use query::{Expr, Predicate, LinkPredicate, CompareOp, Query, SortKey};
pub use links::{Direction, GraphScope, LinkGraph, UnresolvedLink};
pub use mutation::{MutationError, MutationReport, PlannedChange, UpdateBuilder};
