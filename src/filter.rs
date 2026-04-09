use std::path::Path;

use regex::Regex;

use crate::error::{Result, VaultdbError};
use crate::record::{FieldValue, Record};

#[derive(Debug, Clone)]
pub enum CompareOp {
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
pub struct WhereExpr {
    pub field: String,
    pub op: CompareOp,
    pub negated: bool,
    /// None for Exists/Missing operators.
    pub value: Option<String>,
}

/// A where clause is one `--where` argument, which may contain OR-ed expressions.
/// Multiple `--where` arguments are AND-ed together.
#[derive(Debug, Clone)]
pub struct WhereClause {
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

    /// Evaluate this clause against a record. Returns true if ANY alternative matches.
    pub fn matches_with_links(
        &self,
        record: &Record,
        vault_root: &Path,
        link_index: Option<&crate::links::LinkIndex>,
    ) -> bool {
        self.alternatives
            .iter()
            .any(|expr| expr.matches_with_links(record, vault_root, link_index))
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

    /// Evaluate this expression against a record.
    pub fn matches(&self, record: &Record, vault_root: &Path) -> bool {
        self.matches_with_links(record, vault_root, None)
    }

    /// Evaluate with optional link index for graph virtual fields.
    pub fn matches_with_links(
        &self,
        record: &Record,
        vault_root: &Path,
        link_index: Option<&crate::links::LinkIndex>,
    ) -> bool {
        let result = self.eval(record, vault_root, link_index);
        if self.negated { !result } else { result }
    }

    fn eval(
        &self,
        record: &Record,
        vault_root: &Path,
        link_index: Option<&crate::links::LinkIndex>,
    ) -> bool {
        let field_val = record.get_with_links(&self.field, vault_root, link_index);

        match self.op {
            CompareOp::Exists => {
                matches!(field_val, Some(v) if !matches!(v, FieldValue::Null))
            }
            CompareOp::Missing => {
                matches!(field_val, None | Some(FieldValue::Null))
            }
            _ => {
                let rhs = match &self.value {
                    Some(v) => v,
                    None => return false,
                };

                match field_val {
                    None | Some(FieldValue::Null) => {
                        // Null field: only matches "= " (empty) or "missing"
                        matches!(self.op, CompareOp::Eq) && rhs.is_empty()
                    }
                    Some(val) => self.compare_value(&val, rhs),
                }
            }
        }
    }

    fn compare_value(&self, lhs: &FieldValue, rhs: &str) -> bool {
        match self.op {
            CompareOp::Contains => lhs.list_contains(rhs),
            CompareOp::StartsWith => lhs.display_value().starts_with(rhs),
            CompareOp::EndsWith => lhs.display_value().ends_with(rhs),
            CompareOp::Matches => match Regex::new(rhs) {
                Ok(re) => re.is_match(&lhs.display_value()),
                Err(_) => false,
            },
            CompareOp::Eq
            | CompareOp::Neq
            | CompareOp::Gt
            | CompareOp::Lt
            | CompareOp::Gte
            | CompareOp::Lte => {
                // Try numeric comparison first
                if let (Some(lhs_f), Ok(rhs_f)) = (lhs.as_float(), rhs.parse::<f64>()) {
                    let result = lhs_f.partial_cmp(&rhs_f);
                    return match self.op {
                        CompareOp::Eq => result == Some(std::cmp::Ordering::Equal),
                        CompareOp::Neq => result != Some(std::cmp::Ordering::Equal),
                        CompareOp::Gt => result == Some(std::cmp::Ordering::Greater),
                        CompareOp::Lt => result == Some(std::cmp::Ordering::Less),
                        CompareOp::Gte => matches!(
                            result,
                            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                        ),
                        CompareOp::Lte => matches!(
                            result,
                            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                        ),
                        _ => unreachable!(),
                    };
                }

                // Fall back to string comparison
                let lhs_str = lhs.display_value();
                match self.op {
                    CompareOp::Eq => lhs_str == rhs,
                    CompareOp::Neq => lhs_str != rhs,
                    CompareOp::Gt => lhs_str.as_str() > rhs,
                    CompareOp::Lt => lhs_str.as_str() < rhs,
                    CompareOp::Gte => lhs_str.as_str() >= rhs,
                    CompareOp::Lte => lhs_str.as_str() <= rhs,
                    _ => unreachable!(),
                }
            }
            CompareOp::Exists | CompareOp::Missing => unreachable!(),
        }
    }
}

/// Evaluate all where-clauses (AND between clauses, OR within each clause).
pub fn matches_all(clauses: &[WhereClause], record: &Record, vault_root: &Path) -> bool {
    matches_all_with_links(clauses, record, vault_root, None)
}

/// Evaluate all where-clauses with link index support.
pub fn matches_all_with_links(
    clauses: &[WhereClause],
    record: &Record,
    vault_root: &Path,
    link_index: Option<&crate::links::LinkIndex>,
) -> bool {
    clauses
        .iter()
        .all(|clause| clause.matches_with_links(record, vault_root, link_index))
}

/// Evaluate WhereExpr slices (for backward compat with relational filters).
pub fn matches_exprs_with_links(
    exprs: &[WhereExpr],
    record: &Record,
    vault_root: &Path,
    link_index: Option<&crate::links::LinkIndex>,
) -> bool {
    exprs
        .iter()
        .all(|expr| expr.matches_with_links(record, vault_root, link_index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::FieldValue;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_record(fields: Vec<(&str, FieldValue)>) -> Record {
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

    #[test]
    fn eval_eq_string() {
        let record = make_record(vec![("status", FieldValue::String("to-watch".into()))]);
        let expr = WhereExpr::parse("status = to-watch").unwrap();
        assert!(expr.matches(&record, &vault_root()));

        let expr2 = WhereExpr::parse("status = watched").unwrap();
        assert!(!expr2.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_neq() {
        let record = make_record(vec![("status", FieldValue::String("draft".into()))]);
        let expr = WhereExpr::parse("status != active").unwrap();
        assert!(expr.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_numeric_gt() {
        let record = make_record(vec![("hsk", FieldValue::Integer(3))]);
        let expr = WhereExpr::parse("hsk > 2").unwrap();
        assert!(expr.matches(&record, &vault_root()));

        let expr2 = WhereExpr::parse("hsk > 5").unwrap();
        assert!(!expr2.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_numeric_lte() {
        let record = make_record(vec![("year", FieldValue::Integer(2019))]);
        let expr = WhereExpr::parse("year <= 2020").unwrap();
        assert!(expr.matches(&record, &vault_root()));

        let expr2 = WhereExpr::parse("year <= 2018").unwrap();
        assert!(!expr2.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_list_contains() {
        let record = make_record(vec![(
            "tags",
            FieldValue::List(vec![
                FieldValue::String("type/concept".into()),
                FieldValue::String("topic/chinese".into()),
            ]),
        )]);
        let expr = WhereExpr::parse("tags contains topic/chinese").unwrap();
        assert!(expr.matches(&record, &vault_root()));

        let expr2 = WhereExpr::parse("tags contains topic/movies").unwrap();
        assert!(!expr2.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_string_contains_substring() {
        let record = make_record(vec![("director", FieldValue::String("Sam Mendes".into()))]);
        let expr = WhereExpr::parse("director contains Mendes").unwrap();
        assert!(expr.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_exists_on_non_null() {
        let record = make_record(vec![("status", FieldValue::String("active".into()))]);
        let expr = WhereExpr::parse("status exists").unwrap();
        assert!(expr.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_exists_on_null() {
        let record = make_record(vec![("rating", FieldValue::Null)]);
        let expr = WhereExpr::parse("rating exists").unwrap();
        assert!(!expr.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_missing_on_absent_field() {
        let record = make_record(vec![]);
        let expr = WhereExpr::parse("rating missing").unwrap();
        assert!(expr.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_missing_on_null() {
        let record = make_record(vec![("rating", FieldValue::Null)]);
        let expr = WhereExpr::parse("rating missing").unwrap();
        assert!(expr.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_matches_regex() {
        let record = make_record(vec![("director", FieldValue::String("Sam Mendes".into()))]);
        let expr = WhereExpr::parse("director matches ^Sam").unwrap();
        assert!(expr.matches(&record, &vault_root()));

        let expr2 = WhereExpr::parse("director matches ^Chris").unwrap();
        assert!(!expr2.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_virtual_field_name() {
        let record = Record {
            path: PathBuf::from("/vault/notes/Interstellar.md"),
            fields: BTreeMap::new(),
            raw_content: None,
        };
        let expr = WhereExpr::parse("_name = Interstellar").unwrap();
        assert!(expr.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_virtual_field_folder() {
        let record = Record {
            path: PathBuf::from("/vault/3-Notes/TypeScript.md"),
            fields: BTreeMap::new(),
            raw_content: None,
        };
        let expr = WhereExpr::parse("_folder = 3-Notes").unwrap();
        assert!(expr.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_multiple_and() {
        let record = make_record(vec![
            (
                "tags",
                FieldValue::List(vec![
                    FieldValue::String("type/concept".into()),
                    FieldValue::String("topic/chinese".into()),
                ]),
            ),
            ("hsk", FieldValue::Integer(1)),
        ]);
        let clauses = vec![
            WhereClause::parse("tags contains topic/chinese").unwrap(),
            WhereClause::parse("hsk = 1").unwrap(),
        ];
        assert!(matches_all(&clauses, &record, &vault_root()));

        let clauses2 = vec![
            WhereClause::parse("tags contains topic/chinese").unwrap(),
            WhereClause::parse("hsk > 3").unwrap(),
        ];
        assert!(!matches_all(&clauses2, &record, &vault_root()));
    }

    // ── NOT tests ─────────────────────────────

    #[test]
    fn parse_not_contains() {
        let expr = WhereExpr::parse("tags !contains topic/movies").unwrap();
        assert!(matches!(expr.op, CompareOp::Contains));
        assert!(expr.negated);
        assert_eq!(expr.value.as_deref(), Some("topic/movies"));
    }

    #[test]
    fn parse_not_exists() {
        let expr = WhereExpr::parse("rating !exists").unwrap();
        assert!(matches!(expr.op, CompareOp::Exists));
        assert!(expr.negated);
    }

    #[test]
    fn eval_not_contains() {
        let record = make_record(vec![(
            "tags",
            FieldValue::List(vec![
                FieldValue::String("type/concept".into()),
                FieldValue::String("topic/chinese".into()),
            ]),
        )]);

        // Chinese note should NOT match "!contains topic/chinese"
        let expr = WhereExpr::parse("tags !contains topic/chinese").unwrap();
        assert!(!expr.matches(&record, &vault_root()));

        // But SHOULD match "!contains topic/movies"
        let expr2 = WhereExpr::parse("tags !contains topic/movies").unwrap();
        assert!(expr2.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_not_exists() {
        let record = make_record(vec![("status", FieldValue::String("active".into()))]);

        // status exists, so !exists should be false
        let expr = WhereExpr::parse("status !exists").unwrap();
        assert!(!expr.matches(&record, &vault_root()));

        // rating doesn't exist, so !exists should be true
        let expr2 = WhereExpr::parse("rating !exists").unwrap();
        assert!(expr2.matches(&record, &vault_root()));
    }

    #[test]
    fn eval_not_startswith() {
        let record = make_record(vec![("status", FieldValue::String("to-watch".into()))]);
        let expr = WhereExpr::parse("status !startswith to").unwrap();
        assert!(!expr.matches(&record, &vault_root()));

        let expr2 = WhereExpr::parse("status !startswith xx").unwrap();
        assert!(expr2.matches(&record, &vault_root()));
    }

    // ── OR tests ──────────────────────────────

    #[test]
    fn parse_or_clause() {
        let clause = WhereClause::parse("status = to-watch || status = watching").unwrap();
        assert_eq!(clause.alternatives.len(), 2);
        assert_eq!(clause.alternatives[0].value.as_deref(), Some("to-watch"));
        assert_eq!(clause.alternatives[1].value.as_deref(), Some("watching"));
    }

    #[test]
    fn eval_or_first_matches() {
        let record = make_record(vec![("status", FieldValue::String("to-watch".into()))]);
        let clause = WhereClause::parse("status = to-watch || status = watching").unwrap();
        assert!(clause.matches_with_links(&record, &vault_root(), None));
    }

    #[test]
    fn eval_or_second_matches() {
        let record = make_record(vec![("status", FieldValue::String("watching".into()))]);
        let clause = WhereClause::parse("status = to-watch || status = watching").unwrap();
        assert!(clause.matches_with_links(&record, &vault_root(), None));
    }

    #[test]
    fn eval_or_none_matches() {
        let record = make_record(vec![("status", FieldValue::String("watched".into()))]);
        let clause = WhereClause::parse("status = to-watch || status = watching").unwrap();
        assert!(!clause.matches_with_links(&record, &vault_root(), None));
    }

    #[test]
    fn eval_and_of_or_clauses() {
        let record = make_record(vec![
            ("status", FieldValue::String("to-watch".into())),
            ("year", FieldValue::Integer(2019)),
        ]);

        let clauses = vec![
            WhereClause::parse("status = to-watch || status = watching").unwrap(),
            WhereClause::parse("year > 2000").unwrap(),
        ];
        assert!(matches_all(&clauses, &record, &vault_root()));

        // OR matches, but AND with year fails
        let clauses2 = vec![
            WhereClause::parse("status = to-watch || status = watching").unwrap(),
            WhereClause::parse("year > 2020").unwrap(),
        ];
        assert!(!matches_all(&clauses2, &record, &vault_root()));
    }
}
