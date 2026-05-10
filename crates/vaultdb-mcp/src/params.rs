//! JSON-Schema-deriving param structs for every MCP tool.
//!
//! Each `*Params` struct here corresponds to one tool's input shape.
//! Keeping them in their own module keeps the schema-shaping concerns
//! (serde rename rules, defaults, doc strings used by MCP clients to
//! describe each field) separate from the tool implementations.

use schemars::JsonSchema;
use serde::Deserialize;

// ── Read tools ─────────────────────────────────────────────────────────────

/// Parameters for the `query` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct QueryParams {
    /// Folder to query, relative to the vault root (e.g. "3-Notes").
    pub folder: String,
    /// Optional where-DSL filter, e.g. "status = active" or
    /// "tags contains topic/ai". Multiple expressions can be OR'd with `||`.
    #[serde(default)]
    pub r#where: Option<String>,
    /// Comma-separated field names to project. `None` = all fields.
    #[serde(default)]
    pub select: Option<String>,
    /// Field name to sort by (frontmatter or virtual field like `_name`).
    #[serde(default)]
    pub sort: Option<String>,
    /// Sort descending (default false).
    #[serde(default)]
    pub desc: bool,
    /// Maximum number of records to return.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Walk subfolders recursively (default false).
    #[serde(default)]
    pub recursive: bool,
}

/// Parameters for the `find_by_name` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct FindByNameParams {
    /// Folder containing the note.
    pub folder: String,
    /// Filename without the `.md` extension.
    pub name: String,
}

/// Parameters for the `list_folders` tool. Empty for now — the server
/// always lists folders rooted at the vault root.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListFoldersParams {}

// ── Link / graph tools ─────────────────────────────────────────────────────

/// Parameters for the `links` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct LinksParams {
    /// Note name (filename without `.md`).
    pub name: String,
    /// Direction: `outgoing`, `incoming`, or `both`. Default `both`.
    #[serde(default = "default_both")]
    pub direction: String,
}

fn default_both() -> String {
    "both".into()
}

/// Parameters for the `traverse` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct TraverseParams {
    /// Starting note name.
    pub name: String,
    /// Maximum BFS depth (default 2).
    #[serde(default = "default_traverse_depth")]
    pub depth: usize,
    /// Direction: `outgoing`, `incoming`, or `both`. Default `outgoing`.
    #[serde(default = "default_outgoing")]
    pub direction: String,
}

fn default_traverse_depth() -> usize {
    2
}

fn default_outgoing() -> String {
    "outgoing".into()
}

/// Parameters for the `unresolved` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct UnresolvedParams {}

// ── Schema tools ───────────────────────────────────────────────────────────

/// Parameters for the `schema_show` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SchemaShowParams {
    /// Folder name. If specified, only the matching collection(s) are
    /// returned; otherwise the full schema is returned.
    #[serde(default)]
    pub folder: Option<String>,
}

/// Parameters for the `schema_infer` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SchemaInferParams {
    /// Folder to infer the schema from.
    pub folder: String,
    /// Walk subfolders recursively (default false).
    #[serde(default)]
    pub recursive: bool,
}

// ── Plan-only mutation tools ───────────────────────────────────────────────

/// Parameters for the `plan_update` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PlanUpdateParams {
    pub folder: String,
    /// where-DSL string selecting which records to update.
    pub r#where: String,
    /// `field=value` strings; the value is parsed as integer / float / string.
    #[serde(default)]
    pub set: Vec<String>,
    /// Field names to remove.
    #[serde(default)]
    pub unset: Vec<String>,
    /// Tags to add to the `tags` list.
    #[serde(default)]
    pub add_tag: Vec<String>,
    /// Tags to remove from the `tags` list.
    #[serde(default)]
    pub remove_tag: Vec<String>,
}

/// Parameters for the `plan_delete` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PlanDeleteParams {
    pub folder: String,
    pub r#where: String,
    /// Permanently delete (default false — moves to `.trash/`).
    #[serde(default)]
    pub permanent: bool,
}

/// Parameters for the `plan_move` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PlanMoveParams {
    pub folder: String,
    pub to: String,
    pub r#where: String,
}

/// Parameters for the `plan_rename` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PlanRenameParams {
    pub folder: String,
    pub from: String,
    pub to: String,
}
