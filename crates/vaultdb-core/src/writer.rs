//! Frontmatter write primitives. [`set_field`], [`unset_field`], [`add_tag`],
//! [`remove_tag`] each return `(new_content, ChangeDescription)` without
//! touching disk; [`apply`] flushes a [`WriteResult`] to the filesystem. The
//! public mutation builders in [`crate::mutation`] wrap these.

use crate::error::{Result, VaultdbError};
use crate::record::Value;

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
    /// Replace the entire body (everything after the closing `---` of the
    /// frontmatter). `old_len` / `new_len` are byte counts, surfaced so
    /// the report can say "shrank body from 1.2k → 80 bytes" without
    /// echoing arbitrary user prose.
    SetBody {
        old_len: usize,
        new_len: usize,
    },
    /// Append text to the existing body (separated by a caller-chosen
    /// separator, default `"\n"`). `added_len` is the byte count of the
    /// appended text (excluding separator) for the same reason.
    AppendBody {
        added_len: usize,
    },
    /// Clear the body entirely — semantic shorthand for "set to empty".
    /// Carries `old_len` so an audit log can record what was removed.
    ClearBody {
        old_len: usize,
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
            ChangeDescription::SetBody { old_len, new_len } => {
                write!(f, "set body ({} → {} bytes)", old_len, new_len)
            }
            ChangeDescription::AppendBody { added_len } => {
                write!(f, "append body (+{} bytes)", added_len)
            }
            ChangeDescription::ClearBody { old_len } => {
                write!(f, "clear body (was {} bytes)", old_len)
            }
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
        // No frontmatter block at all: synthesize an empty one and treat the
        // entire content as the body. This lets the write primitives
        // (set_field, add_tag, ...) initialize frontmatter on a bare file
        // instead of refusing. A file that *opens* a frontmatter block but
        // never closes it is still rejected as malformed (see below).
        return Ok((vec!["---", "---"], content));
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
    for line in fm_lines.iter().skip(key_line_idx + 1) {
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
    for line in fm_lines.iter().skip(key_line_idx + 1) {
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
        || value.starts_with('?')
        // Type-ambiguous bare scalars: without quoting, these would
        // parse as a different YAML type when re-read (e.g. `true` →
        // boolean, `42` → integer, `~` → null). Quote them so a
        // `Value::String("true")` round-trips as the string "true"
        // and not the boolean true.
        || is_yaml_type_ambiguous_bare_scalar(value);

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

/// True if `value`, written without YAML quotes, would parse as a
/// non-string scalar (boolean / null / integer / float). Used by
/// `yaml_quote_value` to force quoting on these strings so they
/// round-trip as strings rather than silently changing type.
fn is_yaml_type_ambiguous_bare_scalar(value: &str) -> bool {
    // YAML 1.1 boolean / null literals — same set Obsidian / serde_yaml
    // accept on read. Match case-insensitively to be safe.
    let lower = value.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~"
    ) {
        return true;
    }
    // Numeric: integer or float. `parse::<f64>` accepts both shapes
    // including `+1`, `-0.5`, `1e10`, but rejects empty strings.
    if !value.is_empty() && value.parse::<f64>().is_ok() {
        return true;
    }
    // Leading zero or sign with rest digits could be an int that
    // f64::parse already covers — no extra branch needed.
    false
}

/// Set a scalar field to a new value in the frontmatter. The `value`
/// is treated as a **raw, unquoted scalar** — `yaml_quote_value` is
/// applied to it before writing. Use this when you have a plain Rust
/// string (e.g. `"https://example.com"`, `"true"`) and want it
/// emitted as a properly-quoted YAML scalar.
///
/// For values that are already a valid YAML scalar (e.g. produced by
/// a higher-level renderer that has already applied quoting rules),
/// call [`set_field_preformatted`] instead — double-quoting will
/// otherwise turn `'https://x'` into `"'https://x'"` on disk.
pub fn set_field(content: &str, key: &str, value: &str) -> Result<(String, ChangeDescription)> {
    let quoted_value = yaml_quote_value(value);
    set_field_with_formatted(content, key, &quoted_value, value)
}

/// Set a scalar field to a value that is **already a valid YAML
/// scalar**. The string is written verbatim — no extra quoting.
///
/// This exists to fix a two-layer-quoting bug introduced by callers
/// (notably `UpdateBuilder::set` via `render_value_for_yaml`) that
/// already apply `yaml_quote_value` themselves. If those callers
/// passed their pre-quoted output to [`set_field`], it would be
/// quoted a second time — a URL like `https://example.com`, having
/// already become `'https://example.com'`, would be re-wrapped as
/// `"'https://example.com'"`. Routing through this function instead
/// preserves the intended YAML shape.
///
/// The caller asserts that `yaml_value` parses as the intended YAML
/// scalar. If it doesn't, the on-disk file will fail to re-parse;
/// there is no defence-in-depth check here on purpose, because
/// guessing whether the input is "already-quoted" or "literal text
/// containing quote characters" is ambiguous.
pub fn set_field_preformatted(
    content: &str,
    key: &str,
    yaml_value: &str,
) -> Result<(String, ChangeDescription)> {
    set_field_with_formatted(content, key, yaml_value, yaml_value)
}

/// Shared implementation behind [`set_field`] and
/// [`set_field_preformatted`]. `formatted_value` is what lands on
/// disk; `change_value` is what appears in the `ChangeDescription`
/// surfaced to users / agents (typically the raw, un-quoted form
/// for `set_field`; the same as `formatted_value` for the
/// preformatted path).
fn set_field_with_formatted(
    content: &str,
    key: &str,
    formatted_value: &str,
    change_value: &str,
) -> Result<(String, ChangeDescription)> {
    let (fm_lines, body) = split_frontmatter(content)?;

    if let Some(key_idx) = find_key_line(&fm_lines, key) {
        // Flow-style lists (`[a, b]`) and multiline scalars (`|`, `>`) are
        // intentionally not round-tripped — we won't rewrite those shapes.
        // Block-style lists/maps, however, can be replaced by a scalar: we
        // drop the whole field span and write the new single line. This lets
        // a required field that was stored as the wrong (complex) type be
        // corrected in one set, without an unset that would transiently
        // violate a "required" constraint.
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

        let extent = field_extent(&fm_lines, key_idx);

        // Prior value, for the ChangeDescription only. Scalar → the text
        // after the colon; block list/map → its item lines collapsed.
        let old_value = if extent == 1 {
            let old_line = fm_lines[key_idx];
            old_line
                .find(':')
                .map(|pos| old_line[pos + 1..].trim())
                .unwrap_or("")
                .to_string()
        } else {
            fm_lines[key_idx + 1..key_idx + extent]
                .iter()
                .map(|l| l.trim().trim_start_matches('-').trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        };

        let new_line = format!("{}: {}", key, formatted_value);

        // Replace the field's entire extent (key line + any block-style
        // continuation lines) with the single new scalar line.
        let mut result_lines: Vec<String> = Vec::new();
        for (i, line) in fm_lines.iter().enumerate() {
            if i == key_idx {
                result_lines.push(new_line.clone());
            } else if i > key_idx && i < key_idx + extent {
                continue; // dropped: part of the replaced block
            } else {
                result_lines.push(line.to_string());
            }
        }

        let change = ChangeDescription::SetField {
            field: key.to_string(),
            old_value,
            new_value: change_value.to_string(),
        };

        Ok((reassemble(&result_lines, body, content), change))
    } else {
        // Key doesn't exist — insert before closing ---
        let mut result_lines: Vec<String> = Vec::new();
        for (i, line) in fm_lines.iter().enumerate() {
            if i == fm_lines.len() - 1 && line.trim() == "---" {
                result_lines.push(format!("{}: {}", key, formatted_value));
            }
            result_lines.push(line.to_string());
        }

        let change = ChangeDescription::SetField {
            field: key.to_string(),
            old_value: String::new(),
            new_value: change_value.to_string(),
        };

        Ok((reassemble(&result_lines, body, content), change))
    }
}

/// Set a field to a `Value::List` or `Value::Map`, emitting block-style YAML
/// across multiple lines.
///
/// Use [`set_field`] for scalars. This function is the structured counterpart:
/// it preserves the typed shape of the value through the write rather than
/// flattening it to a quoted string scalar.
///
/// Behavior:
/// - Key absent → insert the rendered block before the closing `---`.
/// - Key present as a block-style list/map → replace the multi-line span.
/// - Key present as a scalar → replace the single line with the block.
/// - Key present as flow-style (`[a, b]`) or a multiline scalar (`|`, `>`):
///   refuses with `InvalidFrontmatter`, matching [`set_field`]'s stance —
///   we won't try to round-trip those styles.
pub fn set_field_block(
    content: &str,
    key: &str,
    value: &Value,
) -> Result<(String, ChangeDescription)> {
    if !matches!(value, Value::List(_) | Value::Map(_)) {
        return Err(VaultdbError::InvalidFrontmatter {
            file: String::new(),
            reason: format!(
                "set_field_block called with a scalar value for '{}'; use set_field instead",
                key
            ),
        });
    }

    let (fm_lines, body) = split_frontmatter(content)?;

    // Render `{key: value}` as YAML so the key sits at column 0 and the
    // contents indent below it. Splitting by lines gives us the block we
    // splice into the frontmatter.
    let mut wrapper: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    wrapper.insert(key.to_string(), value.clone());
    let rendered =
        serde_yaml::to_string(&wrapper).map_err(|e| VaultdbError::InvalidFrontmatter {
            file: String::new(),
            reason: format!("rendering '{}' as YAML: {}", key, e),
        })?;
    let new_lines: Vec<String> = rendered.lines().map(String::from).collect();
    let new_value_summary = serde_yaml::to_string(value)
        .map(|s| s.trim_end().to_string())
        .unwrap_or_default();

    if let Some(key_idx) = find_key_line(&fm_lines, key) {
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

        let extent = field_extent(&fm_lines, key_idx);
        // Old value summary: either the inline scalar (extent == 1) or the
        // joined block (extent > 1). Used for ChangeDescription only.
        let old_value = if extent == 1 {
            fm_lines[key_idx]
                .find(':')
                .map(|pos| fm_lines[key_idx][pos + 1..].trim().to_string())
                .unwrap_or_default()
        } else {
            fm_lines[key_idx..key_idx + extent].join("\n")
        };

        let mut result_lines: Vec<String> = Vec::new();
        for line in &fm_lines[..key_idx] {
            result_lines.push((*line).to_string());
        }
        result_lines.extend(new_lines.iter().cloned());
        for line in &fm_lines[key_idx + extent..] {
            result_lines.push((*line).to_string());
        }

        let change = ChangeDescription::SetField {
            field: key.to_string(),
            old_value,
            new_value: new_value_summary,
        };

        Ok((reassemble(&result_lines, body, content), change))
    } else {
        // Key doesn't exist — insert the block before the closing ---.
        let mut result_lines: Vec<String> = Vec::new();
        for (i, line) in fm_lines.iter().enumerate() {
            if i == fm_lines.len() - 1 && line.trim() == "---" {
                result_lines.extend(new_lines.iter().cloned());
            }
            result_lines.push((*line).to_string());
        }

        let change = ChangeDescription::SetField {
            field: key.to_string(),
            old_value: String::new(),
            new_value: new_value_summary,
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
    let tag_line_idx = fm_lines
        .iter()
        .enumerate()
        .skip(key_idx + 1)
        .take(extent.saturating_sub(1))
        .find_map(|(i, line)| {
            let trimmed = line.trim();
            let tag_value = trimmed.strip_prefix("- ").unwrap_or(trimmed);
            (tag_value == tag).then_some(i)
        });

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

// ── Body mutations ─────────────────────────────────────────────────────────
//
// `set_body`, `append_body`, and `clear_body` operate on the body
// region — everything after the closing `---` of the frontmatter.
// They preserve the frontmatter byte-for-byte and the file's
// line-ending style. Bare files (no frontmatter delimiters) get an
// empty frontmatter synthesized, matching `set_field`'s behaviour;
// note that for `set_body` this means the file's pre-existing
// non-frontmatter content is treated as the body and will be
// replaced. In practice the public mutation API only reaches files
// that already parsed as records (i.e. had valid frontmatter), so
// the synthesise path is mostly exercised by tests.

/// Replace the body with `new_body`. Frontmatter is preserved byte-for-byte.
///
/// `new_body` is written verbatim — no trailing newline auto-append, no
/// leading whitespace stripping. Callers that want a trailing newline
/// should include it in `new_body`.
pub fn set_body(content: &str, new_body: &str) -> Result<(String, ChangeDescription)> {
    let (fm_lines, old_body) = split_frontmatter(content)?;
    let fm_owned: Vec<String> = fm_lines.iter().map(|s| s.to_string()).collect();
    let change = ChangeDescription::SetBody {
        old_len: old_body.len(),
        new_len: new_body.len(),
    };
    Ok((reassemble(&fm_owned, new_body, content), change))
}

/// Clear the body entirely. Equivalent to [`set_body`] with `""` but
/// surfaces a distinct [`ChangeDescription::ClearBody`] so audit logs
/// and dry-run reports can call out the destructive intent.
pub fn clear_body(content: &str) -> Result<(String, ChangeDescription)> {
    let (fm_lines, old_body) = split_frontmatter(content)?;
    let fm_owned: Vec<String> = fm_lines.iter().map(|s| s.to_string()).collect();
    let change = ChangeDescription::ClearBody {
        old_len: old_body.len(),
    };
    Ok((reassemble(&fm_owned, "", content), change))
}

/// Append `text` to the end of the body, joined by `separator`.
///
/// `separator` is inserted only when the existing body is non-empty.
/// When the existing body ends with the same trailing newline(s) as
/// `separator`, those trailing newlines are trimmed before joining so
/// the result doesn't accumulate stacked blank lines on repeated
/// appends. Default callers use `"\n"`, which yields one newline
/// between old and new content regardless of the original body's
/// trailing-newline state.
pub fn append_body(
    content: &str,
    text: &str,
    separator: &str,
) -> Result<(String, ChangeDescription)> {
    let (fm_lines, old_body) = split_frontmatter(content)?;
    let fm_owned: Vec<String> = fm_lines.iter().map(|s| s.to_string()).collect();

    let new_body = if old_body.is_empty() {
        text.to_string()
    } else {
        // Strip any trailing `\n` / `\r\n` runs from the existing body so
        // we don't end up with a blank line between old and new content
        // when the file already ends with a newline (the common case).
        let trimmed = old_body.trim_end_matches(['\n', '\r']);
        format!("{}{}{}", trimmed, separator, text)
    };

    let change = ChangeDescription::AppendBody {
        added_len: text.len(),
    };
    Ok((reassemble(&fm_owned, &new_body, content), change))
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

/// Options controlling how a write touches the filesystem.
///
/// Default values match the previous (pre-Phase-A) `std::fs::write`
/// behaviour: atomic at the rename, but not durable against power loss.
/// Set `fsync: true` to force the data to stable storage before the
/// write returns.
///
/// Designed to be `Copy + Default + serde::*` so it can be piped through
/// the mutation builders, configured from env vars or config files, or
/// surfaced over a Tauri command.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WriteOptions {
    /// fsync the temp file's data, then fsync the parent directory's
    /// metadata, before considering the write complete. Adds one or two
    /// disk-flush IOs per write — typically 1–10ms on consumer SSDs and
    /// 10–50ms on spinning disks. Required for durable mutations (e.g. a
    /// long-lived Tauri app) that need to survive sudden power loss with
    /// the change preserved.
    pub fsync: bool,
}

impl WriteOptions {
    /// Convenience: opts with `fsync` set to true.
    pub fn durable() -> Self {
        Self { fsync: true }
    }
}

/// fsync a directory so its dirent updates (renames, creates, removes)
/// are durable. Best-effort on Windows: opening a directory for sync
/// is supported on NTFS but not all filesystems.
pub fn fsync_dir(dir: &std::path::Path) -> std::io::Result<()> {
    let f = std::fs::File::open(dir)?;
    f.sync_all()
}

/// Atomically replace the contents of `path` with `content` using the
/// default [`WriteOptions`] (no fsync). See [`atomic_write_with`] for the
/// version that takes options.
pub fn atomic_write(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    atomic_write_with(path, content, WriteOptions::default())
}

/// Atomically create a new file at `path` with `content`. **Refuses to
/// overwrite** if `path` already exists — returns
/// `io::ErrorKind::AlreadyExists`. Used by `CreateBuilder::execute` as
/// defence-in-depth against a TOCTOU window between its `dest.exists()`
/// check and the rename: even if an external process slips a file into
/// the destination after the check, this won't clobber it.
///
/// Same atomic tempfile+rename pattern as [`atomic_write_with`]; the
/// only difference is `persist_noclobber` in place of `persist`, which
/// maps to `link(2)` on POSIX and `MoveFileEx` without
/// `MOVEFILE_REPLACE_EXISTING` on Windows.
pub fn atomic_create_with(
    path: &std::path::Path,
    content: &str,
    opts: WriteOptions,
) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::other(format!(
            "atomic_create target has no parent dir: {}",
            path.display()
        ))
    })?;

    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    use std::io::Write;
    tmp.write_all(content.as_bytes())?;
    tmp.flush()?;

    if opts.fsync {
        tmp.as_file().sync_all()?;
    }

    tmp.persist_noclobber(path).map_err(|e| e.error)?;

    if opts.fsync {
        fsync_dir(dir)?;
    }
    Ok(())
}

/// Atomically replace the contents of `path` with `content`, honoring
/// [`WriteOptions`].
///
/// Writes to a temp file in the same directory, optionally fsyncs the
/// temp file's data, then renames over the target. The rename is atomic
/// on POSIX same-filesystem operations and on Windows with
/// `MoveFileEx(MOVEFILE_REPLACE_EXISTING)`. Concurrent readers either
/// see the full old content or the full new content; they never see a
/// partial write.
///
/// When `opts.fsync` is true, the temp file is fsynced before rename
/// AND the parent directory is fsynced after rename, so the change
/// survives power loss the moment this function returns Ok.
pub fn atomic_write_with(
    path: &std::path::Path,
    content: &str,
    opts: WriteOptions,
) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::other(format!(
            "atomic_write target has no parent dir: {}",
            path.display()
        ))
    })?;

    // tempfile::NamedTempFile creates a uniquely-named file in `dir`,
    // which guarantees same-filesystem rename below. The file is
    // cleaned up automatically on drop if `persist` isn't called (e.g.
    // if the write fails mid-way).
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;

    use std::io::Write;
    tmp.write_all(content.as_bytes())?;
    tmp.flush()?;

    // Optional data fsync before the rename. The order matters: if we
    // rename first and then fsync, a power loss between the rename and
    // the fsync can leave the rename visible but pointing at undefined
    // data. POSIX guarantees that data fsynced before the rename is
    // durable as soon as the rename's directory entry is durable.
    if opts.fsync {
        tmp.as_file().sync_all()?;
    }

    // `persist` does the atomic rename. On error it returns the temp
    // file plus the io::Error; we discard the temp file (it'll be
    // cleaned up by Drop) and propagate just the error.
    tmp.persist(path).map_err(|e| e.error)?;

    if opts.fsync {
        fsync_dir(dir)?;
    }
    Ok(())
}

/// Write a WriteResult to disk atomically with default options.
pub fn apply(result: &WriteResult) -> std::io::Result<()> {
    apply_with(result, WriteOptions::default())
}

/// Write a WriteResult to disk atomically, honoring [`WriteOptions`].
pub fn apply_with(result: &WriteResult, opts: WriteOptions) -> std::io::Result<()> {
    atomic_write_with(&result.path, &result.modified_content, opts)
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
        // 1.3.1: `yaml_quote_value` now quotes type-ambiguous bare
        // scalars (booleans, null, numbers) so a `Value::String("8")`
        // round-trips as the string "8" rather than the integer 8.
        // For typed callers (UpdateBuilder via `Value::Integer(8)` →
        // `render_value_for_yaml` → "8" → `set_field_preformatted`)
        // an integer still lands as `rating: 8`. This test exercises
        // the raw `set_field` path directly with a string value, so
        // the quoted form is correct.
        let (result, _) = set_field(MOVIE_FILE, "rating", "8").unwrap();
        assert!(result.contains("rating: '8'"), "got:\n{}", result);
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
    fn set_scalar_over_block_field_replaces() {
        // A scalar set over a block-style list/map now REPLACES the whole
        // field span (it used to be refused as a "complex type"). Flow-style
        // and multiline scalars are still refused — see the dedicated tests.
        let (result, change) = set_field(CHINESE_FILE, "kaliplar", "something").unwrap();
        assert!(result.contains("kaliplar: something"), "got:\n{}", result);
        assert!(!result.contains("快乐")); // old block item gone
        assert!(!result.contains("kuàilè"));
        // Neighbouring fields on both sides of the replaced span survive.
        assert!(result.contains("hsk: 1"));
        assert!(result.contains("ornekler:"));
        assert!(result.contains("Body text."));
        match change {
            ChangeDescription::SetField {
                field, new_value, ..
            } => {
                assert_eq!(field, "kaliplar");
                assert_eq!(new_value, "something");
            }
            _ => panic!("expected SetField"),
        }
    }

    #[test]
    fn set_field_initializes_frontmatter_on_bare_file() {
        // A file with no frontmatter block at all gets one synthesized so the
        // field can be added, rather than the write being refused.
        let bare = "# Just a heading\n\nSome body text.\n";
        let (result, _) = set_field(bare, "db-table", "rusen-wiki").unwrap();
        assert!(result.starts_with("---\n"), "got:\n{}", result);
        assert!(result.contains("db-table: rusen-wiki"));
        // Body preserved after the synthesized frontmatter.
        assert!(result.contains("# Just a heading"));
        assert!(result.contains("Some body text."));
        // Frontmatter re-parses and carries the new field.
        let fm_end = result[4..].find("\n---\n").unwrap() + 4;
        let fm = &result[4..fm_end];
        let parsed: serde_yaml::Value = serde_yaml::from_str(fm).unwrap();
        assert_eq!(
            parsed
                .as_mapping()
                .and_then(|m| m.get("db-table"))
                .and_then(|v| v.as_str()),
            Some("rusen-wiki")
        );
    }

    #[test]
    fn set_value_needing_quotes() {
        let (result, _) = set_field(MOVIE_FILE, "note", "key: value").unwrap();
        assert!(result.contains("note: 'key: value'"));
    }

    // ── set_field_block (typed list/map writes) ──────────────────────────

    #[test]
    fn set_field_block_inserts_new_list_as_block_yaml() {
        let value = Value::List(vec![Value::String("kedi".into())]);
        let (result, change) = set_field_block(MOVIE_FILE, "anlamlar", &value).unwrap();
        // Block-style: a `key:` line followed by `- item` lines, NOT
        // `anlamlar: '- kedi'` (the pre-fix quoted-scalar shape).
        assert!(result.contains("anlamlar:\n- kedi"));
        assert!(!result.contains("anlamlar: '- kedi'"));
        // Inserted before closing `---`.
        let closing_idx = result.rfind("\n---\n").unwrap();
        assert!(result.find("anlamlar:").unwrap() < closing_idx);
        match change {
            ChangeDescription::SetField {
                field, new_value, ..
            } => {
                assert_eq!(field, "anlamlar");
                assert_eq!(new_value.trim_end(), "- kedi");
            }
            _ => panic!("expected SetField"),
        }
    }

    #[test]
    fn set_field_block_multi_item_list_round_trips() {
        let value = Value::List(vec![
            Value::String("猫が好きです。".into()),
            Value::String("私の猫は黒いです。".into()),
        ]);
        let (result, _) = set_field_block(MOVIE_FILE, "ornekler_jp", &value).unwrap();
        assert!(result.contains("ornekler_jp:\n- 猫が好きです。\n- 私の猫は黒いです。"));
        // Parse the result back and confirm the field is a list, not a string.
        let fm_end = result[4..].find("\n---\n").unwrap() + 4;
        let fm = &result[4..fm_end];
        let parsed: serde_yaml::Value = serde_yaml::from_str(fm).unwrap();
        let items = parsed
            .as_mapping()
            .and_then(|m| m.get("ornekler_jp"))
            .and_then(|v| v.as_sequence())
            .expect("ornekler_jp must round-trip as a YAML sequence");
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn set_field_block_replaces_existing_block_list() {
        // CHINESE_FILE has `kaliplar` as a block-style list of maps. Replace
        // it with a fresh list and confirm the old span is gone.
        let value = Value::List(vec![Value::String("replaced".into())]);
        let (result, _) = set_field_block(CHINESE_FILE, "kaliplar", &value).unwrap();
        assert!(result.contains("kaliplar:\n- replaced"));
        assert!(!result.contains("快乐")); // old item gone
        assert!(!result.contains("kuàilè")); // old nested key gone
        // Adjacent fields preserved.
        assert!(result.contains("hsk: 1"));
        assert!(result.contains("ornekler:"));
    }

    #[test]
    fn set_field_block_writes_map_as_nested_yaml() {
        let mut m: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
        m.insert("k1".into(), Value::String("v1".into()));
        m.insert("k2".into(), Value::Integer(2));
        let value = Value::Map(m);
        let (result, _) = set_field_block(MOVIE_FILE, "meta", &value).unwrap();
        assert!(result.contains("meta:\n  k1: v1\n  k2: 2"));
    }

    #[test]
    fn set_field_block_rejects_flow_style_existing() {
        let content = "---\ntags: [a, b]\n---\nbody\n";
        let value = Value::List(vec![Value::String("c".into())]);
        let err = set_field_block(content, "tags", &value).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("flow-style"), "got: {}", msg);
    }

    #[test]
    fn set_field_block_rejects_scalar_value() {
        // Programmer error guard: scalars must go through set_field, not here.
        let err =
            set_field_block(MOVIE_FILE, "status", &Value::String("watched".into())).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("scalar value"), "got: {}", msg);
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

    #[test]
    fn atomic_create_refuses_to_overwrite_existing_file() {
        // Defence-in-depth: even if a CreateBuilder's compute-time
        // `dest.exists()` check passed (or was bypassed), the
        // create-with-no-clobber write must still refuse to silently
        // overwrite a file that appeared in the gap.
        use std::fs;
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("note.md");
        fs::write(&target, "existing content\n").unwrap();

        let err = atomic_create_with(&target, "would clobber\n", WriteOptions::default())
            .expect_err("atomic_create must refuse to overwrite");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);

        // Original file content is intact.
        assert_eq!(fs::read_to_string(&target).unwrap(), "existing content\n");
    }

    #[test]
    fn atomic_create_writes_to_new_path() {
        use std::fs;
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("fresh.md");
        atomic_create_with(&target, "hello\n", WriteOptions::default()).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello\n");
    }

    #[test]
    fn set_field_preformatted_writes_value_verbatim() {
        // `set_field_preformatted` must NOT call yaml_quote_value on its
        // input — it's the caller's contract that the value is already
        // a valid YAML scalar. Pre-fix, an already-single-quoted URL
        // was re-wrapped in double quotes by `set_field`.
        let content = "---\nurl:\n---\nBody\n";
        let preformatted = "'https://www.amazon.com.tr/foo'";
        let (out, _) = set_field_preformatted(content, "url", preformatted).unwrap();
        assert!(
            out.contains("url: 'https://www.amazon.com.tr/foo'"),
            "got:\n{}",
            out
        );
        assert!(
            !out.contains("url: \"'"),
            "preformatted value was double-quoted; got:\n{}",
            out
        );
    }

    // ── Body mutations ───────────────────────────────────────────────────

    #[test]
    fn set_body_replaces_existing_body() {
        let (result, change) = set_body(MOVIE_FILE, "New body content.\n").unwrap();
        // Frontmatter intact.
        assert!(result.contains("director: Sam Mendes"));
        assert!(result.contains("status: to-watch"));
        // Old body gone, new body present.
        assert!(!result.contains("Part of [[Watchlist]]"));
        assert!(result.contains("New body content."));
        // Body sits immediately after the closing `---\n`.
        assert!(result.ends_with("---\nNew body content.\n"));
        match change {
            ChangeDescription::SetBody { new_len, .. } => assert_eq!(new_len, 18),
            other => panic!("expected SetBody, got {:?}", other),
        }
    }

    #[test]
    fn set_body_writes_verbatim_no_trailing_newline_added() {
        // Caller controls the trailing-newline behaviour. If they pass a
        // body without `\n`, the file ends without one.
        let (result, _) = set_body(MOVIE_FILE, "no newline").unwrap();
        assert!(result.ends_with("---\nno newline"));
    }

    #[test]
    fn set_body_on_frontmatter_only_file() {
        // File with frontmatter and empty body — set_body fills it in.
        let fm_only = "---\nstatus: x\n---\n";
        let (result, _) = set_body(fm_only, "Hello.\n").unwrap();
        assert_eq!(result, "---\nstatus: x\n---\nHello.\n");
    }

    #[test]
    fn set_body_on_bare_file_synthesizes_frontmatter() {
        // No frontmatter delimiters at all — split_frontmatter synthesizes
        // empty frontmatter and treats the original content as body.
        // set_body then replaces that body.
        let bare = "Just a bare note.\n";
        let (result, change) = set_body(bare, "Replaced.\n").unwrap();
        assert!(result.starts_with("---\n---\n"));
        assert!(result.ends_with("Replaced.\n"));
        assert!(!result.contains("Just a bare note"));
        match change {
            ChangeDescription::SetBody { old_len, new_len } => {
                assert_eq!(old_len, bare.len());
                assert_eq!(new_len, "Replaced.\n".len());
            }
            other => panic!("expected SetBody, got {:?}", other),
        }
    }

    #[test]
    fn clear_body_keeps_frontmatter_and_drops_body() {
        let (result, change) = clear_body(MOVIE_FILE).unwrap();
        assert!(result.contains("director: Sam Mendes"));
        assert!(!result.contains("Part of [[Watchlist]]"));
        // Body region is empty: ends with the closing `---\n`.
        assert!(result.ends_with("---\n"));
        match change {
            ChangeDescription::ClearBody { old_len } => assert!(old_len > 0),
            other => panic!("expected ClearBody, got {:?}", other),
        }
    }

    #[test]
    fn append_body_on_existing_body_uses_separator() {
        // MOVIE_FILE body is "\nPart of [[Watchlist]]\n" — note the leading
        // blank line is part of the body. Appending "next" with "\n"
        // separator strips the trailing newline from the existing body,
        // then joins.
        let (result, change) = append_body(MOVIE_FILE, "Next line.", "\n").unwrap();
        assert!(result.contains("Part of [[Watchlist]]"));
        assert!(result.ends_with("Part of [[Watchlist]]\nNext line."));
        match change {
            ChangeDescription::AppendBody { added_len } => assert_eq!(added_len, 10),
            other => panic!("expected AppendBody, got {:?}", other),
        }
    }

    #[test]
    fn append_body_with_custom_separator() {
        // Pass a blank-line separator. Result should have one blank line
        // between old and new content.
        let fm_with_body = "---\nstatus: x\n---\nFirst.\n";
        let (result, _) = append_body(fm_with_body, "Second.", "\n\n").unwrap();
        assert!(result.ends_with("First.\n\nSecond."));
    }

    #[test]
    fn append_body_on_empty_body_skips_separator() {
        // Empty body → no separator added; appended text becomes the body.
        let fm_only = "---\nstatus: x\n---\n";
        let (result, _) = append_body(fm_only, "First line.", "\n").unwrap();
        assert_eq!(result, "---\nstatus: x\n---\nFirst line.");
    }

    #[test]
    fn append_body_idempotent_against_trailing_newlines() {
        // Repeatedly appending with "\n" separator should not accumulate
        // blank lines, even if each append leaves a trailing newline.
        let start = "---\n---\n";
        let (r1, _) = append_body(start, "a\n", "\n").unwrap();
        let (r2, _) = append_body(&r1, "b\n", "\n").unwrap();
        let (r3, _) = append_body(&r2, "c\n", "\n").unwrap();
        assert_eq!(r3, "---\n---\na\nb\nc\n");
    }

    #[test]
    fn append_body_on_bare_file_appends_after_original() {
        // Bare file — synthesized fm, body is the original content.
        // Append adds to the end of that content.
        let bare = "Existing.\n";
        let (result, _) = append_body(bare, "More.", "\n").unwrap();
        assert!(result.starts_with("---\n---\n"));
        assert!(result.ends_with("Existing.\nMore."));
    }

    #[test]
    fn set_field_still_quotes_raw_values() {
        // Sanity: the public `set_field` is unchanged — raw strings
        // with special characters still get quoted exactly once.
        let content = "---\nurl:\n---\n";
        let (out, _) = set_field(content, "url", "https://www.example.com").unwrap();
        assert!(
            out.contains("url: 'https://www.example.com'"),
            "got:\n{}",
            out
        );
        // No double-quoted wrapping.
        assert!(!out.contains("url: \"'"), "got:\n{}", out);
    }
}
