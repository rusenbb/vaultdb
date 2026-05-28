//! JSON-Schema-deriving param structs for every MCP tool.
//!
//! Each `*Params` struct here corresponds to one tool's input shape.
//! Keeping them in their own module keeps the schema-shaping concerns
//! (serde rename rules, defaults, doc strings used by MCP clients to
//! describe each field) separate from the tool implementations.

use schemars::JsonSchema;
use serde::Deserialize;

/// Common export side-effect for read tools. When `export` is set, the
/// tool serializes its result to a file under the vault root (in
/// addition to returning it in-band). Inferred format from the
/// extension matches the CLI: `.csv`, `.tsv`, `.json`, `.yaml`/`.yml`,
/// `.xlsx`.
///
/// Embedded into every read tool's param struct via `#[serde(flatten)]`
/// so the wire schema looks flat to MCP clients.
#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
pub struct ExportOptions {
    /// Optional vault-relative path to also save the result to. Absolute
    /// paths and `..` escapes are rejected; `.md` is reserved for vault
    /// notes. On filename collision the file is auto-suffixed `(1)`,
    /// `(2)`, ... — there is no overwrite mode.
    #[serde(default)]
    pub export: Option<String>,
    /// CSV / TSV delimiter when exporting to `.csv` or `.tsv`. One of
    /// `comma` (default), `semicolon`, `tab`. Ignored for other formats.
    #[serde(default)]
    pub csv_delimiter: Option<String>,
}

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
    #[serde(flatten)]
    pub export_opts: ExportOptions,
}

/// Parameters for the `find_by_name` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct FindByNameParams {
    /// Folder containing the note.
    pub folder: String,
    /// Filename without the `.md` extension.
    pub name: String,
    #[serde(flatten)]
    pub export_opts: ExportOptions,
}

/// Parameters for the `list_folders` tool. Empty for now — the server
/// always lists folders rooted at the vault root.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListFoldersParams {
    #[serde(flatten)]
    pub export_opts: ExportOptions,
}

// ── Link / graph tools ─────────────────────────────────────────────────────

/// Parameters for the `links` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct LinksParams {
    /// Note name (filename without `.md`).
    pub name: String,
    /// Direction: `outgoing`, `incoming`, or `both`. Default `both`.
    #[serde(default = "default_both")]
    pub direction: String,
    #[serde(flatten)]
    pub export_opts: ExportOptions,
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
    #[serde(flatten)]
    pub export_opts: ExportOptions,
}

fn default_traverse_depth() -> usize {
    2
}

fn default_outgoing() -> String {
    "outgoing".into()
}

/// Parameters for the `unresolved` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct UnresolvedParams {
    #[serde(flatten)]
    pub export_opts: ExportOptions,
}

// ── Schema tools ───────────────────────────────────────────────────────────

/// Parameters for the `schema_show` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SchemaShowParams {
    /// Folder name. If specified, only the matching collection(s) are
    /// returned; otherwise the full schema is returned.
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(flatten)]
    pub export_opts: ExportOptions,
}

/// Parameters for the `schema_infer` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SchemaInferParams {
    /// Folder to infer the schema from.
    pub folder: String,
    /// Walk subfolders recursively (default false).
    #[serde(default)]
    pub recursive: bool,
    /// Save the inferred collection to `<vault>/vaultdb-schema.yaml`,
    /// merging with any existing collections (replacing one at the same
    /// folder key). Default false — by default the tool returns YAML
    /// for review only.
    #[serde(default)]
    pub write: bool,
    #[serde(flatten)]
    pub export_opts: ExportOptions,
}

// ── Plan-only mutation tools ───────────────────────────────────────────────

/// Parameters for the `plan_update` tool.
///
/// Two ways to specify field updates:
/// - `set` (legacy) — `"field=value"` strings; value parsed via
///   `Value::parse_scalar` (i64 → f64 → String fallback). Kept for
///   backward compatibility with older MCP clients.
/// - `set_typed` (preferred) — typed JSON object matching
///   `plan_create`'s shape. Strings stay strings, numbers stay typed,
///   lists/maps recurse. New clients should use this; mixing both
///   forms is allowed (`set_typed` wins on key collision).
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PlanUpdateParams {
    pub folder: String,
    /// where-DSL string selecting which records to update.
    pub r#where: String,
    /// Legacy `field=value` strings.
    #[serde(default)]
    pub set: Vec<String>,
    /// Preferred typed-map alternative to `set`. Same shape as
    /// `plan_create`'s `set` field.
    #[serde(default)]
    pub set_typed: std::collections::BTreeMap<String, serde_json::Value>,
    /// Field names to remove.
    #[serde(default)]
    pub unset: Vec<String>,
    /// Tags to add to the `tags` list.
    #[serde(default)]
    pub add_tag: Vec<String>,
    /// Tags to remove from the `tags` list.
    #[serde(default)]
    pub remove_tag: Vec<String>,
    /// Replace the body (everything after the closing `---` of the
    /// frontmatter) with this text. Written verbatim.
    #[serde(default)]
    pub set_body: Option<String>,
    /// Append each entry to the body, joined by `body_separator`
    /// (default `"\n"`). Multiple entries accumulate.
    #[serde(default)]
    pub append_body: Vec<String>,
    /// Clear the body entirely. Applied before `set_body` / `append_body`.
    #[serde(default)]
    pub clear_body: bool,
    /// Separator inserted between the existing body and each appended
    /// chunk. Default `"\n"`. Pass `"\n\n"` for a blank-line section
    /// break. The value is taken verbatim — no escape interpretation.
    #[serde(default)]
    pub body_separator: Option<String>,
    /// Recurse into subfolders when selecting records to update (default
    /// false — only files directly in `folder` are considered).
    #[serde(default)]
    pub recursive: bool,
}

/// Parameters for the `plan_delete` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PlanDeleteParams {
    pub folder: String,
    pub r#where: String,
    /// Permanently delete (default false — moves to `.trash/`).
    #[serde(default)]
    pub permanent: bool,
    /// Recurse into subfolders when selecting records (default false).
    #[serde(default)]
    pub recursive: bool,
}

/// Parameters for the `plan_move` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PlanMoveParams {
    pub folder: String,
    pub to: String,
    pub r#where: String,
    /// Recurse into subfolders when selecting records (default false).
    #[serde(default)]
    pub recursive: bool,
}

/// Parameters for the `plan_rename` tool.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PlanRenameParams {
    pub folder: String,
    pub from: String,
    pub to: String,
}

/// Parameters for the `plan_create` tool.
///
/// Unlike `plan_update`'s string-based `set: Vec<String>` (a legacy of
/// the CLI's flat `--set field=value` interface), `plan_create` accepts a
/// typed JSON object for `set` — values flow through as `Value::String`,
/// `Value::Integer`, etc., which is what an MCP client would propose
/// naturally and matches the typed Rust API in `CreateBuilder`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PlanCreateParams {
    /// Target folder, relative to the vault root.
    pub folder: String,
    /// Note name (becomes `<name>.md` under `folder`).
    pub name: String,
    /// Optional template file path, relative to the vault root.
    #[serde(default)]
    pub template: Option<String>,
    /// Frontmatter overrides keyed by field name. JSON values map to
    /// vaultdb's `Value` directly — strings stay strings, numbers
    /// become integer/float, booleans stay booleans, arrays become
    /// lists, objects become maps.
    #[serde(default)]
    pub set: std::collections::BTreeMap<String, serde_json::Value>,
    /// Optional body content for the new note. Overrides the template's
    /// body (frontmatter is still merged) and the default `# {name}`
    /// placeholder. Written verbatim.
    #[serde(default)]
    pub body: Option<String>,
}
