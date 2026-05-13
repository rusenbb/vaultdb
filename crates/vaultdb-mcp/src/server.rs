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

use rmcp::{ErrorData, handler::server::wrapper::Parameters, tool, tool_router};
use serde::Serialize;
use vaultdb_core::vault::Vault;

use crate::params::{
    FindByNameParams, LinksParams, ListFoldersParams, PlanCreateParams, PlanDeleteParams,
    PlanMoveParams, PlanRenameParams, PlanUpdateParams, QueryParams, SchemaInferParams,
    SchemaShowParams, TraverseParams, UnresolvedParams,
};
use crate::tools;

/// MCP server state. Holds an optional [`Vault`] (the server boots even
/// without a vault so tool calls can return a typed error rather than
/// the binary crashing on launch).
#[derive(Clone)]
pub struct VaultdbServer {
    vault: std::sync::Arc<Option<Vault>>,
}

impl VaultdbServer {
    pub fn new(vault: Option<Vault>) -> Self {
        Self {
            vault: std::sync::Arc::new(vault),
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
}

/// Serialize a value to pretty-printed JSON, mapping serde failures to
/// a clean `ErrorData`. Centralises the MCP-boundary conversion.
fn json_string<T: Serialize>(value: T) -> Result<String, ErrorData> {
    serde_json::to_string_pretty(&value)
        .map_err(|e| ErrorData::internal_error(format!("serialize failed: {}", e), None))
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
        description = "Run a structured query against a folder. Filter with the where-DSL (e.g. \"status = active\", \"tags contains topic/ai\"). Returns matching records as JSON, minus their raw body content."
    )]
    fn query(&self, params: Parameters<QueryParams>) -> Result<String, ErrorData> {
        json_string(tools::query::query(self.vault()?, params.0)?)
    }

    #[tool(
        description = "Look up a single record by filename (without .md). Returns JSON null when the record doesn't exist."
    )]
    fn find_by_name(&self, params: Parameters<FindByNameParams>) -> Result<String, ErrorData> {
        json_string(tools::query::find_by_name(self.vault()?, params.0)?)
    }

    #[tool(
        description = "List the top-level folders in the vault that contain at least one .md file. Useful for discovering what folders are queryable."
    )]
    fn list_folders(&self, params: Parameters<ListFoldersParams>) -> Result<String, ErrorData> {
        json_string(tools::query::list_folders(self.vault()?, params.0)?)
    }

    // ── Graph tools ────────────────────────────────────────────────────────

    #[tool(
        description = "Show outgoing and incoming wikilinks for a single note. Direction is one of 'outgoing', 'incoming', 'both'."
    )]
    fn links(&self, params: Parameters<LinksParams>) -> Result<String, ErrorData> {
        json_string(tools::links::links(self.vault()?, params.0)?)
    }

    #[tool(
        description = "BFS traversal from a starting note up to a given depth. Returns each reached note paired with the depth it was found at."
    )]
    fn traverse(&self, params: Parameters<TraverseParams>) -> Result<String, ErrorData> {
        json_string(tools::links::traverse(self.vault()?, params.0)?)
    }

    #[tool(
        description = "List all wikilinks across the vault that point at notes which don't exist."
    )]
    fn unresolved(&self, params: Parameters<UnresolvedParams>) -> Result<String, ErrorData> {
        json_string(tools::links::unresolved(self.vault()?, params.0)?)
    }

    // ── Schema tools ───────────────────────────────────────────────────────

    #[tool(
        description = "Show the persisted schema (vaultdb-schema.yaml). Optionally filter to one folder."
    )]
    fn schema_show(&self, params: Parameters<SchemaShowParams>) -> Result<String, ErrorData> {
        json_string(tools::schema::schema_show(self.vault()?, params.0)?)
    }

    #[tool(
        description = "Infer a schema from existing records in a folder. Returns the structured schema and a YAML rendering for human review."
    )]
    fn schema_infer(&self, params: Parameters<SchemaInferParams>) -> Result<String, ErrorData> {
        json_string(tools::schema::schema_infer(self.vault()?, params.0)?)
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
}
