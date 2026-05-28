use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "vaultdb",
    about = "Database-like operations on Obsidian markdown files"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Path to vault root (default: auto-detect .obsidian/ from cwd upward)
    #[arg(long, global = true)]
    pub vault: Option<PathBuf>,

    /// Include subfolders recursively
    #[arg(long, global = true)]
    pub recursive: bool,

    /// Verbose output
    #[arg(long, short, global = true)]
    pub verbose: bool,

    /// Preview changes without writing (for mutation commands)
    #[arg(long, global = true)]
    pub dry_run: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Query records and display results
    Query {
        /// Folder to query (relative to vault root)
        folder: String,

        /// Filter conditions (AND-ed). Example: "tags contains topic/chinese"
        #[arg(long = "where", num_args = 1)]
        where_exprs: Vec<String>,

        /// Comma-separated fields to display
        #[arg(long)]
        select: Option<String>,

        /// Field to sort by
        #[arg(long)]
        sort: Option<String>,

        /// Reverse sort order
        #[arg(long)]
        desc: bool,

        /// Maximum number of results
        #[arg(long)]
        limit: Option<usize>,

        /// Output format
        #[arg(long, default_value = "table")]
        format: OutputFormat,

        /// Filter to notes that link to this note
        #[arg(long = "links-to", num_args = 1)]
        links_to: Vec<String>,

        /// Filter to notes linked from this note
        #[arg(long = "linked-from", num_args = 1)]
        linked_from: Vec<String>,

        /// Filter to notes that link to any note matching this condition
        #[arg(long = "links-to-where", num_args = 1)]
        links_to_where: Vec<String>,

        /// Filter to notes linked from any note matching this condition
        #[arg(long = "linked-from-where", num_args = 1)]
        linked_from_where: Vec<String>,

        /// Save results to a file under the vault root.
        ///
        /// Path is vault-relative; `..` and absolute paths are rejected.
        /// The file format is inferred from the extension: `.csv`, `.tsv`,
        /// `.json`, `.yaml`/`.yml`, `.xlsx`. On filename collision the
        /// renderer auto-suffixes `(1)`, `(2)`, ... When set, results are
        /// not printed to stdout (the resolved output path is printed
        /// instead).
        #[arg(long)]
        output: Option<std::path::PathBuf>,

        /// CSV/TSV delimiter override. Applies only when `--output`'s
        /// extension is `.csv` or `.tsv`.
        #[arg(long = "csv-delimiter", default_value = "comma")]
        csv_delimiter: CsvDelimiter,
    },

    /// Count matching records
    Count {
        /// Folder to count in
        folder: String,

        /// Filter conditions
        #[arg(long = "where", num_args = 1)]
        where_exprs: Vec<String>,
    },

    /// List all frontmatter fields with types and frequencies
    Fields {
        /// Folder to inspect
        folder: String,
    },

    /// List all tags with counts
    Tags {
        /// Folder to inspect
        folder: String,
    },

    /// Find all [[wiki-links]] pointing to non-existent files
    Unresolved {
        /// Folder to scan
        #[arg(default_value = "3-Notes")]
        folder: String,

        /// Limit scope to notes reachable from this note
        #[arg(long)]
        from: Option<String>,

        /// Max traversal depth when using --from (default: 2)
        #[arg(long, default_value = "2")]
        depth: usize,
    },

    /// Show links for a note (outgoing, incoming, or both)
    Links {
        /// Note name (filename without .md)
        name: String,

        /// Folder to scan for building the link graph
        #[arg(long, default_value = "3-Notes")]
        folder: String,

        /// Direction: outgoing, incoming, or both
        #[arg(long, default_value = "both")]
        direction: LinkDirection,
    },

    /// Traverse the link graph from a starting note
    Traverse {
        /// Starting note name (filename without .md)
        name: String,

        /// Folder to scan
        #[arg(long, default_value = "3-Notes")]
        folder: String,

        /// Maximum traversal depth
        #[arg(long, default_value = "2")]
        depth: usize,

        /// Direction: outgoing, incoming, or both
        #[arg(long, default_value = "outgoing")]
        direction: LinkDirection,

        /// Filter results (applied to traversed notes)
        #[arg(long = "where", num_args = 1)]
        where_exprs: Vec<String>,

        /// Comma-separated fields to display alongside names
        #[arg(long)]
        select: Option<String>,
    },

    /// Create a new note, optionally from a template
    Create {
        /// Target folder (relative to vault root)
        folder: String,

        /// Note name (becomes the filename without .md)
        #[arg(long)]
        name: String,

        /// Template file path (relative to vault root)
        #[arg(long)]
        template: Option<String>,

        /// Set frontmatter fields (FIELD=VALUE), applied on top of template
        #[arg(long, num_args = 1)]
        set: Vec<String>,

        /// Body content for the new note. Overrides the template's body
        /// (frontmatter is still merged) and the default "# {name}"
        /// placeholder. Written verbatim — escape sequences are NOT
        /// interpreted; use shell ANSI-C quoting (`$'...'`) if you
        /// need literal newlines.
        #[arg(long)]
        body: Option<String>,
    },

    /// Rename a note and auto-update all wiki-links across the vault
    Rename {
        /// Current note name (filename without .md)
        old_name: String,

        /// New note name
        new_name: String,

        /// Folder where the note lives
        #[arg(long, default_value = "3-Notes")]
        folder: String,
    },

    /// Update frontmatter fields and/or body on matching records
    Update {
        /// Folder to update in
        folder: String,

        /// Filter conditions (required)
        #[arg(long = "where", num_args = 1)]
        where_exprs: Vec<String>,

        /// Set a field value (FIELD=VALUE)
        #[arg(long, num_args = 1)]
        set: Vec<String>,

        /// Remove a field
        #[arg(long, num_args = 1)]
        unset: Vec<String>,

        /// Add a tag
        #[arg(long = "add-tag", num_args = 1)]
        add_tag: Vec<String>,

        /// Remove a tag
        #[arg(long = "remove-tag", num_args = 1)]
        remove_tag: Vec<String>,

        /// Replace the body with this text (everything after the
        /// closing `---` of the frontmatter). Written verbatim —
        /// escape sequences are NOT interpreted; use shell ANSI-C
        /// quoting (`$'...'`) if you need literal newlines.
        #[arg(long = "set-body")]
        set_body: Option<String>,

        /// Append this text to the existing body, joined by
        /// `--body-separator` (default `\n`). Repeatable. Body text
        /// is written verbatim — escape sequences are NOT
        /// interpreted (only `--body-separator` is).
        #[arg(long = "append-body", num_args = 1)]
        append_body: Vec<String>,

        /// Clear the body entirely. Applied before `--set-body` /
        /// `--append-body` in the same call.
        #[arg(long = "clear-body")]
        clear_body: bool,

        /// Separator between existing body and each appended chunk.
        /// Default `\n` (compact join). Pass `\n\n` for a blank-line
        /// section break. Escape sequences are interpreted: `\n`
        /// becomes a newline, `\t` a tab.
        #[arg(long = "body-separator")]
        body_separator: Option<String>,
    },

    /// Move matching files to another folder
    Move {
        /// Source folder
        folder: String,

        /// Filter conditions (required)
        #[arg(long = "where", num_args = 1)]
        where_exprs: Vec<String>,

        /// Destination folder
        #[arg(long)]
        to: String,
    },

    /// Delete matching files (moves to .trash/ by default)
    Delete {
        /// Folder to delete from
        folder: String,

        /// Filter conditions (required)
        #[arg(long = "where", num_args = 1)]
        where_exprs: Vec<String>,

        /// Permanently delete instead of moving to .trash/
        #[arg(long)]
        force: bool,
    },

    /// Schema operations
    Schema {
        #[command(subcommand)]
        action: SchemaAction,
    },
}

#[derive(Subcommand)]
pub enum SchemaAction {
    /// Show schema for a folder
    Show { folder: String },
    /// Validate records against schema
    Validate { folder: String },
    /// Infer schema from existing data. Prints to stdout by default;
    /// pass `--write` to merge the inferred collection into
    /// `<vault>/vaultdb-schema.yaml` (existing collections at the same
    /// folder are replaced, others preserved).
    Init {
        folder: String,
        /// Save the inferred schema to `<vault>/vaultdb-schema.yaml`.
        #[arg(long)]
        write: bool,
    },
}

#[derive(Debug, Clone, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
    Yaml,
    Csv,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum LinkDirection {
    Outgoing,
    Incoming,
    Both,
}

/// CSV / TSV delimiter selector for `--csv-delimiter`. Named variants
/// because shells make literal `;` and `\t` awkward to pass through.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CsvDelimiter {
    Comma,
    Semicolon,
    Tab,
}

impl CsvDelimiter {
    /// Byte value for `csv::WriterBuilder::delimiter`.
    pub fn as_byte(self) -> u8 {
        match self {
            CsvDelimiter::Comma => b',',
            CsvDelimiter::Semicolon => b';',
            CsvDelimiter::Tab => b'\t',
        }
    }
}
