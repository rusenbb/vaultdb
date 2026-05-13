//! vaultdb-mcp: a stdio MCP server exposing vaultdb-core to LLM agents.
//!
//! Vault resolution priority: `--vault <path>` flag, `VAULTDB_VAULT` env
//! var, then walking up from cwd looking for `.obsidian/`. If none of
//! these find a vault the server still starts; tool calls return a
//! typed error explaining the resolution failure.

mod params;
mod server;
mod tools;

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use rmcp::{ServiceExt, transport::stdio};
use vaultdb_core::vault::Vault;

/// CLI arguments for the MCP server.
#[derive(Parser)]
#[command(name = "vaultdb-mcp", about = "MCP server for vaultdb")]
struct Cli {
    /// Path to the Obsidian vault root (overrides VAULTDB_VAULT and auto-discovery).
    #[arg(long)]
    vault: Option<PathBuf>,

    /// Allow execute_create — the MCP server will actually write new
    /// notes when the client calls execute_create. Without this flag,
    /// only plan_create (preview) is exposed.
    #[arg(long)]
    dangerously_allow_create: bool,

    /// Allow execute_update, execute_move, execute_rename — the MCP
    /// server will actually modify existing notes. Without this flag,
    /// only the corresponding plan_* tools are exposed.
    #[arg(long)]
    dangerously_allow_update: bool,

    /// Allow execute_delete — soft-delete by default (moves to
    /// `.trash/`). Permanent deletion still requires the stronger
    /// --dangerously-allow-permanent-delete in addition.
    #[arg(long)]
    dangerously_allow_delete: bool,

    /// Allow execute_delete with permanent=true. Requires
    /// --dangerously-allow-delete as well. Files removed this way go
    /// to /dev/null — no undo.
    #[arg(long)]
    dangerously_allow_permanent_delete: bool,
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs go to stderr so they don't pollute the JSON-RPC framing on
    // stdout. RUST_LOG controls verbosity (default: warn).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let cli = Cli::parse();
    let vault = resolve_vault(&cli);
    let permissions = server::ExecutePermissions {
        create: cli.dangerously_allow_create,
        update: cli.dangerously_allow_update,
        delete: cli.dangerously_allow_delete,
        permanent_delete: cli.dangerously_allow_permanent_delete,
    };
    // Surface what's enabled so the user can spot misconfigurations
    // in the logs without guessing.
    if permissions.any() {
        tracing::warn!(
            create = permissions.create,
            update = permissions.update,
            delete = permissions.delete,
            permanent_delete = permissions.permanent_delete,
            "execute tools enabled — agent can mutate the vault"
        );
    }
    let server = server::VaultdbServer::new(vault, permissions);

    let running = server
        .serve(stdio())
        .await
        .context("failed to start MCP server over stdio")?;
    running
        .waiting()
        .await
        .context("MCP server exited with error")?;
    Ok(())
}
