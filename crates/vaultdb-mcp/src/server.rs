//! [`VaultdbServer`] — the MCP server's state. Holds an optional [`Vault`]
//! (optional so the server starts even when no vault is found, and tool
//! calls can return a typed error rather than the binary refusing to run).
//!
//! Tools are wired via the `#[tool_router(server_handler)]` macro at the
//! bottom of this file. The macro generates the `ServerHandler` impl that
//! routes incoming `tools/call` JSON-RPC requests to the methods marked
//! `#[tool]`. Each tool method delegates to a free function in
//! `crate::tools` so the implementations stay testable without needing
//! an `rmcp` runtime.

use rmcp::{ErrorData, tool, tool_router};
use vaultdb_core::vault::Vault;

/// MCP server state.
///
/// `vault` is `Option<Vault>` so the server can start even if the
/// configured vault path doesn't resolve to a real Obsidian vault. Tool
/// calls that need a vault use [`VaultdbServer::vault`] which returns a
/// typed `ErrorData` when none is available — the LLM client gets a clean
/// error message instead of a broken connection.
#[derive(Clone)]
pub struct VaultdbServer {
    vault: std::sync::Arc<Option<Vault>>,
}

impl VaultdbServer {
    /// Construct a server with a resolved vault (or `None` if resolution failed).
    pub fn new(vault: Option<Vault>) -> Self {
        Self {
            vault: std::sync::Arc::new(vault),
        }
    }

    /// Borrow the vault, or return an `ErrorData::invalid_params` describing
    /// why no vault was resolved. Use this in every tool implementation that
    /// needs the vault.
    pub fn vault(&self) -> Result<&Vault, ErrorData> {
        self.vault.as_ref().as_ref().ok_or_else(|| {
            ErrorData::invalid_params(
                "no vault configured: pass --vault <path>, set VAULTDB_VAULT, or run from inside a directory whose ancestors contain `.obsidian/`",
                None,
            )
        })
    }
}

/// MCP tool handlers.
///
/// `#[tool_router(server_handler)]` generates the `ServerHandler` impl
/// that exposes every `#[tool]`-tagged method to MCP clients.
#[tool_router(server_handler)]
impl VaultdbServer {
    /// Liveness check. Returns "pong" so clients can verify the server
    /// process is alive and the transport is wired correctly.
    #[tool(description = "Liveness check. Returns 'pong' if the server is alive.")]
    fn ping(&self) -> String {
        "pong".to_string()
    }
}
