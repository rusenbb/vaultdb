//! Evaluator for the public [`crate::query::Expr`] AST.
//!
//! Walks `Expr` and `Predicate` trees against a [`Record`] (and an optional
//! [`crate::links::LinkGraph`] for graph predicates) and returns `bool`.
//! Also hosts the where-DSL parser and its internal `WhereClause`/`WhereExpr`
//! types — the parser produces the internal AST and converts to `Expr` at
//! the public boundary via `WhereClause::to_expr`.

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
                if matches!(op, CompareOp::Matches)
                    && let Some(ref v) = value
                    && Regex::new(v).is_err()
                {
                    return Err(VaultdbError::RegexError {
                        pattern: v.clone(),
                        reason: "invalid regex syntax".into(),
                    });
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

/// Parse a where-DSL string into the public [`crate::query::Expr`] AST.
///
/// Grammar (no parens, no quoting yet):
///   expr   := and_term ( "&&" and_term )*           // AND between terms
///   and_term := or_term  ( "||" or_term )*          // OR between alternatives
///   or_term  := <leaf>                              // a single comparison
///
/// `&&` binds looser than `||` per common SQL convention: the input
/// "a = 1 || b = 2 && c = 3" parses as `(a=1 || b=2) AND (c=3)`. To get
/// the other grouping, split into multiple `--where` arguments at the CLI
/// or build the `Expr` directly via the public AST.
pub(crate) fn parse_where_clause(input: &str) -> Result<crate::query::Expr> {
    let and_parts: Vec<&str> = input.split("&&").collect();
    let mut and_exprs: Vec<crate::query::Expr> = Vec::with_capacity(and_parts.len());
    for and_part in and_parts {
        let trimmed = and_part.trim();
        if trimmed.is_empty() {
            return Err(VaultdbError::InvalidWhereExpr(format!(
                "empty conjunct in: {}",
                input
            )));
        }
        let clause = WhereClause::parse(trimmed)?;
        and_exprs.push(clause.to_expr());
    }
    Ok(match and_exprs.len() {
        0 => crate::query::Expr::And(Vec::new()),
        1 => and_exprs.into_iter().next().unwrap(),
        _ => crate::query::Expr::And(and_exprs),
    })
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
#[allow(clippy::items_after_test_module)]
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

    #[test]
    fn compare_values_cross_numeric_uses_float_scale() {
        use crate::record::Value as V;
        use std::cmp::Ordering;

        // Integer vs Float should not fall through to debug-string ordering
        // (which would put "Float(2.5)" before "Integer(3)" alphabetically).
        assert_eq!(
            compare_values(&V::Integer(3), &V::Float(2.5)),
            Ordering::Greater
        );
        assert_eq!(
            compare_values(&V::Float(2.5), &V::Integer(3)),
            Ordering::Less
        );
        assert_eq!(
            compare_values(&V::Integer(2), &V::Float(2.0)),
            Ordering::Equal
        );
    }

    #[test]
    fn compare_values_cross_type_non_numeric_returns_equal() {
        // The previous implementation fell through to debug-string
        // comparison for type-mismatched pairs, which produced
        // alphabetically-sensible-but-semantically-meaningless results
        // (e.g. comparing `Integer(3)` with `String("3")` would order by
        // their debug representations). The new contract: cross-type
        // non-numeric pairs are Equal, and the predicate-evaluator
        // semantics flow from that.
        use crate::record::Value as V;
        use std::cmp::Ordering;

        // String vs Integer
        assert_eq!(
            compare_values(&V::String("3".into()), &V::Integer(3)),
            Ordering::Equal
        );
        assert_eq!(
            compare_values(&V::Integer(3), &V::String("3".into())),
            Ordering::Equal
        );

        // Bool vs String
        assert_eq!(
            compare_values(&V::Bool(true), &V::String("true".into())),
            Ordering::Equal
        );

        // List vs Integer
        assert_eq!(
            compare_values(&V::List(vec![V::Integer(1)]), &V::Integer(1)),
            Ordering::Equal
        );

        // Map vs anything
        let mut m = std::collections::BTreeMap::new();
        m.insert("k".into(), V::Integer(1));
        assert_eq!(
            compare_values(&V::Map(m.clone()), &V::String("k".into())),
            Ordering::Equal
        );

        // List vs List is currently mixed-type (because the variants are
        // structurally different inside) — this codifies that today they
        // also return Equal. If a future change wants element-wise list
        // comparison, this test will need to change deliberately.
        assert_eq!(
            compare_values(
                &V::List(vec![V::Integer(1), V::Integer(2)]),
                &V::List(vec![V::Integer(1), V::Integer(3)])
            ),
            Ordering::Equal
        );
    }

    #[test]
    fn predicate_compare_with_mismatched_types_returns_consistent_results() {
        // Validate the documented downstream behaviour: with the new
        // Equal-on-mismatch comparator, Predicate::Compare's six op
        // variants produce predictable results.
        use crate::query::{CompareOp, Predicate};
        use crate::record::Value as V;
        use std::collections::BTreeMap;
        use std::path::PathBuf;

        let mut fields = BTreeMap::new();
        fields.insert("year".into(), V::String("2020".into())); // stored as string
        let record = Record {
            path: PathBuf::from("/v/notes/r.md"),
            fields,
            raw_content: None,
        };
        let vault_root = PathBuf::from("/v");

        let mk = |op| Predicate::Compare {
            field: "year".into(),
            op,
            value: V::Integer(2020),
        };

        // Equal -> Lt false, Le true, Gt false, Ge true, Ne false.
        assert!(!evaluate_predicate(
            &mk(CompareOp::Lt),
            &record,
            &vault_root,
            None
        ));
        assert!(evaluate_predicate(
            &mk(CompareOp::Le),
            &record,
            &vault_root,
            None
        ));
        assert!(!evaluate_predicate(
            &mk(CompareOp::Gt),
            &record,
            &vault_root,
            None
        ));
        assert!(evaluate_predicate(
            &mk(CompareOp::Ge),
            &record,
            &vault_root,
            None
        ));
        assert!(!evaluate_predicate(
            &mk(CompareOp::Ne),
            &record,
            &vault_root,
            None
        ));
    }

    #[test]
    fn parse_and_combinator() {
        use crate::query::Expr as E;

        // "a = 1 && b = 2" -> And([Equals a, Equals b])
        let e = E::parse("hsk = 1 && status = active").unwrap();
        match e {
            E::And(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(
                    parts[0],
                    E::Predicate(crate::query::Predicate::Equals { .. })
                ));
                assert!(matches!(
                    parts[1],
                    E::Predicate(crate::query::Predicate::Equals { .. })
                ));
            }
            other => panic!("expected And, got {:?}", other),
        }
    }

    #[test]
    fn parse_and_binds_looser_than_or() {
        use crate::query::Expr as E;

        // "a = 1 || b = 2 && c = 3" should parse as (a=1 || b=2) AND (c=3).
        let e = E::parse("status = draft || status = active && hsk = 1").unwrap();
        match e {
            E::And(parts) => {
                assert_eq!(parts.len(), 2, "expected two AND conjuncts");
                // First conjunct is the OR
                assert!(matches!(parts[0], E::Or(_)), "first should be Or");
                // Second is the single hsk = 1 predicate
                assert!(matches!(
                    parts[1],
                    E::Predicate(crate::query::Predicate::Equals { .. })
                ));
            }
            other => panic!("expected And, got {:?}", other),
        }
    }

    #[test]
    fn parse_empty_and_conjunct_errors() {
        use crate::query::Expr as E;

        // "a = 1 && && b = 2" — middle conjunct is empty, should error rather
        // than silently parse as a degenerate clause.
        let e = E::parse("a = 1 && && b = 2");
        assert!(e.is_err(), "expected parse error for empty conjunct");
    }

    #[test]
    fn expr_uses_links_detects_graph_virtual_field_predicates() {
        // Bug repro: `_link_count > 0` predicates didn't trigger the
        // link-graph build path because expr_uses_links only inspected
        // LinksTo/LinkedFrom variants. Vault::query would skip the graph
        // build, and the predicate would silently return false for every
        // record (record.get_with_links of a graph field returns None
        // when no link_index is provided).
        use crate::query::Expr as E;

        let e = E::parse("_link_count > 0").unwrap();
        assert!(
            expr_uses_links(&e),
            "_link_count > 0 must trigger link-graph build"
        );

        let e2 = E::parse("_backlink_count = 5").unwrap();
        assert!(expr_uses_links(&e2));

        let e3 = E::parse("_backlinks contains React").unwrap();
        assert!(expr_uses_links(&e3));

        // Non-graph predicates still must NOT trigger the build (otherwise
        // every query pays the link-graph cost).
        let e4 = E::parse("status = active").unwrap();
        assert!(!expr_uses_links(&e4));
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Query evaluator helpers (public interface for evaluating Expr/Predicate)
// ───────────────────────────────────────────────────────────────────────────

/// Graph virtual fields whose evaluation requires a `LinkGraph`. Kept as a
/// const so consumers (and tests) can rely on the canonical list.
pub const GRAPH_VIRTUAL_FIELDS: &[&str] =
    &["_links", "_link_count", "_backlinks", "_backlink_count"];

/// Returns true if any node of `expr` references the link graph — either
/// via a [`crate::query::Expr::LinksTo`] / [`crate::query::Expr::LinkedFrom`]
/// variant, or via a predicate whose field is one of the graph virtual
/// fields ([`GRAPH_VIRTUAL_FIELDS`]).
///
/// `Vault::query` and the mutation builders use this to decide whether to
/// load raw_content and build a `LinkGraph` before evaluating the filter.
/// Missing the predicate-field case here is what made `--where "_link_count
/// > 0"` silently return zero results before this fix landed.
pub fn expr_uses_links(expr: &crate::query::Expr) -> bool {
    use crate::query::Expr;
    match expr {
        Expr::LinksTo(_) | Expr::LinkedFrom(_) => true,
        Expr::Predicate(p) => predicate_uses_links(p),
        Expr::And(es) | Expr::Or(es) => es.iter().any(expr_uses_links),
        Expr::Not(e) => expr_uses_links(e),
    }
}

/// Returns true if a leaf predicate references a graph virtual field.
fn predicate_uses_links(p: &crate::query::Predicate) -> bool {
    use crate::query::Predicate;
    let field = match p {
        Predicate::Equals { field, .. }
        | Predicate::Contains { field, .. }
        | Predicate::Compare { field, .. }
        | Predicate::Matches { field, .. }
        | Predicate::StartsWith { field, .. }
        | Predicate::EndsWith { field, .. }
        | Predicate::Exists { field }
        | Predicate::Missing { field } => field,
    };
    GRAPH_VIRTUAL_FIELDS.contains(&field.as_str())
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
        Expr::And(es) => es
            .iter()
            .all(|e| evaluate_expr(e, record, vault_root, link_index)),
        Expr::Or(es) => es
            .iter()
            .any(|e| evaluate_expr(e, record, vault_root, link_index)),
        Expr::Not(e) => !evaluate_expr(e, record, vault_root, link_index),
        Expr::LinksTo(lp) => match (link_index, lp) {
            (Some(idx), LinkPredicate::Target(name)) => idx
                .outgoing_links(&record.virtual_name())
                .contains(&name.as_str()),
            (Some(idx), LinkPredicate::Where(inner)) => idx
                .outgoing_links(&record.virtual_name())
                .iter()
                .any(|target_name| {
                    idx.record_by_name(target_name)
                        .is_some_and(|target_record| {
                            evaluate_expr(inner, target_record, vault_root, Some(idx))
                        })
                }),
            (None, _) => false,
        },
        Expr::LinkedFrom(lp) => match (link_index, lp) {
            (Some(idx), LinkPredicate::Target(name)) => idx
                .incoming_links(&record.virtual_name())
                .contains(&name.as_str()),
            (Some(idx), LinkPredicate::Where(inner)) => idx
                .incoming_links(&record.virtual_name())
                .iter()
                .any(|source_name| {
                    idx.record_by_name(source_name)
                        .is_some_and(|source_record| {
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
            Some(Value::String(s)) => regex::Regex::new(regex).is_ok_and(|re| re.is_match(&s)),
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

/// Total order over `Value` for sorting and `Compare` predicate evaluation.
///
/// ## Comparison rules
///
/// - **Same type** (`Integer/Integer`, `Float/Float`, `String/String`,
///   `Bool/Bool`): compared directly.
/// - **Cross-numeric** (`Integer` vs `Float`): coerced to `f64` and compared
///   on the common scale, so `hsk > 2` and `rating < 8.5` behave the same
///   regardless of whether the YAML parser produced an integer or a float.
/// - **`Null`**: sorts before all other values; two nulls compare equal.
/// - **Anything else** (e.g. `String` vs `Integer`, `Bool` vs `List`,
///   `Map` vs `Map`, `List` vs `List`): treated as `Ordering::Equal` and
///   a `tracing::warn!` is emitted at the call site. This is a deliberate
///   "do not surprise the caller" choice: the previous behaviour fell
///   through to debug-string comparison, which produced
///   alphabetically-sensible-but-semantically-meaningless orderings (e.g.
///   `Integer(3) > String("3")` ordering by debug repr `"Integer(3)"` vs
///   `"\"3\""`). Returning `Equal` makes sort stable across mixed-type
///   pairs and surfaces the actual problem to whoever wired up the schema.
///
/// ## Implications for `Predicate::Compare`
///
/// `Predicate::Compare { op: Lt | Gt, .. }` returns `false` for
/// type-mismatched pairs (since `Equal` is neither `Less` nor `Greater`).
/// `Le | Ge` return `true`. `Ne` returns `false`. Code that needs strict
/// cross-type rejection should validate field types via the schema layer
/// before evaluating filters; this comparator's job is to produce a
/// deterministic total order, not to enforce a type system.
pub fn compare_values(a: &crate::record::Value, b: &crate::record::Value) -> std::cmp::Ordering {
    use crate::record::Value;
    match (a, b) {
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        (Value::Null, _) => std::cmp::Ordering::Less,
        (_, Value::Null) => std::cmp::Ordering::Greater,
        (Value::Integer(x), Value::Integer(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        // Cross-numeric: Integer vs Float — compare on common float scale.
        (Value::Integer(_) | Value::Float(_), Value::Integer(_) | Value::Float(_)) => {
            let af = a.as_float().unwrap_or(0.0);
            let bf = b.as_float().unwrap_or(0.0);
            af.partial_cmp(&bf).unwrap_or(std::cmp::Ordering::Equal)
        }
        // Cross-type non-numeric: Equal + warn. The caller almost certainly
        // has a schema mismatch (e.g. one record's `year` is a string,
        // another's is an int because YAML quoted one and not the other).
        // Surface it; don't silently produce alphabetical-debug-string
        // nonsense.
        _ => {
            tracing::warn!(
                left_type = a.type_name(),
                right_type = b.type_name(),
                "compare_values called on type-mismatched pair; returning Equal. \
                 Validate field types via the schema layer to avoid this."
            );
            std::cmp::Ordering::Equal
        }
    }
}
