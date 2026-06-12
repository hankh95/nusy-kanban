//! Agent assignment and availability tracking.
//!
//! Tracks which agent is working on what, agent availability (0 in-progress
//! items = available), and agent capabilities for assignment suggestions.

use crate::reader::WorkGraph;

/// Known agent capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfile {
    /// Agent name (e.g., "M5", "DGX", "Mini").
    pub name: String,
    /// What this agent specializes in.
    pub capabilities: Vec<Capability>,
}

/// Agent capability tags used for assignment matching.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Capability {
    /// GPU compute (training, large-scale eval).
    Gpu,
    /// Architecture and design work.
    Architecture,
    /// Infrastructure, services, CI/CD.
    Infrastructure,
    /// General development (all agents have this).
    General,
    /// Rust/V14 development.
    Rust,
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Capability::Gpu => write!(f, "gpu"),
            Capability::Architecture => write!(f, "architecture"),
            Capability::Infrastructure => write!(f, "infrastructure"),
            Capability::General => write!(f, "general"),
            Capability::Rust => write!(f, "rust"),
        }
    }
}

/// Default agent profiles for the NuSy fleet.
pub fn default_profiles() -> Vec<AgentProfile> {
    vec![
        AgentProfile {
            name: "M5".to_string(),
            capabilities: vec![
                Capability::Architecture,
                Capability::Rust,
                Capability::General,
            ],
        },
        AgentProfile {
            name: "DGX".to_string(),
            capabilities: vec![Capability::Gpu, Capability::Rust, Capability::General],
        },
        AgentProfile {
            name: "Mini".to_string(),
            capabilities: vec![
                Capability::Infrastructure,
                Capability::Rust,
                Capability::General,
            ],
        },
    ]
}

/// Current state of an agent derived from the work graph.
#[derive(Debug, Clone)]
pub struct AgentState {
    /// Agent profile.
    pub profile: AgentProfile,
    /// Items currently assigned and in progress.
    pub in_progress: Vec<String>,
    /// Items assigned but in other states (backlog, review, etc.).
    pub other_assigned: Vec<String>,
    /// Whether the agent is available for new work.
    pub available: bool,
}

/// Assignment suggestion for a work item.
#[derive(Debug, Clone)]
pub struct AssignmentSuggestion {
    /// The item that needs assignment.
    pub item_id: String,
    /// Suggested agent, if any.
    pub suggested_agent: Option<String>,
    /// Why this agent was suggested.
    pub reason: String,
}

/// The assignee tracker: combines work graph data with agent profiles.
pub struct AssigneeTracker {
    profiles: Vec<AgentProfile>,
}

impl AssigneeTracker {
    /// Create a tracker with the given agent profiles.
    pub fn new(profiles: Vec<AgentProfile>) -> Self {
        AssigneeTracker { profiles }
    }

    /// Create a tracker with the default NuSy fleet profiles.
    pub fn with_defaults() -> Self {
        Self::new(default_profiles())
    }

    /// Compute the current state of all agents from the work graph.
    pub fn agent_states(&self, graph: &WorkGraph) -> Vec<AgentState> {
        self.profiles
            .iter()
            .map(|profile| {
                let assigned: Vec<_> = graph.items_by_assignee(&profile.name);

                let in_progress: Vec<String> = assigned
                    .iter()
                    .filter(|item| item.status == "in_progress")
                    .map(|item| item.id.clone())
                    .collect();

                let other_assigned: Vec<String> = assigned
                    .iter()
                    .filter(|item| item.status != "in_progress")
                    .map(|item| item.id.clone())
                    .collect();

                let available = in_progress.is_empty();

                AgentState {
                    profile: profile.clone(),
                    in_progress,
                    other_assigned,
                    available,
                }
            })
            .collect()
    }

    /// Get available agents (those with 0 in-progress items).
    pub fn available_agents(&self, graph: &WorkGraph) -> Vec<&AgentProfile> {
        let states = self.agent_states(graph);
        states
            .iter()
            .filter(|s| s.available)
            .map(|s| {
                self.profiles
                    .iter()
                    .find(|p| p.name == s.profile.name)
                    .expect("profile exists")
            })
            .collect()
    }

    /// Suggest an assignment for an unassigned item.
    ///
    /// Matching logic:
    /// 1. Filter to available agents
    /// 2. Score by capability match (tags + title keywords)
    /// 3. Break ties by current workload (fewer assigned items preferred)
    pub fn suggest_assignment(&self, graph: &WorkGraph, item_id: &str) -> AssignmentSuggestion {
        let item = match graph.items.get(item_id) {
            Some(i) => i,
            None => {
                return AssignmentSuggestion {
                    item_id: item_id.to_string(),
                    suggested_agent: None,
                    reason: "item not found in work graph".to_string(),
                };
            }
        };

        // Already assigned?
        if let Some(ref assignee) = item.assignee {
            return AssignmentSuggestion {
                item_id: item_id.to_string(),
                suggested_agent: Some(assignee.clone()),
                reason: format!("already assigned to {assignee}"),
            };
        }

        let states = self.agent_states(graph);
        let available: Vec<&AgentState> = states.iter().filter(|s| s.available).collect();

        if available.is_empty() {
            return AssignmentSuggestion {
                item_id: item_id.to_string(),
                suggested_agent: None,
                reason: "no agents available (all have in-progress work)".to_string(),
            };
        }

        // Score each available agent
        let required_caps = infer_capabilities(item);
        let mut best_agent: Option<(&AgentState, usize, usize)> = None; // (state, cap_score, total_assigned)

        for state in &available {
            let cap_score = required_caps
                .iter()
                .filter(|cap| state.profile.capabilities.contains(cap))
                .count();
            let total_assigned = state.in_progress.len() + state.other_assigned.len();

            let is_better = match best_agent {
                None => true,
                Some((_, best_score, best_total)) => {
                    cap_score > best_score
                        || (cap_score == best_score && total_assigned < best_total)
                }
            };

            if is_better {
                best_agent = Some((state, cap_score, total_assigned));
            }
        }

        match best_agent {
            Some((state, cap_score, _)) => {
                let matched_caps: Vec<String> = required_caps
                    .iter()
                    .filter(|cap| state.profile.capabilities.contains(cap))
                    .map(|c| c.to_string())
                    .collect();

                let reason = if cap_score > 0 {
                    format!(
                        "best capability match: {} ({})",
                        state.profile.name,
                        matched_caps.join(", ")
                    )
                } else {
                    format!("least loaded available agent: {}", state.profile.name)
                };

                AssignmentSuggestion {
                    item_id: item_id.to_string(),
                    suggested_agent: Some(state.profile.name.clone()),
                    reason,
                }
            }
            None => AssignmentSuggestion {
                item_id: item_id.to_string(),
                suggested_agent: None,
                reason: "no suitable agent found".to_string(),
            },
        }
    }
}

/// Infer required capabilities from an item's tags and title.
fn infer_capabilities(item: &crate::reader::WorkItem) -> Vec<Capability> {
    let mut caps = vec![Capability::General];
    let tags_lower: Vec<String> = item.tags.iter().map(|t| t.to_lowercase()).collect();
    let title_lower = item.title.to_lowercase();

    // GPU capability
    if tags_lower
        .iter()
        .any(|t| t.contains("gpu") || t.contains("training"))
        || title_lower.contains("gpu")
        || title_lower.contains("training")
        || title_lower.contains("fine-tun")
    {
        caps.push(Capability::Gpu);
    }

    // Rust / V14 capability
    if tags_lower
        .iter()
        .any(|t| t.contains("v14") || t.contains("rust") || t.contains("arrow"))
        || title_lower.contains("rust")
        || title_lower.contains("arrow")
        || title_lower.contains("v14")
        || title_lower.contains("crate")
    {
        caps.push(Capability::Rust);
    }

    // Architecture capability
    if title_lower.contains("architect")
        || title_lower.contains("design")
        || title_lower.contains("refactor")
        || title_lower.contains("restructur")
    {
        caps.push(Capability::Architecture);
    }

    // Infrastructure capability
    if tags_lower
        .iter()
        .any(|t| t.contains("infra") || t.contains("ci") || t.contains("deploy"))
        || title_lower.contains("infra")
        || title_lower.contains("deploy")
        || title_lower.contains("ci/cd")
        || title_lower.contains("server")
        || title_lower.contains("nats")
        || title_lower.contains("launchd")
    {
        caps.push(Capability::Infrastructure);
    }

    caps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::WorkItem;

    fn make_item(id: &str, status: &str, assignee: Option<&str>) -> WorkItem {
        WorkItem {
            id: id.to_string(),
            title: "Test item".to_string(),
            item_type: "expedition".to_string(),
            status: status.to_string(),
            priority: Some("medium".to_string()),
            assignee: assignee.map(String::from),
            board: Some("development".to_string()),
            tags: vec![],
            related: vec![],
            depends_on: vec![],
            body: None,
        }
    }

    fn sample_graph() -> WorkGraph {
        WorkGraph::from_items(vec![
            make_item("EX-100", "in_progress", Some("M5")),
            make_item("EX-101", "in_progress", Some("DGX")),
            make_item("EX-102", "backlog", None),
            make_item("EX-103", "backlog", None),
            make_item("EX-104", "review", Some("M5")),
        ])
    }

    #[test]
    fn test_agent_states() {
        let tracker = AssigneeTracker::with_defaults();
        let graph = sample_graph();
        let states = tracker.agent_states(&graph);

        assert_eq!(states.len(), 3);

        // M5: 1 in-progress (EX-100), 1 other (EX-104 in review)
        let m5 = states.iter().find(|s| s.profile.name == "M5").unwrap();
        assert_eq!(m5.in_progress.len(), 1);
        assert!(m5.in_progress.contains(&"EX-100".to_string()));
        assert_eq!(m5.other_assigned.len(), 1);
        assert!(!m5.available);

        // DGX: 1 in-progress (EX-101)
        let dgx = states.iter().find(|s| s.profile.name == "DGX").unwrap();
        assert_eq!(dgx.in_progress.len(), 1);
        assert!(!dgx.available);

        // Mini: 0 in-progress, 0 other → available
        let mini = states.iter().find(|s| s.profile.name == "Mini").unwrap();
        assert_eq!(mini.in_progress.len(), 0);
        assert!(mini.available);
    }

    #[test]
    fn test_available_agents() {
        let tracker = AssigneeTracker::with_defaults();
        let graph = sample_graph();
        let available = tracker.available_agents(&graph);

        // Only Mini is available
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].name, "Mini");
    }

    #[test]
    fn test_available_agents_all_free() {
        let tracker = AssigneeTracker::with_defaults();
        let graph = WorkGraph::from_items(vec![
            make_item("EX-100", "backlog", None),
            make_item("EX-101", "done", Some("M5")),
        ]);
        let available = tracker.available_agents(&graph);

        // All 3 agents should be available (done items don't count as in_progress)
        assert_eq!(available.len(), 3);
    }

    #[test]
    fn test_suggest_assignment_gpu_work() {
        let tracker = AssigneeTracker::with_defaults();
        let mut graph = WorkGraph::from_items(vec![{
            let mut item = make_item("EX-200", "backlog", None);
            item.title = "GPU training pipeline".to_string();
            item.tags = vec!["gpu".to_string(), "training".to_string()];
            item
        }]);
        // All agents are available — DGX should be suggested for GPU work
        let _ = &mut graph; // ensure mutable ref is dropped

        let suggestion = tracker.suggest_assignment(&graph, "EX-200");
        assert_eq!(suggestion.suggested_agent, Some("DGX".to_string()));
        assert!(suggestion.reason.contains("gpu"));
    }

    #[test]
    fn test_suggest_assignment_infrastructure_work() {
        let tracker = AssigneeTracker::with_defaults();
        let graph = WorkGraph::from_items(vec![{
            let mut item = make_item("EX-201", "backlog", None);
            item.title = "Deploy NATS server on launchd".to_string();
            item
        }]);

        let suggestion = tracker.suggest_assignment(&graph, "EX-201");
        assert_eq!(suggestion.suggested_agent, Some("Mini".to_string()));
        assert!(suggestion.reason.contains("infrastructure"));
    }

    #[test]
    fn test_suggest_assignment_architecture_work() {
        let tracker = AssigneeTracker::with_defaults();
        let graph = WorkGraph::from_items(vec![{
            let mut item = make_item("EX-202", "backlog", None);
            item.title = "Architecture design for V15".to_string();
            item
        }]);

        let suggestion = tracker.suggest_assignment(&graph, "EX-202");
        assert_eq!(suggestion.suggested_agent, Some("M5".to_string()));
        assert!(suggestion.reason.contains("architecture"));
    }

    #[test]
    fn test_suggest_assignment_already_assigned() {
        let tracker = AssigneeTracker::with_defaults();
        let graph = WorkGraph::from_items(vec![make_item("EX-203", "in_progress", Some("DGX"))]);

        let suggestion = tracker.suggest_assignment(&graph, "EX-203");
        assert_eq!(suggestion.suggested_agent, Some("DGX".to_string()));
        assert!(suggestion.reason.contains("already assigned"));
    }

    #[test]
    fn test_suggest_assignment_no_available_agents() {
        let tracker = AssigneeTracker::with_defaults();
        let graph = WorkGraph::from_items(vec![
            make_item("EX-300", "in_progress", Some("M5")),
            make_item("EX-301", "in_progress", Some("DGX")),
            make_item("EX-302", "in_progress", Some("Mini")),
            make_item("EX-303", "backlog", None), // needs assignment
        ]);

        let suggestion = tracker.suggest_assignment(&graph, "EX-303");
        assert!(suggestion.suggested_agent.is_none());
        assert!(suggestion.reason.contains("no agents available"));
    }

    #[test]
    fn test_suggest_assignment_nonexistent_item() {
        let tracker = AssigneeTracker::with_defaults();
        let graph = WorkGraph::default();

        let suggestion = tracker.suggest_assignment(&graph, "EX-9999");
        assert!(suggestion.suggested_agent.is_none());
        assert!(suggestion.reason.contains("not found"));
    }

    #[test]
    fn test_suggest_assignment_least_loaded_tiebreak() {
        let tracker = AssigneeTracker::with_defaults();
        // All agents available, but M5 has 2 backlog items, DGX has 1, Mini has 0
        let graph = WorkGraph::from_items(vec![
            make_item("EX-400", "backlog", Some("M5")),
            make_item("EX-401", "backlog", Some("M5")),
            make_item("EX-402", "backlog", Some("DGX")),
            // Generic item with no capability signals
            make_item("EX-403", "backlog", None),
        ]);

        let suggestion = tracker.suggest_assignment(&graph, "EX-403");
        // Mini has fewest assigned items (0), should be preferred
        assert_eq!(suggestion.suggested_agent, Some("Mini".to_string()));
    }

    #[test]
    fn test_infer_capabilities_v14_rust() {
        let mut item = make_item("EX-500", "backlog", None);
        item.title = "Arrow-native crate for V14".to_string();
        item.tags = vec!["v14".to_string(), "rust".to_string()];

        let caps = infer_capabilities(&item);
        assert!(caps.contains(&Capability::Rust));
        assert!(caps.contains(&Capability::General));
    }

    #[test]
    fn test_infer_capabilities_generic() {
        let item = make_item("CH-500", "backlog", None);
        let caps = infer_capabilities(&item);
        // Only General for a generic "Test item"
        assert_eq!(caps, vec![Capability::General]);
    }

    #[test]
    fn test_default_profiles() {
        let profiles = default_profiles();
        assert_eq!(profiles.len(), 3);

        let m5 = profiles.iter().find(|p| p.name == "M5").unwrap();
        assert!(m5.capabilities.contains(&Capability::Architecture));
        assert!(m5.capabilities.contains(&Capability::Rust));

        let dgx = profiles.iter().find(|p| p.name == "DGX").unwrap();
        assert!(dgx.capabilities.contains(&Capability::Gpu));

        let mini = profiles.iter().find(|p| p.name == "Mini").unwrap();
        assert!(mini.capabilities.contains(&Capability::Infrastructure));
    }

    #[test]
    fn test_tracker_identifies_available_and_suggests() {
        // This is the expedition's "done when" test for Phase 3:
        // "Tracker correctly identifies available agents and suggests assignment"
        let tracker = AssigneeTracker::with_defaults();
        let graph = WorkGraph::from_items(vec![
            make_item("EX-600", "in_progress", Some("M5")),
            make_item("EX-601", "backlog", None),
        ]);

        // Available: DGX and Mini (M5 is busy)
        let available = tracker.available_agents(&graph);
        assert_eq!(available.len(), 2);
        let names: Vec<&str> = available.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"DGX"));
        assert!(names.contains(&"Mini"));

        // Should suggest one of the available agents for EX-601
        let suggestion = tracker.suggest_assignment(&graph, "EX-601");
        assert!(suggestion.suggested_agent.is_some());
        let suggested = suggestion.suggested_agent.unwrap();
        assert!(suggested == "DGX" || suggested == "Mini");
    }
}
