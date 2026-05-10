//! Parser for the where-DSL — the human-readable filter syntax used by
//! `Expr::parse`, the CLI's `--where` flag, and `vaultdb-mcp`'s tool
//! parameters. The grammar lives at `src/where_dsl.pest`; this module
//! drives `pest` and lowers the resulting parse tree into the public
//! `crate::query::Expr` AST.
//!
//! ## Grammar summary
//!
//! ```text
//! expr     := and_expr ("||" and_expr)*
//! and_expr := not_expr ("&&" not_expr)*
//! not_expr := "NOT" not_expr | atom
//! atom     := "(" expr ")" | predicate
//!
//! predicate := IN | IS-NULL/exists | matches | binary
//!
//! field IN (a, b, c)              field NOT IN (a, b, c)
//! field IS NULL | IS NOT NULL     field exists | !exists | missing | !missing
//! field matches REGEX             field !matches REGEX
//! field <op> value                where op is = != < > <= >= contains !contains
//!                                 startswith !startswith endswith !endswith
//! ```
//!
//! Precedence (low to high): `||`, `&&`, `NOT`, atomic. SQL-convention:
//! `AND` binds tighter than `OR`, so `a || b && c` parses as
//! `a || (b && c)`. Use parens to override.
//!
//! ## Backwards-compat note
//!
//! Earlier versions of vaultdb-core (≤ 0.3.0) used a string-split parser
//! with a single `||` separator and (in 0.3.0) a `&&` separator that
//! incorrectly bound *looser* than `||`. The pest-based parser introduced
//! in 0.4.0 fixes this to match SQL convention. Existing `--where`
//! strings that relied on the old `&&`-binds-looser behaviour need to
//! add explicit parens; everything else parses identically.

use pest::Parser;
use pest::iterators::{Pair, Pairs};
use pest_derive::Parser as PestParser;

use crate::error::{Result, VaultdbError};
use crate::query::{CompareOp, Expr, Predicate};
use crate::record::Value;

#[derive(PestParser)]
#[grammar = "where_dsl.pest"]
struct WhereParser;

/// Public entry point: parse a where-DSL string into a query `Expr`.
pub(crate) fn parse(input: &str) -> Result<Expr> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(VaultdbError::InvalidWhereExpr(
            "where expression is empty".into(),
        ));
    }

    let mut pairs = WhereParser::parse(Rule::expr_root, trimmed)
        .map_err(|e| VaultdbError::InvalidWhereExpr(format!("{}", e)))?;

    // expr_root is silent (`_`), so the iterator yields the inner
    // `expr` pair directly.
    let expr_pair = pairs
        .next()
        .ok_or_else(|| VaultdbError::InvalidWhereExpr("parser returned no expression".into()))?;
    lower_expr(expr_pair)
}

fn lower_expr(pair: Pair<Rule>) -> Result<Expr> {
    match pair.as_rule() {
        Rule::expr => lower_expr(only_child(pair)?),
        Rule::or_expr => lower_or(pair.into_inner()),
        Rule::and_expr => lower_and(pair.into_inner()),
        Rule::not_expr => lower_not(pair.into_inner()),
        Rule::atom => lower_expr(only_child(pair)?),
        Rule::paren_expr => {
            // paren_expr := "(" ~ expr ~ ")"; the only meaningful child
            // is the inner expr.
            let inner = pair.into_inner().next().ok_or_else(|| {
                VaultdbError::InvalidWhereExpr("empty parenthesised expression".into())
            })?;
            lower_expr(inner)
        }
        Rule::predicate => lower_predicate(only_child(pair)?),
        other => Err(VaultdbError::InvalidWhereExpr(format!(
            "unexpected grammar node: {:?}",
            other
        ))),
    }
}

fn lower_or(pairs: Pairs<Rule>) -> Result<Expr> {
    let exprs: Vec<Expr> = pairs.map(lower_expr).collect::<Result<Vec<_>>>()?;
    Ok(if exprs.len() == 1 {
        exprs.into_iter().next().unwrap()
    } else {
        Expr::Or(exprs)
    })
}

fn lower_and(pairs: Pairs<Rule>) -> Result<Expr> {
    let exprs: Vec<Expr> = pairs.map(lower_expr).collect::<Result<Vec<_>>>()?;
    Ok(if exprs.len() == 1 {
        exprs.into_iter().next().unwrap()
    } else {
        Expr::And(exprs)
    })
}

fn lower_not(mut pairs: Pairs<Rule>) -> Result<Expr> {
    // not_expr := not_word ~ not_expr | atom
    // not_word is atomic (`@`) which means it appears in the pairs.
    // So we either see [atom] (no NOT) or [not_word, not_expr] (one
    // NOT prefix; the inner not_expr handles any further nesting).
    let first = pairs
        .next()
        .ok_or_else(|| VaultdbError::InvalidWhereExpr("empty not_expr".into()))?;
    match first.as_rule() {
        Rule::atom => lower_expr(first),
        Rule::not_word => {
            // The NOT keyword was matched; its operand is the next pair.
            let operand = pairs
                .next()
                .ok_or_else(|| VaultdbError::InvalidWhereExpr("NOT without operand".into()))?;
            let inner = lower_expr(operand)?;
            Ok(Expr::Not(Box::new(inner)))
        }
        other => Err(VaultdbError::InvalidWhereExpr(format!(
            "unexpected child of not_expr: {:?}",
            other
        ))),
    }
}

fn lower_predicate(pair: Pair<Rule>) -> Result<Expr> {
    match pair.as_rule() {
        Rule::in_predicate => lower_in(pair),
        Rule::is_null_predicate => lower_is_null(pair),
        Rule::regex_predicate => lower_regex(pair),
        Rule::binary_predicate => lower_binary(pair),
        Rule::exists_predicate => {
            // Identical to is_null_predicate's exists/missing arms.
            // Currently unreachable from the grammar (is_null_predicate
            // covers all exists/missing forms), but kept for symmetry.
            lower_is_null(pair)
        }
        other => Err(VaultdbError::InvalidWhereExpr(format!(
            "unexpected predicate variant: {:?}",
            other
        ))),
    }
}

fn lower_in(pair: Pair<Rule>) -> Result<Expr> {
    let mut inner = pair.into_inner();
    let field = read_field(next_pair(&mut inner)?)?;
    let in_op = next_pair(&mut inner)?;
    let negated = matches!(only_child(in_op)?.as_rule(), Rule::not_in_kw);
    let value_list_pair = next_pair(&mut inner)?;
    let values: Vec<Value> = value_list_pair
        .into_inner()
        .map(read_value)
        .collect::<Result<Vec<_>>>()?;

    if values.is_empty() {
        return Err(VaultdbError::InvalidWhereExpr(format!(
            "IN list for field '{}' is empty",
            field
        )));
    }

    // `field IN (a, b, c)` desugars to `field = a || field = b || field = c`.
    let alternatives: Vec<Expr> = values
        .into_iter()
        .map(|v| {
            Expr::Predicate(Predicate::Equals {
                field: field.clone(),
                value: v,
            })
        })
        .collect();
    let union = if alternatives.len() == 1 {
        alternatives.into_iter().next().unwrap()
    } else {
        Expr::Or(alternatives)
    };
    Ok(if negated {
        Expr::Not(Box::new(union))
    } else {
        union
    })
}

fn lower_is_null(pair: Pair<Rule>) -> Result<Expr> {
    let mut inner = pair.into_inner();
    let field = read_field(next_pair(&mut inner)?)?;
    let op_pair = next_pair(&mut inner)?;
    let op_kind = only_child(op_pair)?;
    let predicate = match op_kind.as_rule() {
        Rule::is_null_kw | Rule::missing_kw => Predicate::Missing { field },
        Rule::is_not_null_kw | Rule::exists_kw => Predicate::Exists { field },
        Rule::not_missing_kw => Predicate::Exists { field },
        Rule::not_exists_kw => Predicate::Missing { field },
        other => {
            return Err(VaultdbError::InvalidWhereExpr(format!(
                "unexpected null/exists op: {:?}",
                other
            )));
        }
    };
    Ok(Expr::Predicate(predicate))
}

fn lower_regex(pair: Pair<Rule>) -> Result<Expr> {
    let mut inner = pair.into_inner();
    let field = read_field(next_pair(&mut inner)?)?;
    let op_pair = next_pair(&mut inner)?;
    let negated = matches!(only_child(op_pair)?.as_rule(), Rule::not_matches_kw);
    let regex = read_regex_value(next_pair(&mut inner)?)?;
    // Validate the regex at parse time so the user sees a parse error
    // immediately rather than a silent no-match later.
    if regex::Regex::new(&regex).is_err() {
        return Err(VaultdbError::RegexError {
            pattern: regex,
            reason: "invalid regex syntax".into(),
        });
    }
    let pred = Expr::Predicate(Predicate::Matches { field, regex });
    Ok(if negated {
        Expr::Not(Box::new(pred))
    } else {
        pred
    })
}

/// Read a `regex_value` parse pair and produce the regex string.
/// regex_value := quoted_string | regex_unquoted.
fn read_regex_value(pair: Pair<Rule>) -> Result<String> {
    if pair.as_rule() != Rule::regex_value {
        return Err(VaultdbError::InvalidWhereExpr(format!(
            "expected regex value, got {:?}",
            pair.as_rule()
        )));
    }
    let inner = only_child(pair)?;
    match inner.as_rule() {
        Rule::quoted_string => match read_value_from_quoted(inner)? {
            Value::String(s) => Ok(s),
            other => Ok(other.display_value()),
        },
        Rule::regex_unquoted => Ok(inner.as_str().to_string()),
        other => Err(VaultdbError::InvalidWhereExpr(format!(
            "unexpected regex_value variant: {:?}",
            other
        ))),
    }
}

/// Lower a `quoted_string` pair into a `Value::String` with escapes
/// processed. Shared between `read_value` and `read_regex_value`.
fn read_value_from_quoted(pair: Pair<Rule>) -> Result<Value> {
    let qstring = only_child(pair)?;
    let raw = match qstring.as_rule() {
        Rule::dq_string | Rule::sq_string => {
            let s = qstring.as_str();
            s[1..s.len() - 1].to_string()
        }
        other => {
            return Err(VaultdbError::InvalidWhereExpr(format!(
                "unexpected quoted variant: {:?}",
                other
            )));
        }
    };
    Ok(Value::String(unescape(&raw)))
}

fn lower_binary(pair: Pair<Rule>) -> Result<Expr> {
    let mut inner = pair.into_inner();
    let field = read_field(next_pair(&mut inner)?)?;
    let op_str = next_pair(&mut inner)?.as_str().trim();
    let value = read_value(next_pair(&mut inner)?)?;

    let predicate = match op_str {
        "=" => Predicate::Equals {
            field,
            value: coerce_for_equals(value),
        },
        "!=" => Predicate::Compare {
            field,
            op: CompareOp::Ne,
            value: coerce_for_compare(value),
        },
        "<" => Predicate::Compare {
            field,
            op: CompareOp::Lt,
            value: coerce_for_compare(value),
        },
        ">" => Predicate::Compare {
            field,
            op: CompareOp::Gt,
            value: coerce_for_compare(value),
        },
        "<=" => Predicate::Compare {
            field,
            op: CompareOp::Le,
            value: coerce_for_compare(value),
        },
        ">=" => Predicate::Compare {
            field,
            op: CompareOp::Ge,
            value: coerce_for_compare(value),
        },
        "contains" => Predicate::Contains {
            field,
            value: coerce_for_equals(value),
        },
        "!contains" => {
            let inner = Expr::Predicate(Predicate::Contains {
                field,
                value: coerce_for_equals(value),
            });
            return Ok(Expr::Not(Box::new(inner)));
        }
        "startswith" => Predicate::StartsWith {
            field,
            value: stringify_value(value),
        },
        "!startswith" => {
            let inner = Expr::Predicate(Predicate::StartsWith {
                field,
                value: stringify_value(value),
            });
            return Ok(Expr::Not(Box::new(inner)));
        }
        "endswith" => Predicate::EndsWith {
            field,
            value: stringify_value(value),
        },
        "!endswith" => {
            let inner = Expr::Predicate(Predicate::EndsWith {
                field,
                value: stringify_value(value),
            });
            return Ok(Expr::Not(Box::new(inner)));
        }
        other => {
            return Err(VaultdbError::InvalidWhereExpr(format!(
                "unrecognised binary op: {}",
                other
            )));
        }
    };

    Ok(Expr::Predicate(predicate))
}

// ── helpers ────────────────────────────────────────────────────────────────

fn only_child(pair: Pair<Rule>) -> Result<Pair<Rule>> {
    let mut iter = pair.into_inner();
    let first = iter.next().ok_or_else(|| {
        VaultdbError::InvalidWhereExpr("expected one child node, got none".into())
    })?;
    if iter.next().is_some() {
        return Err(VaultdbError::InvalidWhereExpr(
            "expected one child node, got multiple".into(),
        ));
    }
    Ok(first)
}

fn next_pair<'a>(pairs: &mut Pairs<'a, Rule>) -> Result<Pair<'a, Rule>> {
    pairs
        .next()
        .ok_or_else(|| VaultdbError::InvalidWhereExpr("missing required child node".into()))
}

fn read_field(pair: Pair<Rule>) -> Result<String> {
    if pair.as_rule() != Rule::field {
        return Err(VaultdbError::InvalidWhereExpr(format!(
            "expected field name, got {:?}",
            pair.as_rule()
        )));
    }
    Ok(pair.as_str().to_string())
}

fn read_value(pair: Pair<Rule>) -> Result<Value> {
    if pair.as_rule() != Rule::value {
        return Err(VaultdbError::InvalidWhereExpr(format!(
            "expected value, got {:?}",
            pair.as_rule()
        )));
    }
    let inner = only_child(pair)?;
    match inner.as_rule() {
        Rule::quoted_string => read_value_from_quoted(inner),
        Rule::unquoted_value => Ok(Value::String(inner.as_str().to_string())),
        other => Err(VaultdbError::InvalidWhereExpr(format!(
            "unexpected value variant: {:?}",
            other
        ))),
    }
}

/// Process backslash escapes inside quoted strings: `\"`, `\'`, `\\`,
/// `\n`, `\t`. Anything else after `\` is kept literally so unknown
/// escapes don't silently strip backslashes.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Best-effort numeric coercion of a string-typed value, used for ops
/// that compare on a common numeric scale (`=`, `contains`, etc.).
/// String values that parse as i64 → `Value::Integer`; else as f64 →
/// `Value::Float`; else kept as `Value::String`.
fn coerce_for_equals(v: Value) -> Value {
    if let Value::String(ref s) = v {
        if let Ok(i) = s.parse::<i64>() {
            return Value::Integer(i);
        }
        if let Ok(f) = s.parse::<f64>() {
            return Value::Float(f);
        }
    }
    v
}

/// Same coercion as `coerce_for_equals`, but used at the call sites
/// of comparison ops (`<`, `<=`, etc.) to make the intent explicit.
fn coerce_for_compare(v: Value) -> Value {
    coerce_for_equals(v)
}

/// String-only projection of a value, for ops whose RHS is conceptually
/// a string (`startswith`, `endswith`, `matches`).
fn stringify_value(v: Value) -> String {
    match v {
        Value::String(s) => s,
        other => other.display_value(),
    }
}

// LinkPredicate-aware sugar isn't currently part of the where-DSL.
// Consumers that want graph predicates build `Expr::LinksTo` /
// `LinkedFrom` directly via the public AST. A future grammar
// revision could add `links to <name>` etc.; the parser will gain
// matching rules at that time.

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(input: &str) -> Expr {
        parse(input).unwrap_or_else(|e| panic!("expected parse to succeed for {:?}: {}", input, e))
    }

    #[test]
    fn simple_equals() {
        let e = parse_ok("status = active");
        match e {
            Expr::Predicate(Predicate::Equals { field, value }) => {
                assert_eq!(field, "status");
                assert_eq!(value, Value::String("active".into()));
            }
            other => panic!("expected Equals, got {:?}", other),
        }
    }

    #[test]
    fn numeric_coercion_on_equals() {
        let e = parse_ok("year = 2020");
        if let Expr::Predicate(Predicate::Equals { value, .. }) = e {
            assert_eq!(value, Value::Integer(2020));
        } else {
            panic!("expected Equals");
        }
    }

    #[test]
    fn quoted_string_with_spaces() {
        let e = parse_ok(r#"title = "two words""#);
        if let Expr::Predicate(Predicate::Equals { value, .. }) = e {
            assert_eq!(value, Value::String("two words".into()));
        } else {
            panic!("expected Equals");
        }
    }

    #[test]
    fn quoted_string_with_escaped_quote() {
        let e = parse_ok(r#"label = "she said \"hi\"""#);
        if let Expr::Predicate(Predicate::Equals { value, .. }) = e {
            assert_eq!(value, Value::String(r#"she said "hi""#.into()));
        } else {
            panic!("expected Equals");
        }
    }

    #[test]
    fn single_quoted_string() {
        let e = parse_ok("status = 'in review'");
        if let Expr::Predicate(Predicate::Equals { value, .. }) = e {
            assert_eq!(value, Value::String("in review".into()));
        } else {
            panic!("expected Equals");
        }
    }

    #[test]
    fn contains_with_unquoted_path_value() {
        let e = parse_ok("tags contains topic/ai");
        if let Expr::Predicate(Predicate::Contains { field, value }) = e {
            assert_eq!(field, "tags");
            assert_eq!(value, Value::String("topic/ai".into()));
        } else {
            panic!("expected Contains");
        }
    }

    #[test]
    fn negation_via_bang_op() {
        let e = parse_ok("tags !contains topic/movies");
        if let Expr::Not(inner) = e {
            if let Expr::Predicate(Predicate::Contains { .. }) = *inner {
                // ok
            } else {
                panic!("expected Not(Contains)");
            }
        } else {
            panic!("expected Not");
        }
    }

    #[test]
    fn negation_via_not_word() {
        let e = parse_ok("NOT status = draft");
        assert!(matches!(e, Expr::Not(_)));
    }

    #[test]
    fn exists_and_missing() {
        assert!(matches!(
            parse_ok("title exists"),
            Expr::Predicate(Predicate::Exists { .. })
        ));
        assert!(matches!(
            parse_ok("title missing"),
            Expr::Predicate(Predicate::Missing { .. })
        ));
        assert!(matches!(
            parse_ok("title !exists"),
            Expr::Predicate(Predicate::Missing { .. })
        ));
        assert!(matches!(
            parse_ok("title !missing"),
            Expr::Predicate(Predicate::Exists { .. })
        ));
    }

    #[test]
    fn is_null_and_is_not_null() {
        assert!(matches!(
            parse_ok("title IS NULL"),
            Expr::Predicate(Predicate::Missing { .. })
        ));
        assert!(matches!(
            parse_ok("title IS NOT NULL"),
            Expr::Predicate(Predicate::Exists { .. })
        ));
    }

    #[test]
    fn matches_and_not_matches() {
        let e = parse_ok("director matches ^Sam");
        assert!(matches!(e, Expr::Predicate(Predicate::Matches { .. })));
        let e = parse_ok("director !matches ^Sam");
        assert!(matches!(e, Expr::Not(_)));
    }

    #[test]
    fn invalid_regex_at_parse_time() {
        let result = parse("director matches [unclosed");
        assert!(matches!(result, Err(VaultdbError::RegexError { .. })));
    }

    #[test]
    fn comparison_ops_coerce_to_numeric() {
        let e = parse_ok("year > 2020");
        if let Expr::Predicate(Predicate::Compare { op, value, .. }) = e {
            assert_eq!(op, CompareOp::Gt);
            assert_eq!(value, Value::Integer(2020));
        } else {
            panic!("expected Compare");
        }
    }

    #[test]
    fn or_combines_two_clauses() {
        let e = parse_ok("status = draft || status = active");
        match e {
            Expr::Or(parts) => assert_eq!(parts.len(), 2),
            other => panic!("expected Or, got {:?}", other),
        }
    }

    #[test]
    fn and_combines_two_clauses() {
        let e = parse_ok("year > 2020 && status = active");
        match e {
            Expr::And(parts) => assert_eq!(parts.len(), 2),
            other => panic!("expected And, got {:?}", other),
        }
    }

    #[test]
    fn and_binds_tighter_than_or_sql_convention() {
        // "a = 1 || b = 2 && c = 3" should parse as (a = 1) || ((b = 2) && (c = 3)).
        // Top-level is Or; second arm is And. This matches SQL convention
        // and replaces the buggy 0.3.0 behaviour.
        let e = parse_ok("status = draft || status = active && hsk = 1");
        match e {
            Expr::Or(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(
                    matches!(parts[0], Expr::Predicate(Predicate::Equals { .. })),
                    "first arm should be a single Equals predicate, got {:?}",
                    parts[0]
                );
                assert!(
                    matches!(parts[1], Expr::And(_)),
                    "second arm should be And, got {:?}",
                    parts[1]
                );
            }
            other => panic!("expected Or at top level, got {:?}", other),
        }
    }

    #[test]
    fn parens_override_precedence() {
        // Without parens this would be a || (b && c). With parens we
        // get (a || b) && c.
        let e = parse_ok("(status = draft || status = active) && hsk = 1");
        match e {
            Expr::And(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(parts[0], Expr::Or(_)));
            }
            other => panic!("expected And, got {:?}", other),
        }
    }

    #[test]
    fn nested_parens() {
        let e = parse_ok("((status = draft))");
        assert!(matches!(e, Expr::Predicate(Predicate::Equals { .. })));
    }

    #[test]
    fn in_predicate_desugars_to_or() {
        let e = parse_ok("status IN (draft, active, pending)");
        match e {
            Expr::Or(parts) => {
                assert_eq!(parts.len(), 3);
                for p in &parts {
                    assert!(matches!(p, Expr::Predicate(Predicate::Equals { .. })));
                }
            }
            other => panic!("expected Or, got {:?}", other),
        }
    }

    #[test]
    fn in_predicate_with_quoted_values() {
        let e = parse_ok(r#"status IN ("in review", "needs follow-up")"#);
        match e {
            Expr::Or(parts) => {
                assert_eq!(parts.len(), 2);
                if let Expr::Predicate(Predicate::Equals { value, .. }) = &parts[0] {
                    assert_eq!(*value, Value::String("in review".into()));
                }
            }
            other => panic!("expected Or, got {:?}", other),
        }
    }

    #[test]
    fn in_predicate_single_value_does_not_or_wrap() {
        let e = parse_ok("status IN (draft)");
        assert!(matches!(e, Expr::Predicate(Predicate::Equals { .. })));
    }

    #[test]
    fn not_in_predicate_is_negated() {
        let e = parse_ok("status NOT IN (draft, archived)");
        assert!(matches!(e, Expr::Not(_)));
    }

    #[test]
    fn empty_input_errors() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }

    #[test]
    fn unbalanced_parens_error() {
        assert!(parse("(status = active").is_err());
        assert!(parse("status = active)").is_err());
    }

    #[test]
    fn unknown_op_errors() {
        assert!(parse("status :- active").is_err());
    }

    #[test]
    fn deeply_nested_combinator_tree() {
        let e = parse_ok("((a = 1 || b = 2) && (c = 3 || d = 4)) || NOT (e contains foo)");
        // We don't pin the structure deeply; just verify parse works
        // and the top-level is an Or with at least one And and one Not.
        match e {
            Expr::Or(parts) => {
                assert_eq!(parts.len(), 2);
            }
            other => panic!("expected Or, got {:?}", other),
        }
    }
}
