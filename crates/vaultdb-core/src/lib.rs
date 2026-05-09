//! vaultdb-core — library engine for vaultdb.

pub mod error;
pub mod filter;
pub mod frontmatter;
pub mod links;
pub mod query;
pub mod record;
pub mod schema;
pub mod vault;
pub mod writer;

pub use record::{Record, Value};
pub use error::ParseError;
pub use vault::{LoadResult, Vault};
pub use query::{Expr, Predicate, LinkPredicate, CompareOp, Query, SortKey};
