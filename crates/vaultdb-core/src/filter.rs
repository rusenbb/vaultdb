//! Evaluator for the public [`crate::query::Expr`] AST.
//!
//! Walks `Expr` and `Predicate` trees against a [`Record`] (and an optional
//! [`crate::links::LinkGraph`] for graph predicates) and returns `bool`.
//! Also still hosts the legacy internal `WhereClause`/`WhereExpr` types and
//! their where-DSL parser, used by `<Expr as FromStr>::from_str` via a
//! conversion shim.

use std::path::Path;

use regex::Regex;

use crate::error::{Result, VaultdbError};
use crate::record::Record;

#[derive(Debug, Clone)]
pub(crate) enum CompareOp {
    Eq,
    Neq,
    Gt,
    Lt,
    Gte,
    Lte,
    Contains,
    StartsWith,
    EndsWith,
    Exists,
    Missing,
    Matches,
}

#[derive(Debug, Clone)]
pub(crate) struct WhereExpr {
    pub field: String,
    pub op: CompareOp,
    pub negated: bool,
    /// None for Exists/Missing operators.
    pub value: Option<String>,
}

/// A where clause is one `--where` argument, which may contain OR-ed expressions.
/// Multiple `--where` arguments are AND-ed together.
#[derive(Debug, Clone)]
pub(crate) struct WhereClause {
    /// Expressions OR-ed within this clause.
    pub alternatives: Vec<WhereExpr>,
}

/// Word-based operators (checked before symbolic ones).
const WORD_OPS: &[(&str, CompareOp)] = &[
    (" !contains ", CompareOp::Contains),
    (" contains ", CompareOp::Contains),
    (" !startswith ", CompareOp::StartsWith),
    (" startswith ", CompareOp::StartsWith),
    (" !endswith ", CompareOp::EndsWith),
    (" endswith ", CompareOp::EndsWith),
    (" !matches ", CompareOp::Matches),
    (" matches ", CompareOp::Matches),
    (" !exists", CompareOp::Exists),
    (" exists", CompareOp::Exists),
    (" !missing", CompareOp::Missing),
    (" missing", CompareOp::Missing),
];

/// Symbolic operators (checked in order: longest first to avoid ambiguity).
const SYMBOL_OPS: &[(&str, CompareOp)] = &[
    (" >= ", CompareOp::Gte),
    (" <= ", CompareOp::Lte),
    (" != ", CompareOp::Neq),
    (" > ", CompareOp::Gt),
    (" < ", CompareOp::Lt),
    (" = ", CompareOp::Eq),
];

impl WhereClause {
    /// Parse a where clause string, which may contain `||` for OR.
    ///
    /// Examples:
    ///   "status = to-watch"                        -> single expression
    ///   "status = to-watch || status = watching"    -> OR of two expressions
    pub fn parse(input: &str) -> Result<Self> {
        let parts: Vec<&str> = input.split("||").collect();
        let mut alternatives = Vec::new();
        for part in parts {
            alternatives.push(WhereExpr::parse(part)?);
        }
        Ok(WhereClause { alternatives })
    }

}

impl WhereExpr {
    /// Parse a single where-expression string.
    ///
    /// Examples:
    ///   "status = to-watch"
    ///   "tags contains topic/chinese"
    ///   "tags !contains topic/chinese"   (negated)
    ///   "hsk > 2"
    ///   "rating exists"
    ///   "rating !exists"                 (negated)
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();

        // Try word-based operators first
        for (pattern, op) in WORD_OPS {
            if let Some(pos) = input.find(pattern) {
                let field = input[..pos].trim().to_string();
                let value_str = input[pos + pattern.len()..].trim();
                let negated = pattern.contains('!');

                if field.is_empty() {
                    return Err(VaultdbError::InvalidWhereExpr(format!(
                        "missing field name in: {}",
                        input
                    )));
                }

                let value = match op {
                    CompareOp::Exists | CompareOp::Missing => None,
                    _ => Some(value_str.to_string()),
                };

                // Validate regex at parse time
                if matches!(op, CompareOp::Matches) {
                    if let Some(ref v) = value {
                        if Regex::new(v).is_err() {
                            return Err(VaultdbError::RegexError {
                                pattern: v.clone(),
                                reason: "invalid regex syntax".into(),
                            });
                        }
                    }
                }

                return Ok(WhereExpr {
                    field,
                    op: op.clone(),
                    negated,
                    value,
                });
            }
        }

        // Try symbolic operators
        for (pattern, op) in SYMBOL_OPS {
            if let Some(pos) = input.find(pattern) {
                let field = input[..pos].trim().to_string();
                let value_str = input[pos + pattern.len()..].trim().to_string();

                if field.is_empty() {
                    return Err(VaultdbError::InvalidWhereExpr(format!(
                        "missing field name in: {}",
                        input
                    )));
                }

                return Ok(WhereExpr {
                    field,
                    op: op.clone(),
                    negated: false,
                    value: Some(value_str),
                });
            }
        }

        Err(VaultdbError::InvalidWhereExpr(format!(
            "no valid operator found in: {}",
            input
        )))
    }

    // The legacy `matches_with_links` evaluator is no longer needed: every
    // call site has migrated to evaluating the public `Expr` AST via
    // `evaluate_expr`/`evaluate_predicate` later in this file. The internal
    // `WhereExpr`/`WhereClause` types now exist only as the parser's output
    // (via `parse_where_clause`) which is then converted to `Expr` through
    // `to_expr`/`to_predicate_expr`/`to_predicate`.
}


// ── Public query AST bridge ────────────────────────────────────────────────

/// Parse a where-DSL string into a `WhereClause` (internal AST).
///
/// Public-but-internal: the public `Expr` type's `FromStr` delegates here.
/// This function will be removed once `filter.rs` is fully migrated to the
/// new AST in a later task.
pub(crate) fn parse_where_clause(input: &str) -> Result<WhereClause> {
    WhereClause::parse(input)
}

impl WhereClause {
    /// Convert this internal AST into the new public `Expr` type.
    ///
    /// `WhereClause` holds a `Vec<WhereExpr>` with OR semantics.
    /// - If there is exactly one alternative (the common case), return the
    ///   expression directly to avoid a redundant single-element `Or`.
    /// - If there are multiple alternatives, wrap them in `Expr::Or`.
    pub fn to_expr(&self) -> crate::query::Expr {
        let mut exprs: Vec<crate::query::Expr> = self
            .alternatives
            .iter()
            .map(|alt| alt.to_predicate_expr())
            .collect();

        match exprs.len() {
            0 => {
                // Degenerate empty clause — treat as always-true; callers
                // should not produce empty WhereClause, but be defensive.
                crate::query::Expr::And(vec![])
            }
            1 => exprs.remove(0),
            _ => crate::query::Expr::Or(exprs),
        }
    }
}

impl WhereExpr {
    /// Convert this single internal expression into an `Expr`.
    ///
    /// Handles the `negated` flag by wrapping in `Expr::Not` when set.
    fn to_predicate_expr(&self) -> crate::query::Expr {
        let pred = crate::query::Expr::Predicate(self.to_predicate());
        if self.negated {
            crate::query::Expr::Not(Box::new(pred))
        } else {
            pred
        }
    }

    /// Convert this single internal predicate into a `Predicate`.
    ///
    /// Value coercion: the internal AST stores values as `Option<String>`.
    /// For `Equals` and `Contains` we coerce to `Value::Integer` / `Value::Float`
    /// when the string parses as a number, falling back to `Value::String`.
    /// For `Compare` ops the test suite expects `Value::Integer(2020)` from
    /// `"year > 2020"`, so the same coercion applies there too.
    pub fn to_predicate(&self) -> crate::query::Predicate {
        use crate::filter::CompareOp as IOp;
        use crate::query::{CompareOp as QOp, Predicate};
        use crate::record::Value;

        /// Best-effort numeric coercion of a where-clause RHS string.
        fn coerce(s: &str) -> Value {
            if let Ok(i) = s.parse::<i64>() {
                return Value::Integer(i);
            }
            if let Ok(f) = s.parse::<f64>() {
                return Value::Float(f);
            }
            Value::String(s.to_string())
        }

        let field = self.field.clone();
        let rhs_str = self.value.as_deref().unwrap_or("");

        match self.op {
            IOp::Eq => Predicate::Equals {
                field,
                value: coerce(rhs_str),
            },
            IOp::Neq => Predicate::Compare {
                field,
                op: QOp::Ne,
                value: coerce(rhs_str),
            },
            IOp::Gt => Predicate::Compare {
                field,
                op: QOp::Gt,
                value: coerce(rhs_str),
            },
            IOp::Lt => Predicate::Compare {
                field,
                op: QOp::Lt,
                value: coerce(rhs_str),
            },
            IOp::Gte => Predicate::Compare {
                field,
                op: QOp::Ge,
                value: coerce(rhs_str),
            },
            IOp::Lte => Predicate::Compare {
                field,
                op: QOp::Le,
                value: coerce(rhs_str),
            },
            IOp::Contains => Predicate::Contains {
                field,
                value: coerce(rhs_str),
            },
            IOp::StartsWith => Predicate::StartsWith {
                field,
                value: rhs_str.to_string(),
            },
            IOp::EndsWith => Predicate::EndsWith {
                field,
                value: rhs_str.to_string(),
            },
            IOp::Matches => Predicate::Matches {
                field,
                regex: rhs_str.to_string(),
            },
            IOp::Exists => Predicate::Exists { field },
            IOp::Missing => Predicate::Missing { field },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Value;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_record(fields: Vec<(&str, Value)>) -> Record {
        let mut map = BTreeMap::new();
        for (k, v) in fields {
            map.insert(k.to_string(), v);
        }
        Record {
            path: PathBuf::from("/vault/notes/Test.md"),
            fields: map,
            raw_content: None,
        }
    }

    fn vault_root() -> PathBuf {
        PathBuf::from("/vault")
    }

    #[test]
    fn parse_eq() {
        let expr = WhereExpr::parse("status = to-watch").unwrap();
        assert_eq!(expr.field, "status");
        assert!(matches!(expr.op, CompareOp::Eq));
        assert_eq!(expr.value.as_deref(), Some("to-watch"));
    }

    #[test]
    fn parse_neq() {
        let expr = WhereExpr::parse("status != draft").unwrap();
        assert!(matches!(expr.op, CompareOp::Neq));
    }

    #[test]
    fn parse_gt() {
        let expr = WhereExpr::parse("hsk > 2").unwrap();
        assert_eq!(expr.field, "hsk");
        assert!(matches!(expr.op, CompareOp::Gt));
        assert_eq!(expr.value.as_deref(), Some("2"));
    }

    #[test]
    fn parse_gte() {
        let expr = WhereExpr::parse("year >= 2000").unwrap();
        assert!(matches!(expr.op, CompareOp::Gte));
    }

    #[test]
    fn parse_contains() {
        let expr = WhereExpr::parse("tags contains topic/chinese").unwrap();
        assert_eq!(expr.field, "tags");
        assert!(matches!(expr.op, CompareOp::Contains));
        assert_eq!(expr.value.as_deref(), Some("topic/chinese"));
    }

    #[test]
    fn parse_exists() {
        let expr = WhereExpr::parse("rating exists").unwrap();
        assert_eq!(expr.field, "rating");
        assert!(matches!(expr.op, CompareOp::Exists));
        assert!(expr.value.is_none());
    }

    #[test]
    fn parse_missing() {
        let expr = WhereExpr::parse("rating missing").unwrap();
        assert!(matches!(expr.op, CompareOp::Missing));
    }

    #[test]
    fn parse_matches() {
        let expr = WhereExpr::parse("_name matches ^The").unwrap();
        assert!(matches!(expr.op, CompareOp::Matches));
        assert_eq!(expr.value.as_deref(), Some("^The"));
    }

    #[test]
    fn parse_startswith() {
        let expr = WhereExpr::parse("status startswith to").unwrap();
        assert!(matches!(expr.op, CompareOp::StartsWith));
    }

    #[test]
    fn parse_invalid() {
        assert!(WhereExpr::parse("no operator here").is_err());
        assert!(WhereExpr::parse(" = value").is_err()); // empty field
    }

    // ── Evaluator tests via the public Expr API ─────────────────────────
    //
    // The legacy `WhereExpr::matches` / `matches_all` evaluator was removed
    // alongside the public AST migration. These tests cover the same surface
    // by parsing into `Expr` and evaluating via `evaluate_expr`.

    use crate::record::Value as V;

    fn eval(record: &Record, where_str: &str) -> bool {
        let expr = crate::query::Expr::parse(where_str).expect("parse");
        evaluate_expr(&expr, record, &vault_root(), None)
    }

    #[test]
    fn eval_eq_string() {
        let record = make_record(vec![("status", V::String("to-watch".into()))]);
        assert!(eval(&record, "status = to-watch"));
        assert!(!eval(&record, "status = watched"));
    }

    #[test]
    fn eval_numeric_compare() {
        let record = make_record(vec![("hsk", V::Integer(3))]);
        assert!(eval(&record, "hsk > 2"));
        assert!(!eval(&record, "hsk > 5"));
        assert!(eval(&record, "hsk <= 3"));
    }

    #[test]
    fn eval_list_and_string_contains() {
        let list = make_record(vec![(
            "tags",
            V::List(vec![V::String("topic/chinese".into())]),
        )]);
        assert!(eval(&list, "tags contains topic/chinese"));
        assert!(!eval(&list, "tags contains topic/movies"));

        let s = make_record(vec![("director", V::String("Sam Mendes".into()))]);
        assert!(eval(&s, "director contains Mendes"));
    }

    #[test]
    fn eval_exists_and_missing() {
        let active = make_record(vec![("status", V::String("active".into()))]);
        assert!(eval(&active, "status exists"));
        assert!(!eval(&active, "status missing"));

        let null = make_record(vec![("rating", V::Null)]);
        assert!(!eval(&null, "rating exists"));
        assert!(eval(&null, "rating missing"));

        let absent = make_record(vec![]);
        assert!(!eval(&absent, "rating exists"));
        assert!(eval(&absent, "rating missing"));
    }

    #[test]
    fn eval_matches_regex() {
        let record = make_record(vec![("director", V::String("Sam Mendes".into()))]);
        assert!(eval(&record, "director matches ^Sam"));
        assert!(!eval(&record, "director matches ^Chris"));
    }

    #[test]
    fn eval_virtual_field_name() {
        let r = Record {
            path: PathBuf::from("/vault/notes/Interstellar.md"),
            fields: BTreeMap::new(),
            raw_content: None,
        };
        assert!(eval(&r, "_name = Interstellar"));
    }

    #[test]
    fn eval_virtual_field_folder() {
        let r = Record {
            path: PathBuf::from("/vault/3-Notes/TypeScript.md"),
            fields: BTreeMap::new(),
            raw_content: None,
        };
        assert!(eval(&r, "_folder = 3-Notes"));
    }

    #[test]
    fn eval_negation_via_not_prefix() {
        let r = make_record(vec![(
            "tags",
            V::List(vec![V::String("topic/chinese".into())]),
        )]);
        assert!(!eval(&r, "tags !contains topic/chinese"));
        assert!(eval(&r, "tags !contains topic/movies"));

        let active = make_record(vec![("status", V::String("active".into()))]);
        assert!(!eval(&active, "status !exists"));
        assert!(eval(&active, "rating !exists"));
    }

    #[test]
    fn eval_or_within_one_clause() {
        let to_watch = make_record(vec![("status", V::String("to-watch".into()))]);
        let watching = make_record(vec![("status", V::String("watching".into()))]);
        let watched = make_record(vec![("status", V::String("watched".into()))]);

        assert!(eval(&to_watch, "status = to-watch || status = watching"));
        assert!(eval(&watching, "status = to-watch || status = watching"));
        assert!(!eval(&watched, "status = to-watch || status = watching"));
    }

    #[test]
    fn parse_not_contains_and_not_exists() {
        // The internal `WhereExpr` parser still exposes the negated flag,
        // so spot-check that `!contains` and `!exists` round-trip correctly
        // through `Expr::parse` (the conversion shim wraps in `Expr::Not`).
        use crate::query::Expr as E;
        let e = E::parse("tags !contains topic/movies").unwrap();
        assert!(matches!(e, E::Not(_)));

        let e2 = E::parse("rating !exists").unwrap();
        assert!(matches!(e2, E::Not(_)));
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Query evaluator helpers (public interface for evaluating Expr/Predicate)
// ───────────────────────────────────────────────────────────────────────────

/// Returns true if any node of `expr` references the link graph.
pub fn expr_uses_links(expr: &crate::query::Expr) -> bool {
    use crate::query::Expr;
    match expr {
        Expr::LinksTo(_) | Expr::LinkedFrom(_) => true,
        Expr::Predicate(_) => false,
        Expr::And(es) | Expr::Or(es) => es.iter().any(expr_uses_links),
        Expr::Not(e) => expr_uses_links(e),
    }
}

/// Evaluate an `Expr` against a single record.
pub fn evaluate_expr(
    expr: &crate::query::Expr,
    record: &Record,
    vault_root: &Path,
    link_index: Option<&crate::links::LinkGraph>,
) -> bool {
    use crate::query::{Expr, LinkPredicate};
    match expr {
        Expr::Predicate(p) => evaluate_predicate(p, record, vault_root, link_index),
        Expr::And(es) => es.iter().all(|e| evaluate_expr(e, record, vault_root, link_index)),
        Expr::Or(es) => es.iter().any(|e| evaluate_expr(e, record, vault_root, link_index)),
        Expr::Not(e) => !evaluate_expr(e, record, vault_root, link_index),
        Expr::LinksTo(lp) => match (link_index, lp) {
            (Some(idx), LinkPredicate::Target(name)) => idx
                .outgoing_links(&record.virtual_name())
                .iter()
                .any(|n| *n == name.as_str()),
            (Some(idx), LinkPredicate::Where(inner)) => idx
                .outgoing_links(&record.virtual_name())
                .iter()
                .any(|target_name| {
                    idx.record_by_name(target_name)
                        .map_or(false, |target_record| {
                            evaluate_expr(inner, target_record, vault_root, Some(idx))
                        })
                }),
            (None, _) => false,
        },
        Expr::LinkedFrom(lp) => match (link_index, lp) {
            (Some(idx), LinkPredicate::Target(name)) => idx
                .incoming_links(&record.virtual_name())
                .iter()
                .any(|n| *n == name.as_str()),
            (Some(idx), LinkPredicate::Where(inner)) => idx
                .incoming_links(&record.virtual_name())
                .iter()
                .any(|source_name| {
                    idx.record_by_name(source_name)
                        .map_or(false, |source_record| {
                            evaluate_expr(inner, source_record, vault_root, Some(idx))
                        })
                }),
            (None, _) => false,
        },
    }
}

/// Evaluate a leaf `Predicate` against a single record.
///
/// `link_index` is used to resolve graph virtual fields (`_links`,
/// `_link_count`, `_backlinks`, `_backlink_count`). Pass `None` if the predicate
/// only references frontmatter and non-graph virtual fields.
pub fn evaluate_predicate(
    p: &crate::query::Predicate,
    record: &Record,
    vault_root: &Path,
    link_index: Option<&crate::links::LinkGraph>,
) -> bool {
    use crate::query::{CompareOp, Predicate};
    use crate::record::Value;

    let get = |field: &str| record.get_with_links(field, vault_root, link_index);

    match p {
        Predicate::Equals { field, value } => get(field).as_ref() == Some(value),
        Predicate::Contains { field, value } => match get(field) {
            Some(Value::String(s)) => match value {
                Value::String(v) => s.contains(v.as_str()),
                _ => false,
            },
            Some(Value::List(list)) => list.iter().any(|item| item == value),
            _ => false,
        },
        Predicate::Compare { field, op, value } => {
            let actual = match get(field) {
                Some(v) => v,
                None => return false,
            };
            let ord = compare_values(&actual, value);
            match op {
                CompareOp::Lt => ord == std::cmp::Ordering::Less,
                CompareOp::Le => ord != std::cmp::Ordering::Greater,
                CompareOp::Gt => ord == std::cmp::Ordering::Greater,
                CompareOp::Ge => ord != std::cmp::Ordering::Less,
                CompareOp::Ne => ord != std::cmp::Ordering::Equal,
            }
        }
        Predicate::Matches { field, regex } => match get(field) {
            Some(Value::String(s)) => regex::Regex::new(regex).map_or(false, |re| re.is_match(&s)),
            _ => false,
        },
        Predicate::StartsWith { field, value } => match get(field) {
            Some(Value::String(s)) => s.starts_with(value.as_str()),
            _ => false,
        },
        Predicate::EndsWith { field, value } => match get(field) {
            Some(Value::String(s)) => s.ends_with(value.as_str()),
            _ => false,
        },
        Predicate::Exists { field } => !matches!(get(field), None | Some(Value::Null)),
        Predicate::Missing { field } => matches!(get(field), None | Some(Value::Null)),
    }
}

/// Total order over `Value` for sorting. Mixed types fall back to debug-string
/// comparison so that sort is always stable.
pub fn compare_values(a: &crate::record::Value, b: &crate::record::Value) -> std::cmp::Ordering {
    use crate::record::Value;
    match (a, b) {
        (Value::Integer(x), Value::Integer(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => {
            x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
        }
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        (Value::Null, _) => std::cmp::Ordering::Less,
        (_, Value::Null) => std::cmp::Ordering::Greater,
        _ => format!("{:?}", a).cmp(&format!("{:?}", b)),
    }
}
