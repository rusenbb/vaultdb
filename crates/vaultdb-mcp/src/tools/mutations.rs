//! Plan-only mutation tools: `plan_update`, `plan_delete`, `plan_move`,
//! `plan_rename`.
//!
//! These tools never write to disk. They produce a `MutationReport`
//! describing what would change. The host is expected to render the
//! report to a human, get explicit consent, and then run the equivalent
//! mutation through a non-MCP path (the CLI, or a Tauri command in the
//! eduport case). This is the "plan-only mutation tools" rule from the
//! spec — agents propose, humans approve, hosts execute.

use rmcp::ErrorData;
use vaultdb_core::mutation::{
    DeleteBuilder, MoveBuilder, MutationReport, RenameBuilder, UpdateBuilder,
};
use vaultdb_core::query::Expr;
use vaultdb_core::record::Value;
use vaultdb_core::vault::Vault;

use crate::params::{PlanDeleteParams, PlanMoveParams, PlanRenameParams, PlanUpdateParams};

pub fn plan_update(vault: &Vault, params: PlanUpdateParams) -> Result<MutationReport, ErrorData> {
    let filter = parse_where(&params.r#where)?;
    let mut builder = UpdateBuilder::new(params.folder, filter);

    for s in params.set {
        let (field, value_str) = s.split_once('=').ok_or_else(|| {
            invalid_params(format!("--set requires FIELD=VALUE format, got: {}", s))
        })?;
        builder = builder.set(field.trim(), parse_set_value(value_str.trim()));
    }
    for field in params.unset {
        builder = builder.unset(field);
    }
    for tag in params.add_tag {
        builder = builder.add_tag(tag);
    }
    for tag in params.remove_tag {
        builder = builder.remove_tag(tag);
    }

    builder
        .plan(vault)
        .map_err(|e| invalid_params(format!("plan_update failed: {}", e)))
}

pub fn plan_delete(vault: &Vault, params: PlanDeleteParams) -> Result<MutationReport, ErrorData> {
    let filter = parse_where(&params.r#where)?;
    DeleteBuilder::new(params.folder, filter)
        .permanent(params.permanent)
        .plan(vault)
        .map_err(|e| invalid_params(format!("plan_delete failed: {}", e)))
}

pub fn plan_move(vault: &Vault, params: PlanMoveParams) -> Result<MutationReport, ErrorData> {
    let filter = parse_where(&params.r#where)?;
    MoveBuilder::new(params.folder, params.to, filter)
        .plan(vault)
        .map_err(|e| invalid_params(format!("plan_move failed: {}", e)))
}

pub fn plan_rename(vault: &Vault, params: PlanRenameParams) -> Result<MutationReport, ErrorData> {
    RenameBuilder::new(params.folder, params.from, params.to)
        .plan(vault)
        .map_err(|e| invalid_params(format!("plan_rename failed: {}", e)))
}

// ── helpers ────────────────────────────────────────────────────────────────

fn parse_where(s: &str) -> Result<Expr, ErrorData> {
    Expr::parse(s).map_err(|e| invalid_params(format!("invalid where expression '{}': {}", s, e)))
}

/// Best-effort numeric-or-string coercion for plan_update's `field=value`.
fn parse_set_value(s: &str) -> Value {
    if let Ok(i) = s.parse::<i64>() {
        return Value::Integer(i);
    }
    if let Ok(f) = s.parse::<f64>() {
        return Value::Float(f);
    }
    Value::String(s.to_string())
}

fn invalid_params(message: String) -> ErrorData {
    ErrorData::invalid_params(message, None)
}
