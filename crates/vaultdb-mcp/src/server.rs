//! [`VaultdbServer`] — the MCP server's state and tool router.
//!
//! Tools are wired via the `#[tool_router(server_handler)]` macro at the
//! bottom of this file. Each tool method delegates to a free function in
//! `crate::tools` so the implementations stay testable without an
//! `rmcp` runtime.
//!
//! Every tool returns `Result<String, ErrorData>` — the string is a
//! pretty-printed JSON document. We deliberately don't use rmcp's
//! `Json<T>` wrapper: that would require every output type to derive
//! `schemars::JsonSchema`, which would force schemars into vaultdb-core
//! and break the library scope discipline rule (spec §7). MCP clients
//! see a single text content block whose body is JSON; LLMs handle this
//! fine and the wire shape stays stable.
//!
//! When a read tool's `export` parameter is set, the response shape
//! changes from `<result>` to `{ "data": <result>, "exported_to": "<path>" }`.
//! That wrapper only appears when export was requested; the in-band
//! shape for plain reads is unchanged.

use std::path::Path;

use rmcp::{ErrorData, handler::server::wrapper::Parameters, tool, tool_router};
use serde::Serialize;
use vaultdb_core::record::Record;
use vaultdb_core::render;
use vaultdb_core::vault::Vault;

use crate::params::{
    ExportOptions, FindByNameParams, LinksParams, ListFoldersParams, PlanCreateParams,
    PlanDeleteParams, PlanMoveParams, PlanRenameParams, PlanUpdateParams, QueryParams,
    SchemaInferParams, SchemaShowParams, TraverseParams, UnresolvedParams,
};
use crate::tools;

/// Which `execute_*` tools are armed. Set by command-line flags at
/// server launch. Each tool method checks the corresponding bit before
/// touching disk and returns a typed error otherwise — the tool surface
/// itself is always present so MCP clients see a stable schema, but
/// calling an unarmed tool fails with a clear "not allowed" message.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExecutePermissions {
    pub create: bool,
    pub update: bool,
    pub delete: bool,
    pub permanent_delete: bool,
}

impl ExecutePermissions {
    pub fn any(&self) -> bool {
        self.create || self.update || self.delete || self.permanent_delete
    }
}

/// MCP server state. Holds an optional [`Vault`] (the server boots even
/// without a vault so tool calls can return a typed error rather than
/// the binary crashing on launch).
#[derive(Clone)]
pub struct VaultdbServer {
    vault: std::sync::Arc<Option<Vault>>,
    permissions: ExecutePermissions,
}

impl VaultdbServer {
    pub fn new(vault: Option<Vault>, permissions: ExecutePermissions) -> Self {
        Self {
            vault: std::sync::Arc::new(vault),
            permissions,
        }
    }

    /// Borrow the vault, or return an `ErrorData::invalid_params` describing
    /// the resolution failure. Every tool that needs the vault should go
    /// through this — it produces a uniform error message for clients.
    fn vault(&self) -> Result<&Vault, ErrorData> {
        self.vault.as_ref().as_ref().ok_or_else(|| {
            ErrorData::invalid_params(
                "no vault configured: pass --vault <path>, set VAULTDB_VAULT, or run from inside a directory whose ancestors contain `.obsidian/`",
                None,
            )
        })
    }

    fn require(&self, allowed: bool, flag: &str) -> Result<(), ErrorData> {
        if allowed {
            Ok(())
        } else {
            Err(ErrorData::invalid_params(
                format!(
                    "this tool is disabled. Launch vaultdb-mcp with --{} to enable it.",
                    flag
                ),
                None,
            ))
        }
    }
}

/// Serialize a value to pretty-printed JSON, mapping serde failures to
/// a clean `ErrorData`. Centralises the MCP-boundary conversion.
fn json_string<T: Serialize>(value: T) -> Result<String, ErrorData> {
    serde_json::to_string_pretty(&value)
        .map_err(|e| ErrorData::internal_error(format!("serialize failed: {}", e), None))
}

/// Resolve the CSV delimiter byte from the optional MCP param. Accepts
/// both named values (`comma`/`semicolon`/`tab`) and literal characters
/// (`,`/`;`/`\t`). Defaults to comma.
fn parse_csv_delimiter(s: Option<&str>) -> Result<u8, ErrorData> {
    match s {
        None | Some("comma") | Some(",") => Ok(b','),
        Some("semicolon") | Some(";") => Ok(b';'),
        Some("tab") | Some("\t") | Some("\\t") => Ok(b'\t'),
        Some(other) => Err(ErrorData::invalid_params(
            format!(
                "csv_delimiter must be one of 'comma', 'semicolon', 'tab'; got '{}'",
                other
            ),
            None,
        )),
    }
}

/// Build the export-aware response for a record-shaped tool result. If
/// `export` is None, returns the records as JSON (existing shape).
/// Otherwise also writes the file via `render::export_records` and
/// wraps the response with `{ data, exported_to }`.
fn respond_records(
    vault: &Vault,
    records: &[Record],
    select: Option<&[String]>,
    link_index: Option<&vaultdb_core::links::LinkGraph>,
    export_opts: &ExportOptions,
) -> Result<String, ErrorData> {
    let body = serde_json::to_value(records)
        .map_err(|e| ErrorData::internal_error(format!("serialize records: {}", e), None))?;
    if let Some(path_str) = &export_opts.export {
        let delim = parse_csv_delimiter(export_opts.csv_delimiter.as_deref())?;
        let path = Path::new(path_str);
        let fmt = render::Format::from_path(path)
            .map_err(|e| ErrorData::invalid_params(format!("export format: {}", e), None))?
            .with_csv_delimiter(delim);
        let written =
            render::export_records(&vault.root, path, fmt, records, select, link_index)
                .map_err(|e| ErrorData::invalid_params(format!("export failed: {}", e), None))?;
        json_string(serde_json::json!({
            "data": body,
            "exported_to": written.display().to_string(),
        }))
    } else {
        json_string(body)
    }
}

/// Build the export-aware response for an arbitrary-shaped tool result.
/// JSON / YAML always work; CSV / XLSX only work for tabular shapes
/// (array of objects, array of scalars, single object) and return a
/// typed error otherwise.
fn respond_value<T: Serialize>(
    vault: &Vault,
    value: &T,
    export_opts: &ExportOptions,
) -> Result<String, ErrorData> {
    let body = serde_json::to_value(value)
        .map_err(|e| ErrorData::internal_error(format!("serialize value: {}", e), None))?;
    if let Some(path_str) = &export_opts.export {
        let delim = parse_csv_delimiter(export_opts.csv_delimiter.as_deref())?;
        let path = Path::new(path_str);
        let fmt = render::Format::from_path(path)
            .map_err(|e| ErrorData::invalid_params(format!("export format: {}", e), None))?
            .with_csv_delimiter(delim);
        let written = render::export_value(&vault.root, path, fmt, &body)
            .map_err(|e| ErrorData::invalid_params(format!("export failed: {}", e), None))?;
        json_string(serde_json::json!({
            "data": body,
            "exported_to": written.display().to_string(),
        }))
    } else {
        json_string(body)
    }
}

#[tool_router(server_handler)]
impl VaultdbServer {
    // ── Liveness ───────────────────────────────────────────────────────────

    #[tool(description = "Liveness check. Returns 'pong' if the server is alive.")]
    fn ping(&self) -> String {
        "pong".to_string()
    }

    // ── Read tools ─────────────────────────────────────────────────────────

    #[tool(
        description = "Run a structured query against a folder. Filter with the where-DSL (e.g. \"status = active\", \"tags contains topic/ai\"). Returns matching records as JSON, minus their raw body content. Pass `export: \"path/foo.csv\"` (vault-relative, .csv/.tsv/.json/.yaml/.xlsx) to also write the results to a file under the vault."
    )]
    fn query(&self, params: Parameters<QueryParams>) -> Result<String, ErrorData> {
        let vault = self.vault()?;
        let export_opts = params.0.export_opts.clone();
        let select_fields: Option<Vec<String>> = params
            .0
            .select
            .as_deref()
            .map(|s| s.split(',').map(|f| f.trim().to_string()).collect());
        let records = tools::query::query(vault, params.0)?;
        let select_ref: Option<&[String]> = select_fields.as_deref();
        // The MCP `query` returns records already stripped of raw_content,
        // so no link index is needed for export — virtual fields like
        // `_name` / `_path` work from the record's path alone.
        respond_records(vault, &records, select_ref, None, &export_opts)
    }

    #[tool(
        description = "Look up a single record by filename (without .md). Returns JSON null when the record doesn't exist. Supports `export` like `query`."
    )]
    fn find_by_name(&self, params: Parameters<FindByNameParams>) -> Result<String, ErrorData> {
        let vault = self.vault()?;
        let export_opts = params.0.export_opts.clone();
        let opt = tools::query::find_by_name(vault, params.0)?;
        let records: Vec<Record> = opt.into_iter().collect();
        respond_records(vault, &records, None, None, &export_opts)
    }

    #[tool(
        description = "List the top-level folders in the vault that contain at least one .md file. Useful for discovering what folders are queryable. Supports `export`."
    )]
    fn list_folders(&self, params: Parameters<ListFoldersParams>) -> Result<String, ErrorData> {
        let vault = self.vault()?;
        let export_opts = params.0.export_opts.clone();
        let folders = tools::query::list_folders(vault, params.0)?;
        respond_value(vault, &folders, &export_opts)
    }

    // ── Graph tools ────────────────────────────────────────────────────────

    #[tool(
        description = "Show outgoing and incoming wikilinks for a single note. Direction is one of 'outgoing', 'incoming', 'both'. Supports `export`."
    )]
    fn links(&self, params: Parameters<LinksParams>) -> Result<String, ErrorData> {
        let vault = self.vault()?;
        let export_opts = params.0.export_opts.clone();
        let output = tools::links::links(vault, params.0)?;
        respond_value(vault, &output, &export_opts)
    }

    #[tool(
        description = "BFS traversal from a starting note up to a given depth. Returns each reached note paired with the depth it was found at. Supports `export`."
    )]
    fn traverse(&self, params: Parameters<TraverseParams>) -> Result<String, ErrorData> {
        let vault = self.vault()?;
        let export_opts = params.0.export_opts.clone();
        let hits = tools::links::traverse(vault, params.0)?;
        respond_value(vault, &hits, &export_opts)
    }

    #[tool(
        description = "List all wikilinks across the vault that point at notes which don't exist. Supports `export`."
    )]
    fn unresolved(&self, params: Parameters<UnresolvedParams>) -> Result<String, ErrorData> {
        let vault = self.vault()?;
        let export_opts = params.0.export_opts.clone();
        let unresolved = tools::links::unresolved(vault, params.0)?;
        respond_value(vault, &unresolved, &export_opts)
    }

    // ── Schema tools ───────────────────────────────────────────────────────

    #[tool(
        description = "Show the persisted schema (vaultdb-schema.yaml). Optionally filter to one folder. Supports `export` (use .json or .yaml — schema shape isn't tabular)."
    )]
    fn schema_show(&self, params: Parameters<SchemaShowParams>) -> Result<String, ErrorData> {
        let vault = self.vault()?;
        let export_opts = params.0.export_opts.clone();
        let output = tools::schema::schema_show(vault, params.0)?;
        respond_value(vault, &output, &export_opts)
    }

    #[tool(
        description = "Infer a schema from existing records in a folder. Returns the structured schema and a YAML rendering for human review. Supports `export` (json/yaml)."
    )]
    fn schema_infer(&self, params: Parameters<SchemaInferParams>) -> Result<String, ErrorData> {
        let vault = self.vault()?;
        let export_opts = params.0.export_opts.clone();
        let output = tools::schema::schema_infer(vault, params.0)?;
        respond_value(vault, &output, &export_opts)
    }

    // ── Plan-only mutation tools ───────────────────────────────────────────
    //
    // These tools NEVER write to disk. They produce a MutationReport
    // describing what would change. The host (CLI / Tauri / human) is
    // expected to run the equivalent execute() if the plan is approved.

    #[tool(
        description = "Plan-only update. Shows what an update would change without writing. Set fields with field=value strings. Use add_tag/remove_tag for the tags list."
    )]
    fn plan_update(&self, params: Parameters<PlanUpdateParams>) -> Result<String, ErrorData> {
        json_string(tools::mutations::plan_update(self.vault()?, params.0)?)
    }

    #[tool(
        description = "Plan-only delete. Shows which files would move to .trash/ (or be permanently deleted with permanent=true). Does NOT delete."
    )]
    fn plan_delete(&self, params: Parameters<PlanDeleteParams>) -> Result<String, ErrorData> {
        json_string(tools::mutations::plan_delete(self.vault()?, params.0)?)
    }

    #[tool(
        description = "Plan-only move. Shows which files would be relocated to a destination folder. Does NOT move."
    )]
    fn plan_move(&self, params: Parameters<PlanMoveParams>) -> Result<String, ErrorData> {
        json_string(tools::mutations::plan_move(self.vault()?, params.0)?)
    }

    #[tool(
        description = "Plan-only rename. Shows the rename plus every backlink rewrite across the vault. Does NOT rename."
    )]
    fn plan_rename(&self, params: Parameters<PlanRenameParams>) -> Result<String, ErrorData> {
        json_string(tools::mutations::plan_rename(self.vault()?, params.0)?)
    }

    #[tool(
        description = "Plan-only create. Shows the file that would be written to <folder>/<name>.md including schema-defaulted fields and required-field checks. Does NOT write. `set` is a typed JSON object: {\"director\":\"...\", \"year\":2021}."
    )]
    fn plan_create(&self, params: Parameters<PlanCreateParams>) -> Result<String, ErrorData> {
        json_string(tools::mutations::plan_create(self.vault()?, params.0)?)
    }

    // ── Execute tools (flag-gated) ─────────────────────────────────────────
    //
    // Each of these mutates the vault and is therefore disabled by
    // default. Launch vaultdb-mcp with the corresponding
    // --dangerously-allow-* flag to arm. Every successful execute call
    // appends a line to `<vault>/.vaultdb/audit.log`.

    #[tool(
        description = "Execute create. Writes the file plan_create would preview. Requires --dangerously-allow-create at launch."
    )]
    fn execute_create(&self, params: Parameters<PlanCreateParams>) -> Result<String, ErrorData> {
        self.require(self.permissions.create, "dangerously-allow-create")?;
        json_string(tools::mutations::execute_create(self.vault()?, params.0)?)
    }

    #[tool(
        description = "Execute update. Applies the changes plan_update would preview. Requires --dangerously-allow-update at launch."
    )]
    fn execute_update(&self, params: Parameters<PlanUpdateParams>) -> Result<String, ErrorData> {
        self.require(self.permissions.update, "dangerously-allow-update")?;
        json_string(tools::mutations::execute_update(self.vault()?, params.0)?)
    }

    #[tool(
        description = "Execute move. Relocates files to the destination folder. Requires --dangerously-allow-update at launch."
    )]
    fn execute_move(&self, params: Parameters<PlanMoveParams>) -> Result<String, ErrorData> {
        self.require(self.permissions.update, "dangerously-allow-update")?;
        json_string(tools::mutations::execute_move(self.vault()?, params.0)?)
    }

    #[tool(
        description = "Execute rename. Renames the file and rewrites every wikilink across the vault. Requires --dangerously-allow-update at launch."
    )]
    fn execute_rename(&self, params: Parameters<PlanRenameParams>) -> Result<String, ErrorData> {
        self.require(self.permissions.update, "dangerously-allow-update")?;
        json_string(tools::mutations::execute_rename(self.vault()?, params.0)?)
    }

    #[tool(
        description = "Execute delete. Soft-deletes to .trash/ by default. Requires --dangerously-allow-delete at launch. Setting permanent=true ALSO requires --dangerously-allow-permanent-delete."
    )]
    fn execute_delete(&self, params: Parameters<PlanDeleteParams>) -> Result<String, ErrorData> {
        self.require(self.permissions.delete, "dangerously-allow-delete")?;
        if params.0.permanent {
            self.require(
                self.permissions.permanent_delete,
                "dangerously-allow-permanent-delete",
            )?;
        }
        json_string(tools::mutations::execute_delete(self.vault()?, params.0)?)
    }
}
