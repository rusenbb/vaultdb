//! MCP mutation tools.
//!
//! Two flavours: `plan_*` previews never touch disk and are always
//! available. `execute_*` actually writes and is gated by the launch
//! flags `--dangerously-allow-create / -update / -delete /
//! -permanent-delete` — without the flag, the corresponding execute
//! tool returns a typed error explaining how to enable it.
//!
//! The default mode (no flags) preserves the "agents propose, humans
//! approve, hosts execute" pattern from the spec. The opt-in flags
//! shift execution to the agent for sessions where that's the desired
//! ergonomics.
//!
//! Every successful execute call appends a line to
//! `<vault>/.vaultdb/audit.log`.

use rmcp::ErrorData;
use vaultdb_core::mutation::{
    CreateBuilder, DeleteBuilder, MoveBuilder, MutationReport, RenameBuilder, UpdateBuilder,
};
use vaultdb_core::query::Expr;
use vaultdb_core::record::Value;
use vaultdb_core::schema;
use vaultdb_core::vault::Vault;

use crate::params::{
    PlanCreateParams, PlanDeleteParams, PlanMoveParams, PlanRenameParams, PlanUpdateParams,
};

pub fn plan_update(vault: &Vault, params: PlanUpdateParams) -> Result<MutationReport, ErrorData> {
    build_update(params)?
        .plan(vault)
        .map_err(|e| invalid_params(format!("plan_update failed: {}", e)))
}

/// Shared UpdateBuilder construction for plan_update / execute_update.
/// Applies legacy `set` first, then `set_typed` (so typed values win
/// on key collision — new code naturally takes precedence).
fn build_update(params: PlanUpdateParams) -> Result<UpdateBuilder, ErrorData> {
    let filter = parse_where(&params.r#where)?;
    let mut builder = UpdateBuilder::new(params.folder, filter);

    for s in params.set {
        let (field, value_str) = s.split_once('=').ok_or_else(|| {
            invalid_params(format!("--set requires FIELD=VALUE format, got: {}", s))
        })?;
        builder = builder.set(field.trim(), Value::parse_scalar(value_str.trim()));
    }
    for (field, json_value) in params.set_typed {
        builder = builder.set(field, json_to_vaultdb_value(json_value));
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
    Ok(builder)
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

pub fn plan_create(vault: &Vault, params: PlanCreateParams) -> Result<MutationReport, ErrorData> {
    build_create(vault, params)?
        .plan(vault)
        .map_err(|e| invalid_params(format!("plan_create failed: {}", e)))
}

// ── Execute variants ──────────────────────────────────────────────────────
//
// Each calls the corresponding `.execute()` on the underlying builder and
// appends a line to `<vault>/.vaultdb/audit.log` on success. The audit
// hook lives at the MCP boundary on purpose — the goal is to record what
// _this MCP server_ did on behalf of an agent, not to track every
// vaultdb-core caller.

pub fn execute_create(
    vault: &Vault,
    params: PlanCreateParams,
) -> Result<MutationReport, ErrorData> {
    let folder = params.folder.clone();
    let name = params.name.clone();
    let builder = build_create(vault, params)?;
    let report = builder
        .execute(vault)
        .map_err(|e| invalid_params(format!("execute_create failed: {}", e)))?;
    audit(
        &vault.root,
        "execute_create",
        &format!("folder={} name={}", folder, name),
        &report,
    );
    Ok(report)
}

pub fn execute_update(
    vault: &Vault,
    params: PlanUpdateParams,
) -> Result<MutationReport, ErrorData> {
    let folder = params.folder.clone();
    let where_str = params.r#where.clone();
    let report = build_update(params)?
        .execute(vault)
        .map_err(|e| invalid_params(format!("execute_update failed: {}", e)))?;
    audit(
        &vault.root,
        "execute_update",
        &format!("folder={} where={:?}", folder, where_str),
        &report,
    );
    Ok(report)
}

pub fn execute_delete(
    vault: &Vault,
    params: PlanDeleteParams,
) -> Result<MutationReport, ErrorData> {
    let folder = params.folder.clone();
    let where_str = params.r#where.clone();
    let permanent = params.permanent;
    let filter = parse_where(&params.r#where)?;
    let report = DeleteBuilder::new(params.folder, filter)
        .permanent(params.permanent)
        .execute(vault)
        .map_err(|e| invalid_params(format!("execute_delete failed: {}", e)))?;
    audit(
        &vault.root,
        "execute_delete",
        &format!(
            "folder={} where={:?} permanent={}",
            folder, where_str, permanent
        ),
        &report,
    );
    Ok(report)
}

pub fn execute_move(vault: &Vault, params: PlanMoveParams) -> Result<MutationReport, ErrorData> {
    let folder = params.folder.clone();
    let to = params.to.clone();
    let where_str = params.r#where.clone();
    let filter = parse_where(&params.r#where)?;
    let report = MoveBuilder::new(params.folder, params.to, filter)
        .execute(vault)
        .map_err(|e| invalid_params(format!("execute_move failed: {}", e)))?;
    audit(
        &vault.root,
        "execute_move",
        &format!("folder={} to={} where={:?}", folder, to, where_str),
        &report,
    );
    Ok(report)
}

pub fn execute_rename(
    vault: &Vault,
    params: PlanRenameParams,
) -> Result<MutationReport, ErrorData> {
    let folder = params.folder.clone();
    let from = params.from.clone();
    let to = params.to.clone();
    let report = RenameBuilder::new(params.folder, params.from, params.to)
        .execute(vault)
        .map_err(|e| invalid_params(format!("execute_rename failed: {}", e)))?;
    audit(
        &vault.root,
        "execute_rename",
        &format!("folder={} from={} to={}", folder, from, to),
        &report,
    );
    Ok(report)
}

/// Shared CreateBuilder construction for plan_create / execute_create.
fn build_create(vault: &Vault, params: PlanCreateParams) -> Result<CreateBuilder, ErrorData> {
    let folder = params.folder.clone();
    let mut builder = CreateBuilder::new(folder.clone(), params.name);

    if let Some(t) = params.template {
        builder = builder.template(t);
    }

    for (field, json_value) in params.set {
        builder = builder.set(field, json_to_vaultdb_value(json_value));
    }

    let schema_path = schema::schema_path(&vault.root);
    if schema_path.is_file() {
        let vault_schema = schema::load_schema(&schema_path)
            .map_err(|e| invalid_params(format!("loading {}: {}", schema_path.display(), e)))?;
        if let Some(collection) = vault_schema.collection_for_folder(&folder) {
            builder = builder.with_schema(collection.clone());
        }
    }

    Ok(builder)
}

/// Append a single line to `<vault>/.vaultdb/audit.log` describing the
/// execute call. Best-effort: failures are silently ignored — losing
/// audit entries is better than failing the actual mutation when, say,
/// the filesystem is read-only. Tracing surfaces the failure for
/// operators who care.
fn audit(vault_root: &std::path::Path, tool: &str, params_summary: &str, report: &MutationReport) {
    let dir = vault_root.join(".vaultdb");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %e, "audit log: cannot create .vaultdb/");
        return;
    }
    let path = dir.join("audit.log");
    let timestamp = audit_timestamp();
    let line = format!(
        "{} {} {} changes={} errors={}\n",
        timestamp,
        tool,
        params_summary,
        report.changes.len(),
        report.errors.len()
    );
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                tracing::warn!(error = %e, path = %path.display(), "audit log write failed");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "audit log open failed");
        }
    }
}

/// `YYYY-MM-DD HH:MM:SSZ` — second-precision UTC timestamp matching the
/// existing `record::now_string` format with an explicit Z suffix for
/// log parsing. Hand-rolled to keep the no-date-crate stance from core.
fn audit_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86400;
    let rem = secs % 86400;
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let s = rem % 60;
    let date = vaultdb_core::record::today_string();
    // today_string uses the same epoch arithmetic; reuse it for the date
    // part, append HH:MM:SS computed from the same `secs`.
    let _ = days; // already accounted for in today_string()
    format!("{}T{:02}:{:02}:{:02}Z", date, h, m, s)
}

// ── helpers ────────────────────────────────────────────────────────────────

fn parse_where(s: &str) -> Result<Expr, ErrorData> {
    Expr::parse(s).map_err(|e| invalid_params(format!("invalid where expression '{}': {}", s, e)))
}

/// Convert a `serde_json::Value` into a `vaultdb_core::Value`. JSON's
/// type tower maps directly onto vaultdb's, including recursive lists
/// and maps. Numbers prefer integer; non-representable values fall
/// back to float; non-finite floats fall back to null. (We hit "null"
/// rather than erroring because JSON Schema clients sometimes serialise
/// NaN/Infinity as JSON null already; preserving that round-trip is
/// less surprising than failing the whole `plan_create`.)
fn json_to_vaultdb_value(v: serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::String(s),
        serde_json::Value::Array(arr) => {
            Value::List(arr.into_iter().map(json_to_vaultdb_value).collect())
        }
        serde_json::Value::Object(obj) => Value::Map(
            obj.into_iter()
                .map(|(k, v)| (k, json_to_vaultdb_value(v)))
                .collect(),
        ),
    }
}

fn invalid_params(message: String) -> ErrorData {
    ErrorData::invalid_params(message, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_scalars_map_to_value() {
        assert_eq!(json_to_vaultdb_value(serde_json::json!(null)), Value::Null);
        assert_eq!(
            json_to_vaultdb_value(serde_json::json!(true)),
            Value::Bool(true)
        );
        assert_eq!(
            json_to_vaultdb_value(serde_json::json!(42)),
            Value::Integer(42)
        );
        assert_eq!(
            json_to_vaultdb_value(serde_json::json!(1.5)),
            Value::Float(1.5)
        );
        assert_eq!(
            json_to_vaultdb_value(serde_json::json!("hi")),
            Value::String("hi".into())
        );
    }

    #[test]
    fn json_list_and_map_recurse() {
        let v = json_to_vaultdb_value(serde_json::json!([1, "x", true]));
        assert_eq!(
            v,
            Value::List(vec![
                Value::Integer(1),
                Value::String("x".into()),
                Value::Bool(true)
            ])
        );

        let v = json_to_vaultdb_value(serde_json::json!({"k": 5, "nested": {"x": "y"}}));
        match v {
            Value::Map(m) => {
                assert_eq!(m.get("k"), Some(&Value::Integer(5)));
                match m.get("nested") {
                    Some(Value::Map(inner)) => {
                        assert_eq!(inner.get("x"), Some(&Value::String("y".into())))
                    }
                    other => panic!("expected nested map, got {:?}", other),
                }
            }
            other => panic!("expected map, got {:?}", other),
        }
    }
}
