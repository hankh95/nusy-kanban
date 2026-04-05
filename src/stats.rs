//! Analytics — velocity, burndown, and agent throughput from RunsTable.
//!
//! All computations operate on Arrow RecordBatches from the runs and items
//! tables. No external dependencies — uses existing RunsTable data.

use crate::schema::{items_col, runs_col};
use arrow::array::{Array, BooleanArray, RecordBatch, StringArray, TimestampMillisecondArray};
use std::collections::HashMap;

/// One week in milliseconds.
const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// One day in milliseconds.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Terminal statuses that count as "completed" for velocity purposes.
const TERMINAL_STATUSES: &[&str] = &["done", "complete", "arrived", "retired", "abandoned"];

/// A single week's velocity measurement.
#[derive(Debug, Clone)]
pub struct WeeklyVelocity {
    /// Week start timestamp (Monday 00:00 UTC), milliseconds.
    pub week_start_ms: i64,
    /// Human-readable week label (e.g., "Mar 11–17").
    pub label: String,
    /// Number of items completed this week.
    pub completed: u32,
}

/// Agent throughput — items completed per agent.
#[derive(Debug, Clone)]
pub struct AgentStats {
    pub agent: String,
    pub completed: u32,
    pub moves: u32,
}

/// A burndown data point.
#[derive(Debug, Clone)]
pub struct BurndownPoint {
    /// Timestamp (milliseconds).
    pub timestamp_ms: i64,
    /// Human-readable date label.
    pub label: String,
    /// Items remaining (not in terminal state).
    pub remaining: u32,
}

/// Compute weekly velocity for the last `weeks` weeks from RunsTable.
///
/// Counts transitions to terminal statuses per week.
pub fn compute_velocity(runs_batches: &[RecordBatch], weeks: u32) -> Vec<WeeklyVelocity> {
    // Add 1ms to now to include transitions that happen at exactly now_ms
    let now_ms = chrono::Utc::now().timestamp_millis() + 1;
    let cutoff_ms = now_ms - (weeks as i64 * WEEK_MS);

    // Collect all terminal transitions after cutoff
    let mut transitions: Vec<i64> = Vec::new();
    for batch in runs_batches {
        let Some(to_statuses) = col_str(batch, runs_col::TO_STATUS) else {
            continue;
        };
        let Some(timestamps) = col_ts(batch, runs_col::TIMESTAMP) else {
            continue;
        };

        for i in 0..batch.num_rows() {
            let ts = timestamps.value(i);
            if ts >= cutoff_ms && is_terminal(to_statuses.value(i)) {
                transitions.push(ts);
            }
        }
    }

    // Bucket into weeks (most recent first)
    let mut result = Vec::new();
    for w in 0..weeks {
        let week_end = now_ms - (w as i64 * WEEK_MS);
        let week_start = week_end - WEEK_MS;
        let count = transitions
            .iter()
            .filter(|&&ts| ts >= week_start && ts < week_end)
            .count() as u32;

        let label = format_week_label(week_start);
        result.push(WeeklyVelocity {
            week_start_ms: week_start,
            label,
            completed: count,
        });
    }

    // Reverse so oldest week is first
    result.reverse();
    result
}

/// Compute burndown: items remaining at weekly snapshots since `since_ms`.
///
/// Walks the RunsTable forward, tracking net additions and completions.
pub fn compute_burndown(
    items_batches: &[RecordBatch],
    runs_batches: &[RecordBatch],
    since_ms: i64,
) -> Vec<BurndownPoint> {
    let now_ms = chrono::Utc::now().timestamp_millis();

    // Count total active (non-deleted, non-terminal) items at current time
    let current_active = count_active_items(items_batches);

    // Collect all terminal transitions with timestamps
    let mut completions: Vec<i64> = Vec::new();
    // Collect all creation events (to_status from null/initial)
    let mut creations: Vec<i64> = Vec::new();

    for batch in runs_batches {
        let Some(to_statuses) = col_str(batch, runs_col::TO_STATUS) else {
            continue;
        };
        let Some(from_statuses) = col_str(batch, runs_col::FROM_STATUS) else {
            continue;
        };
        let Some(timestamps) = col_ts(batch, runs_col::TIMESTAMP) else {
            continue;
        };

        for i in 0..batch.num_rows() {
            let ts = timestamps.value(i);
            if ts < since_ms {
                continue;
            }

            if is_terminal(to_statuses.value(i)) {
                completions.push(ts);
            }
            if from_statuses.is_null(i) || from_statuses.value(i) == "backlog" {
                // Approximate creation events
                if !is_terminal(to_statuses.value(i)) {
                    creations.push(ts);
                }
            }
        }
    }

    // Generate weekly snapshots
    let mut points = Vec::new();

    // First, collect all weekly boundaries
    let mut boundaries = Vec::new();
    let mut bt = now_ms;
    while bt >= since_ms {
        boundaries.push(bt);
        bt -= WEEK_MS;
    }
    boundaries.reverse();

    // For each boundary, count remaining by adjusting from current
    for &boundary in &boundaries {
        // Items completed between boundary and now → were still active at boundary
        let completed_after = completions.iter().filter(|&&ts| ts > boundary).count() as u32;
        // Items created between boundary and now → didn't exist at boundary
        let created_after = creations.iter().filter(|&&ts| ts > boundary).count() as u32;

        let remaining_at =
            current_active + completed_after - created_after.min(current_active + completed_after);

        points.push(BurndownPoint {
            timestamp_ms: boundary,
            label: format_date_label(boundary),
            remaining: remaining_at,
        });
    }

    points
}

/// Compute per-agent throughput from RunsTable.
pub fn compute_agent_stats(runs_batches: &[RecordBatch]) -> Vec<AgentStats> {
    let mut agent_completed: HashMap<String, u32> = HashMap::new();
    let mut agent_moves: HashMap<String, u32> = HashMap::new();

    for batch in runs_batches {
        let Some(agents) = col_str(batch, runs_col::BY_AGENT) else {
            continue;
        };
        let Some(to_statuses) = col_str(batch, runs_col::TO_STATUS) else {
            continue;
        };

        for i in 0..batch.num_rows() {
            if agents.is_null(i) {
                continue;
            }
            let agent = agents.value(i).to_string();
            *agent_moves.entry(agent.clone()).or_insert(0) += 1;
            if is_terminal(to_statuses.value(i)) {
                *agent_completed.entry(agent).or_insert(0) += 1;
            }
        }
    }

    let mut result: Vec<AgentStats> = agent_moves
        .into_iter()
        .map(|(agent, moves)| AgentStats {
            completed: *agent_completed.get(&agent).unwrap_or(&0),
            moves,
            agent,
        })
        .collect();

    result.sort_by(|a, b| b.completed.cmp(&a.completed));
    result
}

/// Filter history: items completed since `since_ms`, optionally by assignee.
pub fn filter_history(
    items_batches: &[RecordBatch],
    runs_batches: &[RecordBatch],
    since_ms: i64,
    by_assignee: Option<&str>,
) -> Vec<HistoryEntry> {
    // Build set of item IDs that transitioned to terminal after since_ms
    let mut completed_ids: HashMap<String, i64> = HashMap::new();
    for batch in runs_batches {
        let Some(item_ids) = col_str(batch, runs_col::ITEM_ID) else {
            continue;
        };
        let Some(to_statuses) = col_str(batch, runs_col::TO_STATUS) else {
            continue;
        };
        let Some(timestamps) = col_ts(batch, runs_col::TIMESTAMP) else {
            continue;
        };

        for i in 0..batch.num_rows() {
            let ts = timestamps.value(i);
            if ts >= since_ms && is_terminal(to_statuses.value(i)) {
                let id = item_ids.value(i).to_string();
                // Keep the latest completion timestamp
                let entry = completed_ids.entry(id).or_insert(0);
                if ts > *entry {
                    *entry = ts;
                }
            }
        }
    }

    // Collect matching items with their details
    let mut entries = Vec::new();
    for batch in items_batches {
        let Some(ids) = col_str(batch, items_col::ID) else {
            continue;
        };
        let Some(titles) = col_str(batch, items_col::TITLE) else {
            continue;
        };
        let Some(assignees) = col_str(batch, items_col::ASSIGNEE) else {
            continue;
        };
        let Some(deleted) = col_bool(batch, items_col::DELETED) else {
            continue;
        };

        for i in 0..batch.num_rows() {
            if deleted.value(i) {
                continue;
            }
            let id = ids.value(i);
            if let Some(&completed_ts) = completed_ids.get(id) {
                let assignee = if assignees.is_null(i) {
                    String::new()
                } else {
                    assignees.value(i).to_string()
                };

                // Filter by assignee if specified
                if let Some(filter_assignee) = by_assignee
                    && !assignee.eq_ignore_ascii_case(filter_assignee)
                {
                    continue;
                }

                entries.push(HistoryEntry {
                    id: id.to_string(),
                    title: titles.value(i).to_string(),
                    assignee,
                    completed_ms: completed_ts,
                    completed_label: format_date_label(completed_ts),
                });
            }
        }
    }

    // Sort by completion time, most recent first
    entries.sort_by(|a, b| b.completed_ms.cmp(&a.completed_ms));
    entries
}

/// A completed item for history display.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id: String,
    pub title: String,
    pub assignee: String,
    pub completed_ms: i64,
    pub completed_label: String,
}

// ─── Formatting ─────────────────────────────────────────────────────────────

/// Format velocity as a terminal table.
pub fn format_velocity(velocity: &[WeeklyVelocity]) -> String {
    if velocity.is_empty() {
        return "No velocity data.\n".to_string();
    }

    let mut lines = Vec::new();
    lines.push("Velocity (items/week)".to_string());
    lines.push(format!(" {}", "─".repeat(40)));

    let max_completed = velocity
        .iter()
        .map(|v| v.completed)
        .max()
        .unwrap_or(1)
        .max(1);

    for v in velocity {
        let bar_len = (v.completed as f64 / max_completed as f64 * 20.0).round() as usize;
        let bar: String = "█".repeat(bar_len);
        lines.push(format!("  {:<14} {:>3}  {}", v.label, v.completed, bar));
    }

    let total: u32 = velocity.iter().map(|v| v.completed).sum();
    let avg = total as f64 / velocity.len() as f64;
    lines.push(format!(" {}", "─".repeat(40)));
    lines.push(format!("  Average: {:.1} items/week", avg));

    lines.join("\n") + "\n"
}

/// Format burndown as a terminal table.
pub fn format_burndown(points: &[BurndownPoint]) -> String {
    if points.is_empty() {
        return "No burndown data.\n".to_string();
    }

    let mut lines = Vec::new();
    lines.push("Burndown (items remaining)".to_string());
    lines.push(format!(" {}", "─".repeat(40)));

    let max_remaining = points.iter().map(|p| p.remaining).max().unwrap_or(1).max(1);

    for p in points {
        let bar_len = (p.remaining as f64 / max_remaining as f64 * 20.0).round() as usize;
        let bar: String = "▓".repeat(bar_len);
        lines.push(format!("  {:<14} {:>3}  {}", p.label, p.remaining, bar));
    }

    lines.join("\n") + "\n"
}

/// Format agent stats as a terminal table.
pub fn format_agent_stats(stats: &[AgentStats]) -> String {
    if stats.is_empty() {
        return "No agent activity.\n".to_string();
    }

    let mut lines = Vec::new();
    lines.push("Agent Throughput".to_string());
    lines.push(format!(" {}", "─".repeat(50)));
    lines.push(format!(
        "  {:<16}{:>10}{:>10}",
        "Agent", "Completed", "Moves"
    ));
    lines.push(format!("  {}", "─".repeat(36)));

    for s in stats {
        lines.push(format!(
            "  {:<16}{:>10}{:>10}",
            s.agent, s.completed, s.moves
        ));
    }

    lines.join("\n") + "\n"
}

/// Format history entries as a terminal table.
pub fn format_history_entries(entries: &[HistoryEntry]) -> String {
    if entries.is_empty() {
        return "No completed items in this period.\n".to_string();
    }

    let mut lines = Vec::new();
    lines.push(format!("Completed Items ({}):", entries.len()));
    lines.push(format!(
        "  {:<14}{:<30}{:<12}{:<12}",
        "ID", "Title", "Assignee", "Completed"
    ));
    lines.push(format!("  {}", "─".repeat(66)));

    for e in entries {
        let title = if e.title.chars().count() > 28 {
            let truncated: String = e.title.chars().take(25).collect();
            format!("{truncated}...")
        } else {
            e.title.clone()
        };
        let assignee = if e.assignee.is_empty() {
            "-"
        } else {
            &e.assignee
        };
        lines.push(format!(
            "  {:<14}{:<30}{:<12}{:<12}",
            e.id, title, assignee, e.completed_label
        ));
    }

    lines.join("\n") + "\n"
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn is_terminal(status: &str) -> bool {
    TERMINAL_STATUSES.contains(&status)
}

fn count_active_items(items_batches: &[RecordBatch]) -> u32 {
    let mut count = 0u32;
    for batch in items_batches {
        let Some(statuses) = col_str(batch, items_col::STATUS) else {
            continue;
        };
        let Some(deleted) = col_bool(batch, items_col::DELETED) else {
            continue;
        };

        for i in 0..batch.num_rows() {
            if !deleted.value(i) && !is_terminal(statuses.value(i)) {
                count += 1;
            }
        }
    }
    count
}

fn col_str(batch: &RecordBatch, col: usize) -> Option<&StringArray> {
    batch.column(col).as_any().downcast_ref::<StringArray>()
}

fn col_ts(batch: &RecordBatch, col: usize) -> Option<&TimestampMillisecondArray> {
    batch
        .column(col)
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
}

fn col_bool(batch: &RecordBatch, col: usize) -> Option<&BooleanArray> {
    batch.column(col).as_any().downcast_ref::<BooleanArray>()
}

/// Format a timestamp as a week label (e.g., "Mar 11–17").
fn format_week_label(start_ms: i64) -> String {
    use chrono::{DateTime, Datelike};
    let start = DateTime::from_timestamp_millis(start_ms).unwrap_or_default();
    let end = DateTime::from_timestamp_millis(start_ms + WEEK_MS - DAY_MS).unwrap_or_default();

    let month = start.format("%b");
    if start.month() == end.month() {
        format!("{} {}–{}", month, start.day(), end.day())
    } else {
        format!(
            "{} {}–{} {}",
            month,
            start.day(),
            end.format("%b"),
            end.day()
        )
    }
}

/// Format a timestamp as a date label (e.g., "Mar 15").
fn format_date_label(ts_ms: i64) -> String {
    use chrono::{DateTime, Datelike};
    let dt = DateTime::from_timestamp_millis(ts_ms).unwrap_or_default();
    format!("{} {}", dt.format("%b"), dt.day())
}

/// Parse a date string like "2026-03-01" to milliseconds since epoch.
pub fn parse_date_to_ms(date_str: &str) -> Option<i64> {
    // Try YYYY-MM-DD format
    if let Ok(date) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
        let dt = date.and_hms_opt(0, 0, 0)?.and_utc();
        return Some(dt.timestamp_millis());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crud::{CreateItemInput, KanbanStore};
    use crate::item_type::ItemType;

    fn make_store_with_runs() -> KanbanStore {
        let mut store = KanbanStore::new();

        // Create some items
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
                assignee: Some("Mini".to_string()),
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");

        store
            .create_item(&CreateItemInput {
                title: "Documentation".to_string(),
                item_type: ItemType::Chore,
                priority: Some("low".to_string()),
                assignee: Some("DGX".to_string()),
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .expect("create");

        // Move some items to done (creates runs)
        store
            .update_status("EX-1300", "in_progress", Some("M5"), false, None)
            .expect("move");
        store
            .update_status("EX-1300", "done", Some("M5"), false, None)
            .expect("move");
        store
            .update_status("CH-1301", "in_progress", Some("Mini"), false, None)
            .expect("move");
        store
            .update_status("CH-1301", "done", Some("Mini"), false, None)
            .expect("move");

        store
    }

    #[test]
    fn test_compute_velocity() {
        let store = make_store_with_runs();
        let velocity = compute_velocity(store.runs_batches(), 4);

        assert_eq!(velocity.len(), 4);
        // The most recent week should have 2 completions (EX-1300 and CH-1301)
        let total: u32 = velocity.iter().map(|v| v.completed).sum();
        assert_eq!(total, 2);
        // Last week (most recent) should have the completions
        assert_eq!(velocity.last().unwrap().completed, 2);
    }

    #[test]
    fn test_compute_agent_stats() {
        let store = make_store_with_runs();
        let stats = compute_agent_stats(store.runs_batches());

        assert!(!stats.is_empty());
        // M5 should have completed 1 item and made 2 moves (backlog→in_progress, in_progress→done)
        let m5 = stats.iter().find(|s| s.agent == "M5").expect("M5 stats");
        assert_eq!(m5.completed, 1);
        assert_eq!(m5.moves, 2);

        // Mini should have completed 1 item
        let mini = stats
            .iter()
            .find(|s| s.agent == "Mini")
            .expect("Mini stats");
        assert_eq!(mini.completed, 1);
    }

    #[test]
    fn test_compute_burndown() {
        let store = make_store_with_runs();
        let since_ms = chrono::Utc::now().timestamp_millis() - (4 * WEEK_MS);
        let points = compute_burndown(store.items_batches(), store.runs_batches(), since_ms);

        assert!(!points.is_empty());
        // The last point should show current active items
        // We created 3 items, 2 are done → 1 remaining
        let last = points.last().unwrap();
        assert_eq!(last.remaining, 1);
    }

    #[test]
    fn test_filter_history() {
        let store = make_store_with_runs();
        let since_ms = chrono::Utc::now().timestamp_millis() - WEEK_MS;
        let entries = filter_history(store.items_batches(), store.runs_batches(), since_ms, None);

        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_filter_history_by_assignee() {
        let store = make_store_with_runs();
        let since_ms = chrono::Utc::now().timestamp_millis() - WEEK_MS;
        let entries = filter_history(
            store.items_batches(),
            store.runs_batches(),
            since_ms,
            Some("M5"),
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "EX-1300");
    }

    #[test]
    fn test_format_velocity() {
        let velocity = vec![
            WeeklyVelocity {
                week_start_ms: 0,
                label: "Mar 4–10".to_string(),
                completed: 3,
            },
            WeeklyVelocity {
                week_start_ms: WEEK_MS,
                label: "Mar 11–17".to_string(),
                completed: 5,
            },
        ];
        let output = format_velocity(&velocity);
        assert!(output.contains("Velocity"));
        assert!(output.contains("Mar 4–10"));
        assert!(output.contains("Mar 11–17"));
        assert!(output.contains("Average: 4.0"));
    }

    #[test]
    fn test_format_agent_stats() {
        let stats = vec![
            AgentStats {
                agent: "M5".to_string(),
                completed: 5,
                moves: 12,
            },
            AgentStats {
                agent: "Mini".to_string(),
                completed: 3,
                moves: 8,
            },
        ];
        let output = format_agent_stats(&stats);
        assert!(output.contains("Agent Throughput"));
        assert!(output.contains("M5"));
        assert!(output.contains("Mini"));
    }

    #[test]
    fn test_parse_date_to_ms() {
        let ms = parse_date_to_ms("2026-03-01").expect("valid date");
        assert!(ms > 0);

        assert!(parse_date_to_ms("invalid").is_none());
        assert!(parse_date_to_ms("").is_none());
    }

    #[test]
    fn test_empty_runs() {
        let store = KanbanStore::new();
        let velocity = compute_velocity(store.runs_batches(), 4);
        assert_eq!(velocity.len(), 4);
        assert!(velocity.iter().all(|v| v.completed == 0));

        let stats = compute_agent_stats(store.runs_batches());
        assert!(stats.is_empty());
    }

    #[test]
    fn test_is_terminal() {
        assert!(is_terminal("done"));
        assert!(is_terminal("complete"));
        assert!(is_terminal("abandoned"));
        assert!(is_terminal("retired"));
        assert!(!is_terminal("backlog"));
        assert!(!is_terminal("in_progress"));
    }

    #[test]
    fn test_format_history_entries() {
        let entries = vec![HistoryEntry {
            id: "EX-1300".to_string(),
            title: "Arrow Engine".to_string(),
            assignee: "M5".to_string(),
            completed_ms: chrono::Utc::now().timestamp_millis(),
            completed_label: "Mar 18".to_string(),
        }];
        let output = format_history_entries(&entries);
        assert!(output.contains("Completed Items (1)"));
        assert!(output.contains("EX-1300"));
        assert!(output.contains("Arrow Engine"));
    }

    #[test]
    fn test_format_burndown() {
        let points = vec![
            BurndownPoint {
                timestamp_ms: 0,
                label: "Mar 4".to_string(),
                remaining: 10,
            },
            BurndownPoint {
                timestamp_ms: WEEK_MS,
                label: "Mar 11".to_string(),
                remaining: 7,
            },
        ];
        let output = format_burndown(&points);
        assert!(output.contains("Burndown"));
        assert!(output.contains("Mar 4"));
        assert!(output.contains("Mar 11"));
    }
}
