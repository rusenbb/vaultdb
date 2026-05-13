//! Plan-only mutation tools: `plan_create`, `plan_update`, `plan_delete`,
//! `plan_move`, `plan_rename`.
//!
//! These tools never write to disk. They produce a `MutationReport`
//! describing what would change. The host is expected to render the
//! report to a human, get explicit consent, and then run the equivalent
//! mutation through a non-MCP path (the CLI, or a Tauri command in the
//! eduport case). This is the "plan-only mutation tools" rule from the
//! spec — agents propose, humans approve, hosts execute.

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
    let filter = parse_where(&params.r#where)?;
    let mut builder = UpdateBuilder::new(params.folder, filter);

    for s in params.set {
        let (field, value_str) = s.split_once('=').ok_or_else(|| {
            invalid_params(format!("--set requires FIELD=VALUE format, got: {}", s))
        })?;
        builder = builder.set(field.trim(), Value::parse_scalar(value_str.trim()));
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

pub fn plan_create(vault: &Vault, params: PlanCreateParams) -> Result<MutationReport, ErrorData> {
    let mut builder = CreateBuilder::new(params.folder.clone(), params.name);

    if let Some(t) = params.template {
        builder = builder.template(t);
    }

    for (field, json_value) in params.set {
        let v = json_to_vaultdb_value(json_value);
        builder = builder.set(field, v);
    }

    // Best-effort schema lookup: same logic as the CLI's `run_create`.
    let schema_path = schema::schema_path(&vault.root);
    if schema_path.is_file() {
        let vault_schema = schema::load_schema(&schema_path)
            .map_err(|e| invalid_params(format!("loading {}: {}", schema_path.display(), e)))?;
        if let Some(collection) = vault_schema.collection_for_folder(&params.folder) {
            builder = builder.with_schema(collection.clone());
        }
    }

    builder
        .plan(vault)
        .map_err(|e| invalid_params(format!("plan_create failed: {}", e)))
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
