//! vaultdb-mcp: a stdio MCP server exposing vaultdb-core to LLM agents.
//!
//! Vault resolution priority: `--vault <path>` flag, `VAULTDB_VAULT` env
//! var, then walking up from cwd looking for `.obsidian/`. If none of
//! these find a vault the server still starts; tool calls return a
//! typed error explaining the resolution failure.

mod server;

use std::path::PathBuf;

use clap::Parser;
use vaultdb_core::vault::Vault;

/// CLI arguments for the MCP server.
#[derive(Parser)]
#[command(name = "vaultdb-mcp", about = "MCP server for vaultdb")]
struct Cli {
    /// Path to the Obsidian vault root (overrides VAULTDB_VAULT and auto-discovery).
    #[arg(long)]
    vault: Option<PathBuf>,
}

/// Resolve the vault using --vault, then VAULTDB_VAULT, then
/// `Vault::discover` from cwd. Returns `None` if none of the strategies
/// find a vault; the server still starts and surfaces the failure as a
/// per-tool error.
fn resolve_vault(cli: &Cli) -> Option<Vault> {
    if let Some(path) = &cli.vault {
        return Some(Vault::with_root(path.clone()));
    }
    if let Ok(path) = std::env::var("VAULTDB_VAULT")
        && !path.is_empty()
    {
        return Some(Vault::with_root(PathBuf::from(path)));
    }
    let cwd = std::env::current_dir().ok()?;
    Vault::discover(&cwd).ok()
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let vault = resolve_vault(&cli);
    let _server = server::VaultdbServer::new(vault);
    eprintln!("vaultdb-mcp scaffold OK (Task 2)");
    Ok(())
}
