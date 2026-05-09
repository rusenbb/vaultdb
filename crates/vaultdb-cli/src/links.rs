use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use std::sync::LazyLock;

use crate::record::{FieldValue, Record};

#[derive(Debug, Clone)]
pub enum TraverseDirection {
    Outgoing,
    Incoming,
    Both,
}

static WIKI_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]\|#]+)(?:#[^\]\|]*)?\|?[^\]]*\]\]").unwrap());

static FENCED_CODE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)```.*?```").unwrap());

static INLINE_CODE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`[^`]+`").unwrap());

/// Strip code blocks (fenced and inline) from content to avoid false link extraction.
fn strip_code_blocks(content: &str) -> String {
    let without_fenced = FENCED_CODE_RE.replace_all(content, "");
    INLINE_CODE_RE.replace_all(&without_fenced, "").into_owned()
}

/// Extract all wiki-link targets from a string.
/// Handles: [[Note]], [[Note|alias]], [[Note#section]], [[Note#section|alias]]
fn extract_links_from_str(text: &str) -> Vec<String> {
    WIKI_LINK_RE
        .captures_iter(text)
        .map(|cap| cap[1].trim().to_string())
        .collect()
}

/// Extract all outgoing wiki-links from a record's full file content.
/// Strips code blocks first to avoid false positives.
pub fn extract_links(content: &str) -> BTreeSet<String> {
    let cleaned = strip_code_blocks(content);
    let mut links = BTreeSet::new();
    for link in extract_links_from_str(&cleaned) {
        links.insert(link);
    }
    links
}

/// Extract links from a record. Requires raw_content to be loaded.
pub fn record_links(record: &Record) -> BTreeSet<String> {
    match &record.raw_content {
        Some(content) => extract_links(content),
        None => BTreeSet::new(),
    }
}

/// A backlink index: maps note name -> set of notes that link to it.
/// Handles duplicate filenames by using path-based resolution.
#[derive(Debug, Default)]
pub struct LinkIndex {
    /// note name -> outgoing link targets (as written in the wiki-links)
    pub outgoing: BTreeMap<String, BTreeSet<String>>,
    /// note name -> names of notes that link to it
    pub incoming: BTreeMap<String, BTreeSet<String>>,
    /// filename -> list of relative paths (for detecting duplicates)
    pub name_to_paths: BTreeMap<String, Vec<String>>,
}

impl LinkIndex {
    /// Build the link index from a set of records.
    /// All records must have raw_content loaded.
    pub fn build(records: &[Record]) -> Self {
        Self::build_with_root(records, None)
    }

    /// Build with a vault root for path resolution.
    pub fn build_with_root(records: &[Record], vault_root: Option<&std::path::Path>) -> Self {
        let mut index = LinkIndex::default();

        // First pass: build name -> paths mapping to detect duplicates
        for record in records {
            let name = record.virtual_name();
            let rel_path = match vault_root {
                Some(root) => record.virtual_path(root),
                None => record.path.to_string_lossy().into_owned(),
            };
            index.name_to_paths.entry(name).or_default().push(rel_path);
        }

        // Second pass: extract links and resolve targets
        for record in records {
            let name = record.virtual_name();
            let links = record_links(record);

            // Resolve each link target to a note name
            for target in &links {
                let resolved = index.resolve_link_target(target);
                index
                    .incoming
                    .entry(resolved.clone())
                    .or_default()
                    .insert(name.clone());
            }

            index.outgoing.insert(name, links);
        }

        index
    }

    /// Resolve a wiki-link target to a note name.
    /// Handles both plain names ([[Note]]) and path-qualified ([[folder/Note]]).
    fn resolve_link_target(&self, target: &str) -> String {
        if target.contains('/') {
            // Path-qualified link like [[folder/Note]] — extract the filename part
            target.rsplit('/').next().unwrap_or(target).to_string()
        } else {
            target.to_string()
        }
    }

    /// Check if a filename has duplicates across folders.
    pub fn is_ambiguous(&self, name: &str) -> bool {
        self.name_to_paths
            .get(name)
            .is_some_and(|paths| paths.len() > 1)
    }

    /// Get all paths for a given filename.
    pub fn paths_for_name(&self, name: &str) -> Vec<&str> {
        self.name_to_paths
            .get(name)
            .map(|paths| paths.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    /// Get outgoing links for a note.
    pub fn outgoing_links(&self, name: &str) -> Vec<&str> {
        self.outgoing
            .get(name)
            .map(|s| s.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    /// Get incoming links (backlinks) for a note.
    pub fn incoming_links(&self, name: &str) -> Vec<&str> {
        self.incoming
            .get(name)
            .map(|s| s.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    /// Count of outgoing links.
    pub fn outgoing_count(&self, name: &str) -> usize {
        self.outgoing.get(name).map(|s| s.len()).unwrap_or(0)
    }

    /// Count of incoming links (backlinks).
    pub fn incoming_count(&self, name: &str) -> usize {
        self.incoming.get(name).map(|s| s.len()).unwrap_or(0)
    }

    /// BFS traversal from a starting note.
    /// Returns (name, depth) pairs for all reachable notes within max_depth.
    pub fn traverse(
        &self,
        start: &str,
        max_depth: usize,
        direction: TraverseDirection,
    ) -> Vec<(String, usize)> {
        use std::collections::VecDeque;

        let mut visited = BTreeSet::new();
        let mut queue = VecDeque::new();
        let mut results = Vec::new();

        visited.insert(start.to_string());
        queue.push_back((start.to_string(), 0usize));
        results.push((start.to_string(), 0));

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }

            let neighbors: Vec<&str> = match direction {
                TraverseDirection::Outgoing => self.outgoing_links(&current),
                TraverseDirection::Incoming => self.incoming_links(&current),
                TraverseDirection::Both => {
                    let mut all = self.outgoing_links(&current);
                    all.extend(self.incoming_links(&current));
                    all
                }
            };

            for neighbor in neighbors {
                if visited.insert(neighbor.to_string()) {
                    let next_depth = depth + 1;
                    results.push((neighbor.to_string(), next_depth));
                    queue.push_back((neighbor.to_string(), next_depth));
                }
            }
        }

        results
    }

    /// Check if note `from` has an outgoing link to note `to`.
    pub fn has_link_to(&self, from: &str, to: &str) -> bool {
        self.outgoing
            .get(from)
            .is_some_and(|links| links.contains(to))
    }

    /// Check if note `to` has an incoming link from note `from`.
    pub fn has_link_from(&self, to: &str, from: &str) -> bool {
        self.incoming
            .get(to)
            .is_some_and(|links| links.contains(from))
    }

    /// Get link data as FieldValues for virtual fields on a record.
    pub fn virtual_fields(&self, name: &str) -> Vec<(&'static str, FieldValue)> {
        let out_links = self.outgoing_links(name);
        let in_links = self.incoming_links(name);

        vec![
            (
                "_links",
                FieldValue::List(
                    out_links
                        .iter()
                        .map(|s| FieldValue::String(s.to_string()))
                        .collect(),
                ),
            ),
            ("_link_count", FieldValue::Integer(out_links.len() as i64)),
            (
                "_backlinks",
                FieldValue::List(
                    in_links
                        .iter()
                        .map(|s| FieldValue::String(s.to_string()))
                        .collect(),
                ),
            ),
            (
                "_backlink_count",
                FieldValue::Integer(in_links.len() as i64),
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[test]
    fn extract_simple_link() {
        let links = extract_links("Some text with [[React]] and [[Node.js]] links.");
        assert!(links.contains("React"));
        assert!(links.contains("Node.js"));
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn extract_link_with_alias() {
        let links = extract_links("See [[React|the React framework]] for details.");
        assert!(links.contains("React"));
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn extract_link_with_section() {
        let links = extract_links("Check [[React#Hooks]] and [[React#State Management|state]].");
        assert!(links.contains("React"));
        assert_eq!(links.len(), 1); // deduped
    }

    #[test]
    fn extract_chinese_links() {
        let links = extract_links("Zıt anlamlısı [[慢]] ile birlikte [[快]] kullanılır.");
        assert!(links.contains("慢"));
        assert!(links.contains("快"));
    }

    #[test]
    fn extract_links_from_frontmatter_value() {
        let content =
            "---\nrelated-to:\n  - \"[[Watchlist]]\"\n  - \"[[2FA Setup]]\"\n---\nBody.\n";
        let links = extract_links(content);
        assert!(links.contains("Watchlist"));
        assert!(links.contains("2FA Setup"));
    }

    #[test]
    fn extract_no_links() {
        let links = extract_links("Plain text with no links at all.");
        assert!(links.is_empty());
    }

    #[test]
    fn ignores_links_in_fenced_code_block() {
        let content = "Real link [[React]].\n```\n[[FakeLink]] in code\n```\nMore text.";
        let links = extract_links(content);
        assert!(links.contains("React"));
        assert!(!links.contains("FakeLink"));
    }

    #[test]
    fn ignores_links_in_inline_code() {
        let content = "Use `[[NotALink]]` but also see [[RealLink]].";
        let links = extract_links(content);
        assert!(links.contains("RealLink"));
        assert!(!links.contains("NotALink"));
    }

    #[test]
    fn build_link_index() {
        let records = vec![
            Record {
                path: PathBuf::from("/vault/A.md"),
                fields: BTreeMap::new(),
                raw_content: Some("Links to [[B]] and [[C]].".into()),
            },
            Record {
                path: PathBuf::from("/vault/B.md"),
                fields: BTreeMap::new(),
                raw_content: Some("Links to [[C]].".into()),
            },
            Record {
                path: PathBuf::from("/vault/C.md"),
                fields: BTreeMap::new(),
                raw_content: Some("No links here.".into()),
            },
        ];

        let index = LinkIndex::build(&records);

        // Outgoing
        assert_eq!(index.outgoing_count("A"), 2);
        assert_eq!(index.outgoing_count("B"), 1);
        assert_eq!(index.outgoing_count("C"), 0);

        // Incoming (backlinks)
        assert_eq!(index.incoming_count("A"), 0); // nothing links to A
        assert_eq!(index.incoming_count("B"), 1); // A links to B
        assert_eq!(index.incoming_count("C"), 2); // A and B link to C

        // Specific backlinks
        let c_backlinks = index.incoming_links("C");
        assert!(c_backlinks.contains(&"A"));
        assert!(c_backlinks.contains(&"B"));
    }

    #[test]
    fn virtual_fields_from_index() {
        let records = vec![
            Record {
                path: PathBuf::from("/vault/A.md"),
                fields: BTreeMap::new(),
                raw_content: Some("Links to [[B]] and [[C]].".into()),
            },
            Record {
                path: PathBuf::from("/vault/B.md"),
                fields: BTreeMap::new(),
                raw_content: Some("Links back to [[A]].".into()),
            },
        ];

        let index = LinkIndex::build(&records);
        let fields = index.virtual_fields("A");

        let link_count = fields.iter().find(|(k, _)| *k == "_link_count").unwrap();
        assert_eq!(link_count.1, FieldValue::Integer(2));

        let backlink_count = fields
            .iter()
            .find(|(k, _)| *k == "_backlink_count")
            .unwrap();
        assert_eq!(backlink_count.1, FieldValue::Integer(1));
    }
}
