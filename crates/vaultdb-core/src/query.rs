//! Public AST types for vault queries.
//!
//! Frontmatter predicates and link-graph predicates are first-class siblings
//! in the same enum (`Expr`), reflecting vaultdb's dual-structure thesis: a
//! markdown vault is *both* a relational table (frontmatter) and a graph
//! (wikilinks), and the query language treats both equally.

use std::str::FromStr;

use crate::error::{Result, VaultdbError};
use crate::record::Value;

/// A composable filter expression. The AST root for vault queries.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum Expr {
    /// A frontmatter or virtual-field predicate.
    Predicate(Predicate),
    /// All sub-expressions must hold.
    And(Vec<Expr>),
    /// At least one sub-expression must hold.
    Or(Vec<Expr>),
    /// Negation of a sub-expression.
    Not(Box<Expr>),
    /// Records that link out to a target matching the inner predicate.
    LinksTo(LinkPredicate),
    /// Records linked from anything matching the inner predicate.
    LinkedFrom(LinkPredicate),
}

/// A leaf predicate over a record's frontmatter or virtual fields.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum Predicate {
    Equals {
        field: String,
        value: Value,
    },
    Contains {
        field: String,
        value: Value,
    },
    Compare {
        field: String,
        op: CompareOp,
        value: Value,
    },
    Matches {
        field: String,
        regex: String,
    },
    StartsWith {
        field: String,
        value: String,
    },
    EndsWith {
        field: String,
        value: String,
    },
    Exists {
        field: String,
    },
    Missing {
        field: String,
    },
}

/// A scalar comparison operator (used by `Predicate::Compare`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum CompareOp {
    Lt,
    Le,
    Gt,
    Ge,
    Ne,
}

/// A predicate over the link graph: either a literal target, or a query into
/// records satisfying a sub-expression. The `Where` variant is what makes
/// joins-via-links possible (e.g., "give me all notes that link to anything
/// tagged `topic/ai`").
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum LinkPredicate {
    Target(String),
    Where(Box<Expr>),
}

/// A complete query: the root expression, optional projection, sort, limit,
/// and the folder to scan.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Query {
    pub folder: String,
    pub filter: Option<Expr>,
    /// `None` means "select all fields".
    pub select: Option<Vec<String>>,
    pub sort: Option<SortKey>,
    pub limit: Option<usize>,
    pub recursive: bool,
}

/// A sort key: which field to sort by, ascending or descending.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SortKey {
    pub field: String,
    pub descending: bool,
}

impl Expr {
    /// Parse a where-DSL string into an `Expr`. Convenience wrapper over
    /// `<Expr as FromStr>::from_str`, so library users have an obvious
    /// discoverable entry point.
    pub fn parse(input: &str) -> Result<Self> {
        input.parse()
    }
}

impl FromStr for Expr {
    type Err = VaultdbError;

    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        // The where-DSL parser lives in `crate::dsl` (pest-driven).
        // It produces a public `Expr` directly. See `where_dsl.pest`
        // for the grammar; precedence is SQL-conventional (AND tighter
        // than OR).
        crate::dsl::parse(input)
    }
}

// Operator overloads for ergonomic programmatic construction.
//
// `a & b`, `a | b`, and `!a` build the corresponding AST nodes. Chains
// of the same operator are flattened (`a & b & c` produces a single
// three-element `And`, not nested two-element ones), so the resulting
// expression mirrors what a hand-written `Expr::And(vec![...])` would.

impl std::ops::BitAnd for Expr {
    type Output = Expr;
    fn bitand(self, rhs: Expr) -> Expr {
        match (self, rhs) {
            (Expr::And(mut a), Expr::And(b)) => {
                a.extend(b);
                Expr::And(a)
            }
            (Expr::And(mut a), other) => {
                a.push(other);
                Expr::And(a)
            }
            (other, Expr::And(mut b)) => {
                b.insert(0, other);
                Expr::And(b)
            }
            (a, b) => Expr::And(vec![a, b]),
        }
    }
}

impl std::ops::BitOr for Expr {
    type Output = Expr;
    fn bitor(self, rhs: Expr) -> Expr {
        match (self, rhs) {
            (Expr::Or(mut a), Expr::Or(b)) => {
                a.extend(b);
                Expr::Or(a)
            }
            (Expr::Or(mut a), other) => {
                a.push(other);
                Expr::Or(a)
            }
            (other, Expr::Or(mut b)) => {
                b.insert(0, other);
                Expr::Or(b)
            }
            (a, b) => Expr::Or(vec![a, b]),
        }
    }
}

impl std::ops::Not for Expr {
    type Output = Expr;
    fn not(self) -> Expr {
        match self {
            // Double negation cancels.
            Expr::Not(inner) => *inner,
            other => Expr::Not(Box::new(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_equals() {
        let e: Expr = "status = active".parse().unwrap();
        match e {
            Expr::Predicate(Predicate::Equals { field, value }) => {
                assert_eq!(field, "status");
                assert_eq!(value, Value::String("active".into()));
            }
            other => panic!("expected Equals, got {:?}", other),
        }
    }

    #[test]
    fn parse_exists() {
        let e: Expr = "title exists".parse().unwrap();
        match e {
            Expr::Predicate(Predicate::Exists { field }) => assert_eq!(field, "title"),
            other => panic!("expected Exists, got {:?}", other),
        }
    }

    #[test]
    fn parse_compare_gt() {
        let e: Expr = "year > 2020".parse().unwrap();
        match e {
            Expr::Predicate(Predicate::Compare { field, op, value }) => {
                assert_eq!(field, "year");
                assert_eq!(op, CompareOp::Gt);
                assert_eq!(value, Value::Integer(2020));
            }
            other => panic!("expected Compare, got {:?}", other),
        }
    }

    #[test]
    fn expr_serializes_via_serde() {
        let e = Expr::Predicate(Predicate::Equals {
            field: "k".into(),
            value: Value::String("v".into()),
        });
        let json = serde_json::to_string(&e).unwrap();
        // Round-trip
        let back: Expr = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn query_struct_construction() {
        let q = Query {
            folder: "notes".into(),
            filter: Some(Expr::Predicate(Predicate::Exists {
                field: "title".into(),
            })),
            select: Some(vec!["_name".into(), "title".into()]),
            sort: Some(SortKey {
                field: "_modified".into(),
                descending: true,
            }),
            limit: Some(10),
            recursive: false,
        };
        assert_eq!(q.folder, "notes");
        assert!(q.filter.is_some());
        assert_eq!(q.limit, Some(10));
    }

    #[test]
    fn link_predicate_target() {
        let lp = LinkPredicate::Target("Foo".into());
        let e = Expr::LinksTo(lp);
        let json = serde_json::to_string(&e).unwrap();
        // Untagged enum representation
        assert!(json.contains("Foo"));
    }

    fn p_eq(field: &str, v: Value) -> Expr {
        Expr::Predicate(Predicate::Equals {
            field: field.into(),
            value: v,
        })
    }

    #[test]
    fn bitand_flattens_chains() {
        let a = p_eq("a", Value::Integer(1));
        let b = p_eq("b", Value::Integer(2));
        let c = p_eq("c", Value::Integer(3));
        let combined = a & b & c;
        match combined {
            Expr::And(parts) => assert_eq!(parts.len(), 3),
            other => panic!("expected flattened And, got {:?}", other),
        }
    }

    #[test]
    fn bitor_flattens_chains() {
        let a = p_eq("a", Value::Integer(1));
        let b = p_eq("b", Value::Integer(2));
        let c = p_eq("c", Value::Integer(3));
        let combined = a | b | c;
        match combined {
            Expr::Or(parts) => assert_eq!(parts.len(), 3),
            other => panic!("expected flattened Or, got {:?}", other),
        }
    }

    #[test]
    fn not_double_negation_cancels() {
        let a = p_eq("a", Value::Integer(1));
        let twice = !!a.clone();
        assert_eq!(a, twice);
    }

    #[test]
    fn mixed_operators_respect_precedence() {
        let a = p_eq("a", Value::Integer(1));
        let b = p_eq("b", Value::Integer(2));
        let c = p_eq("c", Value::Integer(3));
        // & binds tighter than |, so `a | b & c` is `a | (b & c)`.
        let combined = a.clone() | b.clone() & c.clone();
        match combined {
            Expr::Or(parts) if parts.len() == 2 => {
                assert_eq!(parts[0], a);
                assert!(matches!(&parts[1], Expr::And(inner) if inner.len() == 2));
            }
            other => panic!("expected Or-of-(a, And(b,c)), got {:?}", other),
        }
    }

    #[test]
    fn value_from_primitives() {
        let pi = std::f64::consts::PI;
        assert_eq!(Value::from(42_i32), Value::Integer(42));
        assert_eq!(
            Value::from(2_500_000_000_i64),
            Value::Integer(2_500_000_000)
        );
        assert_eq!(Value::from(pi), Value::Float(pi));
        assert_eq!(Value::from(true), Value::Bool(true));
        assert_eq!(Value::from("hi"), Value::String("hi".into()));
        assert_eq!(Value::from(String::from("hi")), Value::String("hi".into()));
        assert_eq!(
            Value::from(vec!["a", "b"]),
            Value::List(vec![Value::String("a".into()), Value::String("b".into())])
        );
    }
}
