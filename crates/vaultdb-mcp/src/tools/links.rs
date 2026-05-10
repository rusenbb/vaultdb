//! Graph-side read tools: `links`, `traverse`, `unresolved`.

use rmcp::ErrorData;
use serde::Serialize;
use vaultdb_core::links::{Direction, GraphScope, UnresolvedLink};
use vaultdb_core::vault::Vault;

use crate::params::{LinksParams, TraverseParams, UnresolvedParams};

/// Output of the `links` tool: the outgoing and incoming names a single
/// note points at / is pointed at by.
#[derive(Debug, Serialize)]
pub struct LinksOutput {
    pub name: String,
    pub outgoing: Vec<String>,
    pub incoming: Vec<String>,
}

pub fn links(vault: &Vault, params: LinksParams) -> Result<LinksOutput, ErrorData> {
    let _direction = parse_direction(&params.direction)?;
    let graph = vault
        .link_graph(GraphScope::All)
        .map_err(|e| invalid("link_graph failed", e))?;
    Ok(LinksOutput {
        outgoing: graph
            .outgoing_links(&params.name)
            .into_iter()
            .map(String::from)
            .collect(),
        incoming: graph
            .incoming_links(&params.name)
            .into_iter()
            .map(String::from)
            .collect(),
        name: params.name,
    })
}

/// Output of `traverse`: every note reached, paired with its BFS depth.
#[derive(Debug, Serialize)]
pub struct TraverseHit {
    pub name: String,
    pub depth: usize,
}

pub fn traverse(
    vault: &Vault,
    params: TraverseParams,
) -> Result<Vec<TraverseHit>, ErrorData> {
    let direction = parse_direction(&params.direction)?;
    let graph = vault
        .link_graph(GraphScope::All)
        .map_err(|e| invalid("link_graph failed", e))?;
    Ok(graph
        .traverse(&params.name, params.depth, direction)
        .into_iter()
        .map(|(name, depth)| TraverseHit { name, depth })
        .collect())
}

pub fn unresolved(
    vault: &Vault,
    _params: UnresolvedParams,
) -> Result<Vec<UnresolvedLink>, ErrorData> {
    let graph = vault
        .link_graph(GraphScope::All)
        .map_err(|e| invalid("link_graph failed", e))?;
    Ok(graph.unresolved())
}

fn parse_direction(s: &str) -> Result<Direction, ErrorData> {
    match s.to_ascii_lowercase().as_str() {
        "outgoing" | "out" => Ok(Direction::Outgoing),
        "incoming" | "in" | "back" => Ok(Direction::Incoming),
        "both" => Ok(Direction::Both),
        other => Err(ErrorData::invalid_params(
            format!(
                "direction must be 'outgoing', 'incoming', or 'both'; got '{}'",
                other
            ),
            None,
        )),
    }
}

fn invalid(context: &str, err: impl std::fmt::Display) -> ErrorData {
    ErrorData::invalid_params(format!("{}: {}", context, err), None)
}
