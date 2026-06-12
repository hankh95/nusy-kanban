//! Query engine — NL filter extraction, SPARQL subset, and hybrid search.
//!
//! Supports four modes:
//! 1. **Structured** — explicit status/type/board/assignee filters
//! 2. **NL decomposition** — extract filters from natural language
//! 3. **SPARQL subset** — parse SELECT/WHERE/FILTER/ORDER BY/LIMIT
//! 4. **Hybrid** — NL extraction + semantic ranking on remaining terms

use crate::embeddings::{self, EmbeddedItem, EmbeddingProvider};
use crate::item_type::ItemType;
use crate::schema::items_col;
use arrow::array::{Array, BooleanArray, RecordBatch, StringArray};
use regex::Regex;

/// Relationship query types for dependency/blocker traversal.
#[derive(Debug, Clone, PartialEq)]
pub enum RelationQuery {
    /// "what blocks EX-3050" → find items that block the target
    BlockersOf(String),
    /// "dependencies of VOY-155" → find items the target depends on
    DependenciesOf(String),
}

/// Extracted query filters from natural language or structured input.
#[derive(Debug, Clone, Default)]
pub struct QueryFilters {
    pub status: Option<String>,
    pub item_type: Option<String>,
    pub board: Option<String>,
    pub assignee: Option<String>,
    pub id_pattern: Option<String>,
    /// Remaining text after filter extraction (for text search).
    pub text_query: Option<String>,
    /// Relationship traversal query (blockers, dependencies).
    pub relation_query: Option<RelationQuery>,
    /// ID range filter: items with numeric ID >= this value.
    pub id_above: Option<u32>,
    /// ID range filter: items with numeric ID <= this value.
    pub id_below: Option<u32>,
}

/// Parse a natural language query into structured filters.
///
/// Examples:
/// - "in-progress expeditions assigned to Mini" → status=in_progress, type=expedition, assignee=Mini
/// - "EXP-1257" → id_pattern=EXP-1257
/// - "arrow migration" → text_query="arrow migration"
/// - "backlog chores" → status=backlog, type=chore
pub fn parse_nl_query(query: &str) -> QueryFilters {
    let mut filters = QueryFilters::default();
    let mut remaining_words: Vec<&str> = Vec::new();
    let words: Vec<&str> = query.split_whitespace().collect();
    // Check for ID pattern first (e.g., "EXP-1257", "VOY-145")
    let id_re = Regex::new(r"^[A-Z]+-\d+$").expect("valid regex");
    if words.len() == 1 && id_re.is_match(words[0]) {
        filters.id_pattern = Some(words[0].to_string());
        return filters;
    }

    // Check for relationship queries: "what blocks EX-3050", "blockers of EX-3050"
    let blocks_re =
        Regex::new(r"(?i)(?:what\s+blocks|blockers?\s+(?:of|for))\s+([A-Z]+-\d+)").unwrap();
    if let Some(caps) = blocks_re.captures(query) {
        filters.relation_query = Some(RelationQuery::BlockersOf(
            caps.get(1).unwrap().as_str().to_string(),
        ));
        return filters;
    }

    // Check for dependency queries: "dependencies of VOY-155", "deps of EX-3050"
    let deps_re =
        Regex::new(r"(?i)(?:dependenc(?:ies|y)|deps?)\s+(?:of|for)\s+([A-Z]+-\d+)").unwrap();
    if let Some(caps) = deps_re.captures(query) {
        filters.relation_query = Some(RelationQuery::DependenciesOf(
            caps.get(1).unwrap().as_str().to_string(),
        ));
        return filters;
    }

    // Check for ID range patterns: "expeditions above 3100", "items above 3050"
    let above_re = Regex::new(r"(?i)above\s+(\d+)").unwrap();
    if let Some(caps) = above_re.captures(query)
        && let Ok(n) = caps.get(1).unwrap().as_str().parse::<u32>()
    {
        filters.id_above = Some(n);
        // Continue parsing to also extract type filters (e.g., "expeditions above 3100")
    }

    // Check for ID range patterns: "EXP-3100-3150" or "EX-3100-3150"
    let range_re = Regex::new(r"(?i)([A-Z]+)-(\d+)-(\d+)").unwrap();
    if words.len() == 1
        && let Some(caps) = range_re.captures(words[0])
    {
        if let (Ok(lower), Ok(upper)) = (
            caps.get(2).unwrap().as_str().parse::<u32>(),
            caps.get(3).unwrap().as_str().parse::<u32>(),
        ) {
            filters.id_above = Some(lower);
            filters.id_below = Some(upper);
            let prefix = caps.get(1).unwrap().as_str().to_lowercase();
            if let Some(item_type) = ItemType::from_str_loose(&prefix) {
                filters.item_type = Some(item_type.as_str().to_string());
            }
        }
        return filters;
    }

    let mut i = 0;
    while i < words.len() {
        let word = words[i];
        let lower = word.to_lowercase();

        // Status extraction
        if matches!(
            lower.as_str(),
            "backlog"
                | "planning"
                | "ready"
                | "in-progress"
                | "in_progress"
                | "review"
                | "done"
                | "draft"
                | "active"
                | "complete"
                | "abandoned"
        ) && filters.status.is_none()
        {
            filters.status = Some(lower.replace('-', "_"));
            i += 1;
            continue;
        }

        // Item type extraction
        if let Some(item_type) = ItemType::from_str_loose(&lower)
            && filters.item_type.is_none()
        {
            filters.item_type = Some(item_type.as_str().to_string());
            i += 1;
            continue;
        }

        // Also check plural forms (including irregular: hypotheses → hypothesis)
        let singular = depluralize(&lower);
        if let Some(item_type) = ItemType::from_str_loose(&singular)
            && filters.item_type.is_none()
        {
            filters.item_type = Some(item_type.as_str().to_string());
            i += 1;
            continue;
        }

        // Board extraction
        if matches!(lower.as_str(), "development" | "dev" | "research") && filters.board.is_none() {
            filters.board = Some(if lower == "dev" {
                "development".to_string()
            } else {
                lower
            });
            i += 1;
            continue;
        }

        // Assignee extraction: "assigned to X" or "by X"
        if (lower == "assigned" || lower == "by")
            && i + 1 < words.len()
            && filters.assignee.is_none()
        {
            // Skip "to" if present
            let next_idx = if lower == "assigned"
                && i + 2 < words.len()
                && words[i + 1].to_lowercase() == "to"
            {
                i + 2
            } else {
                i + 1
            };

            if next_idx < words.len() {
                filters.assignee = Some(words[next_idx].to_string());
                i = next_idx + 1;
                continue;
            }
        }

        // Skip noise words
        if !matches!(
            lower.as_str(),
            "the"
                | "a"
                | "an"
                | "all"
                | "with"
                | "on"
                | "in"
                | "for"
                | "to"
                | "and"
                | "or"
                | "items"
                | "show"
                | "list"
                | "find"
                | "get"
                | "search"
        ) {
            remaining_words.push(word);
        }

        i += 1;
    }

    if !remaining_words.is_empty() {
        filters.text_query = Some(remaining_words.join(" "));
    }

    filters
}

/// Convert a plural word to singular (best-effort for kanban item types).
fn depluralize(word: &str) -> String {
    // Known irregular plurals
    if word == "hypotheses" {
        return "hypothesis".to_string();
    }
    // Regular: strip trailing 's'
    word.strip_suffix('s').unwrap_or(word).to_string()
}

/// Check if an item title matches a text query (case-insensitive substring).
pub fn text_matches(title: &str, query: &str) -> bool {
    let lower_title = title.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| lower_title.contains(word))
}

// ─── SPARQL Subset Parser ───────────────────────────────────────────────────

/// A parsed SPARQL-subset query.
#[derive(Debug, Clone, Default)]
pub struct SparqlQuery {
    /// SELECT variables (e.g., ["?id", "?title", "?status"]).
    pub select_vars: Vec<String>,
    /// WHERE triple patterns — each maps a predicate to a value.
    pub where_clauses: Vec<SparqlClause>,
    /// OPTIONAL clauses (same structure as WHERE).
    pub optional_clauses: Vec<SparqlClause>,
    /// FILTER expressions.
    pub filters: Vec<SparqlFilter>,
    /// ORDER BY field (ascending by default).
    pub order_by: Option<String>,
    /// ORDER BY descending.
    pub order_desc: bool,
    /// LIMIT on result count.
    pub limit: Option<usize>,
}

/// A WHERE or OPTIONAL clause: `?item kb:predicate "value"`.
#[derive(Debug, Clone)]
pub struct SparqlClause {
    /// The predicate (e.g., "status", "type", "priority").
    pub predicate: String,
    /// The expected value.
    pub value: String,
}

/// A FILTER expression: `FILTER(?field op "value")`.
#[derive(Debug, Clone)]
pub struct SparqlFilter {
    /// The field (e.g., "priority", "status").
    pub field: String,
    /// The operator ("=", "!=").
    pub op: FilterOp,
    /// The value to compare against.
    pub value: String,
}

/// Filter comparison operators.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterOp {
    Eq,
    NotEq,
}

/// Parse a SPARQL-subset query string.
///
/// Supports:
/// - `SELECT ?id ?title ?status WHERE { ... }`
/// - Multiple WHERE triple patterns joined by `.`
/// - `OPTIONAL { ?item kb:assignee ?assignee }`
/// - `FILTER(?priority = "critical")`
/// - `ORDER BY ?priority`
/// - `LIMIT N`
pub fn parse_sparql(query: &str) -> SparqlQuery {
    let mut result = SparqlQuery::default();

    // Normalize whitespace
    let q = query.replace(['\n', '\r'], " ");

    // Extract SELECT variables
    let select_re = Regex::new(r"(?i)SELECT\s+(.*?)\s+WHERE").expect("select regex");
    if let Some(caps) = select_re.captures(&q) {
        let vars_str = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        result.select_vars = vars_str
            .split_whitespace()
            .filter(|v| v.starts_with('?'))
            .map(|v| v.to_string())
            .collect();
    }

    // Extract WHERE { ... } block
    let where_re = Regex::new(r"(?i)WHERE\s*\{(.*?)\}").expect("where regex");
    if let Some(caps) = where_re.captures(&q) {
        let where_block = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        for clause in where_block.split('.') {
            let trimmed = clause.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Skip variable type declarations (e.g., `?item a <...Experiment>`)
            if trimmed.contains(" a ") && !trimmed.contains("kb:") {
                continue;
            }
            if let Some(sc) = parse_triple_pattern(trimmed) {
                result.where_clauses.push(sc);
            }
        }
    }

    // Extract OPTIONAL { ... } blocks
    let optional_re = Regex::new(r"(?i)OPTIONAL\s*\{(.*?)\}").expect("optional regex");
    for caps in optional_re.captures_iter(&q) {
        let block = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        for clause in block.split('.') {
            let trimmed = clause.trim();
            if !trimmed.is_empty()
                && let Some(sc) = parse_triple_pattern(trimmed)
            {
                result.optional_clauses.push(sc);
            }
        }
    }

    // Extract FILTER expressions
    let filter_re = Regex::new(r#"(?i)FILTER\s*\(\s*\?(\w+)\s*(!=|=)\s*["']([^"']+)["']\s*\)"#)
        .expect("filter regex");
    for caps in filter_re.captures_iter(&q) {
        let field = caps.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
        let op_str = caps.get(2).map(|m| m.as_str()).unwrap_or("=");
        let value = caps.get(3).map(|m| m.as_str()).unwrap_or("").to_string();
        result.filters.push(SparqlFilter {
            field: normalize_predicate(&field),
            op: if op_str == "!=" {
                FilterOp::NotEq
            } else {
                FilterOp::Eq
            },
            value,
        });
    }

    // TODO: FILTER NOT EXISTS deferred

    // Extract ORDER BY
    let order_re =
        Regex::new(r"(?i)ORDER\s+BY\s+(DESC\s*\(\s*)?\?(\w+)\s*\)?").expect("order regex");
    if let Some(caps) = order_re.captures(&q) {
        result.order_desc = caps.get(1).is_some();
        result.order_by = caps.get(2).map(|field| normalize_predicate(field.as_str()));
    }

    // Extract LIMIT
    let limit_re = Regex::new(r"(?i)LIMIT\s+(\d+)").expect("limit regex");
    if let Some(caps) = limit_re.captures(&q)
        && let Some(n) = caps.get(1)
    {
        result.limit = n.as_str().parse().ok();
    }

    result
}

/// Parse a single triple pattern like `?item kb:status "backlog"`.
fn parse_triple_pattern(pattern: &str) -> Option<SparqlClause> {
    // Match patterns like: ?item kb:status "value" or ?item status "value"
    let re = Regex::new(r#"(?:kb:|<[^>]*>)?(\w+)\s+["']([^"']+)["']"#).expect("triple regex");
    if let Some(caps) = re.captures(pattern) {
        let pred = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let val = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        if !pred.is_empty() && !val.is_empty() {
            return Some(SparqlClause {
                predicate: normalize_predicate(pred),
                value: val.to_string(),
            });
        }
    }
    None
}

/// Normalize predicate names to match schema field names.
fn normalize_predicate(pred: &str) -> String {
    match pred.to_lowercase().as_str() {
        "status" => "status".to_string(),
        "type" | "item_type" | "itemtype" => "item_type".to_string(),
        "priority" => "priority".to_string(),
        "assignee" => "assignee".to_string(),
        "board" => "board".to_string(),
        "id" => "id".to_string(),
        "title" => "title".to_string(),
        "tags" => "tags".to_string(),
        "depends_on" | "dependson" | "blocked_by" => "depends_on".to_string(),
        "related" | "related_to" => "related".to_string(),
        other => other.to_string(),
    }
}

/// Execute a parsed SPARQL query against item batches.
///
/// Returns a list of result rows, each row is a map of variable → value.
pub fn execute_sparql(
    batches: &[RecordBatch],
    query: &SparqlQuery,
) -> Vec<std::collections::BTreeMap<String, String>> {
    let mut rows: Vec<std::collections::BTreeMap<String, String>> = Vec::new();

    for batch in batches {
        let cols = BatchColumns::from_batch(batch);
        let deleted = batch
            .column(items_col::DELETED)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("deleted column");

        for i in 0..batch.num_rows() {
            if deleted.value(i) {
                continue;
            }

            // Check WHERE clauses (all must match)
            let all_match = query.where_clauses.iter().all(|clause| {
                // Handle list-column predicates (depends_on, related)
                if clause.predicate == "depends_on" || clause.predicate == "related" {
                    return match_list_column(batch, &clause.predicate, i, &clause.value);
                }
                cols.get(&clause.predicate, i)
                    .is_some_and(|val| val == clause.value)
            });
            if !all_match {
                continue;
            }

            // Check FILTER expressions
            let filter_pass = query.filters.iter().all(|filter| {
                let item_value = cols.get(&filter.field, i);
                match (&filter.op, &item_value) {
                    (FilterOp::Eq, Some(val)) => val == &filter.value,
                    (FilterOp::Eq, None) => false,
                    (FilterOp::NotEq, Some(val)) => val != &filter.value,
                    (FilterOp::NotEq, None) => true, // null != "value" is true
                }
            });
            if !filter_pass {
                continue;
            }

            // Build result row from SELECT variables (or all fields if SELECT *)
            let vars = if query.select_vars.is_empty() {
                vec![
                    "?id".to_string(),
                    "?title".to_string(),
                    "?status".to_string(),
                ]
            } else {
                query.select_vars.clone()
            };

            let mut row = std::collections::BTreeMap::new();
            for var in &vars {
                let field_name = var.trim_start_matches('?');
                row.insert(
                    var.clone(),
                    cols.get(field_name, i)
                        .unwrap_or_else(|| "null".to_string()),
                );
            }
            rows.push(row);
        }
    }

    // ORDER BY
    if let Some(ref order_field) = query.order_by {
        let order_var = format!("?{order_field}");
        let desc = query.order_desc;
        rows.sort_by(|a, b| {
            let va = a.get(&order_var).cloned().unwrap_or_default();
            let vb = b.get(&order_var).cloned().unwrap_or_default();
            if desc { vb.cmp(&va) } else { va.cmp(&vb) }
        });
    }

    // LIMIT
    if let Some(limit) = query.limit {
        rows.truncate(limit);
    }

    rows
}

/// Column references for a batch — avoids passing 7 separate StringArray refs.
struct BatchColumns<'a> {
    ids: &'a StringArray,
    titles: &'a StringArray,
    types: &'a StringArray,
    statuses: &'a StringArray,
    priorities: &'a StringArray,
    assignees: &'a StringArray,
    boards: &'a StringArray,
}

impl<'a> BatchColumns<'a> {
    fn from_batch(batch: &'a RecordBatch) -> Self {
        Self {
            ids: col_str(batch, items_col::ID),
            titles: col_str(batch, items_col::TITLE),
            types: col_str(batch, items_col::ITEM_TYPE),
            statuses: col_str(batch, items_col::STATUS),
            priorities: col_str(batch, items_col::PRIORITY),
            assignees: col_str(batch, items_col::ASSIGNEE),
            boards: col_str(batch, items_col::BOARD),
        }
    }

    /// Get a field value from a row by field name.
    fn get(&self, field: &str, row: usize) -> Option<String> {
        match field {
            "id" => Some(self.ids.value(row).to_string()),
            "title" => Some(self.titles.value(row).to_string()),
            "item_type" | "type" => Some(self.types.value(row).to_string()),
            "status" => Some(self.statuses.value(row).to_string()),
            "priority" => {
                if self.priorities.is_null(row) {
                    None
                } else {
                    Some(self.priorities.value(row).to_string())
                }
            }
            "assignee" => {
                if self.assignees.is_null(row) {
                    None
                } else {
                    Some(self.assignees.value(row).to_string())
                }
            }
            "board" => Some(self.boards.value(row).to_string()),
            _ => None,
        }
    }
}

/// Format SPARQL results as a table.
pub fn format_sparql_results(
    rows: &[std::collections::BTreeMap<String, String>],
    vars: &[String],
) -> String {
    if rows.is_empty() {
        return "No results.\n".to_string();
    }

    let display_vars = if vars.is_empty() {
        // Collect all variable names from first row
        rows[0].keys().cloned().collect::<Vec<_>>()
    } else {
        vars.to_vec()
    };

    let mut lines = Vec::new();

    // Header
    let header: Vec<String> = display_vars.iter().map(|v| format!("{:<20}", v)).collect();
    lines.push(header.join(""));
    lines.push("─".repeat(20 * display_vars.len()));

    // Rows
    for row in rows {
        let cols: Vec<String> = display_vars
            .iter()
            .map(|v| {
                let val = row.get(v).cloned().unwrap_or_else(|| "null".to_string());
                format!("{:<20}", val)
            })
            .collect();
        lines.push(cols.join(""));
    }

    lines.join("\n") + "\n"
}

/// Check if a list-column (depends_on, related) contains a specific value at row `i`.
fn match_list_column(batch: &RecordBatch, predicate: &str, row: usize, value: &str) -> bool {
    use arrow::array::ListArray;

    let col_idx = match predicate {
        "depends_on" => items_col::DEPENDS_ON,
        "related" => items_col::RELATED,
        _ => return false,
    };

    let Some(list) = batch.column(col_idx).as_any().downcast_ref::<ListArray>() else {
        return false;
    };

    if list.is_null(row) || list.value(row).is_empty() {
        return false;
    }

    let values = list.value(row);
    let Some(str_arr) = values.as_any().downcast_ref::<StringArray>() else {
        return false;
    };

    (0..str_arr.len()).any(|j| !str_arr.is_null(j) && str_arr.value(j) == value)
}

fn col_str(batch: &RecordBatch, col: usize) -> &StringArray {
    batch
        .column(col)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("string column")
}

// ─── Hybrid Query ───────────────────────────────────────────────────────────

/// A ranked query result combining structured filters and semantic scores.
#[derive(Debug, Clone)]
pub struct RankedResult {
    /// Item ID.
    pub id: String,
    /// Item title.
    pub title: String,
    /// Item type.
    pub item_type: String,
    /// Item status.
    pub status: String,
    /// Priority (may be empty).
    pub priority: String,
    /// Assignee (may be empty).
    pub assignee: String,
    /// Combined relevance score (0.0–1.0+).
    pub score: f32,
}

/// Execute a hybrid query: NL decomposition + structured filters + semantic ranking.
///
/// 1. Parse NL query to extract structured filters + remaining text
/// 2. Apply structured filters to narrow candidates
/// 3. If remaining text exists and embeddings are available, rank by semantic similarity
/// 4. Return results sorted by combined score
pub fn hybrid_query(
    batches: &[RecordBatch],
    query_str: &str,
    embeddings: Option<&[EmbeddedItem]>,
    provider: Option<&dyn EmbeddingProvider>,
    top_k: usize,
) -> Vec<RankedResult> {
    let filters = parse_nl_query(query_str);

    // If it's an ID pattern, return just that item
    if let Some(ref id) = filters.id_pattern {
        return collect_items_by_id(batches, id);
    }

    // Collect structurally-filtered candidates
    let mut candidates: Vec<RankedResult> = Vec::new();

    for batch in batches {
        let ids = col_str(batch, items_col::ID);
        let titles = col_str(batch, items_col::TITLE);
        let types = col_str(batch, items_col::ITEM_TYPE);
        let statuses = col_str(batch, items_col::STATUS);
        let priorities = batch
            .column(items_col::PRIORITY)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("priority column");
        let assignees = batch
            .column(items_col::ASSIGNEE)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("assignee column");
        let deleted = batch
            .column(items_col::DELETED)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("deleted column");
        let boards = col_str(batch, items_col::BOARD);

        for i in 0..batch.num_rows() {
            if deleted.value(i) {
                continue;
            }

            // Apply structural filters
            if let Some(s) = &filters.status
                && statuses.value(i) != s
            {
                continue;
            }
            if let Some(t) = &filters.item_type
                && types.value(i) != t
            {
                continue;
            }
            if let Some(b) = &filters.board
                && boards.value(i) != b
            {
                continue;
            }
            if let Some(a) = &filters.assignee
                && (assignees.is_null(i) || assignees.value(i) != a)
            {
                continue;
            }

            let priority = if priorities.is_null(i) {
                String::new()
            } else {
                priorities.value(i).to_string()
            };
            let assignee = if assignees.is_null(i) {
                String::new()
            } else {
                assignees.value(i).to_string()
            };

            // Base score: 1.0 for exact text match, 0.5 for no text query
            let text_score = if let Some(ref text) = filters.text_query {
                if text_matches(titles.value(i), text) {
                    1.0
                } else {
                    0.0
                }
            } else {
                0.5
            };

            candidates.push(RankedResult {
                id: ids.value(i).to_string(),
                title: titles.value(i).to_string(),
                item_type: types.value(i).to_string(),
                status: statuses.value(i).to_string(),
                priority,
                assignee,
                score: text_score,
            });
        }
    }

    // Apply semantic ranking if we have remaining text and embeddings
    if let (Some(text), Some(embeds), Some(prov)) = (&filters.text_query, embeddings, provider)
        && let Ok(sem_results) = embeddings::semantic_search(embeds, text, prov, candidates.len())
    {
        // Build score lookup
        let score_map: std::collections::HashMap<&str, f32> = sem_results
            .iter()
            .map(|r| (r.id.as_str(), r.score))
            .collect();

        // Combine text score (0.6 weight) + semantic score (0.4 weight)
        for candidate in &mut candidates {
            if let Some(&sem_score) = score_map.get(candidate.id.as_str()) {
                candidate.score = candidate.score * 0.6 + sem_score * 0.4;
            }
        }
    }

    // Sort by score descending
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    candidates.truncate(top_k);
    candidates
}

/// Collect items matching an exact ID.
fn collect_items_by_id(batches: &[RecordBatch], target_id: &str) -> Vec<RankedResult> {
    for batch in batches {
        let ids = col_str(batch, items_col::ID);
        let titles = col_str(batch, items_col::TITLE);
        let types = col_str(batch, items_col::ITEM_TYPE);
        let statuses = col_str(batch, items_col::STATUS);
        let priorities = batch
            .column(items_col::PRIORITY)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("priority");
        let assignees = batch
            .column(items_col::ASSIGNEE)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("assignee");
        let deleted = batch
            .column(items_col::DELETED)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("deleted");

        for i in 0..batch.num_rows() {
            if !deleted.value(i) && ids.value(i) == target_id {
                return vec![RankedResult {
                    id: ids.value(i).to_string(),
                    title: titles.value(i).to_string(),
                    item_type: types.value(i).to_string(),
                    status: statuses.value(i).to_string(),
                    priority: if priorities.is_null(i) {
                        String::new()
                    } else {
                        priorities.value(i).to_string()
                    },
                    assignee: if assignees.is_null(i) {
                        String::new()
                    } else {
                        assignees.value(i).to_string()
                    },
                    score: 1.0,
                }];
            }
        }
    }
    Vec::new()
}

/// Format ranked results as a table.
pub fn format_ranked_results(results: &[RankedResult]) -> String {
    if results.is_empty() {
        return "No results.\n".to_string();
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "  {:<14}{:<30}{:<10}{:<12}{:<8}",
        "ID", "Title", "Status", "Priority", "Score"
    ));
    lines.push(format!(" {}", "─".repeat(72)));

    for r in results {
        let title = if r.title.chars().count() > 28 {
            let truncated: String = r.title.chars().take(25).collect();
            format!("{truncated}...")
        } else {
            r.title.clone()
        };

        lines.push(format!(
            "  {:<14}{:<30}{:<10}{:<12}{:.3}",
            r.id,
            title,
            r.status,
            if r.priority.is_empty() {
                "-"
            } else {
                &r.priority
            },
            r.score
        ));
    }

    lines.join("\n") + "\n"
}

/// Format ranked results as JSON.
pub fn format_ranked_results_json(results: &[RankedResult]) -> String {
    let items: Vec<String> = results
        .iter()
        .map(|r| {
            format!(
                r#"  {{"id": "{}", "title": "{}", "type": "{}", "status": "{}", "priority": "{}", "assignee": "{}", "score": {:.4}}}"#,
                escape_json(&r.id),
                escape_json(&r.title),
                escape_json(&r.item_type),
                escape_json(&r.status),
                escape_json(&r.priority),
                escape_json(&r.assignee),
                r.score
            )
        })
        .collect();

    format!("[\n{}\n]\n", items.join(",\n"))
}

/// Minimal JSON string escaping.
fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Format the query decomposition for --verbose mode.
pub fn format_query_decomposition(filters: &QueryFilters) -> String {
    let mut parts = Vec::new();

    if let Some(ref s) = filters.status {
        parts.push(format!("status={s}"));
    }
    if let Some(ref t) = filters.item_type {
        parts.push(format!("type={t}"));
    }
    if let Some(ref b) = filters.board {
        parts.push(format!("board={b}"));
    }
    if let Some(ref a) = filters.assignee {
        parts.push(format!("assignee={a}"));
    }
    if let Some(ref id) = filters.id_pattern {
        parts.push(format!("id={id}"));
    }
    if let Some(ref t) = filters.text_query {
        parts.push(format!("text=\"{t}\""));
    }

    if parts.is_empty() {
        "Query decomposition: (no filters extracted)\n".to_string()
    } else {
        format!("Query decomposition: {}\n", parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_status_filter() {
        let f = parse_nl_query("in-progress expeditions");
        assert_eq!(f.status.as_deref(), Some("in_progress"));
        assert_eq!(f.item_type.as_deref(), Some("expedition"));
    }

    #[test]
    fn test_parse_assignee() {
        let f = parse_nl_query("in-progress expeditions assigned to Mini");
        assert_eq!(f.status.as_deref(), Some("in_progress"));
        assert_eq!(f.item_type.as_deref(), Some("expedition"));
        assert_eq!(f.assignee.as_deref(), Some("Mini"));
    }

    #[test]
    fn test_parse_id_pattern() {
        let f = parse_nl_query("EXP-1257");
        assert_eq!(f.id_pattern.as_deref(), Some("EXP-1257"));
    }

    #[test]
    fn test_parse_text_query() {
        let f = parse_nl_query("arrow migration");
        assert_eq!(f.text_query.as_deref(), Some("arrow migration"));
    }

    #[test]
    fn test_parse_board_filter() {
        let f = parse_nl_query("research hypotheses");
        assert_eq!(f.board.as_deref(), Some("research"));
        assert_eq!(f.item_type.as_deref(), Some("hypothesis"));
    }

    #[test]
    fn test_parse_plural_type() {
        let f = parse_nl_query("backlog chores");
        assert_eq!(f.status.as_deref(), Some("backlog"));
        assert_eq!(f.item_type.as_deref(), Some("chore"));
    }

    #[test]
    fn test_text_matches() {
        assert!(text_matches("Arrow-Kanban Engine", "arrow kanban"));
        assert!(text_matches("Arrow-Kanban Engine", "engine"));
        assert!(!text_matches("Arrow-Kanban Engine", "codegraph"));
    }

    #[test]
    fn test_parse_dev_board_alias() {
        let f = parse_nl_query("dev expeditions");
        assert_eq!(f.board.as_deref(), Some("development"));
    }

    #[test]
    fn test_noise_words_filtered() {
        let f = parse_nl_query("show all the backlog items");
        assert_eq!(f.status.as_deref(), Some("backlog"));
        assert!(f.text_query.is_none());
    }

    // ─── SPARQL Parser Tests ────────────────────────────────────────────

    #[test]
    fn test_sparql_basic_where() {
        let q = parse_sparql(
            r#"SELECT ?id ?title WHERE { ?item kb:status "backlog" . ?item kb:type "expedition" }"#,
        );
        assert_eq!(q.select_vars, vec!["?id", "?title"]);
        assert_eq!(q.where_clauses.len(), 2);
        assert_eq!(q.where_clauses[0].predicate, "status");
        assert_eq!(q.where_clauses[0].value, "backlog");
        assert_eq!(q.where_clauses[1].predicate, "item_type");
        assert_eq!(q.where_clauses[1].value, "expedition");
    }

    #[test]
    fn test_sparql_filter_eq() {
        let q = parse_sparql(
            r#"SELECT ?id WHERE { ?item kb:status "backlog" } FILTER(?priority = "critical")"#,
        );
        assert_eq!(q.filters.len(), 1);
        assert_eq!(q.filters[0].field, "priority");
        assert_eq!(q.filters[0].op, FilterOp::Eq);
        assert_eq!(q.filters[0].value, "critical");
    }

    #[test]
    fn test_sparql_filter_not_eq() {
        let q = parse_sparql(
            r#"SELECT ?id WHERE { ?item kb:status "backlog" } FILTER(?status != "done")"#,
        );
        assert_eq!(q.filters.len(), 1);
        assert_eq!(q.filters[0].op, FilterOp::NotEq);
        assert_eq!(q.filters[0].value, "done");
    }

    #[test]
    fn test_sparql_order_by() {
        let q = parse_sparql(
            r#"SELECT ?id ?title WHERE { ?item kb:status "backlog" } ORDER BY ?priority"#,
        );
        assert_eq!(q.order_by.as_deref(), Some("priority"));
        assert!(!q.order_desc);
    }

    #[test]
    fn test_sparql_order_by_desc() {
        let q = parse_sparql(
            r#"SELECT ?id WHERE { ?item kb:status "backlog" } ORDER BY DESC(?priority)"#,
        );
        assert_eq!(q.order_by.as_deref(), Some("priority"));
        assert!(q.order_desc);
    }

    #[test]
    fn test_sparql_limit() {
        let q = parse_sparql(r#"SELECT ?id WHERE { ?item kb:status "backlog" } LIMIT 10"#);
        assert_eq!(q.limit, Some(10));
    }

    #[test]
    fn test_sparql_execute_basic() {
        use crate::crud::{CreateItemInput, KanbanStore};

        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Arrow Engine".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("critical".to_string()),
                assignee: Some("M5".to_string()),
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");
        store
            .create_item(&CreateItemInput {
                title: "Fix tests".to_string(),
                item_type: ItemType::Chore,
                priority: Some("medium".to_string()),
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");

        let q = parse_sparql(
            r#"SELECT ?id ?title WHERE { ?item kb:status "backlog" . ?item kb:type "expedition" }"#,
        );
        let rows = execute_sparql(store.items_batches(), &q);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("?id").unwrap(), "EX-1300");
        assert_eq!(rows[0].get("?title").unwrap(), "Arrow Engine");
    }

    #[test]
    fn test_sparql_execute_filter_not_eq() {
        use crate::crud::{CreateItemInput, KanbanStore};

        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Item A".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");

        let q = parse_sparql(
            r#"SELECT ?id WHERE { ?item kb:status "backlog" } FILTER(?priority != "critical")"#,
        );
        let rows = execute_sparql(store.items_batches(), &q);
        assert_eq!(rows.len(), 1); // high != critical → passes
    }

    #[test]
    fn test_sparql_execute_limit() {
        use crate::crud::{CreateItemInput, KanbanStore};

        let mut store = KanbanStore::new();
        for i in 0..5 {
            store
                .create_item(&CreateItemInput {
                    title: format!("Item {i}"),
                    item_type: ItemType::Chore,
                    priority: None,
                    assignee: None,
                    tags: vec![],
                    related: vec![],
                    depends_on: vec![],
                    body: None,
                })
                .expect("create");
        }

        let q = parse_sparql(r#"SELECT ?id WHERE { ?item kb:status "backlog" } LIMIT 3"#);
        let rows = execute_sparql(store.items_batches(), &q);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn test_sparql_optional_null_assignee() {
        let q = parse_sparql(
            r#"SELECT ?id ?assignee WHERE { ?item kb:status "backlog" } OPTIONAL { ?item kb:assignee ?assignee }"#,
        );
        assert_eq!(q.optional_clauses.len(), 0); // OPTIONAL with variable binding doesn't produce a clause
        // The important thing is that the parse doesn't crash
        assert_eq!(q.select_vars, vec!["?id", "?assignee"]);
    }

    // ─── Hybrid Query Tests ────────────────────────────────────────────

    #[test]
    fn test_hybrid_query_structured_only() {
        use crate::crud::{CreateItemInput, KanbanStore};

        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Arrow Engine".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");
        store
            .create_item(&CreateItemInput {
                title: "Fix tests".to_string(),
                item_type: ItemType::Chore,
                priority: None,
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");

        let results = hybrid_query(store.items_batches(), "backlog expeditions", None, None, 20);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "EX-1300");
    }

    #[test]
    fn test_hybrid_query_with_text() {
        use crate::crud::{CreateItemInput, KanbanStore};

        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Arrow-Kanban Engine".to_string(),
                item_type: ItemType::Expedition,
                priority: None,
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");
        store
            .create_item(&CreateItemInput {
                title: "Signal Fusion Pipeline".to_string(),
                item_type: ItemType::Expedition,
                priority: None,
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");

        let results = hybrid_query(store.items_batches(), "arrow kanban", None, None, 20);
        // Both match structurally (no filters extracted), but "Arrow-Kanban" has text match
        assert!(results.len() >= 1);
        // The one with text match should score higher
        assert_eq!(results[0].id, "EX-1300");
        assert!(results[0].score > results.last().unwrap().score);
    }

    #[test]
    fn test_hybrid_query_with_semantic_search() {
        use crate::crud::{CreateItemInput, KanbanStore};
        use crate::embeddings::{HashEmbeddingProvider, embed_items};

        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Arrow-Kanban Engine".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: None,
                tags: vec!["arrow".to_string()],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");
        store
            .create_item(&CreateItemInput {
                title: "Signal Fusion Pipeline".to_string(),
                item_type: ItemType::Expedition,
                priority: None,
                assignee: None,
                tags: vec!["signal".to_string()],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");

        let provider = HashEmbeddingProvider;
        let embeddings = embed_items(store.items_batches(), &provider).unwrap();

        let results = hybrid_query(
            store.items_batches(),
            "arrow kanban",
            Some(&embeddings),
            Some(&provider),
            20,
        );
        assert!(results.len() >= 1);
        // All results should have scores
        for r in &results {
            assert!(r.score > -1.0);
        }
    }

    #[test]
    fn test_hybrid_query_id_pattern() {
        use crate::crud::{CreateItemInput, KanbanStore};

        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Arrow Engine".to_string(),
                item_type: ItemType::Expedition,
                priority: None,
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");

        let results = hybrid_query(store.items_batches(), "EX-1300", None, None, 20);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "EX-1300");
        assert_eq!(results[0].score, 1.0);
    }

    #[test]
    fn test_format_ranked_results_json() {
        let results = vec![RankedResult {
            id: "EXP-1".to_string(),
            title: "Arrow Engine".to_string(),
            item_type: "expedition".to_string(),
            status: "backlog".to_string(),
            priority: "high".to_string(),
            assignee: "M5".to_string(),
            score: 0.95,
        }];
        let json = format_ranked_results_json(&results);
        assert!(json.contains("EXP-1"));
        assert!(json.contains("Arrow Engine"));
        assert!(json.contains("0.95"));
    }

    #[test]
    fn test_format_query_decomposition() {
        let f = parse_nl_query("in-progress expeditions assigned to Mini");
        let output = format_query_decomposition(&f);
        assert!(output.contains("status=in_progress"));
        assert!(output.contains("type=expedition"));
        assert!(output.contains("assignee=Mini"));
    }

    #[test]
    fn test_format_sparql_results() {
        let mut row = std::collections::BTreeMap::new();
        row.insert("?id".to_string(), "EXP-1".to_string());
        row.insert("?title".to_string(), "Arrow Engine".to_string());
        let output = format_sparql_results(&[row], &["?id".to_string(), "?title".to_string()]);
        assert!(output.contains("EXP-1"));
        assert!(output.contains("Arrow Engine"));
    }

    // ─── Phase 4: NL Decomposer Extensions ─────────────────────────

    #[test]
    fn test_parse_what_blocks() {
        let f = parse_nl_query("what blocks EX-3050");
        assert_eq!(
            f.relation_query,
            Some(RelationQuery::BlockersOf("EX-3050".to_string()))
        );
    }

    #[test]
    fn test_parse_blockers_of() {
        let f = parse_nl_query("blockers of EX-3050");
        assert_eq!(
            f.relation_query,
            Some(RelationQuery::BlockersOf("EX-3050".to_string()))
        );
    }

    #[test]
    fn test_parse_dependencies_of() {
        let f = parse_nl_query("dependencies of VOY-155");
        assert_eq!(
            f.relation_query,
            Some(RelationQuery::DependenciesOf("VOY-155".to_string()))
        );
    }

    #[test]
    fn test_parse_deps_of() {
        let f = parse_nl_query("deps of EX-3100");
        assert_eq!(
            f.relation_query,
            Some(RelationQuery::DependenciesOf("EX-3100".to_string()))
        );
    }

    #[test]
    fn test_parse_above_id_range() {
        let f = parse_nl_query("expeditions above 3100");
        assert_eq!(f.id_above, Some(3100));
        assert_eq!(f.item_type.as_deref(), Some("expedition"));
    }

    #[test]
    fn test_parse_id_range_pattern() {
        let f = parse_nl_query("EX-3100-3150");
        assert_eq!(f.id_above, Some(3100));
        assert_eq!(f.id_below, Some(3150));
    }

    // ─── Phase 5: SPARQL depends_on ────────────────────────────────

    #[test]
    fn test_sparql_depends_on() {
        use crate::crud::{CreateItemInput, KanbanStore};

        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Blocker".to_string(),
                item_type: ItemType::Expedition,
                priority: None,
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");
        let blocker_id = "EX-1300"; // First item

        store
            .create_item(&CreateItemInput {
                title: "Blocked Item".to_string(),
                item_type: ItemType::Expedition,
                priority: None,
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![blocker_id.to_string()],
                body: None,
            })
            .expect("create");

        let q = parse_sparql(&format!(
            r#"SELECT ?id ?title WHERE {{ ?item kb:depends_on "{blocker_id}" }}"#
        ));
        assert_eq!(q.where_clauses.len(), 1);
        assert_eq!(q.where_clauses[0].predicate, "depends_on");

        let rows = execute_sparql(store.items_batches(), &q);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].get("?title").unwrap().contains("Blocked"));
    }

    #[test]
    fn test_normalize_depends_on() {
        assert_eq!(normalize_predicate("depends_on"), "depends_on");
        assert_eq!(normalize_predicate("blocked_by"), "depends_on");
        assert_eq!(normalize_predicate("related"), "related");
        assert_eq!(normalize_predicate("related_to"), "related");
    }
}
