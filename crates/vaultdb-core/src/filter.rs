//! Evaluator for the public [`crate::query::Expr`] AST.
//!
//! Walks `Expr` and `Predicate` trees against a [`Record`] (and an optional
//! [`crate::links::LinkGraph`] for graph predicates) and returns `bool`.
//!
//! The where-DSL **parser** lives in [`crate::dsl`] (pest-driven). This
//! module is now strictly the runtime evaluator.

use std::path::Path;

use crate::record::Record;

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

    // The legacy `WhereExpr::parse` direct-AST tests have been removed
    // along with the legacy parser. Parser-shape tests now live in
    // `crate::dsl::tests`. The evaluator tests below exercise the new
    // pest-driven public API end-to-end (parse + evaluate) and so cover
    // both layers.

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

    // Parser-shape tests for negation forms (`!contains`, `!exists`,
    // word-NOT) live in `crate::dsl::tests` now. The 0.4.0 parser
    // produces structurally normalized output (`!exists` → `Missing`
    // rather than `Not(Exists)`), so this module's tests focus on the
    // evaluator semantics rather than the AST shape.

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
    fn parse_and_binds_tighter_than_or_sql_convention() {
        use crate::query::Expr as E;

        // SQL convention: AND binds tighter than OR. So
        // "a = 1 || b = 2 && c = 3" parses as (a=1) || (b=2 && c=3),
        // i.e. top-level Or with one Predicate arm and one And arm.
        //
        // (Earlier 0.3.0 had the opposite behaviour. The pest-based
        // parser introduced in 0.4.0 fixes this.)
        let e = E::parse("status = draft || status = active && hsk = 1").unwrap();
        match e {
            E::Or(parts) => {
                assert_eq!(parts.len(), 2, "expected two OR alternatives");
                assert!(
                    matches!(
                        parts[0],
                        E::Predicate(crate::query::Predicate::Equals { .. })
                    ),
                    "first arm should be a single Equals predicate, got {:?}",
                    parts[0]
                );
                assert!(
                    matches!(parts[1], E::And(_)),
                    "second arm should be And, got {:?}",
                    parts[1]
                );
            }
            other => panic!("expected Or at top, got {:?}", other),
        }
    }

    #[test]
    fn parse_empty_and_conjunct_errors() {
        use crate::query::Expr as E;

        // "a = 1 && && b = 2" — middle conjunct is empty, should error
        // rather than silently parse as a degenerate clause. The pest
        // grammar can't match `&& &&` anywhere, so this falls out as
        // a real parse error.
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
