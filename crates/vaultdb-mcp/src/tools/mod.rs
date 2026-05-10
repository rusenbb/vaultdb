//! Free-function tool implementations, called from the `#[tool]` methods
//! in `crate::server`. Splitting the implementations out of the macro'd
//! impl block keeps the tool router thin and makes each tool unit-testable
//! without an MCP runtime.

pub mod links;
pub mod mutations;
pub mod query;
pub mod schema;
