//! Migration — parse Yurtle-format markdown files into Arrow tables.
//!
//! Walks scan_paths from config, parses YAML frontmatter, extracts Turtle
//! status-change blocks, and builds Items / Relations / Runs RecordBatches.
//!
//! Usage:
//! ```ignore
//! let config = ConfigFile::from_path(&Path::new(".yurtle-kanban/config.yaml"))?;
//! let result = migrate_boards(&root, &config)?;
//! println!("{}", result.summary());
//! ```

use crate::config::{BoardConfig, ConfigFile};
use crate::crud::KanbanStore;
use crate::item_type::ItemType;
use crate::relations::RelationsStore;
use crate::schema::{items_schema, relations_schema, runs_schema};
use arrow::array::{
    BooleanArray, ListBuilder, RecordBatch, StringArray, StringBuilder, TimestampMillisecondArray,
};
use chrono::{NaiveDate, NaiveDateTime};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Pre-compiled regexes for turtle block parsing.
struct TurtleRegexes {
    block: Regex,
    status: Regex,
    timestamp: Regex,
    agent: Regex,
    forced: Regex,
}

fn turtle_regexes() -> &'static TurtleRegexes {
    static REGEXES: OnceLock<TurtleRegexes> = OnceLock::new();
    REGEXES.get_or_init(|| TurtleRegexes {
        block: Regex::new(r"```(?:yurtle|turtle)\s*\n([\s\S]*?)```").expect("valid regex"),
        status: Regex::new(r"kb:status\s+kb:(\w+)").expect("valid regex"),
        timestamp: Regex::new(r#"kb:at\s+"([^"]+)""#).expect("valid regex"),
        agent: Regex::new(r#"kb:by\s+"([^"]+)""#).expect("valid regex"),
        forced: Regex::new(r"kb:forcedMove").expect("valid regex"),
    })
}

/// Errors during migration.
#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML parse error in {path}: {message}")]
    Yaml { path: String, message: String },

    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("Missing required field '{field}' in {path}")]
    MissingField { field: String, path: String },

    #[error("Unknown item type '{item_type}' in {path}")]
    UnknownType { item_type: String, path: String },
}

pub type Result<T> = std::result::Result<T, MigrateError>;

/// A parsed item from a markdown file (before Arrow conversion).
#[derive(Debug, Clone)]
pub struct ParsedItem {
    pub id: String,
    pub title: String,
    pub item_type: ItemType,
    pub status: String,
    pub priority: Option<String>,
    pub created: Option<String>,
    pub assignee: Option<String>,
    pub board: String,
    pub tags: Vec<String>,
    pub related: Vec<String>,
    pub depends_on: Vec<String>,
    pub body: String,
    pub body_hash: String,
    pub source_path: PathBuf,
}

/// A parsed status change from a Turtle block.
#[derive(Debug, Clone)]
pub struct ParsedRun {
    pub item_id: String,
    pub to_status: String,
    pub timestamp: Option<String>,
    pub by_agent: Option<String>,
    pub forced: bool,
}

/// A parsed relation from frontmatter fields.
#[derive(Debug, Clone)]
pub struct ParsedRelation {
    pub source_id: String,
    pub target_id: String,
    pub predicate: String,
}

/// Result of migrating all boards.
#[derive(Debug)]
pub struct MigrateResult {
    pub items: Vec<ParsedItem>,
    pub runs: Vec<ParsedRun>,
    pub relations: Vec<ParsedRelation>,
    pub errors: Vec<(PathBuf, String)>,
}

impl MigrateResult {
    /// Build an Items RecordBatch from parsed items.
    pub fn items_batch(&self) -> Result<RecordBatch> {
        build_items_batch(&self.items)
    }

    /// Build a Runs RecordBatch from parsed runs.
    pub fn runs_batch(&self) -> Result<RecordBatch> {
        build_runs_batch(&self.runs)
    }

    /// Build a Relations RecordBatch from parsed relations.
    pub fn relations_batch(&self) -> Result<RecordBatch> {
        build_relations_batch(&self.relations)
    }

    /// Load migration results into a KanbanStore and RelationsStore.
    pub fn into_stores(&self) -> Result<(KanbanStore, RelationsStore)> {
        let mut store = KanbanStore::new();
        let mut rel_store = RelationsStore::new();

        if !self.items.is_empty() {
            let items_batch = self.items_batch()?;
            store.load_items(vec![items_batch]);
        }

        if !self.runs.is_empty() {
            let runs_batch = self.runs_batch()?;
            store.load_runs(vec![runs_batch]);
        }

        if !self.relations.is_empty() {
            let rel_batch = self.relations_batch()?;
            rel_store.load(vec![rel_batch]);
        }

        Ok((store, rel_store))
    }

    /// Human-readable summary.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        s.push_str("=== Migration Summary ===\n");
        s.push_str(&format!("Items migrated: {}\n", self.items.len()));
        s.push_str(&format!("Status changes: {}\n", self.runs.len()));
        s.push_str(&format!("Relations: {}\n", self.relations.len()));

        // Count by type
        let mut type_counts: HashMap<&str, usize> = HashMap::new();
        for item in &self.items {
            *type_counts.entry(item.item_type.as_str()).or_default() += 1;
        }
        let mut sorted: Vec<_> = type_counts.into_iter().collect();
        sorted.sort_by_key(|(k, _)| *k);
        for (typ, count) in &sorted {
            s.push_str(&format!("  {typ}: {count}\n"));
        }

        // Count by board
        let dev = self
            .items
            .iter()
            .filter(|i| i.board == "development")
            .count();
        let res = self.items.iter().filter(|i| i.board == "research").count();
        s.push_str(&format!("Development board: {dev}\n"));
        s.push_str(&format!("Research board: {res}\n"));

        if !self.errors.is_empty() {
            let dupes: Vec<_> = self
                .errors
                .iter()
                .filter(|(_, e)| e.contains("Duplicate ID"))
                .collect();
            let parse_errs: Vec<_> = self
                .errors
                .iter()
                .filter(|(_, e)| !e.contains("Duplicate ID"))
                .collect();

            if !dupes.is_empty() {
                s.push_str(&format!(
                    "Duplicate IDs: {} (renamed with .N suffix)\n",
                    dupes.len()
                ));
                for (path, err) in &dupes {
                    s.push_str(&format!("  {}: {}\n", path.display(), err));
                }
            }
            if !parse_errs.is_empty() {
                s.push_str(&format!("Parse errors: {}\n", parse_errs.len()));
                for (path, err) in &parse_errs {
                    s.push_str(&format!("  {}: {}\n", path.display(), err));
                }
            }
        }

        s
    }
}

/// Migrate all boards defined in config.
pub fn migrate_boards(root: &Path, config: &ConfigFile) -> Result<MigrateResult> {
    let mut all_items = Vec::new();
    let mut all_runs = Vec::new();
    let mut all_relations = Vec::new();
    let mut all_errors = Vec::new();

    for board in &config.boards {
        let result = migrate_board(root, board)?;
        all_items.extend(result.items);
        all_runs.extend(result.runs);
        all_relations.extend(result.relations);
        all_errors.extend(result.errors);
    }

    // Deduplicate IDs — if two files share the same id: field, append a .N suffix
    // to subsequent occurrences (e.g., EXP-1179 → EXP-1179.1). No data is lost.
    // Runs and relations are left pointing to the original ID since we can't
    // distinguish which file they came from after parsing.
    let mut seen_ids: HashMap<String, usize> = HashMap::new();
    for item in &mut all_items {
        let count = seen_ids.entry(item.id.clone()).or_insert(0);
        if *count > 0 {
            let new_id = format!("{}.{}", item.id, count);
            all_errors.push((
                item.source_path.clone(),
                format!("Duplicate ID '{}' renamed to '{new_id}'", item.id),
            ));
            item.id = new_id;
        }
        *count += 1;
    }

    Ok(MigrateResult {
        items: all_items,
        runs: all_runs,
        relations: all_relations,
        errors: all_errors,
    })
}

/// Migrate a single board by walking its scan_paths.
pub fn migrate_board(root: &Path, board: &BoardConfig) -> Result<MigrateResult> {
    let mut items = Vec::new();
    let mut runs = Vec::new();
    let mut relations = Vec::new();
    let mut errors = Vec::new();

    // Compile ignore patterns
    let ignore_patterns: Vec<Regex> = board
        .ignore
        .iter()
        .filter_map(|p| glob_to_regex(p))
        .collect();

    for scan_path in &board.scan_paths {
        let dir = root.join(scan_path);
        if !dir.is_dir() {
            continue;
        }

        let md_files = collect_markdown_files(&dir)?;

        for file_path in &md_files {
            // Check ignore patterns
            let rel_path = file_path
                .strip_prefix(root)
                .unwrap_or(file_path)
                .to_string_lossy();
            if ignore_patterns.iter().any(|re| re.is_match(&rel_path)) {
                continue;
            }

            match parse_markdown_file(file_path, &board.name) {
                Ok((parsed, content)) => {
                    // Extract relations from frontmatter
                    let file_relations = extract_relations(&parsed);
                    relations.extend(file_relations);

                    // Extract runs from turtle blocks (reuses content already read)
                    let file_runs = parse_turtle_blocks(&parsed.id, &content);
                    runs.extend(file_runs);

                    items.push(parsed);
                }
                Err(e) => {
                    errors.push((file_path.clone(), e.to_string()));
                }
            }
        }
    }

    Ok(MigrateResult {
        items,
        runs,
        relations,
        errors,
    })
}

/// Parse a single markdown file with YAML frontmatter.
///
/// Returns the parsed item and the raw file content (to avoid re-reading for
/// turtle block extraction).
pub fn parse_markdown_file(path: &Path, board_name: &str) -> Result<(ParsedItem, String)> {
    let content = std::fs::read_to_string(path).map_err(MigrateError::Io)?;
    let path_str = path.display().to_string();

    // Split frontmatter and body
    let (frontmatter, body) = split_frontmatter(&content).ok_or_else(|| MigrateError::Yaml {
        path: path_str.clone(),
        message: "No YAML frontmatter found (expected --- delimiters)".to_string(),
    })?;

    // Parse YAML — if duplicate keys exist, deduplicate (keep last) and retry
    let yaml: serde_yaml::Value = serde_yaml::from_str(&frontmatter)
        .or_else(|e| {
            if e.to_string().contains("duplicate entry") {
                let deduped = dedup_yaml_keys(&frontmatter);
                serde_yaml::from_str(&deduped)
            } else {
                Err(e)
            }
        })
        .map_err(|e| MigrateError::Yaml {
            path: path_str.clone(),
            message: e.to_string(),
        })?;

    let map = yaml.as_mapping().ok_or_else(|| MigrateError::Yaml {
        path: path_str.clone(),
        message: "Frontmatter is not a YAML mapping".to_string(),
    })?;

    // Extract ID — from frontmatter `id:` field, or infer from filename.
    // Files without an explicit `id:` must have a `status:` field AND either a
    // recognized `type:` or a recognizable ID prefix to be considered a kanban item.
    // This prevents ingesting non-kanban documents (READMEs, analysis reports, etc.)
    // that happen to have `status:` in their YAML.
    let id = match get_str(map, "id") {
        Some(id) => id,
        None => {
            if get_str(map, "status").is_some() {
                infer_id_from_path(path)
            } else {
                return Err(MigrateError::MissingField {
                    field: "id".to_string(),
                    path: path_str.clone(),
                });
            }
        }
    };

    let title = get_str(map, "title").unwrap_or_else(|| id.clone());

    let type_str = get_str(map, "type").unwrap_or_default();
    let item_type = match ItemType::from_str_loose(&type_str) {
        Some(t) => t,
        None => {
            if !type_str.is_empty() {
                // File has a `type:` field with a non-kanban value (e.g.,
                // "expedition-report", "technical-doc", "review-doc").
                // These are supporting documents, not kanban items — skip them.
                return Err(MigrateError::UnknownType {
                    item_type: type_str,
                    path: path_str,
                });
            }
            // type: field is empty/missing — try to infer from ID prefix,
            // fall back to Chore for legacy items without type metadata.
            infer_type_from_id(&id).unwrap_or(ItemType::Chore)
        }
    };

    let raw_status = get_str(map, "status").unwrap_or_else(|| "backlog".to_string());
    let status = normalize_status(&raw_status, board_name);
    let priority = get_str(map, "priority");
    let created = get_str(map, "created");
    let assignee = get_str(map, "assignee");
    let tags = get_str_list(map, "tags");
    let related = get_str_list(map, "related");
    let depends_on = get_str_list(map, "depends_on");

    // Body hash (SHA-256 of content after frontmatter)
    let body_hash = sha256_hex(&body);

    Ok((
        ParsedItem {
            id,
            title,
            item_type,
            status,
            priority,
            created,
            assignee,
            board: board_name.to_string(),
            tags,
            related,
            depends_on,
            body: body.clone(),
            body_hash,
            source_path: path.to_path_buf(),
        },
        content,
    ))
}

/// Extract relations from a parsed item's frontmatter fields.
pub fn extract_relations(item: &ParsedItem) -> Vec<ParsedRelation> {
    let mut relations = Vec::new();

    for target in &item.related {
        relations.push(ParsedRelation {
            source_id: item.id.clone(),
            target_id: target.clone(),
            predicate: "related_to".to_string(),
        });
    }

    for target in &item.depends_on {
        relations.push(ParsedRelation {
            source_id: item.id.clone(),
            target_id: target.clone(),
            predicate: "blocked_by".to_string(),
        });
    }

    relations
}

/// Parse Turtle status-change blocks from file content.
///
/// Extracts `kb:statusChange` blocks with status, timestamp, agent, forced flag.
pub fn parse_turtle_blocks(item_id: &str, content: &str) -> Vec<ParsedRun> {
    let mut runs = Vec::new();
    let re = turtle_regexes();

    for block_match in re.block.captures_iter(content) {
        let block = &block_match[1];

        // A block can contain multiple status changes separated by ],
        // Split on status change boundaries
        let changes: Vec<&str> = split_status_changes(block);

        for change in changes {
            if let Some(status_cap) = re.status.captures(change) {
                let to_status = status_cap[1].to_string();
                let timestamp = re.timestamp.captures(change).map(|c| c[1].to_string());
                let by_agent = re.agent.captures(change).map(|c| c[1].to_string());
                let forced = re.forced.is_match(change);

                runs.push(ParsedRun {
                    item_id: item_id.to_string(),
                    to_status,
                    timestamp,
                    by_agent,
                    forced,
                });
            }
        }
    }

    runs
}

/// Split a turtle block into individual status change entries.
fn split_status_changes(block: &str) -> Vec<&str> {
    // Status changes are separated by ], within the kb:statusChange [...] structure.
    // Each change starts with [ or is the first entry.
    let mut changes = Vec::new();
    let mut start = 0;

    // Find each [ ... ] or [ ... , block
    for (i, c) in block.char_indices() {
        if c == '[' {
            start = i;
        } else if c == ']' {
            changes.push(&block[start..=i]);
        }
    }

    // If no brackets found, treat the whole block as one change
    if changes.is_empty() {
        changes.push(block);
    }

    changes
}

// ── Arrow batch builders ────────────────────────────────────────────

/// Build an Items RecordBatch from parsed items.
fn build_items_batch(items: &[ParsedItem]) -> Result<RecordBatch> {
    let schema = items_schema();
    let n = items.len();

    let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
    let titles: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
    let types: Vec<&str> = items.iter().map(|i| i.item_type.as_str()).collect();
    let statuses: Vec<&str> = items.iter().map(|i| i.status.as_str()).collect();
    let priorities: Vec<Option<&str>> = items.iter().map(|i| i.priority.as_deref()).collect();
    let assignees: Vec<Option<&str>> = items.iter().map(|i| i.assignee.as_deref()).collect();
    let boards: Vec<&str> = items.iter().map(|i| i.board.as_str()).collect();
    let bodies: Vec<Option<&str>> = items
        .iter()
        .map(|i| {
            let trimmed = i.body.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .collect();
    let body_hashes: Vec<Option<&str>> = items
        .iter()
        .zip(bodies.iter())
        .map(|(i, body)| {
            if body.is_some() {
                Some(i.body_hash.as_str())
            } else {
                None
            }
        })
        .collect();
    let deleteds: Vec<bool> = vec![false; n];

    // Parse created dates to timestamps
    let created_ms: Vec<i64> = items
        .iter()
        .map(|i| parse_date_to_millis(i.created.as_deref()))
        .collect();

    // Build list columns
    let mut tags_builder = ListBuilder::new(StringBuilder::new());
    for item in items {
        for tag in &item.tags {
            tags_builder.values().append_value(tag);
        }
        tags_builder.append(true);
    }

    let mut related_builder = ListBuilder::new(StringBuilder::new());
    for item in items {
        for rel in &item.related {
            related_builder.values().append_value(rel);
        }
        related_builder.append(true);
    }

    let mut depends_builder = ListBuilder::new(StringBuilder::new());
    for item in items {
        for dep in &item.depends_on {
            depends_builder.values().append_value(dep);
        }
        depends_builder.append(true);
    }

    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(ids)),
            Arc::new(StringArray::from(titles)),
            Arc::new(StringArray::from(types)),
            Arc::new(StringArray::from(statuses)),
            Arc::new(StringArray::from(priorities)),
            Arc::new(TimestampMillisecondArray::from(created_ms).with_timezone("UTC")),
            Arc::new(StringArray::from(assignees)),
            Arc::new(StringArray::from(boards)),
            Arc::new(tags_builder.finish()),
            Arc::new(related_builder.finish()),
            Arc::new(depends_builder.finish()),
            Arc::new(StringArray::from(bodies)),
            Arc::new(StringArray::from(body_hashes)),
            Arc::new(BooleanArray::from(deleteds)),
            Arc::new(StringArray::from(vec![None::<&str>; n])), // resolution
            Arc::new(StringArray::from(vec![None::<&str>; n])), // closed_by
            Arc::new(TimestampMillisecondArray::from(vec![None::<i64>; n]).with_timezone("UTC")), // updated_at
            Arc::new(arrow::array::Int32Array::from(vec![None::<i32>; n])), // priority_rank
        ],
    )?;

    Ok(batch)
}

/// Build a Runs RecordBatch from parsed runs.
fn build_runs_batch(runs: &[ParsedRun]) -> Result<RecordBatch> {
    let schema = runs_schema();

    let run_ids: Vec<String> = (0..runs.len())
        .map(|_| uuid::Uuid::new_v4().to_string())
        .collect();
    let run_id_refs: Vec<&str> = run_ids.iter().map(|s| s.as_str()).collect();
    let item_ids: Vec<&str> = runs.iter().map(|r| r.item_id.as_str()).collect();
    let from_statuses: Vec<Option<&str>> = vec![None; runs.len()]; // from_status not in turtle
    let to_statuses: Vec<&str> = runs.iter().map(|r| r.to_status.as_str()).collect();
    let agents: Vec<Option<&str>> = runs.iter().map(|r| r.by_agent.as_deref()).collect();
    let forceds: Vec<bool> = runs.iter().map(|r| r.forced).collect();
    let reasons: Vec<Option<&str>> = vec![None; runs.len()];

    // Parse timestamps
    let timestamps: Vec<i64> = runs
        .iter()
        .map(|r| parse_datetime_to_millis(r.timestamp.as_deref()))
        .collect();

    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(run_id_refs)),
            Arc::new(StringArray::from(item_ids)),
            Arc::new(StringArray::from(from_statuses)),
            Arc::new(StringArray::from(to_statuses)),
            Arc::new(TimestampMillisecondArray::from(timestamps).with_timezone("UTC")),
            Arc::new(StringArray::from(agents)),
            Arc::new(BooleanArray::from(forceds)),
            Arc::new(StringArray::from(reasons)),
        ],
    )?;

    Ok(batch)
}

/// Build a Relations RecordBatch from parsed relations.
fn build_relations_batch(relations: &[ParsedRelation]) -> Result<RecordBatch> {
    let schema = relations_schema();

    let rel_ids: Vec<String> = (0..relations.len())
        .map(|_| uuid::Uuid::new_v4().to_string())
        .collect();
    let rel_id_refs: Vec<&str> = rel_ids.iter().map(|s| s.as_str()).collect();
    let source_ids: Vec<&str> = relations.iter().map(|r| r.source_id.as_str()).collect();
    let target_ids: Vec<&str> = relations.iter().map(|r| r.target_id.as_str()).collect();
    let predicates: Vec<&str> = relations.iter().map(|r| r.predicate.as_str()).collect();
    let deleteds: Vec<bool> = vec![false; relations.len()];

    let now_ms = chrono::Utc::now().timestamp_millis();
    let timestamps: Vec<i64> = vec![now_ms; relations.len()];

    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(rel_id_refs)),
            Arc::new(StringArray::from(source_ids)),
            Arc::new(StringArray::from(target_ids)),
            Arc::new(StringArray::from(predicates)),
            Arc::new(TimestampMillisecondArray::from(timestamps).with_timezone("UTC")),
            Arc::new(BooleanArray::from(deleteds)),
        ],
    )?;

    Ok(batch)
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Split file content into (frontmatter, body).
fn split_frontmatter(content: &str) -> Option<(String, String)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }

    // Find the closing ---
    let after_first = &trimmed[3..];
    let end = after_first.find("\n---")?;
    let frontmatter = after_first[..end].trim().to_string();
    let body = after_first[end + 4..].to_string();

    Some((frontmatter, body))
}

/// Get a string value from a YAML mapping.
fn get_str(map: &serde_yaml::Mapping, key: &str) -> Option<String> {
    let val = map.get(serde_yaml::Value::String(key.to_string()))?;
    match val {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Get a list of strings from a YAML mapping.
fn get_str_list(map: &serde_yaml::Mapping, key: &str) -> Vec<String> {
    let Some(val) = map.get(serde_yaml::Value::String(key.to_string())) else {
        return Vec::new();
    };

    match val {
        serde_yaml::Value::Sequence(seq) => seq
            .iter()
            .filter_map(|v| match v {
                serde_yaml::Value::String(s) => Some(s.clone()),
                serde_yaml::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect(),
        serde_yaml::Value::String(s) => {
            // Single string — could be comma-separated
            s.split(',').map(|p| p.trim().to_string()).collect()
        }
        _ => Vec::new(),
    }
}

/// Parse a date string (YYYY-MM-DD or ISO 8601) to epoch millis.
fn parse_date_to_millis(date_str: Option<&str>) -> i64 {
    let Some(s) = date_str else {
        return 0;
    };

    // Try ISO 8601 datetime first
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return dt.and_utc().timestamp_millis();
    }

    // Try date only
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return d
            .and_hms_opt(0, 0, 0)
            .expect("midnight is valid")
            .and_utc()
            .timestamp_millis();
    }

    0
}

/// Parse a datetime string from Turtle (ISO 8601 with optional timezone) to epoch millis.
fn parse_datetime_to_millis(dt_str: Option<&str>) -> i64 {
    let Some(s) = dt_str else {
        return 0;
    };

    // Try parsing various formats
    // "2026-03-14T18:14:29"
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return dt.and_utc().timestamp_millis();
    }

    // "2026-03-14T18:14:29.123"
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
        return dt.and_utc().timestamp_millis();
    }

    0
}

/// SHA-256 hash of a string, returned as hex.
fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Infer an item ID from a file path when `id:` is missing from frontmatter.
///
/// Converts filename (without .md) to an ID using underscores for separators,
/// matching yurtle-kanban's behavior for legacy items.
fn infer_id_from_path(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .replace('-', "_")
        .to_uppercase()
}

/// Remove duplicate top-level YAML keys, keeping the last occurrence.
/// This handles legacy files where `depends_on:` appears twice.
fn dedup_yaml_keys(yaml: &str) -> String {
    let mut seen_keys: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let lines: Vec<&str> = yaml.lines().collect();

    // First pass: find which keys have duplicates, record last occurrence
    for (i, line) in lines.iter().enumerate() {
        if !line.starts_with(' ')
            && !line.starts_with('#')
            && let Some(colon_pos) = line.find(':')
        {
            let key = line[..colon_pos].trim().to_string();
            seen_keys.insert(key, i);
        }
    }

    // Second pass: keep lines whose key's last occurrence is this line
    // (or lines that aren't top-level keys)
    let mut result = Vec::new();
    let mut skip_until_next_key = false;
    for (i, line) in lines.iter().enumerate() {
        if !line.starts_with(' ')
            && !line.starts_with('#')
            && let Some(colon_pos) = line.find(':')
        {
            let key = line[..colon_pos].trim().to_string();
            if seen_keys.get(&key) == Some(&i) {
                skip_until_next_key = false;
                result.push(*line);
            } else {
                skip_until_next_key = true;
            }
            continue;
        }
        if !skip_until_next_key {
            result.push(*line);
        }
    }

    result.join("\n")
}

/// Infer item type from the ID prefix when `type:` is missing from frontmatter.
/// Many older items (pre-V11) don't have a `type:` field but follow the
/// convention `EXP-123`, `CHORE-045`, `VOY-10`, etc.
fn infer_type_from_id(id: &str) -> Option<ItemType> {
    let upper = id.to_uppercase();
    // Accept both old (EXP-) and new (EX-) prefixes, plus dash/underscore separators
    if upper.starts_with("EXP-") || upper.starts_with("EXP_") || upper.starts_with("EX-") {
        Some(ItemType::Expedition)
    } else if upper.starts_with("CHORE-") || upper.starts_with("CH-") {
        Some(ItemType::Chore)
    } else if upper.starts_with("VOY-") || upper.starts_with("VY-") {
        Some(ItemType::Voyage)
    } else if upper.starts_with("HAZ-") || upper.starts_with("HZ-") {
        Some(ItemType::Hazard)
    } else if upper.starts_with("SIG-") || upper.starts_with("SG-") {
        Some(ItemType::Signal)
    } else if upper.starts_with("FEAT-") || upper.starts_with("FT-") {
        Some(ItemType::Feature)
    } else if upper.starts_with("PAPER-") {
        Some(ItemType::Paper)
    } else if upper.starts_with("EXPR-") {
        Some(ItemType::Experiment)
    } else if upper.starts_with("IDEA-") {
        Some(ItemType::Idea)
    } else if upper.starts_with("LIT-") {
        Some(ItemType::Literature)
    } else if upper.starts_with("M-") {
        Some(ItemType::Measure)
    } else if upper.starts_with('H') && upper.chars().nth(1).is_some_and(|c| c.is_ascii_digit()) {
        Some(ItemType::Hypothesis)
    } else {
        None
    }
}

/// Normalize a status string to canonical form for the given board.
///
/// Maps legacy/variant statuses to their canonical equivalents. If the
/// normalized status isn't valid for the board, falls back to the board's
/// default state (backlog for dev, draft for research).
fn normalize_status(raw: &str, board_name: &str) -> String {
    let lower = raw.to_lowercase();

    let canonical = match lower.as_str() {
        // ── Canonical statuses — pass through ──
        "backlog" | "planning" | "ready" | "in_progress" | "review" | "done" => lower,
        "draft" | "active" | "complete" | "abandoned" | "retired" => lower,

        // ── Per-type research states ──
        // Hypothesis: "testing"/"untested" are legacy → canonicalize to "active"
        // (hypothesis stays active; experiment carries the per-version result)
        "testing" | "untested" => "active".to_string(),
        // Paper states (outline/writing preserved; combined → complete)
        "outline" | "writing" => lower,
        "combined" => "complete".to_string(),
        // Experiment states
        "planned" | "running" => lower,
        // Idea states
        "captured" | "formalized" => lower,

        // ── Done variants (dev board) ──
        "completed" | "closed" | "merged" | "merged_awaiting_v7" => "done".to_string(),

        // ── In-progress variants (dev board) ──
        "in-progress" | "in progress" | "wip" | "underway" | "sailing" | "implementation" => {
            "in_progress".to_string()
        }

        // ── Backlog variants (dev board) ──
        "queued" | "pending" | "intake" | "loading" | "proposal" | "proposed" | "correction"
        | "cancelled" => "backlog".to_string(),

        // ── Abandoned/stranded ──
        "stranded" | "rejected" => "abandoned".to_string(),

        // ── Research normalizations ──
        // "validated"/"refuted" are experiment results, not hypothesis states.
        // Hypotheses with these in frontmatter should be "active" (the hypothesis
        // itself is still active — the experiment validated/refuted it per-version).
        "validated" | "confirmed" | "proven" => "active".to_string(),
        "refuted" => "active".to_string(),

        // ── Truncated "in" ──
        "in" => "in_progress".to_string(),

        // ── Unknown — keep as-is but lowercase ──
        other => other.to_string(),
    };

    // Board-specific fallback: if the normalized status isn't valid for this
    // board, map to the board's default state.
    let dev_states = [
        "backlog",
        "planning",
        "ready",
        "in_progress",
        "review",
        "done",
    ];
    // Research board accepts all per-type canonical states
    // (testing/untested/combined are mapped above, not listed here)
    let research_states = [
        "draft",
        "active",
        "complete",
        "abandoned",
        "retired",
        // Paper
        "outline",
        "writing",
        // Experiment
        "planned",
        "running",
        // Idea
        "captured",
        "formalized",
    ];

    let valid_states: &[&str] = match board_name {
        "development" => &dev_states,
        "research" => &research_states,
        _ => return canonical,
    };

    if valid_states.contains(&canonical.as_str()) {
        canonical
    } else {
        // Default to first state (backlog for dev, draft for research)
        valid_states[0].to_string()
    }
}

/// Convert a simple glob pattern to a regex.
fn glob_to_regex(glob: &str) -> Option<Regex> {
    let mut re = String::from("(?i)");
    for c in glob.chars() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '.' => re.push_str("\\."),
            _ => re.push(c),
        }
    }
    Regex::new(&re).ok()
}

/// Recursively collect all `.md` files in a directory.
fn collect_markdown_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_md_recursive(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_md_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            collect_md_recursive(&path, files)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::items_col;

    #[test]
    fn test_split_frontmatter() {
        let content = "---\nid: EXP-1\ntitle: Test\n---\n\n# Body\n";
        let (fm, body) = split_frontmatter(content).expect("should parse");
        assert!(fm.contains("id: EXP-1"));
        assert!(body.contains("# Body"));
    }

    #[test]
    fn test_split_frontmatter_no_frontmatter() {
        let content = "# Just a heading\nNo frontmatter here.";
        assert!(split_frontmatter(content).is_none());
    }

    #[test]
    fn test_parse_markdown_file() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let file_path = dir.path().join("EXP-42-Test.md");
        std::fs::write(
            &file_path,
            r#"---
id: EXP-42
title: "Test Expedition"
type: expedition
status: in_progress
priority: high
created: 2026-03-14
assignee: DGX
tags: [v14, arrow]
related: [VOY-145, EXP-41]
depends_on: [EXP-40]
---

# EXP-42: Test Expedition

Some body content here.
"#,
        )
        .expect("write file");

        let (item, _content) = parse_markdown_file(&file_path, "development").expect("parse");
        assert_eq!(item.id, "EXP-42");
        assert_eq!(item.title, "Test Expedition");
        assert_eq!(item.item_type, ItemType::Expedition);
        assert_eq!(item.status, "in_progress");
        assert_eq!(item.priority.as_deref(), Some("high"));
        assert_eq!(item.assignee.as_deref(), Some("DGX"));
        assert_eq!(item.board, "development");
        assert_eq!(item.tags, vec!["v14", "arrow"]);
        assert_eq!(item.related, vec!["VOY-145", "EXP-41"]);
        assert_eq!(item.depends_on, vec!["EXP-40"]);
        assert!(!item.body_hash.is_empty());
    }

    #[test]
    fn test_parse_markdown_missing_id_infers_from_filename() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let file_path = dir.path().join("MY-ITEM.md");
        std::fs::write(
            &file_path,
            "---\ntitle: No ID\ntype: expedition\nstatus: backlog\n---\nBody\n",
        )
        .expect("write");

        // ID should be inferred from filename
        let result = parse_markdown_file(&file_path, "development");
        assert!(result.is_ok(), "should succeed with inferred ID");
        let (item, _) = result.unwrap();
        assert_eq!(item.id, "MY_ITEM");
        assert_eq!(item.title, "No ID");
    }

    #[test]
    fn test_parse_markdown_no_frontmatter() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let file_path = dir.path().join("no-fm.md");
        std::fs::write(&file_path, "# Just a heading\nNo frontmatter.\n").expect("write");

        let result = parse_markdown_file(&file_path, "development");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_turtle_blocks_single() {
        let content = r#"
# Some heading

```yurtle
@prefix kb: <https://yurtle.dev/kanban/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

<> kb:statusChange [
    kb:status kb:done ;
    kb:at "2026-03-10T15:47:20"^^xsd:dateTime ;
    kb:by "DGX" ;
  ] .
```
"#;

        let runs = parse_turtle_blocks("EXP-100", content);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].item_id, "EXP-100");
        assert_eq!(runs[0].to_status, "done");
        assert_eq!(runs[0].by_agent.as_deref(), Some("DGX"));
        assert!(!runs[0].forced);
        assert!(runs[0].timestamp.is_some());
    }

    #[test]
    fn test_parse_turtle_blocks_multiple() {
        let content = r#"
```yurtle
@prefix kb: <https://yurtle.dev/kanban/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

<> kb:statusChange [
    kb:status kb:in_progress ;
    kb:at "2026-03-04T07:03:19"^^xsd:dateTime ;
    kb:by "hankh1844" ;
    kb:forcedMove "true"^^xsd:boolean ;
  ],
  [
    kb:status kb:done ;
    kb:at "2026-03-04T12:06:40"^^xsd:dateTime ;
    kb:by "hankh1844" ;
  ] .
```
"#;

        let runs = parse_turtle_blocks("CHORE-102", content);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].to_status, "in_progress");
        assert!(runs[0].forced);
        assert_eq!(runs[1].to_status, "done");
        assert!(!runs[1].forced);
    }

    #[test]
    fn test_parse_turtle_blocks_no_blocks() {
        let content = "# Just markdown\nNo turtle blocks here.\n";
        let runs = parse_turtle_blocks("EXP-1", content);
        assert!(runs.is_empty());
    }

    #[test]
    fn test_extract_relations() {
        let item = ParsedItem {
            id: "EXP-42".to_string(),
            title: "Test".to_string(),
            item_type: ItemType::Expedition,
            status: "backlog".to_string(),
            priority: None,
            created: None,
            assignee: None,
            board: "development".to_string(),
            tags: vec![],
            related: vec!["VOY-145".to_string(), "EXP-41".to_string()],
            depends_on: vec!["EXP-40".to_string()],
            body: String::new(),
            body_hash: String::new(),
            source_path: PathBuf::new(),
        };

        let relations = extract_relations(&item);
        assert_eq!(relations.len(), 3);

        // related_to relations
        assert_eq!(relations[0].source_id, "EXP-42");
        assert_eq!(relations[0].target_id, "VOY-145");
        assert_eq!(relations[0].predicate, "related_to");

        assert_eq!(relations[1].target_id, "EXP-41");
        assert_eq!(relations[1].predicate, "related_to");

        // blocked_by relation
        assert_eq!(relations[2].target_id, "EXP-40");
        assert_eq!(relations[2].predicate, "blocked_by");
    }

    #[test]
    fn test_build_items_batch() {
        let items = vec![ParsedItem {
            id: "EXP-1".to_string(),
            title: "Test".to_string(),
            item_type: ItemType::Expedition,
            status: "backlog".to_string(),
            priority: Some("high".to_string()),
            created: Some("2026-03-14".to_string()),
            assignee: Some("DGX".to_string()),
            board: "development".to_string(),
            tags: vec!["v14".to_string()],
            related: vec!["VOY-1".to_string()],
            depends_on: vec![],
            body: "# EXP-1: Test\n\nSome content.".to_string(),
            body_hash: "abc123".to_string(),
            source_path: PathBuf::new(),
        }];

        let batch = build_items_batch(&items).expect("build batch");
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), 18);

        let ids = batch
            .column(items_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("ids");
        assert_eq!(ids.value(0), "EXP-1");
    }

    #[test]
    fn test_build_runs_batch() {
        let runs = vec![ParsedRun {
            item_id: "EXP-1".to_string(),
            to_status: "done".to_string(),
            timestamp: Some("2026-03-14T10:00:00".to_string()),
            by_agent: Some("DGX".to_string()),
            forced: false,
        }];

        let batch = build_runs_batch(&runs).expect("build batch");
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), 8);
    }

    #[test]
    fn test_build_relations_batch() {
        let relations = vec![ParsedRelation {
            source_id: "EXP-1".to_string(),
            target_id: "VOY-1".to_string(),
            predicate: "related_to".to_string(),
        }];

        let batch = build_relations_batch(&relations).expect("build batch");
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), 6);
    }

    #[test]
    fn test_parse_date_to_millis() {
        let ms = parse_date_to_millis(Some("2026-03-14"));
        assert!(ms > 0);

        let ms_dt = parse_date_to_millis(Some("2026-03-14T10:00:00"));
        assert!(ms_dt > ms); // datetime should be later in the day

        let ms_none = parse_date_to_millis(None);
        assert_eq!(ms_none, 0);
    }

    #[test]
    fn test_parse_datetime_to_millis() {
        let ms = parse_datetime_to_millis(Some("2026-03-14T18:14:29"));
        assert!(ms > 0);

        let ms_none = parse_datetime_to_millis(None);
        assert_eq!(ms_none, 0);
    }

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex("hello");
        assert_eq!(hash.len(), 64); // SHA-256 is 32 bytes = 64 hex chars
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_normalize_status() {
        // Canonical dev statuses pass through
        assert_eq!(normalize_status("done", "development"), "done");
        assert_eq!(
            normalize_status("in_progress", "development"),
            "in_progress"
        );

        // Canonical research statuses pass through
        assert_eq!(normalize_status("active", "research"), "active");
        assert_eq!(normalize_status("abandoned", "research"), "abandoned");

        // Case normalization
        assert_eq!(normalize_status("DONE", "development"), "done");
        assert_eq!(normalize_status("Backlog", "development"), "backlog");

        // Legacy status normalization
        assert_eq!(normalize_status("completed", "development"), "done");
        assert_eq!(
            normalize_status("in-progress", "development"),
            "in_progress"
        );
        assert_eq!(normalize_status("sailing", "development"), "in_progress");
        assert_eq!(normalize_status("planned", "development"), "backlog");
        assert_eq!(normalize_status("proposal", "development"), "backlog");
        assert_eq!(normalize_status("cancelled", "development"), "backlog");
        assert_eq!(normalize_status("in", "development"), "in_progress");

        // Board-specific fallback: "abandoned" is valid for research but not dev
        assert_eq!(normalize_status("abandoned", "development"), "backlog");
        assert_eq!(normalize_status("stranded", "development"), "backlog");
        assert_eq!(normalize_status("stranded", "research"), "abandoned");

        // Unknown status on dev board — falls back to backlog
        assert_eq!(normalize_status("custom_state", "development"), "backlog");
    }

    #[test]
    fn test_normalize_status_per_type_research() {
        // Per-type states preserved on research board
        // "testing"/"untested" → "active" (hypothesis stays active, experiment has result)
        assert_eq!(normalize_status("testing", "research"), "active");
        assert_eq!(normalize_status("untested", "research"), "active");
        assert_eq!(normalize_status("outline", "research"), "outline");
        assert_eq!(normalize_status("writing", "research"), "writing");
        assert_eq!(normalize_status("planned", "research"), "planned");
        assert_eq!(normalize_status("running", "research"), "running");
        assert_eq!(normalize_status("captured", "research"), "captured");
        assert_eq!(normalize_status("formalized", "research"), "formalized");
        // "combined" → "complete" (paper merged into another)
        assert_eq!(normalize_status("combined", "research"), "complete");
        assert_eq!(normalize_status("retired", "research"), "retired");

        // "validated"/"refuted" → "active" (hypothesis stays active,
        // experiment carries the per-version result)
        assert_eq!(normalize_status("validated", "research"), "active");
        assert_eq!(normalize_status("refuted", "research"), "active");

        // "planned" preserved on research (experiment state)
        assert_eq!(normalize_status("planned", "research"), "planned");
        // but "planned" on dev board → backlog
        assert_eq!(normalize_status("planned", "development"), "backlog");
    }

    #[test]
    fn test_glob_to_regex() {
        let re = glob_to_regex("**/archive/**").expect("valid");
        assert!(re.is_match("kanban-work/expeditions/archive/old.md"));
        assert!(!re.is_match("kanban-work/expeditions/EXP-1.md"));
    }

    #[test]
    fn test_migrate_board_from_temp_dir() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let exp_dir = dir.path().join("kanban-work/expeditions");
        std::fs::create_dir_all(&exp_dir).expect("create dir");

        // Write two expedition files
        std::fs::write(
            exp_dir.join("EXP-1-Test.md"),
            r#"---
id: EXP-1
title: "First Expedition"
type: expedition
status: done
priority: medium
created: 2026-01-01
tags: [test]
related: [EXP-2]
depends_on: []
---

# EXP-1: First Expedition

Body content.

```yurtle
@prefix kb: <https://yurtle.dev/kanban/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

<> kb:statusChange [
    kb:status kb:done ;
    kb:at "2026-01-15T10:00:00"^^xsd:dateTime ;
    kb:by "DGX" ;
  ] .
```
"#,
        )
        .expect("write");

        std::fs::write(
            exp_dir.join("EXP-2-Another.md"),
            r#"---
id: EXP-2
title: "Second Expedition"
type: expedition
status: in_progress
priority: high
created: 2026-02-01
assignee: M5
tags: [v14]
related: []
depends_on: [EXP-1]
---

# EXP-2: Second Expedition

Another body.
"#,
        )
        .expect("write");

        let board = BoardConfig {
            name: "development".to_string(),
            preset: "nautical".to_string(),
            path: "kanban-work/".to_string(),
            scan_paths: vec!["kanban-work/expeditions/".to_string()],
            ignore: vec![],
            wip_exempt_types: vec![],
            wip_limits: HashMap::new(),
            states: vec![
                "backlog".to_string(),
                "in_progress".to_string(),
                "done".to_string(),
            ],
            phases: vec![],
            type_states: HashMap::new(),
        };

        let result = migrate_board(dir.path(), &board).expect("migrate");

        assert_eq!(result.items.len(), 2);
        assert_eq!(result.runs.len(), 1); // Only EXP-1 has a turtle block
        assert_eq!(result.relations.len(), 2); // EXP-1 related EXP-2, EXP-2 depends EXP-1
        assert!(result.errors.is_empty());

        // Verify items batch builds correctly
        let batch = result.items_batch().expect("items batch");
        assert_eq!(batch.num_rows(), 2);
        assert_eq!(batch.num_columns(), 18);

        // Verify runs batch
        let runs_batch = result.runs_batch().expect("runs batch");
        assert_eq!(runs_batch.num_rows(), 1);

        // Verify relations batch
        let rel_batch = result.relations_batch().expect("relations batch");
        assert_eq!(rel_batch.num_rows(), 2);

        // Verify summary
        let summary = result.summary();
        assert!(summary.contains("Items migrated: 2"));
        assert!(summary.contains("expedition: 2"));
    }

    #[test]
    fn test_migrate_respects_ignore_patterns() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let exp_dir = dir.path().join("kanban-work/expeditions");
        let archive_dir = exp_dir.join("archive");
        std::fs::create_dir_all(&archive_dir).expect("create dirs");

        // Active file
        std::fs::write(
            exp_dir.join("EXP-1-Active.md"),
            "---\nid: EXP-1\ntitle: Active\ntype: expedition\nstatus: backlog\n---\nBody\n",
        )
        .expect("write");

        // Archived file (should be ignored)
        std::fs::write(
            archive_dir.join("EXP-old.md"),
            "---\nid: EXP-old\ntitle: Old\ntype: expedition\nstatus: done\n---\nBody\n",
        )
        .expect("write");

        let board = BoardConfig {
            name: "development".to_string(),
            preset: "nautical".to_string(),
            path: "kanban-work/".to_string(),
            scan_paths: vec!["kanban-work/expeditions/".to_string()],
            ignore: vec!["**/archive/**".to_string()],
            wip_exempt_types: vec![],
            wip_limits: HashMap::new(),
            states: vec!["backlog".to_string(), "done".to_string()],
            phases: vec![],
            type_states: HashMap::new(),
        };

        let result = migrate_board(dir.path(), &board).expect("migrate");
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "EXP-1");
    }

    #[test]
    fn test_into_stores() {
        let result = MigrateResult {
            items: vec![ParsedItem {
                id: "EXP-1".to_string(),
                title: "Test".to_string(),
                item_type: ItemType::Expedition,
                status: "backlog".to_string(),
                priority: None,
                created: Some("2026-01-01".to_string()),
                assignee: None,
                board: "development".to_string(),
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: String::new(),
                body_hash: "hash".to_string(),
                source_path: PathBuf::new(),
            }],
            runs: vec![],
            relations: vec![],
            errors: vec![],
        };

        let (store, _rel_store) = result.into_stores().expect("into stores");
        assert_eq!(store.active_item_count(), 1);
    }

    #[test]
    fn test_research_item_types() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let hyp_dir = dir.path().join("research/hypotheses");
        std::fs::create_dir_all(&hyp_dir).expect("create dir");

        std::fs::write(
            hyp_dir.join("H-001-Test.md"),
            r#"---
id: H-001
title: "Test Hypothesis"
type: hypothesis
status: active
priority: high
created: 2026-03-08
tags: [tool-use]
related: [EXP-1133]
depends_on: []
---

# H-001: Test Hypothesis

Claim content.
"#,
        )
        .expect("write");

        let board = BoardConfig {
            name: "research".to_string(),
            preset: "hdd".to_string(),
            path: "research/".to_string(),
            scan_paths: vec!["research/hypotheses/".to_string()],
            ignore: vec![],
            wip_exempt_types: vec![],
            wip_limits: HashMap::new(),
            states: vec![
                "draft".to_string(),
                "active".to_string(),
                "complete".to_string(),
            ],
            phases: vec![],
            type_states: HashMap::new(),
        };

        let result = migrate_board(dir.path(), &board).expect("migrate");
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].item_type, ItemType::Hypothesis);
        assert_eq!(result.items[0].board, "research");
    }

    #[test]
    fn test_duplicate_id_detection() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let exp_dir = dir.path().join("kanban-work/expeditions");
        let chore_dir = dir.path().join("kanban-work/chores");
        std::fs::create_dir_all(&exp_dir).expect("create exp dir");
        std::fs::create_dir_all(&chore_dir).expect("create chore dir");

        // Two files with the same ID
        std::fs::write(
            exp_dir.join("EXP-1-First.md"),
            "---\nid: EXP-1\ntitle: First\ntype: expedition\nstatus: backlog\n---\nBody 1\n",
        )
        .expect("write");
        std::fs::write(
            chore_dir.join("EXP-1-Dupe.md"),
            "---\nid: EXP-1\ntitle: Duplicate\ntype: expedition\nstatus: done\n---\nBody 2\n",
        )
        .expect("write");

        // A unique item
        std::fs::write(
            exp_dir.join("EXP-2-Unique.md"),
            "---\nid: EXP-2\ntitle: Unique\ntype: expedition\nstatus: backlog\n---\nBody 3\n",
        )
        .expect("write");

        // Write a minimal config file
        let config_dir = dir.path().join(".yurtle-kanban");
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        std::fs::write(
            config_dir.join("config.yaml"),
            r#"version: "2.0"
boards:
  - name: development
    preset: nautical
    path: kanban-work/
    scan_paths:
      - "kanban-work/expeditions/"
      - "kanban-work/chores/"
    ignore: []
    wip_limits: {}
    states: [backlog, done]
default_board: development
"#,
        )
        .expect("write config");
        let config = ConfigFile::from_path(&config_dir.join("config.yaml")).expect("load config");

        let result = migrate_boards(dir.path(), &config).expect("migrate");

        // All 3 items should be present (duplicate renamed, not removed)
        assert_eq!(result.items.len(), 3);

        // First EXP-1 keeps its original ID
        assert!(result.items.iter().any(|i| i.id == "EXP-1"));
        // Duplicate gets .1 suffix
        assert!(result.items.iter().any(|i| i.id == "EXP-1.1"));
        // Unique item unchanged
        assert!(result.items.iter().any(|i| i.id == "EXP-2"));

        // Duplicate rename should be reported
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].1.contains("Duplicate ID 'EXP-1'"));
        assert!(result.errors[0].1.contains("EXP-1.1"));
    }
}
