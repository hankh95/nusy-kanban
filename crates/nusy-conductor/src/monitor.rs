//! Blocker detection + progress tracking + daily summary.
//!
//! Monitors the work graph for:
//! - Stale items (in_progress longer than threshold)
//! - Blocked items (unfinished dependencies)
//! - Resource conflicts (e.g., GPU queue contention)
//!
//! Generates human-readable daily summaries broadcast via NATS.

use crate::reader::{WorkGraph, WorkItem};
use crate::state::{AgentState, AssigneeTracker};
use std::collections::HashMap;
use std::time::Duration;

/// Default staleness threshold: 2 days.
const DEFAULT_STALE_THRESHOLD: Duration = Duration::from_secs(2 * 24 * 3600);

/// Configuration for the blocker monitor.
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    /// How long an item can be in_progress before it's flagged as stale.
    pub stale_threshold: Duration,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        MonitorConfig {
            stale_threshold: DEFAULT_STALE_THRESHOLD,
        }
    }
}

/// A flagged item in the monitoring report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlaggedItem {
    /// The item ID.
    pub id: String,
    /// The item title.
    pub title: String,
    /// Assignee, if any.
    pub assignee: Option<String>,
    /// Why this item was flagged.
    pub reason: FlagReason,
}

/// Why an item was flagged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagReason {
    /// In progress longer than the threshold.
    Stale { status: String },
    /// Blocked by one or more incomplete dependencies.
    BlockedByDeps { blockers: Vec<String> },
    /// Resource conflict (e.g., GPU needed but occupied).
    ResourceConflict { resource: String, held_by: String },
}

impl std::fmt::Display for FlagReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stale { status } => write!(f, "stale ({status})"),
            Self::BlockedByDeps { blockers } => {
                write!(f, "blocked by: {}", blockers.join(", "))
            }
            Self::ResourceConflict { resource, held_by } => {
                write!(f, "{resource} conflict (held by {held_by})")
            }
        }
    }
}

/// Daily summary report.
#[derive(Debug, Clone)]
pub struct DailySummary {
    /// Items completed (status = done) — for the summary period.
    pub completed: Vec<SummaryItem>,
    /// Items currently in progress.
    pub in_progress: Vec<SummaryItem>,
    /// Flagged items (stale, blocked, conflicts).
    pub flagged: Vec<FlaggedItem>,
    /// Agent availability summary.
    pub agent_states: Vec<AgentState>,
    /// Status counts across the board.
    pub status_counts: HashMap<String, usize>,
}

/// A summarized item for the daily report.
#[derive(Debug, Clone)]
pub struct SummaryItem {
    pub id: String,
    pub title: String,
    pub assignee: Option<String>,
    pub status: String,
}

impl From<&WorkItem> for SummaryItem {
    fn from(item: &WorkItem) -> Self {
        SummaryItem {
            id: item.id.clone(),
            title: item.title.clone(),
            assignee: item.assignee.clone(),
            status: item.status.clone(),
        }
    }
}

/// The blocker monitor — analyzes the work graph for issues.
pub struct BlockerMonitor {
    /// TODO: Use stale_threshold when WorkItem gains timestamps.
    #[allow(dead_code)]
    config: MonitorConfig,
    tracker: AssigneeTracker,
}

impl BlockerMonitor {
    /// Create with default config and fleet profiles.
    pub fn new() -> Self {
        BlockerMonitor {
            config: MonitorConfig::default(),
            tracker: AssigneeTracker::with_defaults(),
        }
    }

    /// Create with custom config.
    pub fn with_config(config: MonitorConfig) -> Self {
        BlockerMonitor {
            config,
            tracker: AssigneeTracker::with_defaults(),
        }
    }

    /// Detect all flagged items in the work graph.
    pub fn detect_issues(&self, graph: &WorkGraph) -> Vec<FlaggedItem> {
        let mut flagged = Vec::new();
        flagged.extend(self.detect_stale_items(graph));
        flagged.extend(self.detect_blocked_items(graph));
        flagged.extend(self.detect_resource_conflicts(graph));
        // Sort by ID for deterministic output
        flagged.sort_by(|a, b| a.id.cmp(&b.id));
        flagged
    }

    /// Detect items that have been in_progress too long.
    ///
    /// Note: Without timestamps in the current WorkItem model, we flag ALL
    /// in_progress items as potentially stale. The conductor service will
    /// track actual timestamps when running as a persistent process.
    /// For now, this flags items for human review.
    pub fn detect_stale_items(&self, graph: &WorkGraph) -> Vec<FlaggedItem> {
        graph
            .items_by_status("in_progress")
            .into_iter()
            .map(|item| FlaggedItem {
                id: item.id.clone(),
                title: item.title.clone(),
                assignee: item.assignee.clone(),
                reason: FlagReason::Stale {
                    status: "in_progress".to_string(),
                },
            })
            .collect()
    }

    /// Detect items blocked by incomplete dependencies.
    pub fn detect_blocked_items(&self, graph: &WorkGraph) -> Vec<FlaggedItem> {
        let mut flagged = Vec::new();

        for item in graph.items.values() {
            // Only check non-done items
            if item.status == "done" || item.status == "complete" {
                continue;
            }

            if let Some(deps) = graph.depends_on.get(&item.id) {
                let incomplete_deps: Vec<String> = deps
                    .iter()
                    .filter(|dep_id| {
                        graph
                            .items
                            .get(dep_id.as_str())
                            .is_some_and(|dep| dep.status != "done" && dep.status != "complete")
                    })
                    .cloned()
                    .collect();

                if !incomplete_deps.is_empty() {
                    flagged.push(FlaggedItem {
                        id: item.id.clone(),
                        title: item.title.clone(),
                        assignee: item.assignee.clone(),
                        reason: FlagReason::BlockedByDeps {
                            blockers: incomplete_deps,
                        },
                    });
                }
            }
        }

        flagged
    }

    /// Detect resource conflicts (GPU contention).
    ///
    /// If multiple in_progress items are tagged "gpu" and assigned to different
    /// agents, flag the conflict.
    pub fn detect_resource_conflicts(&self, graph: &WorkGraph) -> Vec<FlaggedItem> {
        let mut gpu_items: Vec<&WorkItem> = graph
            .items
            .values()
            .filter(|item| {
                item.status == "in_progress"
                    && item.tags.iter().any(|t| {
                        t.to_lowercase().contains("gpu") || t.to_lowercase().contains("training")
                    })
            })
            .collect();
        // Sort for deterministic "first holder" selection
        gpu_items.sort_by(|a, b| a.id.cmp(&b.id));

        // If more than one GPU item is in_progress, flag all but the first
        if gpu_items.len() <= 1 {
            return Vec::new();
        }

        let first_holder = gpu_items[0].assignee.as_deref().unwrap_or("unassigned");

        gpu_items[1..]
            .iter()
            .map(|item| FlaggedItem {
                id: item.id.clone(),
                title: item.title.clone(),
                assignee: item.assignee.clone(),
                reason: FlagReason::ResourceConflict {
                    resource: "GPU".to_string(),
                    held_by: first_holder.to_string(),
                },
            })
            .collect()
    }

    /// Generate a daily summary of the work graph.
    pub fn daily_summary(&self, graph: &WorkGraph) -> DailySummary {
        let completed: Vec<SummaryItem> = graph
            .items_by_status("done")
            .into_iter()
            .map(SummaryItem::from)
            .collect();

        let in_progress: Vec<SummaryItem> = graph
            .items_by_status("in_progress")
            .into_iter()
            .map(SummaryItem::from)
            .collect();

        let flagged = self.detect_issues(graph);
        let agent_states = self.tracker.agent_states(graph);
        let status_counts = graph.status_summary();

        DailySummary {
            completed,
            in_progress,
            flagged,
            agent_states,
            status_counts,
        }
    }
}

impl Default for BlockerMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// Format a daily summary as human-readable markdown.
pub fn format_daily_summary(summary: &DailySummary) -> String {
    let mut out = String::new();

    out.push_str("# Daily Board Summary\n\n");

    // Status overview
    out.push_str("## Status Overview\n\n");
    let mut statuses: Vec<_> = summary.status_counts.iter().collect();
    statuses.sort_by_key(|(k, _)| (*k).clone());
    for (status, count) in &statuses {
        out.push_str(&format!("  {status}: {count}\n"));
    }
    out.push('\n');

    // In progress
    if !summary.in_progress.is_empty() {
        out.push_str("## In Progress\n\n");
        for item in &summary.in_progress {
            let assignee = item.assignee.as_deref().unwrap_or("unassigned");
            out.push_str(&format!("  {} {} ({})\n", item.id, item.title, assignee));
        }
        out.push('\n');
    }

    // Flagged items
    if !summary.flagged.is_empty() {
        out.push_str("## Flagged Items\n\n");
        for item in &summary.flagged {
            out.push_str(&format!(
                "  ! {} {} — {}\n",
                item.id, item.title, item.reason
            ));
        }
        out.push('\n');
    } else {
        out.push_str("## Flagged Items\n\n  None\n\n");
    }

    // Agent availability
    out.push_str("## Agent Status\n\n");
    for state in &summary.agent_states {
        let status = if state.available { "available" } else { "busy" };
        let wip = state.in_progress.len();
        out.push_str(&format!(
            "  {} — {} ({} in-progress)\n",
            state.profile.name, status, wip
        ));
    }
    out.push('\n');

    // Completed count
    out.push_str(&format!(
        "## Completed: {} items\n",
        summary.completed.len()
    ));

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::WorkItem;

    fn make_item(
        id: &str,
        title: &str,
        status: &str,
        assignee: Option<&str>,
        tags: Vec<&str>,
        depends_on: Vec<&str>,
    ) -> WorkItem {
        WorkItem {
            id: id.to_string(),
            title: title.to_string(),
            item_type: "expedition".to_string(),
            status: status.to_string(),
            priority: Some("medium".to_string()),
            assignee: assignee.map(String::from),
            board: Some("development".to_string()),
            tags: tags.into_iter().map(String::from).collect(),
            related: vec![],
            depends_on: depends_on.into_iter().map(String::from).collect(),
            body: None,
        }
    }

    fn sample_graph() -> WorkGraph {
        WorkGraph::from_items(vec![
            make_item(
                "EX-100",
                "Arrow schemas",
                "done",
                Some("M5"),
                vec![],
                vec![],
            ),
            make_item(
                "EX-101",
                "NATS server",
                "in_progress",
                Some("DGX"),
                vec![],
                vec!["EX-100"],
            ),
            make_item(
                "EX-102",
                "Integration tests",
                "backlog",
                None,
                vec![],
                vec!["EX-101"],
            ),
            make_item(
                "EX-103",
                "GPU training pipeline",
                "in_progress",
                Some("DGX"),
                vec!["gpu", "training"],
                vec![],
            ),
            make_item(
                "EX-104",
                "GPU eval suite",
                "in_progress",
                Some("Mini"),
                vec!["gpu"],
                vec![],
            ),
            make_item("CH-200", "Clean up CI", "backlog", None, vec![], vec![]),
        ])
    }

    // ── Stale detection ─────────────────────────────────────────────────

    #[test]
    fn test_detect_stale_items() {
        let monitor = BlockerMonitor::new();
        let graph = sample_graph();

        let stale = monitor.detect_stale_items(&graph);
        // EX-101, EX-103, EX-104 are in_progress
        assert_eq!(stale.len(), 3);
        let ids: Vec<&str> = stale.iter().map(|f| f.id.as_str()).collect();
        assert!(ids.contains(&"EX-101"));
        assert!(ids.contains(&"EX-103"));
        assert!(ids.contains(&"EX-104"));
    }

    #[test]
    fn test_stale_items_excludes_done() {
        let monitor = BlockerMonitor::new();
        let graph = sample_graph();
        let stale = monitor.detect_stale_items(&graph);
        let ids: Vec<&str> = stale.iter().map(|f| f.id.as_str()).collect();
        assert!(!ids.contains(&"EX-100")); // done
        assert!(!ids.contains(&"CH-200")); // backlog
    }

    // ── Blocked detection ───────────────────────────────────────────────

    #[test]
    fn test_detect_blocked_items() {
        let monitor = BlockerMonitor::new();
        let graph = sample_graph();

        let blocked = monitor.detect_blocked_items(&graph);
        // EX-102 depends on EX-101 (in_progress)
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].id, "EX-102");
        if let FlagReason::BlockedByDeps { blockers } = &blocked[0].reason {
            assert!(blockers.contains(&"EX-101".to_string()));
        } else {
            panic!("expected BlockedByDeps");
        }
    }

    #[test]
    fn test_blocked_clears_when_dep_done() {
        let monitor = BlockerMonitor::new();
        let mut graph = sample_graph();
        graph.apply_moved("EX-101", "done");

        let blocked = monitor.detect_blocked_items(&graph);
        // EX-102 depends on EX-101 which is now done → not blocked
        assert!(blocked.is_empty());
    }

    #[test]
    fn test_done_items_not_flagged_as_blocked() {
        let monitor = BlockerMonitor::new();
        let graph = sample_graph();
        let blocked = monitor.detect_blocked_items(&graph);
        let ids: Vec<&str> = blocked.iter().map(|f| f.id.as_str()).collect();
        assert!(!ids.contains(&"EX-100")); // done items never flagged
    }

    // ── Resource conflict detection ─────────────────────────────────────

    #[test]
    fn test_detect_gpu_conflict() {
        let monitor = BlockerMonitor::new();
        let graph = sample_graph();

        let conflicts = monitor.detect_resource_conflicts(&graph);
        // EX-103 (gpu, DGX) and EX-104 (gpu, Mini) are both in_progress
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].id, "EX-104"); // second GPU item flagged
        if let FlagReason::ResourceConflict { resource, held_by } = &conflicts[0].reason {
            assert_eq!(resource, "GPU");
            assert_eq!(held_by, "DGX");
        }
    }

    #[test]
    fn test_no_gpu_conflict_single_item() {
        let monitor = BlockerMonitor::new();
        let graph = WorkGraph::from_items(vec![make_item(
            "EX-103",
            "GPU training",
            "in_progress",
            Some("DGX"),
            vec!["gpu"],
            vec![],
        )]);

        let conflicts = monitor.detect_resource_conflicts(&graph);
        assert!(conflicts.is_empty());
    }

    // ── Combined detection ──────────────────────────────────────────────

    #[test]
    fn test_detect_issues_all_types() {
        let monitor = BlockerMonitor::new();
        let graph = sample_graph();

        let issues = monitor.detect_issues(&graph);
        // Should have stale + blocked + GPU conflict
        assert!(issues.len() >= 4); // 3 stale + 1 blocked + 1 conflict (some overlap)
    }

    // ── Daily summary ───────────────────────────────────────────────────

    #[test]
    fn test_daily_summary_counts() {
        let monitor = BlockerMonitor::new();
        let graph = sample_graph();

        let summary = monitor.daily_summary(&graph);
        assert_eq!(summary.completed.len(), 1); // EX-100
        assert_eq!(summary.in_progress.len(), 3); // EX-101, 103, 104
        assert!(!summary.flagged.is_empty());
        assert_eq!(summary.agent_states.len(), 3); // M5, DGX, Mini
    }

    #[test]
    fn test_daily_summary_empty_graph() {
        let monitor = BlockerMonitor::new();
        let graph = WorkGraph::from_items(vec![]);

        let summary = monitor.daily_summary(&graph);
        assert!(summary.completed.is_empty());
        assert!(summary.in_progress.is_empty());
        assert!(summary.flagged.is_empty());
    }

    #[test]
    fn test_daily_summary_status_counts() {
        let monitor = BlockerMonitor::new();
        let graph = sample_graph();
        let summary = monitor.daily_summary(&graph);

        assert_eq!(summary.status_counts.get("done"), Some(&1));
        assert_eq!(summary.status_counts.get("in_progress"), Some(&3));
        assert_eq!(summary.status_counts.get("backlog"), Some(&2));
    }

    // ── Format ──────────────────────────────────────────────────────────

    #[test]
    fn test_format_daily_summary() {
        let monitor = BlockerMonitor::new();
        let graph = sample_graph();
        let summary = monitor.daily_summary(&graph);
        let formatted = format_daily_summary(&summary);

        assert!(formatted.contains("# Daily Board Summary"));
        assert!(formatted.contains("## Status Overview"));
        assert!(formatted.contains("## In Progress"));
        assert!(formatted.contains("## Flagged Items"));
        assert!(formatted.contains("## Agent Status"));
        assert!(formatted.contains("DGX"));
        assert!(formatted.contains("GPU"));
    }

    #[test]
    fn test_format_empty_summary() {
        let monitor = BlockerMonitor::new();
        let graph = WorkGraph::from_items(vec![]);
        let summary = monitor.daily_summary(&graph);
        let formatted = format_daily_summary(&summary);

        assert!(formatted.contains("Flagged Items"));
        assert!(formatted.contains("None"));
        assert!(formatted.contains("Completed: 0"));
    }

    // ── Config ──────────────────────────────────────────────────────────

    #[test]
    fn test_custom_config() {
        let config = MonitorConfig {
            stale_threshold: Duration::from_secs(3600), // 1 hour
        };
        let monitor = BlockerMonitor::with_config(config.clone());
        assert_eq!(monitor.config.stale_threshold, Duration::from_secs(3600));
    }

    #[test]
    fn test_default_config() {
        let config = MonitorConfig::default();
        assert_eq!(config.stale_threshold, Duration::from_secs(2 * 24 * 3600));
    }
}
