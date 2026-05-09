//! [`VaultdbServer`] — the MCP server's state. Holds an optional [`Vault`]
//! (optional so the server starts even when no vault is found, and tool
//! calls can return a typed error rather than the binary refusing to run).

use rmcp::ErrorData;
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
