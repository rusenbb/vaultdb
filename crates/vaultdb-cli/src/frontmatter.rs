use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Result, VaultdbError};
use crate::record::{FieldValue, Record};

/// Extract the raw frontmatter string from markdown content.
///
/// Returns `(frontmatter_text, body_start_byte_offset)` or `None` if
/// the file has no valid frontmatter delimiters.
pub fn extract_frontmatter(content: &str) -> Option<(&str, usize)> {
    // Must start with "---" followed by a newline
    let content = if content.starts_with("\u{feff}") {
        // Skip BOM if present
        &content[3..]
    } else {
        content
    };

    if !content.starts_with("---") {
        return None;
    }

    let after_opening = &content[3..];
    if !after_opening.starts_with('\n') && !after_opening.starts_with("\r\n") {
        return None;
    }

    let search_start = if after_opening.starts_with("\r\n") {
        5 // "---\r\n"
    } else {
        4 // "---\n"
    };

    // Check for empty frontmatter: closing --- immediately after opening
    let rest = &content[search_start..];
    if rest.starts_with("---\n") {
        return Some(("", search_start + 4));
    }
    if rest.starts_with("---\r\n") {
        return Some(("", search_start + 5));
    }
    if rest == "---" {
        return Some(("", search_start + 3));
    }

    // Find closing "---" on its own line (preceded by a newline)
    // Try all line-ending variants and pick the earliest match
    let closing_patterns = ["\n---\n", "\n---\r\n"];
    let mut best: Option<(usize, usize)> = None; // (newline_pos, after_delimiter)

    for pattern in &closing_patterns {
        if let Some(pos) = rest.find(pattern) {
            let abs_pos = search_start + pos;
            let delimiter_end = abs_pos + pattern.len();
            match best {
                None => best = Some((abs_pos, delimiter_end)),
                Some((prev, _)) if abs_pos < prev => best = Some((abs_pos, delimiter_end)),
                _ => {}
            }
        }
    }

    // Also check for closing --- at end of file (no trailing newline)
    if let Some(pos) = rest.find("\n---") {
        let abs_pos = search_start + pos;
        // Make sure this is actually end-of-content or followed by only a newline
        let after = abs_pos + 4; // past "\n---"
        if after == content.len() {
            match best {
                None => best = Some((abs_pos, after)),
                Some((prev, _)) if abs_pos < prev => best = Some((abs_pos, after)),
                _ => {}
            }
        }
    }

    let (newline_pos, body_start) = best?;

    // Include content up to (but not including) the \n before closing ---
    let fm_text = &content[search_start..newline_pos];
    Some((fm_text, body_start))
}

/// Parse a frontmatter YAML string into a field map.
pub fn parse_frontmatter(yaml_text: &str) -> Result<BTreeMap<String, FieldValue>> {
    if yaml_text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }

    let value: serde_yaml::Value = serde_yaml::from_str(yaml_text)?;

    match value {
        serde_yaml::Value::Mapping(map) => {
            let mut fields = BTreeMap::new();
            for (k, v) in map {
                if let serde_yaml::Value::String(key) = k {
                    fields.insert(key, yaml_to_field_value(v));
                }
            }
            Ok(fields)
        }
        serde_yaml::Value::Null => Ok(BTreeMap::new()),
        _ => Err(VaultdbError::InvalidFrontmatter {
            file: "<unknown>".into(),
            reason: "frontmatter is not a YAML mapping".into(),
        }),
    }
}

/// Convert a serde_yaml::Value to our FieldValue enum.
fn yaml_to_field_value(value: serde_yaml::Value) -> FieldValue {
    match value {
        serde_yaml::Value::Null => FieldValue::Null,
        serde_yaml::Value::Bool(b) => FieldValue::Bool(b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                FieldValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                FieldValue::Float(f)
            } else {
                FieldValue::String(n.to_string())
            }
        }
        serde_yaml::Value::String(s) => FieldValue::String(s),
        serde_yaml::Value::Sequence(seq) => {
            FieldValue::List(seq.into_iter().map(yaml_to_field_value).collect())
        }
        serde_yaml::Value::Mapping(map) => {
            let mut fields = BTreeMap::new();
            for (k, v) in map {
                let key = match k {
                    serde_yaml::Value::String(s) => s,
                    other => other.as_str().unwrap_or("").to_string(),
                };
                fields.insert(key, yaml_to_field_value(v));
            }
            FieldValue::Map(fields)
        }
        serde_yaml::Value::Tagged(tagged) => yaml_to_field_value(tagged.value),
    }
}

/// Load a Record from a file path (frontmatter only, no raw content).
pub fn load_record(path: &Path) -> Result<Record> {
    let content = std::fs::read_to_string(path)?;
    let fields = match extract_frontmatter(&content) {
        Some((fm_text, _)) => {
            parse_frontmatter(fm_text).map_err(|_| VaultdbError::InvalidFrontmatter {
                file: path.display().to_string(),
                reason: "failed to parse YAML".into(),
            })?
        }
        None => {
            return Err(VaultdbError::NoFrontmatter(path.display().to_string()));
        }
    };

    Ok(Record {
        path: path.to_path_buf(),
        fields,
        raw_content: None,
    })
}

/// Load a Record with raw content preserved (for write operations).
pub fn load_record_with_content(path: &Path) -> Result<Record> {
    let content = std::fs::read_to_string(path)?;
    let fields = match extract_frontmatter(&content) {
        Some((fm_text, _)) => {
            parse_frontmatter(fm_text).map_err(|_| VaultdbError::InvalidFrontmatter {
                file: path.display().to_string(),
                reason: "failed to parse YAML".into(),
            })?
        }
        None => {
            return Err(VaultdbError::NoFrontmatter(path.display().to_string()));
        }
    };

    Ok(Record {
        path: path.to_path_buf(),
        fields,
        raw_content: Some(content),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_simple_frontmatter() {
        let content = "---\ntitle: hello\n---\nBody text here.\n";
        let (fm, body_start) = extract_frontmatter(content).unwrap();
        assert_eq!(fm, "title: hello");
        assert_eq!(&content[body_start..], "Body text here.\n");
    }

    #[test]
    fn extract_no_frontmatter() {
        let content = "# Just a heading\n\nSome body.\n";
        assert!(extract_frontmatter(content).is_none());
    }

    #[test]
    fn extract_empty_frontmatter() {
        let content = "---\n---\nBody.\n";
        let (fm, _) = extract_frontmatter(content).unwrap();
        assert_eq!(fm, "");
    }

    #[test]
    fn extract_task_file_no_frontmatter() {
        let content = "## Today's Tasks\n- [ ] Study OS\n";
        assert!(extract_frontmatter(content).is_none());
    }

    #[test]
    fn parse_movie_frontmatter() {
        let yaml = r#"aliases:
tags:
  - type/leaf
  - topic/movies
  - source/video
  - genre/drama
  - genre/war
  - director/sam-mendes
status: to-watch
rating:
director: Sam Mendes
year: 2019
related-to:
"#;
        let fields = parse_frontmatter(yaml).unwrap();

        assert_eq!(
            fields.get("status"),
            Some(&FieldValue::String("to-watch".into()))
        );
        assert_eq!(fields.get("rating"), Some(&FieldValue::Null));
        assert_eq!(
            fields.get("director"),
            Some(&FieldValue::String("Sam Mendes".into()))
        );
        assert_eq!(fields.get("year"), Some(&FieldValue::Integer(2019)));

        // Tags should be a list
        match fields.get("tags") {
            Some(FieldValue::List(tags)) => {
                assert_eq!(tags.len(), 6);
                assert_eq!(tags[0], FieldValue::String("type/leaf".into()));
                assert_eq!(tags[3], FieldValue::String("genre/drama".into()));
            }
            other => panic!("expected List for tags, got {:?}", other),
        }
    }

    #[test]
    fn parse_chinese_vocab_frontmatter() {
        let yaml = r#"aliases:
- kuài
tags:
- type/concept
- topic/chinese
- source/self-study
pinyin: kuài
anlam: hızlı
tür: sifat
hsk: 1
kaliplar:
- kalip: 快乐
  pinyin: kuàilè
  anlam: mutlu, neşeli
- kalip: 快要
  pinyin: kuàiyào
  anlam: yakında, az kaldı
ornekler:
- cumle: 他跑得很快。
  pinyin: Tā pǎo de hěn kuài.
  anlam: O çok hızlı koşuyor.
related-to:
"#;
        let fields = parse_frontmatter(yaml).unwrap();

        assert_eq!(
            fields.get("pinyin"),
            Some(&FieldValue::String("kuài".into()))
        );
        assert_eq!(
            fields.get("anlam"),
            Some(&FieldValue::String("hızlı".into()))
        );
        assert_eq!(fields.get("hsk"), Some(&FieldValue::Integer(1)));

        // kaliplar should be a list of maps
        match fields.get("kaliplar") {
            Some(FieldValue::List(items)) => {
                assert_eq!(items.len(), 2);
                match &items[0] {
                    FieldValue::Map(m) => {
                        assert_eq!(m.get("kalip"), Some(&FieldValue::String("快乐".into())));
                        assert_eq!(m.get("pinyin"), Some(&FieldValue::String("kuàilè".into())));
                    }
                    other => panic!("expected Map in kaliplar, got {:?}", other),
                }
            }
            other => panic!("expected List for kaliplar, got {:?}", other),
        }
    }

    #[test]
    fn parse_wiki_links_in_frontmatter() {
        let yaml = r#"aliases:
tags:
  - type/leaf
related-to:
  - "[[2FA Setup - Yubi]]"
  - "[[Watchlist]]"
"#;
        let fields = parse_frontmatter(yaml).unwrap();

        match fields.get("related-to") {
            Some(FieldValue::List(items)) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0], FieldValue::String("[[2FA Setup - Yubi]]".into()));
            }
            other => panic!("expected List for related-to, got {:?}", other),
        }
    }

    #[test]
    fn parse_null_aliases_and_related_to() {
        let yaml = "aliases:\ntags:\n  - type/concept\nrelated-to:\n";
        let fields = parse_frontmatter(yaml).unwrap();
        assert_eq!(fields.get("aliases"), Some(&FieldValue::Null));
        assert_eq!(fields.get("related-to"), Some(&FieldValue::Null));
    }

    #[test]
    fn parse_empty_frontmatter_string() {
        let fields = parse_frontmatter("").unwrap();
        assert!(fields.is_empty());
    }

    #[test]
    fn parse_only_whitespace_frontmatter() {
        let fields = parse_frontmatter("   \n  \n").unwrap();
        assert!(fields.is_empty());
    }

    #[test]
    fn roundtrip_full_file_extraction() {
        let content = "---\naliases:\ntags:\n- type/concept\n- topic/chinese\npinyin: kuài\n---\n\n# 快 (kuài)\n\nBody text.\n";
        let (fm, body_start) = extract_frontmatter(content).unwrap();
        let fields = parse_frontmatter(fm).unwrap();

        assert_eq!(
            fields.get("pinyin"),
            Some(&FieldValue::String("kuài".into()))
        );
        assert!(content[body_start..].contains("Body text."));
    }
}
