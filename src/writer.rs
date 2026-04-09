use crate::error::{Result, VaultdbError};

/// Describes a single change made to a file.
#[derive(Debug)]
pub enum ChangeDescription {
    SetField {
        field: String,
        old_value: String,
        new_value: String,
    },
    UnsetField {
        field: String,
        old_value: String,
    },
    AddTag {
        tag: String,
    },
    RemoveTag {
        tag: String,
    },
}

impl std::fmt::Display for ChangeDescription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeDescription::SetField {
                field,
                old_value,
                new_value,
            } => write!(f, "set {} = {} (was: {})", field, new_value, old_value),
            ChangeDescription::UnsetField { field, old_value } => {
                write!(f, "unset {} (was: {})", field, old_value)
            }
            ChangeDescription::AddTag { tag } => write!(f, "add tag: {}", tag),
            ChangeDescription::RemoveTag { tag } => write!(f, "remove tag: {}", tag),
        }
    }
}

/// Result of a write operation on a single file.
pub struct WriteResult {
    pub path: std::path::PathBuf,
    pub original_content: String,
    pub modified_content: String,
    pub changes: Vec<ChangeDescription>,
}

/// Split file content into frontmatter lines and body.
/// Returns (frontmatter_lines_including_delimiters, body_str).
fn split_frontmatter(content: &str) -> Result<(Vec<&str>, &str)> {
    let lines: Vec<&str> = content.lines().collect();

    if lines.is_empty() || lines[0].trim() != "---" {
        return Err(VaultdbError::NoFrontmatter("content".into()));
    }

    // Find closing ---
    let close_idx = lines[1..]
        .iter()
        .position(|l| l.trim() == "---")
        .map(|i| i + 1); // offset by 1 because we started from lines[1..]

    match close_idx {
        Some(idx) => {
            let fm_lines = &lines[..=idx];
            // Body starts after the closing --- line
            // We need to find the byte offset of the body
            let mut byte_offset = 0;
            for (i, line) in content.lines().enumerate() {
                byte_offset += line.len();
                // Account for the newline character
                if byte_offset < content.len() {
                    if content.as_bytes().get(byte_offset) == Some(&b'\r') {
                        byte_offset += 1; // \r
                    }
                    if byte_offset < content.len() {
                        byte_offset += 1; // \n
                    }
                }
                if i == idx {
                    break;
                }
            }
            let body = &content[byte_offset..];
            Ok((fm_lines.to_vec(), body))
        }
        None => Err(VaultdbError::NoFrontmatter("content".into())),
    }
}

/// Detect the indentation used for list items under a key.
/// Returns the prefix string (e.g., "  - " or "- ").
fn detect_list_indent(fm_lines: &[&str], key_line_idx: usize) -> String {
    // Look at the line after the key line
    for i in (key_line_idx + 1)..fm_lines.len() {
        let line = fm_lines[i];
        let trimmed = line.trim();

        // Stop if we hit another top-level key or delimiter
        if trimmed == "---"
            || (!line.starts_with(' ') && !line.starts_with('-') && trimmed.contains(':'))
        {
            break;
        }

        if trimmed.starts_with("- ") || trimmed == "-" {
            // Return the actual prefix including whitespace
            let dash_pos = line.find('-').unwrap();
            let prefix = &line[..dash_pos];
            return format!("{}- ", prefix);
        }
    }
    // Default: 2-space indent
    "  - ".to_string()
}

/// Find the line index of a top-level key in frontmatter lines (between delimiters).
fn find_key_line(fm_lines: &[&str], key: &str) -> Option<usize> {
    let patterns = [format!("{}:", key), format!("{} :", key)];
    for (i, line) in fm_lines.iter().enumerate() {
        if i == 0 || line.trim() == "---" {
            continue; // skip delimiters
        }
        let trimmed = line.trim_start();
        for pattern in &patterns {
            if trimmed.starts_with(pattern) {
                // Make sure we matched the full key, not a prefix
                let after = &trimmed[pattern.len()..];
                if after.is_empty() || after.starts_with(' ') || after.starts_with('\t') {
                    return Some(i);
                }
            }
        }
    }
    None
}

/// Determine how many lines a field spans (including nested list/map items).
fn field_extent(fm_lines: &[&str], key_line_idx: usize) -> usize {
    let key_line = fm_lines[key_line_idx];
    let key_indent = key_line.len() - key_line.trim_start().len();

    // Check if the key has an inline value (not a list/map)
    let after_colon = key_line.trim_start();
    if let Some(colon_pos) = after_colon.find(':') {
        let value_part = after_colon[colon_pos + 1..].trim();
        if !value_part.is_empty() && !value_part.starts_with('[') && !value_part.starts_with('{') {
            // Inline scalar value — single line
            return 1;
        }
    }

    let mut extent = 1;
    for i in (key_line_idx + 1)..fm_lines.len() {
        let line = fm_lines[i];
        let trimmed = line.trim();

        // Stop at closing delimiter
        if trimmed == "---" {
            break;
        }

        // Empty line ends the field
        if trimmed.is_empty() {
            break;
        }

        let line_indent = line.len() - line.trim_start().len();

        // If this line is at the same or lesser indentation and doesn't start with '-',
        // it's a new top-level key
        if line_indent <= key_indent && !trimmed.starts_with('-') {
            break;
        }

        // Lines starting with '-' at the same indent level are list items of this key
        if line_indent == key_indent && trimmed.starts_with('-') {
            extent += 1;
            continue;
        }

        // Indented lines are continuations
        if line_indent > key_indent {
            extent += 1;
            continue;
        }

        break;
    }
    extent
}

/// Check if a field line uses flow-style list syntax: `key: [a, b, c]`
fn is_flow_style_list(line: &str) -> bool {
    if let Some(colon_pos) = line.find(':') {
        let value = line[colon_pos + 1..].trim();
        value.starts_with('[') && value.ends_with(']')
    } else {
        false
    }
}

/// Check if a field line uses a multiline scalar indicator: `key: |` or `key: >`
fn is_multiline_scalar(line: &str) -> bool {
    if let Some(colon_pos) = line.find(':') {
        let value = line[colon_pos + 1..].trim();
        value == "|"
            || value == ">"
            || value == "|+"
            || value == "|-"
            || value == ">+"
            || value == ">-"
    } else {
        false
    }
}

/// Quote a YAML value if it contains special characters.
pub fn quote_value(value: &str) -> String {
    yaml_quote_value(value)
}

fn yaml_quote_value(value: &str) -> String {
    let needs_quoting = value.contains(':')
        || value.contains('#')
        || value.contains('[')
        || value.contains(']')
        || value.contains('{')
        || value.contains('}')
        || value.contains('\'')
        || value.contains('"')
        || value.contains('&')
        || value.contains('*')
        || value.contains('!')
        || value.contains('|')
        || value.contains('>')
        || value.contains('%')
        || value.contains('@')
        || value.starts_with(' ')
        || value.ends_with(' ')
        || value.starts_with('-')
        || value.starts_with('?');

    if needs_quoting {
        if value.contains('\'') {
            format!("\"{}\"", value.replace('"', "\\\""))
        } else {
            format!("'{}'", value)
        }
    } else {
        value.to_string()
    }
}

/// Set a scalar field to a new value in the frontmatter.
pub fn set_field(content: &str, key: &str, value: &str) -> Result<(String, ChangeDescription)> {
    let (fm_lines, body) = split_frontmatter(content)?;
    let quoted_value = yaml_quote_value(value);

    if let Some(key_idx) = find_key_line(&fm_lines, key) {
        let extent = field_extent(&fm_lines, key_idx);
        if extent > 1 {
            return Err(VaultdbError::InvalidFrontmatter {
                file: String::new(),
                reason: format!(
                    "field '{}' is a complex type (list/map). Use --unset first, then re-add.",
                    key
                ),
            });
        }

        if is_flow_style_list(fm_lines[key_idx]) {
            return Err(VaultdbError::InvalidFrontmatter {
                file: String::new(),
                reason: format!(
                    "field '{}' uses flow-style YAML (e.g., [a, b]). Use --unset first, then re-add.",
                    key
                ),
            });
        }

        if is_multiline_scalar(fm_lines[key_idx]) {
            return Err(VaultdbError::InvalidFrontmatter {
                file: String::new(),
                reason: format!(
                    "field '{}' uses a multiline scalar (| or >). Use --unset first, then re-add.",
                    key
                ),
            });
        }

        let old_line = fm_lines[key_idx];
        // Extract old value for the change description
        let old_value = old_line
            .find(':')
            .map(|pos| old_line[pos + 1..].trim())
            .unwrap_or("")
            .to_string();

        let new_line = format!("{}: {}", key, quoted_value);

        let mut result_lines: Vec<String> = Vec::new();
        for (i, line) in fm_lines.iter().enumerate() {
            if i == key_idx {
                result_lines.push(new_line.clone());
            } else {
                result_lines.push(line.to_string());
            }
        }

        let change = ChangeDescription::SetField {
            field: key.to_string(),
            old_value,
            new_value: value.to_string(),
        };

        Ok((reassemble(&result_lines, body, content), change))
    } else {
        // Key doesn't exist — insert before closing ---
        let mut result_lines: Vec<String> = Vec::new();
        for (i, line) in fm_lines.iter().enumerate() {
            if i == fm_lines.len() - 1 && line.trim() == "---" {
                result_lines.push(format!("{}: {}", key, quoted_value));
            }
            result_lines.push(line.to_string());
        }

        let change = ChangeDescription::SetField {
            field: key.to_string(),
            old_value: String::new(),
            new_value: value.to_string(),
        };

        Ok((reassemble(&result_lines, body, content), change))
    }
}

/// Remove a field entirely from the frontmatter.
pub fn unset_field(content: &str, key: &str) -> Result<(String, ChangeDescription)> {
    let (fm_lines, body) = split_frontmatter(content)?;

    let key_idx =
        find_key_line(&fm_lines, key).ok_or_else(|| VaultdbError::InvalidFrontmatter {
            file: String::new(),
            reason: format!("field '{}' not found", key),
        })?;

    let extent = field_extent(&fm_lines, key_idx);
    let old_value = fm_lines[key_idx]
        .find(':')
        .map(|pos| fm_lines[key_idx][pos + 1..].trim())
        .unwrap_or("")
        .to_string();

    let mut result_lines: Vec<String> = Vec::new();
    for (i, line) in fm_lines.iter().enumerate() {
        if i >= key_idx && i < key_idx + extent {
            continue; // skip this field's lines
        }
        result_lines.push(line.to_string());
    }

    let change = ChangeDescription::UnsetField {
        field: key.to_string(),
        old_value,
    };

    Ok((reassemble(&result_lines, body, content), change))
}

/// Add a tag to the tags list.
pub fn add_tag(content: &str, tag: &str) -> Result<(String, ChangeDescription)> {
    let (fm_lines, body) = split_frontmatter(content)?;

    let key_idx =
        find_key_line(&fm_lines, "tags").ok_or_else(|| VaultdbError::InvalidFrontmatter {
            file: String::new(),
            reason: "no 'tags' field found".into(),
        })?;

    if is_flow_style_list(fm_lines[key_idx]) {
        return Err(VaultdbError::InvalidFrontmatter {
            file: String::new(),
            reason: "tags field uses flow-style YAML (e.g., tags: [a, b]). Convert to block-style first.".into(),
        });
    }

    let indent_prefix = detect_list_indent(&fm_lines, key_idx);
    let extent = field_extent(&fm_lines, key_idx);
    let insert_after = key_idx + extent - 1; // last line of the tags section

    let new_tag_line = format!("{}{}", indent_prefix, tag);

    let mut result_lines: Vec<String> = Vec::new();
    for (i, line) in fm_lines.iter().enumerate() {
        result_lines.push(line.to_string());
        if i == insert_after {
            result_lines.push(new_tag_line.clone());
        }
    }

    let change = ChangeDescription::AddTag {
        tag: tag.to_string(),
    };

    Ok((reassemble(&result_lines, body, content), change))
}

/// Remove a tag from the tags list.
pub fn remove_tag(content: &str, tag: &str) -> Result<(String, ChangeDescription)> {
    let (fm_lines, body) = split_frontmatter(content)?;

    let key_idx =
        find_key_line(&fm_lines, "tags").ok_or_else(|| VaultdbError::InvalidFrontmatter {
            file: String::new(),
            reason: "no 'tags' field found".into(),
        })?;

    if is_flow_style_list(fm_lines[key_idx]) {
        return Err(VaultdbError::InvalidFrontmatter {
            file: String::new(),
            reason: "tags field uses flow-style YAML (e.g., tags: [a, b]). Convert to block-style first.".into(),
        });
    }

    let extent = field_extent(&fm_lines, key_idx);

    // Find the tag line within the tags section
    let mut tag_line_idx = None;
    for i in (key_idx + 1)..(key_idx + extent) {
        let trimmed = fm_lines[i].trim();
        let tag_value = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        if tag_value == tag {
            tag_line_idx = Some(i);
            break;
        }
    }

    let tag_line_idx = tag_line_idx.ok_or_else(|| VaultdbError::InvalidFrontmatter {
        file: String::new(),
        reason: format!("tag '{}' not found in tags list", tag),
    })?;

    let mut result_lines: Vec<String> = Vec::new();
    for (i, line) in fm_lines.iter().enumerate() {
        if i == tag_line_idx {
            continue;
        }
        result_lines.push(line.to_string());
    }

    let change = ChangeDescription::RemoveTag {
        tag: tag.to_string(),
    };

    Ok((reassemble(&result_lines, body, content), change))
}

/// Reassemble a file from frontmatter lines and body, preserving the original line ending style.
fn reassemble(fm_lines: &[String], body: &str, original: &str) -> String {
    let line_ending = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    let mut result = fm_lines.join(line_ending);
    result.push_str(line_ending);
    result.push_str(body);
    result
}

/// Write a WriteResult to disk.
pub fn apply(result: &WriteResult) -> std::io::Result<()> {
    std::fs::write(&result.path, &result.modified_content)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOVIE_FILE: &str = "\
---
aliases:
tags:
  - type/leaf
  - topic/movies
  - source/video
  - genre/drama
status: to-watch
rating:
director: Sam Mendes
year: 2019
related-to:
---

Part of [[Watchlist]]
";

    const CHINESE_FILE: &str = "\
---
aliases:
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
ornekler:
- cumle: 他跑得很快。
  pinyin: Tā pǎo de hěn kuài.
  anlam: O çok hızlı koşuyor.
related-to:
---

# 快 (kuài) — hızlı

Body text.
";

    #[test]
    fn set_existing_scalar_field() {
        let (result, change) = set_field(MOVIE_FILE, "status", "watched").unwrap();
        assert!(result.contains("status: watched"));
        assert!(!result.contains("to-watch"));
        // Body preserved
        assert!(result.contains("Part of [[Watchlist]]"));
        match change {
            ChangeDescription::SetField {
                field,
                old_value,
                new_value,
            } => {
                assert_eq!(field, "status");
                assert_eq!(old_value, "to-watch");
                assert_eq!(new_value, "watched");
            }
            _ => panic!("expected SetField"),
        }
    }

    #[test]
    fn set_null_field() {
        let (result, _) = set_field(MOVIE_FILE, "rating", "8").unwrap();
        assert!(result.contains("rating: 8"));
    }

    #[test]
    fn set_new_field() {
        let (result, _) = set_field(MOVIE_FILE, "language", "English").unwrap();
        assert!(result.contains("language: English"));
        // Should be inserted before closing ---
        let closing_idx = result.rfind("\n---\n").unwrap();
        let lang_idx = result.find("language: English").unwrap();
        assert!(lang_idx < closing_idx);
    }

    #[test]
    fn set_complex_field_rejected() {
        let result = set_field(CHINESE_FILE, "kaliplar", "something");
        assert!(result.is_err());
    }

    #[test]
    fn set_value_needing_quotes() {
        let (result, _) = set_field(MOVIE_FILE, "note", "key: value").unwrap();
        assert!(result.contains("note: 'key: value'"));
    }

    #[test]
    fn unset_scalar_field() {
        let (result, _) = unset_field(MOVIE_FILE, "director").unwrap();
        assert!(!result.contains("director:"));
        // Other fields preserved
        assert!(result.contains("status: to-watch"));
        assert!(result.contains("year: 2019"));
        assert!(result.contains("Part of [[Watchlist]]"));
    }

    #[test]
    fn unset_list_field() {
        let (result, _) = unset_field(CHINESE_FILE, "kaliplar").unwrap();
        assert!(!result.contains("kaliplar:"));
        assert!(!result.contains("快乐"));
        // Other fields preserved
        assert!(result.contains("pinyin: kuài"));
        assert!(result.contains("Body text."));
    }

    #[test]
    fn unset_nonexistent_field() {
        let result = unset_field(MOVIE_FILE, "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn add_tag_2space_indent() {
        let (result, _) = add_tag(MOVIE_FILE, "genre/war").unwrap();
        assert!(result.contains("  - genre/war"));
        // Existing tags still present
        assert!(result.contains("  - type/leaf"));
        assert!(result.contains("  - genre/drama"));
    }

    #[test]
    fn add_tag_0indent() {
        let (result, _) = add_tag(CHINESE_FILE, "topic/hsk1").unwrap();
        assert!(result.contains("- topic/hsk1"));
        // Existing tags preserved
        assert!(result.contains("- type/concept"));
        assert!(result.contains("- topic/chinese"));
    }

    #[test]
    fn remove_tag_2space_indent() {
        let (result, _) = remove_tag(MOVIE_FILE, "genre/drama").unwrap();
        assert!(!result.contains("genre/drama"));
        // Other tags preserved
        assert!(result.contains("  - type/leaf"));
        assert!(result.contains("  - source/video"));
    }

    #[test]
    fn remove_tag_0indent() {
        let (result, _) = remove_tag(CHINESE_FILE, "topic/chinese").unwrap();
        assert!(!result.contains("topic/chinese"));
        assert!(result.contains("- type/concept"));
        assert!(result.contains("- source/self-study"));
    }

    #[test]
    fn remove_nonexistent_tag() {
        let result = remove_tag(MOVIE_FILE, "nonexistent/tag");
        assert!(result.is_err());
    }

    #[test]
    fn body_preserved_after_set() {
        let (result, _) = set_field(MOVIE_FILE, "status", "watched").unwrap();
        assert!(result.ends_with("Part of [[Watchlist]]\n"));
    }

    #[test]
    fn body_preserved_after_unset() {
        let (result, _) = unset_field(CHINESE_FILE, "hsk").unwrap();
        assert!(result.contains("# 快 (kuài) — hızlı"));
        assert!(result.contains("Body text."));
    }

    #[test]
    fn body_preserved_after_add_tag() {
        let (result, _) = add_tag(CHINESE_FILE, "topic/hsk1").unwrap();
        assert!(result.contains("# 快 (kuài) — hızlı"));
    }

    #[test]
    fn chinese_content_preserved() {
        let (result, _) = set_field(CHINESE_FILE, "hsk", "2").unwrap();
        assert!(result.contains("pinyin: kuài"));
        assert!(result.contains("anlam: hızlı"));
        assert!(result.contains("tür: sifat"));
        assert!(result.contains("kalip: 快乐"));
        assert!(result.contains("cumle: 他跑得很快。"));
    }

    // ── Safety checks ─────────────────────────

    #[test]
    fn set_field_rejects_flow_style() {
        let content = "---\ntags: [a, b, c]\n---\nBody.\n";
        let result = set_field(content, "tags", "x");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("flow-style"));
    }

    #[test]
    fn set_field_rejects_multiline_scalar() {
        let content = "---\ndescription: |\n  Multi line\n  content here\n---\nBody.\n";
        let result = set_field(content, "description", "new value");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("multiline"));
    }

    #[test]
    fn add_tag_rejects_flow_style() {
        let content = "---\ntags: [type/concept, topic/ai]\n---\nBody.\n";
        let result = add_tag(content, "topic/new");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("flow-style"));
    }

    #[test]
    fn remove_tag_rejects_flow_style() {
        let content = "---\ntags: [type/concept, topic/ai]\n---\nBody.\n";
        let result = remove_tag(content, "topic/ai");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("flow-style"));
    }
}
