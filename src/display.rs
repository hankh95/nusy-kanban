//! Display formatting — render items and board views to terminal output.

use crate::schema::items_col;
use arrow::array::{
    Array, BooleanArray, ListArray, RecordBatch, StringArray, TimestampMillisecondArray,
};

/// Format a list of item batches as a table (matching yurtle-kanban output style).
pub fn format_item_table(batches: &[RecordBatch]) -> String {
    if batches.is_empty() {
        return "No items found.\n".to_string();
    }

    let mut lines = Vec::new();

    // Header
    lines.push(format!(
        "  {:<14}{:<28}{:<10}{:<12}{:<16}",
        "ID", "Title", "Status", "Priority", "Assignee"
    ));
    lines.push(format!(" {}", "─".repeat(78)));

    for batch in batches {
        let ids = col_str(batch, items_col::ID);
        let titles = col_str(batch, items_col::TITLE);
        let statuses = col_str(batch, items_col::STATUS);
        let priorities = col_str(batch, items_col::PRIORITY);
        let assignees = col_str(batch, items_col::ASSIGNEE);
        let types = col_str(batch, items_col::ITEM_TYPE);
        let deleted = batch
            .column(items_col::DELETED)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("deleted column");

        for i in 0..batch.num_rows() {
            if deleted.value(i) {
                continue;
            }

            let id = ids.value(i);
            let title = titles.value(i);
            let status = statuses.value(i);
            let priority = if priorities.is_null(i) {
                "-"
            } else {
                priorities.value(i)
            };
            let assignee = if assignees.is_null(i) {
                "-"
            } else {
                assignees.value(i)
            };
            let type_prefix = type_icon(types.value(i));

            // Truncate ID and title to fit (char-aware for multi-byte UTF-8)
            let display_id = truncate_with_ellipsis(id, 12);
            let display_title = truncate_with_ellipsis(title, 26);

            lines.push(format!(
                "  {type_prefix} {:<12}{:<28}{:<10}{:<12}{:<16}",
                display_id, display_title, status, priority, assignee
            ));
        }
    }

    lines.join("\n") + "\n"
}

/// Format a single item for `show` command.
pub fn format_item_detail(batch: &RecordBatch) -> String {
    let ids = col_str(batch, items_col::ID);
    let titles = col_str(batch, items_col::TITLE);
    let types = col_str(batch, items_col::ITEM_TYPE);
    let statuses = col_str(batch, items_col::STATUS);
    let priorities = col_str(batch, items_col::PRIORITY);
    let assignees = col_str(batch, items_col::ASSIGNEE);
    let boards = col_str(batch, items_col::BOARD);

    let id = ids.value(0);
    let title = titles.value(0);
    let item_type = types.value(0);
    let status = statuses.value(0);
    let priority = if priorities.is_null(0) {
        "-"
    } else {
        priorities.value(0)
    };
    let assignee = if assignees.is_null(0) {
        "unassigned"
    } else {
        assignees.value(0)
    };
    let board = boards.value(0);

    let icon = type_icon(item_type);

    let mut lines = Vec::new();
    let border = "─".repeat(78);
    lines.push(format!("╭{border}╮"));
    lines.push(format!("│ {icon} {id}: {title}"));
    lines.push(format!("╰{border}╯"));
    lines.push(String::new());
    lines.push(format!("  Type       {item_type}"));
    lines.push(format!("  Status     {status}"));
    lines.push(format!("  Priority   {priority}"));
    lines.push(format!("  Assignee   {assignee}"));
    lines.push(format!("  Board      {board}"));

    // Show tags if present
    if batch.num_columns() > items_col::TAGS
        && let Some(tags_list) = batch
            .column(items_col::TAGS)
            .as_any()
            .downcast_ref::<ListArray>()
        && !tags_list.is_null(0)
        && !tags_list.value(0).is_empty()
    {
        let values = tags_list.value(0);
        if let Some(strs) = values.as_any().downcast_ref::<StringArray>() {
            let tag_str: Vec<&str> = (0..strs.len()).map(|i| strs.value(i)).collect();
            lines.push(format!("  Tags       {}", tag_str.join(", ")));
        }
    }

    // Show depends_on if present
    if batch.num_columns() > items_col::DEPENDS_ON
        && let Some(deps_list) = batch
            .column(items_col::DEPENDS_ON)
            .as_any()
            .downcast_ref::<ListArray>()
        && !deps_list.is_null(0)
        && !deps_list.value(0).is_empty()
    {
        let values = deps_list.value(0);
        if let Some(strs) = values.as_any().downcast_ref::<StringArray>() {
            let dep_str: Vec<&str> = (0..strs.len())
                .filter(|&i| !strs.is_null(i))
                .map(|i| strs.value(i))
                .collect();
            if !dep_str.is_empty() {
                lines.push(format!("  Depends on {}", dep_str.join(", ")));
            }
        }
    }

    // Show related if present
    if batch.num_columns() > items_col::RELATED
        && let Some(rel_list) = batch
            .column(items_col::RELATED)
            .as_any()
            .downcast_ref::<ListArray>()
        && !rel_list.is_null(0)
        && !rel_list.value(0).is_empty()
    {
        let values = rel_list.value(0);
        if let Some(strs) = values.as_any().downcast_ref::<StringArray>() {
            let rel_str: Vec<&str> = (0..strs.len())
                .filter(|&i| !strs.is_null(i))
                .map(|i| strs.value(i))
                .collect();
            if !rel_str.is_empty() {
                lines.push(format!("  Related    {}", rel_str.join(", ")));
            }
        }
    }

    // Show resolution if present
    let resolutions = col_str(batch, items_col::RESOLUTION);
    if !resolutions.is_null(0) {
        lines.push(format!("  Resolution {}", resolutions.value(0)));
    }

    // Show closed_by if present
    let closed_bys = col_str(batch, items_col::CLOSED_BY);
    if !closed_bys.is_null(0) {
        lines.push(format!("  Closed by  {}", closed_bys.value(0)));
    }

    // Show updated_at if present
    if batch.num_columns() > items_col::UPDATED_AT {
        let updated_col = batch
            .column(items_col::UPDATED_AT)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>();
        if let Some(updated) = updated_col
            && !updated.is_null(0)
        {
            let ts = updated.value(0);
            let dt = chrono::DateTime::from_timestamp_millis(ts)
                .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            lines.push(format!("  Updated    {dt}"));
        }
    }

    // Show body content if present
    let bodies = col_str(batch, items_col::BODY);
    if !bodies.is_null(0) {
        let body = bodies.value(0);
        if !body.trim().is_empty() {
            lines.push(String::new());
            lines.push(format!("  {}", "─".repeat(76)));
            lines.push(String::new());
            lines.push(body.to_string());
        }
    }

    lines.join("\n") + "\n"
}

/// Format item detail with comments from the CommentsStore.
pub fn format_item_detail_with_comments(
    batch: &RecordBatch,
    comments: &[crate::comments::Comment],
) -> String {
    let base = format_item_detail(batch);
    let comment_section = crate::comments::format_comments(comments);
    if comment_section.is_empty() {
        base
    } else {
        format!("{}{}\n", base.trim_end(), comment_section)
    }
}

/// Format board view — items grouped by status.
pub fn format_board_view(batches: &[RecordBatch], states: &[String]) -> String {
    let mut lines = Vec::new();

    for state in states {
        let items: Vec<(&RecordBatch, usize)> = batches
            .iter()
            .flat_map(|batch| {
                let statuses = col_str(batch, items_col::STATUS);
                let deleted = batch
                    .column(items_col::DELETED)
                    .as_any()
                    .downcast_ref::<BooleanArray>()
                    .expect("deleted");
                (0..batch.num_rows())
                    .filter(|&i| !deleted.value(i) && statuses.value(i) == state)
                    .map(|i| (batch, i))
                    .collect::<Vec<_>>()
            })
            .collect();

        lines.push(format!("── {} ({}) ──", state, items.len()));

        if items.is_empty() {
            lines.push("  (empty)".to_string());
        } else {
            for (batch, i) in &items {
                let ids = col_str(batch, items_col::ID);
                let titles = col_str(batch, items_col::TITLE);
                let types = col_str(batch, items_col::ITEM_TYPE);
                let assignees = col_str(batch, items_col::ASSIGNEE);

                let icon = type_icon(types.value(*i));
                let assignee = if assignees.is_null(*i) {
                    ""
                } else {
                    assignees.value(*i)
                };

                let title = titles.value(*i);
                let display_title = truncate_with_ellipsis(title, 40);

                if assignee.is_empty() {
                    lines.push(format!("  {icon} {:<14}{}", ids.value(*i), display_title));
                } else {
                    lines.push(format!(
                        "  {icon} {:<14}{:<42}[{}]",
                        ids.value(*i),
                        display_title,
                        assignee
                    ));
                }
            }
        }
        lines.push(String::new());
    }

    lines.join("\n")
}

/// Format stats output.
pub fn format_stats(batches: &[RecordBatch], states: &[String]) -> String {
    let mut lines = Vec::new();
    let mut total = 0u32;

    lines.push("Board Statistics".to_string());
    lines.push(format!(" {}", "─".repeat(40)));

    // Count by status — show configured states first, then any extra statuses
    lines.push(String::new());
    lines.push("By Status:".to_string());
    let all_statuses = collect_unique_values(batches, items_col::STATUS);
    for state in states {
        let count = count_at_status(batches, state);
        total += count;
        lines.push(format!("  {:<20}{:>5}", state, count));
    }
    // Include items with non-configured statuses (e.g., research items with dev statuses)
    for status in &all_statuses {
        if !states.iter().any(|s| s == status) {
            let count = count_at_status(batches, status);
            total += count;
            lines.push(format!("  {:<20}{:>5}", status, count));
        }
    }
    lines.push(format!("  {:<20}{:>5}", "TOTAL", total));

    // Count by type
    lines.push(String::new());
    lines.push("By Type:".to_string());
    let types = collect_unique_values(batches, items_col::ITEM_TYPE);
    for t in &types {
        let count = count_at_type(batches, t);
        lines.push(format!("  {:<20}{:>5}", t, count));
    }

    lines.join("\n") + "\n"
}

/// Format recent history (items moved to done).
pub fn format_history(batches: &[RecordBatch], done_status: &str) -> String {
    let mut done_items = Vec::new();

    for batch in batches {
        let statuses = col_str(batch, items_col::STATUS);
        let deleted = batch
            .column(items_col::DELETED)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("deleted");

        for i in 0..batch.num_rows() {
            if !deleted.value(i) && statuses.value(i) == done_status {
                let ids = col_str(batch, items_col::ID);
                let titles = col_str(batch, items_col::TITLE);
                done_items.push(format!("  {} {}", ids.value(i), titles.value(i)));
            }
        }
    }

    if done_items.is_empty() {
        "No completed items.\n".to_string()
    } else {
        let mut lines = vec![format!("Completed Items ({}):", done_items.len())];
        lines.extend(done_items);
        lines.join("\n") + "\n"
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Truncate a string to `max_chars` characters, appending "..." if truncated.
/// Uses char boundaries to avoid panicking on multi-byte UTF-8.
fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars - 3).collect();
        format!("{truncated}...")
    }
}

fn col_str(batch: &RecordBatch, col: usize) -> &StringArray {
    batch
        .column(col)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("string column")
}

fn type_icon(item_type: &str) -> &'static str {
    match item_type {
        "expedition" => "X",
        "chore" => "C",
        "voyage" => "V",
        "hazard" => "!",
        "signal" => "~",
        "feature" => "F",
        "paper" => "P",
        "hypothesis" => "H",
        "experiment" => "E",
        "measure" => "M",
        "idea" => "?",
        "literature" => "L",
        _ => "•",
    }
}

fn count_at_status(batches: &[RecordBatch], status: &str) -> u32 {
    let mut count = 0u32;
    for batch in batches {
        let statuses = col_str(batch, items_col::STATUS);
        let deleted = batch
            .column(items_col::DELETED)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("deleted");
        for i in 0..batch.num_rows() {
            if !deleted.value(i) && statuses.value(i) == status {
                count += 1;
            }
        }
    }
    count
}

fn count_at_type(batches: &[RecordBatch], item_type: &str) -> u32 {
    let mut count = 0u32;
    for batch in batches {
        let types = col_str(batch, items_col::ITEM_TYPE);
        let deleted = batch
            .column(items_col::DELETED)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("deleted");
        for i in 0..batch.num_rows() {
            if !deleted.value(i) && types.value(i) == item_type {
                count += 1;
            }
        }
    }
    count
}

fn collect_unique_values(batches: &[RecordBatch], col: usize) -> Vec<String> {
    let mut values = std::collections::BTreeSet::new();
    for batch in batches {
        let arr = col_str(batch, col);
        let deleted = batch
            .column(items_col::DELETED)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("deleted");
        for i in 0..batch.num_rows() {
            if !deleted.value(i) {
                values.insert(arr.value(i).to_string());
            }
        }
    }
    values.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crud::{CreateItemInput, KanbanStore};
    use crate::item_type::ItemType;

    fn make_store() -> KanbanStore {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Arrow Engine".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
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
        store
    }

    #[test]
    fn test_format_item_table() {
        let store = make_store();
        let output = format_item_table(store.items_batches());
        assert!(output.contains("EX-1300"));
        assert!(output.contains("Arrow Engine"));
        assert!(output.contains("CH-1301"));
    }

    #[test]
    fn test_format_item_detail() {
        let store = make_store();
        let item = store.get_item("EX-1300").expect("get item");
        let output = format_item_detail(&item);
        assert!(output.contains("EX-1300"));
        assert!(output.contains("Arrow Engine"));
        assert!(output.contains("expedition"));
        assert!(output.contains("M5"));
    }

    #[test]
    fn test_format_board_view() {
        let store = make_store();
        let states = vec![
            "backlog".to_string(),
            "in_progress".to_string(),
            "done".to_string(),
        ];
        let output = format_board_view(store.items_batches(), &states);
        assert!(output.contains("backlog (2)"));
        assert!(output.contains("in_progress (0)"));
    }

    #[test]
    fn test_format_stats() {
        let store = make_store();
        let states = vec!["backlog".to_string(), "done".to_string()];
        let output = format_stats(store.items_batches(), &states);
        assert!(output.contains("backlog"));
        assert!(output.contains("TOTAL"));
    }

    #[test]
    fn test_format_empty_table() {
        let output = format_item_table(&[]);
        assert!(output.contains("No items found"));
    }

    #[test]
    fn test_truncate_multibyte_chars() {
        // Em-dash is 3 bytes — must not panic
        let title = "Arrow-Kanban Strangler Cutover — Replace yurtle-kanban";
        let result = truncate_with_ellipsis(title, 26);
        assert!(result.ends_with("..."));
        assert!(result.chars().count() <= 26);

        // Short title — no truncation
        let short = "Fix tests";
        assert_eq!(truncate_with_ellipsis(short, 26), "Fix tests");

        // Exactly at limit
        let exact = "a".repeat(26);
        assert_eq!(truncate_with_ellipsis(&exact, 26), exact);
    }

    #[test]
    fn test_list_with_multibyte_titles() {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Y-Layer Architecture — Baseline Metrics".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");
        // Should not panic on multi-byte chars
        let output = format_item_table(store.items_batches());
        assert!(output.contains("EX-1300"));
    }

    #[test]
    fn test_type_icons() {
        assert_eq!(type_icon("expedition"), "X");
        assert_eq!(type_icon("voyage"), "V");
        assert_eq!(type_icon("chore"), "C");
    }

    #[test]
    fn test_format_item_detail_with_body() {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Rich Content".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: Some("DGX".to_string()),
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: Some("## Phase 1\n\nDo the thing.".to_string()),
            })
            .expect("create");
        let item = store.get_item("EX-1300").expect("get item");
        let output = format_item_detail(&item);
        assert!(output.contains("Rich Content"));
        assert!(output.contains("## Phase 1"));
        assert!(output.contains("Do the thing."));
    }

    #[test]
    fn test_format_item_detail_without_body() {
        let store = make_store();
        let item = store.get_item("EX-1300").expect("get item");
        let output = format_item_detail(&item);
        assert!(output.contains("Arrow Engine"));
        // No body separator should appear
        assert!(!output.contains("## Phase"));
    }

    #[test]
    fn test_format_item_detail_shows_tags() {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Tagged Item".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: Some("M5".to_string()),
                tags: vec!["v14".to_string(), "reasoning-path".to_string()],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");
        let item = store.get_item("EX-1300").expect("get item");
        let output = format_item_detail(&item);
        assert!(
            output.contains("Tags       v14, reasoning-path"),
            "Tags should appear in show output. Got:\n{output}"
        );
    }

    #[test]
    fn test_format_item_detail_hides_empty_tags() {
        let store = make_store();
        let item = store.get_item("EX-1300").expect("get item");
        let output = format_item_detail(&item);
        assert!(
            !output.contains("Tags"),
            "Empty tags should not appear in show output"
        );
    }

    #[test]
    fn test_format_item_detail_shows_related() {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Linked Item".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: None,
                tags: vec![],
                related: vec!["VY-100".to_string(), "H-200".to_string()],
                depends_on: vec![],
                body: None,
            })
            .expect("create");
        let item = store.get_item("EX-1300").expect("get item");
        let output = format_item_detail(&item);
        assert!(
            output.contains("Related    VY-100, H-200"),
            "Related items should appear in show output. Got:\n{output}"
        );
    }

    #[test]
    fn test_format_item_detail_shows_depends_on() {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Dependent Item".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec!["EX-50".to_string()],
                body: None,
            })
            .expect("create");
        let item = store.get_item("EX-1300").expect("get item");
        let output = format_item_detail(&item);
        assert!(
            output.contains("Depends on EX-50"),
            "Depends on should appear in show output. Got:\n{output}"
        );
    }

    #[test]
    fn test_format_item_detail_hides_empty_related() {
        let store = make_store();
        let item = store.get_item("EX-1300").expect("get item");
        let output = format_item_detail(&item);
        assert!(
            !output.contains("Related"),
            "Empty related should not appear in show output"
        );
        assert!(
            !output.contains("Depends on"),
            "Empty depends_on should not appear in show output"
        );
    }

    #[test]
    fn test_format_item_detail_shows_both_related_and_depends() {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Fully Linked".to_string(),
                item_type: ItemType::Voyage,
                priority: Some("critical".to_string()),
                assignee: Some("M5".to_string()),
                tags: vec!["v14".to_string()],
                related: vec![
                    "H-10".to_string(),
                    "M-20".to_string(),
                    "EXPR-30".to_string(),
                ],
                depends_on: vec!["EX-5".to_string(), "EX-6".to_string()],
                body: Some("Phase 1: Do things".to_string()),
            })
            .expect("create");
        let item = store.get_item("VY-1300").expect("get item");
        let output = format_item_detail(&item);
        assert!(output.contains("Depends on EX-5, EX-6"), "Got:\n{output}");
        assert!(
            output.contains("Related    H-10, M-20, EXPR-30"),
            "Got:\n{output}"
        );
        assert!(output.contains("Tags       v14"), "Got:\n{output}");
        assert!(output.contains("Phase 1"), "Body should still render");
    }
}
