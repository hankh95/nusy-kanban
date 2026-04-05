//! Critical path analysis — topological sort, dependency depth, and worklist generation.
//!
//! Builds a DAG from `depends_on` edges across kanban items, then computes:
//! - Topological ordering (Kahn's algorithm)
//! - Depth for each node (longest path from any root)
//! - Parallel groups (items at the same depth)
//! - The critical path (longest dependency chain)
//! - Blocked vs ready status
//! - Voyage grouping (via `related` field)

use arrow::array::{Array, BooleanArray, ListArray, RecordBatch, StringArray};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::schema::items_col;

/// Lightweight item info extracted from Arrow batches.
#[derive(Debug, Clone)]
pub struct ItemInfo {
    pub id: String,
    pub title: String,
    pub item_type: String,
    pub status: String,
    pub priority: String,
    pub assignee: String,
    pub related: Vec<String>,
    pub depends_on: Vec<String>,
}

/// Result of critical path analysis.
#[derive(Debug)]
pub struct CriticalPathResult {
    /// Items in topological order (roots first).
    pub ordered: Vec<String>,
    /// Depth of each item (longest path from any root). Depth 0 = no unmet dependencies.
    pub depth: HashMap<String, u32>,
    /// Items grouped by depth level.
    pub depth_groups: BTreeMap<u32, Vec<String>>,
    /// The longest dependency chain (the actual critical path).
    pub longest_path: Vec<String>,
    /// Items that have all dependencies met (or no dependencies).
    pub ready: Vec<String>,
    /// Items with at least one unmet dependency.
    pub blocked: Vec<String>,
    /// Transitive dependent count per item (how many items depend on this, transitively).
    pub downstream_count: HashMap<String, usize>,
}

/// Items grouped under their parent voyage.
#[derive(Debug)]
pub struct VoyageGroup {
    pub voyage_id: String,
    pub voyage_title: String,
    pub voyage_status: String,
    pub items: Vec<String>,
    pub done_count: usize,
    pub total_count: usize,
}

/// Agent assignment recommendation.
#[derive(Debug)]
pub struct WorklistEntry {
    pub agent: String,
    pub items: Vec<WorklistItem>,
}

#[derive(Debug)]
pub struct WorklistItem {
    pub id: String,
    pub title: String,
    pub blocked: bool,
}

// ─── Extract ────────────────────────────────────────────────────────────────

/// Extract lightweight item info from Arrow batches.
pub fn extract_items(batches: &[RecordBatch]) -> Vec<ItemInfo> {
    let mut items = Vec::new();

    for batch in batches {
        let ids = col_str(batch, items_col::ID);
        let titles = col_str(batch, items_col::TITLE);
        let types = col_str(batch, items_col::ITEM_TYPE);
        let statuses = col_str(batch, items_col::STATUS);
        let priorities = col_str(batch, items_col::PRIORITY);
        let assignees = col_str(batch, items_col::ASSIGNEE);
        let deleted = batch
            .column(items_col::DELETED)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("deleted column");
        let related_col = batch
            .column(items_col::RELATED)
            .as_any()
            .downcast_ref::<ListArray>()
            .expect("related column");
        let depends_col = batch
            .column(items_col::DEPENDS_ON)
            .as_any()
            .downcast_ref::<ListArray>()
            .expect("depends_on column");

        for i in 0..batch.num_rows() {
            if deleted.value(i) {
                continue;
            }

            let related = extract_list_values(related_col, i);
            let depends_on = extract_list_values(depends_col, i);

            items.push(ItemInfo {
                id: ids.value(i).to_string(),
                title: titles.value(i).to_string(),
                item_type: types.value(i).to_string(),
                status: statuses.value(i).to_string(),
                priority: if priorities.is_null(i) {
                    "-".to_string()
                } else {
                    priorities.value(i).to_string()
                },
                assignee: if assignees.is_null(i) {
                    "-".to_string()
                } else {
                    assignees.value(i).to_string()
                },
                related,
                depends_on,
            });
        }
    }

    items
}

// ─── Critical Path ──────────────────────────────────────────────────────────

/// Compute critical path analysis for non-done items.
///
/// Uses Kahn's algorithm for topological sort, then computes depth
/// (longest path from any root) for dependency ordering.
pub fn compute_critical_path(items: &[ItemInfo]) -> Result<CriticalPathResult, String> {
    // Build the item lookup (all items, including done — needed for dep resolution)
    let item_map: HashMap<&str, &ItemInfo> = items.iter().map(|i| (i.id.as_str(), i)).collect();

    // Done items set
    let done_ids: HashSet<&str> = items
        .iter()
        .filter(|i| i.status == "done" || i.status == "complete" || i.status == "retired")
        .map(|i| i.id.as_str())
        .collect();

    // Active items (not done, not deleted)
    let active: Vec<&ItemInfo> = items
        .iter()
        .filter(|i| !done_ids.contains(i.id.as_str()))
        .collect();

    let active_ids: HashSet<&str> = active.iter().map(|i| i.id.as_str()).collect();

    // Build adjacency list and in-degree count (only among active items)
    // Edge: dependency → dependent (if A depends on B, edge is B → A)
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut in_degree: HashMap<&str, usize> = HashMap::new();

    for item in &active {
        adj.entry(item.id.as_str()).or_default();
        in_degree.entry(item.id.as_str()).or_insert(0);

        for dep_id in &item.depends_on {
            // Only count active (non-done) dependencies
            if active_ids.contains(dep_id.as_str()) {
                adj.entry(dep_id.as_str())
                    .or_default()
                    .push(item.id.as_str());
                *in_degree.entry(item.id.as_str()).or_insert(0) += 1;
            }
        }
    }

    // Kahn's algorithm — topological sort
    let mut queue: VecDeque<&str> = VecDeque::new();
    for (&id, &deg) in &in_degree {
        if deg == 0 {
            queue.push_back(id);
        }
    }

    let mut ordered: Vec<String> = Vec::new();
    let mut depth: HashMap<String, u32> = HashMap::new();

    // Initialize roots at depth 0
    for &id in queue.iter() {
        depth.insert(id.to_string(), 0);
    }

    while let Some(node) = queue.pop_front() {
        ordered.push(node.to_string());
        let node_depth = depth[node];

        if let Some(dependents) = adj.get(node) {
            for &dep in dependents {
                // Propagate depth: dependent is at least 1 deeper than this node
                let new_depth = node_depth + 1;
                let current = depth.entry(dep.to_string()).or_insert(0);
                if new_depth > *current {
                    *current = new_depth;
                }

                let deg = in_degree.get_mut(dep).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(dep);
                }
            }
        }
    }

    // Cycle detection
    if ordered.len() < active.len() {
        let in_cycle: Vec<String> = active
            .iter()
            .filter(|i| !ordered.contains(&i.id))
            .map(|i| format!("{} ({})", i.id, i.title))
            .collect();
        return Err(format!(
            "Dependency cycle detected among: {}",
            in_cycle.join(", ")
        ));
    }

    // Group by depth
    let mut depth_groups: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for (id, &d) in &depth {
        depth_groups.entry(d).or_default().push(id.clone());
    }
    // Sort within each group by priority
    for group in depth_groups.values_mut() {
        group.sort_by(|a, b| {
            let pa = item_map
                .get(a.as_str())
                .map(|i| priority_rank(&i.priority))
                .unwrap_or(99);
            let pb = item_map
                .get(b.as_str())
                .map(|i| priority_rank(&i.priority))
                .unwrap_or(99);
            pa.cmp(&pb)
        });
    }

    // Compute downstream (transitive dependent) count
    let mut downstream_count: HashMap<String, usize> = HashMap::new();
    // Process in reverse topological order
    for id in ordered.iter().rev() {
        let count = if let Some(dependents) = adj.get(id.as_str()) {
            dependents
                .iter()
                .map(|&d| 1 + downstream_count.get(d).copied().unwrap_or(0))
                .sum()
        } else {
            0
        };
        downstream_count.insert(id.clone(), count);
    }

    // Find longest path (critical path) by backtracking from deepest node
    let max_depth = depth.values().copied().max().unwrap_or(0);
    let longest_path = trace_longest_path(&ordered, &depth, &active, max_depth);

    // Ready vs blocked
    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    for item in &active {
        let has_unmet = item
            .depends_on
            .iter()
            .any(|dep| active_ids.contains(dep.as_str()));
        if has_unmet {
            blocked.push(item.id.clone());
        } else {
            ready.push(item.id.clone());
        }
    }

    // Sort ready by priority
    ready.sort_by(|a, b| {
        let pa = item_map
            .get(a.as_str())
            .map(|i| priority_rank(&i.priority))
            .unwrap_or(99);
        let pb = item_map
            .get(b.as_str())
            .map(|i| priority_rank(&i.priority))
            .unwrap_or(99);
        pa.cmp(&pb)
    });

    Ok(CriticalPathResult {
        ordered,
        depth,
        depth_groups,
        longest_path,
        ready,
        blocked,
        downstream_count,
    })
}

/// Trace the longest path through the DAG by picking the deepest node at each level.
fn trace_longest_path(
    ordered: &[String],
    depth: &HashMap<String, u32>,
    active: &[&ItemInfo],
    max_depth: u32,
) -> Vec<String> {
    if max_depth == 0 {
        return Vec::new();
    }

    let item_map: HashMap<&str, &ItemInfo> = active.iter().map(|&i| (i.id.as_str(), i)).collect();

    // Build reverse adjacency: for each item, what are its active dependencies?
    let mut deps_of: HashMap<&str, Vec<&str>> = HashMap::new();
    for &item in active {
        let active_deps: Vec<&str> = item
            .depends_on
            .iter()
            .filter(|d| item_map.contains_key(d.as_str()))
            .map(|d| d.as_str())
            .collect();
        deps_of.insert(item.id.as_str(), active_deps);
    }

    // Start from the deepest node
    let mut path = Vec::new();
    let deepest = ordered
        .iter()
        .find(|id| depth.get(id.as_str()).copied() == Some(max_depth));

    if let Some(start) = deepest {
        let mut current = start.as_str();
        path.push(current.to_string());

        // Walk backward through dependencies
        loop {
            let deps = deps_of.get(current).cloned().unwrap_or_default();
            if deps.is_empty() {
                break;
            }
            // Pick the dependency with the highest depth (stay on critical path)
            let next = deps
                .iter()
                .max_by_key(|d| depth.get(**d).copied().unwrap_or(0));
            if let Some(&n) = next {
                path.push(n.to_string());
                current = n;
            } else {
                break;
            }
        }

        path.reverse();
    }

    path
}

// ─── Voyage Grouping ────────────────────────────────────────────────────────

/// Group items under their parent voyages using the `related` field.
///
/// An item belongs to a voyage if its `related` list contains a voyage ID,
/// or if it IS a voyage.
pub fn group_by_voyage(items: &[ItemInfo]) -> (Vec<VoyageGroup>, Vec<String>) {
    let item_map: HashMap<&str, &ItemInfo> = items.iter().map(|i| (i.id.as_str(), i)).collect();

    // Find all voyages
    let voyages: Vec<&ItemInfo> = items.iter().filter(|i| i.item_type == "voyage").collect();

    let voyage_ids: HashSet<&str> = voyages.iter().map(|v| v.id.as_str()).collect();

    // Map items to their voyage(s)
    let mut voyage_items: HashMap<&str, Vec<String>> = HashMap::new();
    let mut assigned_to_voyage: HashSet<&str> = HashSet::new();

    for item in items {
        if item.item_type == "voyage" {
            continue; // Voyages themselves aren't children
        }
        for rel in &item.related {
            if voyage_ids.contains(rel.as_str()) {
                voyage_items
                    .entry(rel.as_str())
                    .or_default()
                    .push(item.id.clone());
                assigned_to_voyage.insert(item.id.as_str());
            }
        }
    }

    // Build voyage groups
    let mut groups: Vec<VoyageGroup> = Vec::new();
    for voyage in &voyages {
        // Skip done voyages with no active children
        let children = voyage_items
            .get(voyage.id.as_str())
            .cloned()
            .unwrap_or_default();
        let done_count = children
            .iter()
            .filter(|id| {
                item_map
                    .get(id.as_str())
                    .map(|i| i.status == "done" || i.status == "complete" || i.status == "retired")
                    .unwrap_or(false)
            })
            .count();
        let total = children.len();

        // Skip voyages that are done AND have no active children
        if (voyage.status == "done" || voyage.status == "complete") && done_count == total {
            continue;
        }

        groups.push(VoyageGroup {
            voyage_id: voyage.id.clone(),
            voyage_title: voyage.title.clone(),
            voyage_status: voyage.status.clone(),
            items: children,
            done_count,
            total_count: total,
        });
    }

    // Sort voyages: in_progress first, then backlog, then by ID
    groups.sort_by(|a, b| {
        let sa = status_sort_order(&a.voyage_status);
        let sb = status_sort_order(&b.voyage_status);
        sa.cmp(&sb).then_with(|| a.voyage_id.cmp(&b.voyage_id))
    });

    // Orphan items (not assigned to any voyage, not a voyage, not done)
    let orphans: Vec<String> = items
        .iter()
        .filter(|i| {
            i.item_type != "voyage"
                && !assigned_to_voyage.contains(i.id.as_str())
                && i.status != "done"
                && i.status != "complete"
                && i.status != "retired"
        })
        .map(|i| i.id.clone())
        .collect();

    (groups, orphans)
}

// ─── Worklist ───────────────────────────────────────────────────────────────

/// Generate agent work assignments based on ready items and current assignments.
pub fn generate_worklist(
    items: &[ItemInfo],
    cp: &CriticalPathResult,
    agents: &[String],
    depth_limit: usize,
) -> Vec<WorklistEntry> {
    let item_map: HashMap<&str, &ItemInfo> = items.iter().map(|i| (i.id.as_str(), i)).collect();
    let blocked_set: HashSet<&str> = cp.blocked.iter().map(|s| s.as_str()).collect();

    // Items currently in progress (already assigned)
    let in_progress: HashMap<&str, &str> = items
        .iter()
        .filter(|i| i.status == "in_progress" && i.assignee != "-")
        .map(|i| (i.assignee.as_str(), i.id.as_str()))
        .collect();

    let mut entries: Vec<WorklistEntry> = Vec::new();

    for agent in agents {
        let mut agent_items: Vec<WorklistItem> = Vec::new();

        // First: current in-progress work
        if let Some(&current_id) = in_progress.get(agent.as_str())
            && let Some(info) = item_map.get(current_id)
        {
            agent_items.push(WorklistItem {
                id: current_id.to_string(),
                title: truncate(&info.title, 40),
                blocked: false,
            });
        }

        // Then: ready items suitable for this agent
        let mut available: Vec<&str> = cp
            .ready
            .iter()
            .filter(|id| {
                let info = item_map.get(id.as_str());
                if let Some(info) = info {
                    // Skip items already in progress
                    if info.status != "backlog" {
                        return false;
                    }
                    // If assigned to a specific agent, only show for that agent
                    if info.assignee != "-" && info.assignee != *agent {
                        return false;
                    }
                    // Check if DGX-only (has gpu/cuda tags or assigned to DGX)
                    if info.assignee == "DGX" && *agent != "DGX" {
                        return false;
                    }
                    true
                } else {
                    false
                }
            })
            .map(|s| s.as_str())
            .collect();

        // Sort by: assigned-to-this-agent first, then by downstream count (bottlenecks first)
        available.sort_by(|&a, &b| {
            let a_assigned = item_map
                .get(a)
                .map(|i| i.assignee == *agent)
                .unwrap_or(false);
            let b_assigned = item_map
                .get(b)
                .map(|i| i.assignee == *agent)
                .unwrap_or(false);
            b_assigned.cmp(&a_assigned).then_with(|| {
                let da = cp.downstream_count.get(a).copied().unwrap_or(0);
                let db = cp.downstream_count.get(b).copied().unwrap_or(0);
                db.cmp(&da)
            })
        });

        // Fill up to depth_limit
        for &id in &available {
            if agent_items.len() >= depth_limit {
                break;
            }
            // Skip if already assigned to another agent in this worklist
            if let Some(info) = item_map.get(id) {
                agent_items.push(WorklistItem {
                    id: id.to_string(),
                    title: truncate(&info.title, 40),
                    blocked: blocked_set.contains(id),
                });
            }
        }

        // Add next blocked items that will become ready
        if agent_items.len() < depth_limit {
            for id in &cp.blocked {
                if agent_items.len() >= depth_limit {
                    break;
                }
                if let Some(info) = item_map.get(id.as_str()) {
                    if info.status != "backlog" {
                        continue;
                    }
                    if info.assignee != "-" && info.assignee != *agent {
                        continue;
                    }
                    if info.assignee == "DGX" && *agent != "DGX" {
                        continue;
                    }
                    // Check if this will unblock after current agent work completes
                    agent_items.push(WorklistItem {
                        id: id.clone(),
                        title: truncate(&info.title, 40),
                        blocked: true,
                    });
                }
            }
        }

        entries.push(WorklistEntry {
            agent: agent.clone(),
            items: agent_items,
        });
    }

    entries
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn col_str(batch: &RecordBatch, col: usize) -> &StringArray {
    batch
        .column(col)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("string column")
}

fn extract_list_values(list_col: &ListArray, row: usize) -> Vec<String> {
    if list_col.is_null(row) {
        return Vec::new();
    }
    let values = list_col.value(row);
    if values.is_empty() {
        return Vec::new();
    }
    let strings = values
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("list values");
    (0..strings.len())
        .map(|j| strings.value(j).to_string())
        .collect()
}

pub fn priority_rank(p: &str) -> i32 {
    match p.to_lowercase().as_str() {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 99,
    }
}

fn status_sort_order(s: &str) -> i32 {
    match s {
        "in_progress" => 0,
        "backlog" => 1,
        "review" => 2,
        "done" | "complete" | "retired" => 3,
        _ => 4,
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max - 3).collect();
        format!("{t}...")
    }
}

// ─── Display Formatting ─────────────────────────────────────────────────────

/// Format the voyage-grouped roadmap view.
pub fn format_roadmap(
    items: &[ItemInfo],
    groups: &[VoyageGroup],
    orphans: &[String],
    cp: &CriticalPathResult,
) -> String {
    let item_map: HashMap<&str, &ItemInfo> = items.iter().map(|i| (i.id.as_str(), i)).collect();
    let blocked_set: HashSet<&str> = cp.blocked.iter().map(|s| s.as_str()).collect();
    let mut lines = Vec::new();

    for group in groups {
        let progress = format!(
            "[{}/{}{}]",
            group.done_count,
            group.total_count,
            if group.total_count > 0 { " done" } else { "" }
        );
        lines.push(format!(
            "{}: {} {}",
            group.voyage_id, group.voyage_title, progress
        ));

        // Sort items by depth (roots first), then priority
        let mut sorted_items = group.items.clone();
        sorted_items.sort_by(|a, b| {
            let da = cp.depth.get(a.as_str()).copied().unwrap_or(0);
            let db = cp.depth.get(b.as_str()).copied().unwrap_or(0);
            da.cmp(&db).then_with(|| {
                let pa = item_map
                    .get(a.as_str())
                    .map(|i| priority_rank(&i.priority))
                    .unwrap_or(99);
                let pb = item_map
                    .get(b.as_str())
                    .map(|i| priority_rank(&i.priority))
                    .unwrap_or(99);
                pa.cmp(&pb)
            })
        });

        for id in &sorted_items {
            if let Some(info) = item_map.get(id.as_str()) {
                let (icon, status_tag) = item_status_display(info, &blocked_set);
                let assignee_tag = if info.assignee != "-" {
                    format!("  {}", info.assignee)
                } else {
                    String::new()
                };
                let dep_tag = if blocked_set.contains(id.as_str()) {
                    let unmet: Vec<&str> = info
                        .depends_on
                        .iter()
                        .filter(|d| {
                            item_map
                                .get(d.as_str())
                                .map(|i| {
                                    i.status != "done"
                                        && i.status != "complete"
                                        && i.status != "retired"
                                })
                                .unwrap_or(false)
                        })
                        .map(|d| d.as_str())
                        .collect();
                    format!(" -> {}", unmet.join(", "))
                } else {
                    String::new()
                };
                let title = truncate(&info.title, 44);
                lines.push(format!(
                    "  {icon} {:<14}{:<46}{}{assignee_tag}{dep_tag}",
                    id, title, status_tag
                ));
            }
        }
        lines.push(String::new());
    }

    // Orphan section
    if !orphans.is_empty() {
        lines.push("Ungrouped:".to_string());
        for id in orphans {
            if let Some(info) = item_map.get(id.as_str()) {
                let (icon, status_tag) = item_status_display(info, &blocked_set);
                let assignee_tag = if info.assignee != "-" {
                    format!("  {}", info.assignee)
                } else {
                    String::new()
                };
                let title = truncate(&info.title, 44);
                lines.push(format!(
                    "  {icon} {:<14}{:<46}{}{assignee_tag}",
                    id, title, status_tag
                ));
            }
        }
        lines.push(String::new());
    }

    lines.join("\n")
}

/// Format critical path display.
pub fn format_critical_path(items: &[ItemInfo], cp: &CriticalPathResult) -> String {
    let item_map: HashMap<&str, &ItemInfo> = items.iter().map(|i| (i.id.as_str(), i)).collect();
    let blocked_set: HashSet<&str> = cp.blocked.iter().map(|s| s.as_str()).collect();
    let max_depth = cp.depth_groups.keys().last().copied().unwrap_or(0);
    let total_items: usize = cp.depth_groups.values().map(|v| v.len()).sum();

    let mut lines = Vec::new();
    lines.push(format!(
        "Critical Path ({} items, {} depth levels):",
        total_items,
        max_depth + 1
    ));
    lines.push(String::new());

    for (&depth_level, group) in &cp.depth_groups {
        let label = if depth_level == 0 {
            "start now".to_string()
        } else {
            format!("after depth {}", depth_level - 1)
        };
        let parallel = if group.len() > 1 {
            format!(" — {} parallel", group.len())
        } else {
            String::new()
        };
        lines.push(format!("Depth {} ({label}{parallel}):", depth_level));

        for id in group {
            if let Some(info) = item_map.get(id.as_str()) {
                let assignee = if info.assignee != "-" {
                    format!("{:<8}", info.assignee)
                } else {
                    "Any     ".to_string()
                };
                let title = truncate(&info.title, 44);
                let dep_note = if blocked_set.contains(id.as_str()) {
                    let unmet: Vec<&str> = info
                        .depends_on
                        .iter()
                        .filter(|d| {
                            item_map
                                .get(d.as_str())
                                .map(|i| {
                                    i.status != "done"
                                        && i.status != "complete"
                                        && i.status != "retired"
                                })
                                .unwrap_or(false)
                        })
                        .map(|d| d.as_str())
                        .collect();
                    format!("  <- {}", unmet.join(", "))
                } else {
                    String::new()
                };
                lines.push(format!("  {:<14}{:<46}{assignee}{dep_note}", id, title));
            }
        }
        lines.push(String::new());
    }

    // Bottlenecks
    let mut bottlenecks: Vec<(&String, &usize)> = cp.downstream_count.iter().collect();
    bottlenecks.sort_by(|a, b| b.1.cmp(a.1));
    let top_bottlenecks: Vec<_> = bottlenecks
        .into_iter()
        .take(5)
        .filter(|(_, c)| **c > 0)
        .collect();

    if !top_bottlenecks.is_empty() {
        lines.push("Bottlenecks (most downstream dependents):".to_string());
        for (id, count) in top_bottlenecks {
            if let Some(info) = item_map.get(id.as_str()) {
                let title = truncate(&info.title, 40);
                lines.push(format!(
                    "  {:<14}{:<42} -> {} items depend",
                    id, title, count
                ));
            }
        }
        lines.push(String::new());
    }

    lines.join("\n")
}

/// Format worklist display.
pub fn format_worklist(entries: &[WorklistEntry]) -> String {
    let mut lines = Vec::new();

    // Header
    let headers: Vec<String> = entries.iter().map(|e| format!("{:<24}", e.agent)).collect();
    lines.push(format!("| {} |", headers.join(" | ")));
    let dividers: Vec<String> = entries.iter().map(|_| "-".repeat(24)).collect();
    lines.push(format!("| {} |", dividers.join(" | ")));

    // Find max rows
    let max_rows = entries.iter().map(|e| e.items.len()).max().unwrap_or(0);

    for row in 0..max_rows {
        let cells: Vec<String> = entries
            .iter()
            .map(|e| {
                if row < e.items.len() {
                    let item = &e.items[row];
                    let blocked_mark = if item.blocked { " ~" } else { "" };
                    let cell = format!("{} {}{}", item.id, truncate(&item.title, 10), blocked_mark);
                    format!("{:<24}", truncate(&cell, 24))
                } else {
                    " ".repeat(24)
                }
            })
            .collect();
        lines.push(format!("| {} |", cells.join(" | ")));
    }

    lines.push(String::new());
    lines.push("~ = blocked, will become ready after current work completes".to_string());

    lines.join("\n")
}

fn item_status_display(info: &ItemInfo, blocked: &HashSet<&str>) -> (&'static str, &'static str) {
    match info.status.as_str() {
        "done" | "complete" | "retired" => ("✓", "done"),
        "in_progress" => ("▶", "IN PROGRESS"),
        "review" => ("⊙", "REVIEW"),
        _ if blocked.contains(info.id.as_str()) => ("◇", "BLOCKED"),
        _ => ("→", "READY"),
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_items() -> Vec<ItemInfo> {
        vec![
            ItemInfo {
                id: "EX-1".into(),
                title: "Root task A".into(),
                item_type: "expedition".into(),
                status: "backlog".into(),
                priority: "critical".into(),
                assignee: "-".into(),
                related: vec!["VY-1".into()],
                depends_on: vec![],
            },
            ItemInfo {
                id: "EX-2".into(),
                title: "Root task B".into(),
                item_type: "expedition".into(),
                status: "backlog".into(),
                priority: "high".into(),
                assignee: "DGX".into(),
                related: vec!["VY-1".into()],
                depends_on: vec![],
            },
            ItemInfo {
                id: "EX-3".into(),
                title: "Depends on A".into(),
                item_type: "expedition".into(),
                status: "backlog".into(),
                priority: "high".into(),
                assignee: "-".into(),
                related: vec!["VY-1".into()],
                depends_on: vec!["EX-1".into()],
            },
            ItemInfo {
                id: "EX-4".into(),
                title: "Depends on A and B".into(),
                item_type: "expedition".into(),
                status: "backlog".into(),
                priority: "critical".into(),
                assignee: "-".into(),
                related: vec!["VY-1".into()],
                depends_on: vec!["EX-1".into(), "EX-2".into()],
            },
            ItemInfo {
                id: "EX-5".into(),
                title: "Depends on 3 and 4".into(),
                item_type: "expedition".into(),
                status: "backlog".into(),
                priority: "medium".into(),
                assignee: "-".into(),
                related: vec!["VY-1".into()],
                depends_on: vec!["EX-3".into(), "EX-4".into()],
            },
            ItemInfo {
                id: "VY-1".into(),
                title: "Test Voyage".into(),
                item_type: "voyage".into(),
                status: "backlog".into(),
                priority: "critical".into(),
                assignee: "-".into(),
                related: vec![],
                depends_on: vec![],
            },
            ItemInfo {
                id: "EX-6".into(),
                title: "Orphan task".into(),
                item_type: "expedition".into(),
                status: "backlog".into(),
                priority: "low".into(),
                assignee: "-".into(),
                related: vec![],
                depends_on: vec![],
            },
            ItemInfo {
                id: "EX-99".into(),
                title: "Done task".into(),
                item_type: "expedition".into(),
                status: "done".into(),
                priority: "high".into(),
                assignee: "M5".into(),
                related: vec!["VY-1".into()],
                depends_on: vec![],
            },
        ]
    }

    #[test]
    fn test_topological_sort_ordering() {
        let items = make_items();
        let cp = compute_critical_path(&items).unwrap();

        // EX-1 and EX-2 should come before EX-3 and EX-4
        let pos = |id: &str| cp.ordered.iter().position(|x| x == id).unwrap();
        assert!(pos("EX-1") < pos("EX-3"));
        assert!(pos("EX-1") < pos("EX-4"));
        assert!(pos("EX-2") < pos("EX-4"));
        assert!(pos("EX-3") < pos("EX-5"));
        assert!(pos("EX-4") < pos("EX-5"));
    }

    #[test]
    fn test_depth_computation() {
        let items = make_items();
        let cp = compute_critical_path(&items).unwrap();

        assert_eq!(cp.depth["EX-1"], 0);
        assert_eq!(cp.depth["EX-2"], 0);
        assert_eq!(cp.depth["EX-3"], 1);
        assert_eq!(cp.depth["EX-4"], 1);
        assert_eq!(cp.depth["EX-5"], 2);
    }

    #[test]
    fn test_ready_vs_blocked() {
        let items = make_items();
        let cp = compute_critical_path(&items).unwrap();

        // EX-1, EX-2, VY-1, EX-6 are ready (no active deps)
        assert!(cp.ready.contains(&"EX-1".to_string()));
        assert!(cp.ready.contains(&"EX-2".to_string()));
        assert!(cp.ready.contains(&"EX-6".to_string()));

        // EX-3, EX-4, EX-5 are blocked
        assert!(cp.blocked.contains(&"EX-3".to_string()));
        assert!(cp.blocked.contains(&"EX-4".to_string()));
        assert!(cp.blocked.contains(&"EX-5".to_string()));
    }

    #[test]
    fn test_done_items_excluded() {
        let items = make_items();
        let cp = compute_critical_path(&items).unwrap();

        // EX-99 is done, should not appear in any output
        assert!(!cp.ordered.contains(&"EX-99".to_string()));
        assert!(!cp.ready.contains(&"EX-99".to_string()));
        assert!(!cp.blocked.contains(&"EX-99".to_string()));
    }

    #[test]
    fn test_downstream_count() {
        let items = make_items();
        let cp = compute_critical_path(&items).unwrap();

        // EX-1 -> EX-3, EX-4 -> EX-5 (3 transitive)
        assert!(cp.downstream_count["EX-1"] >= 3);
        // EX-5 has no dependents
        assert_eq!(cp.downstream_count["EX-5"], 0);
    }

    #[test]
    fn test_cycle_detection() {
        let items = vec![
            ItemInfo {
                id: "A".into(),
                title: "A".into(),
                item_type: "expedition".into(),
                status: "backlog".into(),
                priority: "-".into(),
                assignee: "-".into(),
                related: vec![],
                depends_on: vec!["B".into()],
            },
            ItemInfo {
                id: "B".into(),
                title: "B".into(),
                item_type: "expedition".into(),
                status: "backlog".into(),
                priority: "-".into(),
                assignee: "-".into(),
                related: vec![],
                depends_on: vec!["A".into()],
            },
        ];
        let result = compute_critical_path(&items);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cycle"));
    }

    #[test]
    fn test_voyage_grouping() {
        let items = make_items();
        let (groups, orphans) = group_by_voyage(&items);

        // VY-1 should have EX-1 through EX-5 + EX-99
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].voyage_id, "VY-1");
        assert_eq!(groups[0].total_count, 6); // EX-1..5 + EX-99
        assert_eq!(groups[0].done_count, 1); // EX-99

        // EX-6 is an orphan
        assert!(orphans.contains(&"EX-6".to_string()));
    }

    #[test]
    fn test_depth_groups() {
        let items = make_items();
        let cp = compute_critical_path(&items).unwrap();

        // Depth 0: EX-1, EX-2, VY-1, EX-6
        assert!(cp.depth_groups[&0].contains(&"EX-1".to_string()));
        assert!(cp.depth_groups[&0].contains(&"EX-2".to_string()));

        // Depth 2: EX-5
        assert!(cp.depth_groups[&2].contains(&"EX-5".to_string()));
    }

    #[test]
    fn test_worklist_generation() {
        let items = make_items();
        let cp = compute_critical_path(&items).unwrap();
        let agents = vec!["DGX".to_string(), "M5".to_string(), "Mini".to_string()];
        let worklist = generate_worklist(&items, &cp, &agents, 3);

        assert_eq!(worklist.len(), 3);
        // DGX should get EX-2 (assigned to DGX)
        let dgx = &worklist[0];
        assert_eq!(dgx.agent, "DGX");
        assert!(dgx.items.iter().any(|i| i.id == "EX-2"));
    }

    #[test]
    fn test_empty_items() {
        let items: Vec<ItemInfo> = vec![];
        let cp = compute_critical_path(&items).unwrap();
        assert!(cp.ordered.is_empty());
        assert!(cp.ready.is_empty());
        assert!(cp.blocked.is_empty());
    }

    #[test]
    fn test_longest_path() {
        let items = make_items();
        let cp = compute_critical_path(&items).unwrap();

        // Longest path should go through depth 0 -> 1 -> 2
        assert!(!cp.longest_path.is_empty());
        // First item should be at depth 0, last at max depth
        if let Some(first) = cp.longest_path.first() {
            assert_eq!(cp.depth[first], 0);
        }
        if let Some(last) = cp.longest_path.last() {
            assert_eq!(cp.depth[last], 2);
        }
    }
}
