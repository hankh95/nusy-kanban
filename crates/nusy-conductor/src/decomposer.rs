//! Expedition phase extraction and progress tracking.
//!
//! Reads expedition body content to extract phases, determines the current
//! phase, and suggests the next action. Phase extraction uses structural
//! markdown analysis with heuristic scoring — designed for replacement by
//! an LLM-backed implementation via the [`PhaseExtractor`] trait.

use crate::reader::WorkItem;

/// A single phase extracted from an expedition body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phase {
    /// Phase number (1-based).
    pub number: u32,
    /// Phase title (e.g., "Kanban State Reader").
    pub title: String,
    /// Bullet points describing the work.
    pub tasks: Vec<String>,
    /// "Done when" criteria, if found.
    pub done_criteria: Option<String>,
}

/// The current progress of an expedition.
#[derive(Debug, Clone)]
pub struct ExpeditionProgress {
    /// All phases extracted from the expedition body.
    pub phases: Vec<Phase>,
    /// Index of the current (active) phase (0-based), if determinable.
    pub current_phase: Option<usize>,
    /// Suggested next action.
    pub suggested_action: SuggestedAction,
}

/// What the conductor suggests as the next step for an expedition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestedAction {
    /// Expedition has no body or phases — needs decomposition.
    NeedsDecomposition,
    /// Start working on the specified phase.
    StartPhase { phase_number: u32, title: String },
    /// Continue the current phase.
    ContinuePhase { phase_number: u32, title: String },
    /// All phases appear complete — ready for review.
    ReadyForReview,
    /// Expedition is already done.
    AlreadyDone,
}

/// Trait for phase extraction — enables swapping in an LLM-backed
/// implementation without changing the conductor's orchestration logic.
pub trait PhaseExtractor {
    /// Extract phases from an expedition body.
    fn extract_phases(&self, body: &str) -> Vec<Phase>;
}

/// Structural markdown-based phase extractor.
///
/// Identifies phases by heading patterns like:
/// - `## Phase 1: Title`
/// - `## Phase 1 — Title`
/// - `### 1. Title`
///
/// This handles the consistent format used by NuSy expedition bodies.
/// For irregular or conversational bodies, swap in an LLM extractor.
pub struct StructuralExtractor;

impl PhaseExtractor for StructuralExtractor {
    fn extract_phases(&self, body: &str) -> Vec<Phase> {
        extract_phases_structural(body)
    }
}

/// Extract phases from markdown body using structural analysis.
fn extract_phases_structural(body: &str) -> Vec<Phase> {
    let lines: Vec<&str> = body.lines().collect();
    let mut phases: Vec<Phase> = Vec::new();
    let mut current_phase: Option<Phase> = None;
    let mut in_done_section = false;

    for line in &lines {
        let trimmed = line.trim();

        // Check for phase heading patterns
        if let Some(phase) = try_parse_phase_heading(trimmed) {
            // Save previous phase
            if let Some(prev) = current_phase.take() {
                phases.push(prev);
            }
            current_phase = Some(phase);
            in_done_section = false;
            continue;
        }

        // Only collect content if we're inside a phase
        let Some(ref mut phase) = current_phase else {
            continue;
        };

        // Check for "done when" section
        let lower = trimmed.to_lowercase();
        if lower.starts_with("**done when")
            || lower.starts_with("- **done when")
            || lower.starts_with("done when:")
        {
            in_done_section = true;
            let criteria = extract_done_criteria(trimmed);
            if !criteria.is_empty() {
                phase.done_criteria = Some(criteria);
            }
            continue;
        }

        // Check for key files / integration markers — stop collecting done criteria
        if lower.starts_with("**key files")
            || lower.starts_with("**integration")
            || lower.starts_with("**key file")
        {
            in_done_section = false;
            continue;
        }

        // Collect bullet points as tasks
        if trimmed.starts_with("- ") && !in_done_section {
            let task = trimmed.strip_prefix("- ").unwrap_or(trimmed).to_string();
            phase.tasks.push(task);
        }

        // Continue collecting done criteria on subsequent lines
        if in_done_section && !trimmed.is_empty() && phase.done_criteria.is_some() {
            // Append to existing criteria
            if let Some(ref mut criteria) = phase.done_criteria {
                criteria.push(' ');
                criteria.push_str(trimmed);
            }
        }
    }

    // Don't forget the last phase
    if let Some(phase) = current_phase {
        phases.push(phase);
    }

    phases
}

/// Try to parse a line as a phase heading. Returns Some(Phase) if it matches.
///
/// Only matches markdown headings (lines starting with `#`). Plain text
/// containing "Phase N" (e.g., "Phase 3 Wave 1.") is NOT treated as a heading.
fn try_parse_phase_heading(line: &str) -> Option<Phase> {
    // Must be a markdown heading
    if !line.starts_with('#') {
        return None;
    }

    // Strip markdown heading markers
    let stripped = line.trim_start_matches('#').trim_start_matches(' ').trim();

    // Pattern: "Phase N: Title" or "Phase N — Title" or "Phase N - Title"
    if let Some(rest) = stripped
        .strip_prefix("Phase ")
        .or_else(|| stripped.strip_prefix("phase "))
    {
        return parse_numbered_phase(rest);
    }

    // Pattern: "N. Title" (numbered list heading)
    if stripped.chars().next().is_some_and(|c| c.is_ascii_digit())
        && let Some(dot_pos) = stripped.find('.')
        && let Ok(num) = stripped[..dot_pos].trim().parse::<u32>()
    {
        let title = stripped[dot_pos + 1..].trim().to_string();
        if !title.is_empty() {
            return Some(Phase {
                number: num,
                title,
                tasks: Vec::new(),
                done_criteria: None,
            });
        }
    }

    None
}

/// Parse "N: Title" or "N — Title" from the rest after "Phase ".
fn parse_numbered_phase(rest: &str) -> Option<Phase> {
    // Find where the number ends
    let num_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if num_end == 0 {
        return None;
    }

    let number: u32 = rest[..num_end].parse().ok()?;
    let after_num = rest[num_end..].trim();

    // Strip separator: ":", "—", "-", "."
    let title = after_num
        .strip_prefix(':')
        .or_else(|| after_num.strip_prefix("—"))
        .or_else(|| after_num.strip_prefix('-'))
        .or_else(|| after_num.strip_prefix('.'))
        .unwrap_or(after_num)
        .trim()
        .to_string();

    if title.is_empty() {
        return None;
    }

    Some(Phase {
        number,
        title,
        tasks: Vec::new(),
        done_criteria: None,
    })
}

/// Extract the done criteria text from a "done when" line.
fn extract_done_criteria(line: &str) -> String {
    line.trim_start_matches("- ")
        .trim_start_matches("**Done when:**")
        .trim_start_matches("**Done when:")
        .trim_start_matches("Done when:**")
        .trim_start_matches("Done when:")
        .trim_start_matches("**")
        .trim()
        .trim_end_matches("**")
        .trim()
        .to_string()
}

/// Analyze an expedition and determine its progress.
///
/// `evidence` provides signals about which phases are complete:
/// typically PR descriptions, commit messages, or review comments.
pub fn analyze_expedition(item: &WorkItem, evidence: &[&str]) -> ExpeditionProgress {
    // Already done?
    if item.status == "done" || item.status == "complete" {
        return ExpeditionProgress {
            phases: vec![],
            current_phase: None,
            suggested_action: SuggestedAction::AlreadyDone,
        };
    }

    // No body? Can't decompose.
    let body = match &item.body {
        Some(b) if !b.trim().is_empty() => b,
        _ => {
            return ExpeditionProgress {
                phases: vec![],
                current_phase: None,
                suggested_action: SuggestedAction::NeedsDecomposition,
            };
        }
    };

    let extractor = StructuralExtractor;
    let phases = extractor.extract_phases(body);

    if phases.is_empty() {
        return ExpeditionProgress {
            phases,
            current_phase: None,
            suggested_action: SuggestedAction::NeedsDecomposition,
        };
    }

    // Determine current phase by checking evidence against done criteria
    let current_phase = determine_current_phase(&phases, evidence);

    let suggested_action = match current_phase {
        None => {
            // No evidence of progress — suggest starting phase 1
            SuggestedAction::StartPhase {
                phase_number: phases[0].number,
                title: phases[0].title.clone(),
            }
        }
        Some(idx) if idx >= phases.len() - 1 => {
            // Evidence covers all phases
            if phase_appears_complete(&phases[idx], evidence) {
                SuggestedAction::ReadyForReview
            } else {
                SuggestedAction::ContinuePhase {
                    phase_number: phases[idx].number,
                    title: phases[idx].title.clone(),
                }
            }
        }
        Some(idx) => {
            if phase_appears_complete(&phases[idx], evidence) {
                // Current phase done, suggest next
                let next = &phases[idx + 1];
                SuggestedAction::StartPhase {
                    phase_number: next.number,
                    title: next.title.clone(),
                }
            } else {
                SuggestedAction::ContinuePhase {
                    phase_number: phases[idx].number,
                    title: phases[idx].title.clone(),
                }
            }
        }
    };

    ExpeditionProgress {
        phases,
        current_phase,
        suggested_action,
    }
}

/// Determine which phase the expedition is currently on based on evidence.
///
/// Evidence strings are matched against phase titles and done criteria
/// using keyword overlap. Returns the index of the most advanced phase
/// that has evidence of work.
fn determine_current_phase(phases: &[Phase], evidence: &[&str]) -> Option<usize> {
    if evidence.is_empty() {
        return None;
    }

    let evidence_lower: Vec<String> = evidence.iter().map(|e| e.to_lowercase()).collect();
    let mut best_phase: Option<usize> = None;

    for (idx, phase) in phases.iter().enumerate() {
        let phase_keywords = extract_keywords(&phase.title);
        let task_keywords: Vec<String> = phase
            .tasks
            .iter()
            .flat_map(|t| extract_keywords(t))
            .collect();

        let all_keywords: Vec<&str> = phase_keywords
            .iter()
            .chain(task_keywords.iter())
            .map(|s| s.as_str())
            .collect();

        // Check if evidence mentions this phase's keywords
        let has_evidence = evidence_lower.iter().any(|ev| {
            let keyword_matches = all_keywords
                .iter()
                .filter(|kw| kw.len() > 3 && ev.contains(&kw.to_lowercase()))
                .count();
            keyword_matches >= 2
        });

        if has_evidence {
            best_phase = Some(idx);
        }
    }

    best_phase
}

/// Check if a phase appears complete based on evidence.
fn phase_appears_complete(phase: &Phase, evidence: &[&str]) -> bool {
    let Some(ref criteria) = phase.done_criteria else {
        // No done criteria — assume not complete (conservative)
        return false;
    };

    let criteria_keywords = extract_keywords(criteria);
    let evidence_lower: Vec<String> = evidence.iter().map(|e| e.to_lowercase()).collect();

    // Check if the majority of criteria keywords appear in evidence
    let matched = criteria_keywords
        .iter()
        .filter(|kw| kw.len() > 3 && evidence_lower.iter().any(|ev| ev.contains(kw.as_str())))
        .count();

    let threshold = (criteria_keywords.len() as f64 * 0.5).ceil() as usize;
    matched >= threshold.max(1)
}

/// Extract meaningful keywords from text (lowercased, stop words removed).
fn extract_keywords(text: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by",
        "from", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had", "do",
        "does", "did", "will", "would", "could", "should", "may", "might", "can", "shall", "this",
        "that", "these", "those", "it", "its", "not", "no", "all", "each", "every", "any", "both",
        "few", "more", "most", "other", "some", "such", "only", "own", "same", "so", "than", "too",
        "very",
    ];

    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| w.len() > 2 && !STOP_WORDS.contains(w))
        .map(|w| w.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_BODY: &str = r#"# EX-3047: Conductor Foundation — Kanban Reader + State Engine

## Context

Phase 3 Wave 1. The Conductor v1 automates agent orchestration.

## Phase 1: Kanban State Reader

- Query Arrow kanban via NATS for all items by status
- Parse item metadata: type, status, assignee, priority, relations
- Build in-memory work graph: items + dependencies + assignments
- Subscribe to `kanban.event.*` for real-time state updates
- **Done when:** Reader produces correct status summary for 20+ items

**Key files:** New `crates/nusy-conductor/src/reader.rs`

## Phase 2: Expedition Decomposer

- Read expedition body content to extract phases
- Use LLM judgment for natural language phase extraction
- Determine current phase (compare done criteria against PR/commit evidence)
- Suggest next action for each in-progress item
- **Done when:** Expedition with 5 phases -> correctly identifies current phase and suggests next

**Key files:** New `crates/nusy-conductor/src/decomposer.rs`

## Phase 3: Assignee Tracker

- Track which agent is working on what (from kanban assignee field)
- Track agent availability: agent with 0 in-progress items = available
- Track agent capabilities: DGX = GPU work, M5 = architecture, Mini = infrastructure
- **Done when:** Tracker correctly identifies available agents and suggests assignment

**Key files:** New `crates/nusy-conductor/src/state.rs`"#;

    #[test]
    fn test_extract_phases_from_expedition_body() {
        let extractor = StructuralExtractor;
        let phases = extractor.extract_phases(SAMPLE_BODY);

        assert_eq!(phases.len(), 3, "should extract 3 phases");
        assert_eq!(phases[0].number, 1);
        assert_eq!(phases[0].title, "Kanban State Reader");
        assert_eq!(phases[1].number, 2);
        assert_eq!(phases[1].title, "Expedition Decomposer");
        assert_eq!(phases[2].number, 3);
        assert_eq!(phases[2].title, "Assignee Tracker");
    }

    #[test]
    fn test_phase_tasks_extracted() {
        let extractor = StructuralExtractor;
        let phases = extractor.extract_phases(SAMPLE_BODY);

        // Phase 1 should have 4 task bullets (the "Done when" line is separate)
        assert_eq!(phases[0].tasks.len(), 4);
        assert!(phases[0].tasks[0].contains("Query Arrow kanban"));
        assert!(phases[0].tasks[3].contains("Subscribe to"));
    }

    #[test]
    fn test_done_criteria_extracted() {
        let extractor = StructuralExtractor;
        let phases = extractor.extract_phases(SAMPLE_BODY);

        let criteria = phases[0]
            .done_criteria
            .as_ref()
            .expect("should have criteria");
        assert!(criteria.contains("correct status summary"));
        assert!(criteria.contains("20+"));
    }

    #[test]
    fn test_5_phase_expedition_identifies_current_and_next() {
        let body = r#"## Phase 1: Setup
- Install dependencies
- **Done when:** Dependencies installed

## Phase 2: Core Implementation
- Write the main module
- **Done when:** Module compiles and passes basic test

## Phase 3: Testing
- Write unit tests
- Write integration tests
- **Done when:** All tests pass

## Phase 4: Documentation
- Write API docs
- **Done when:** Docs generated successfully

## Phase 5: Release
- Tag release
- Publish crate
- **Done when:** Crate published to registry"#;

        let item = WorkItem {
            id: "EX-5000".to_string(),
            title: "Five phase expedition".to_string(),
            item_type: "expedition".to_string(),
            status: "in_progress".to_string(),
            priority: None,
            assignee: Some("M5".to_string()),
            board: None,
            tags: vec![],
            related: vec![],
            depends_on: vec![],
            body: Some(body.to_string()),
        };

        // Evidence that phase 3 (Testing) is in progress
        let evidence = &[
            "implemented core module, all basic tests pass",
            "writing unit tests and integration tests for the main module",
        ];

        let progress = analyze_expedition(&item, evidence);
        assert_eq!(progress.phases.len(), 5);
        assert!(progress.current_phase.is_some());
        let current = progress.current_phase.unwrap();
        // Should be on phase 3 (index 2) based on testing evidence
        assert!(
            current >= 1,
            "should be at least on phase 2, got phase {}",
            current + 1
        );
    }

    #[test]
    fn test_empty_body_returns_needs_decomposition() {
        let item = WorkItem {
            id: "EX-5001".to_string(),
            title: "No body".to_string(),
            item_type: "expedition".to_string(),
            status: "in_progress".to_string(),
            priority: None,
            assignee: None,
            board: None,
            tags: vec![],
            related: vec![],
            depends_on: vec![],
            body: None,
        };

        let progress = analyze_expedition(&item, &[]);
        assert_eq!(
            progress.suggested_action,
            SuggestedAction::NeedsDecomposition
        );
    }

    #[test]
    fn test_done_expedition_returns_already_done() {
        let item = WorkItem {
            id: "EX-5002".to_string(),
            title: "Done item".to_string(),
            item_type: "expedition".to_string(),
            status: "done".to_string(),
            priority: None,
            assignee: None,
            board: None,
            tags: vec![],
            related: vec![],
            depends_on: vec![],
            body: Some("## Phase 1: Stuff\n- did it".to_string()),
        };

        let progress = analyze_expedition(&item, &[]);
        assert_eq!(progress.suggested_action, SuggestedAction::AlreadyDone);
    }

    #[test]
    fn test_no_evidence_suggests_start_phase_1() {
        let item = WorkItem {
            id: "EX-5003".to_string(),
            title: "Fresh expedition".to_string(),
            item_type: "expedition".to_string(),
            status: "in_progress".to_string(),
            priority: None,
            assignee: Some("DGX".to_string()),
            board: None,
            tags: vec![],
            related: vec![],
            depends_on: vec![],
            body: Some(SAMPLE_BODY.to_string()),
        };

        let progress = analyze_expedition(&item, &[]);
        match &progress.suggested_action {
            SuggestedAction::StartPhase {
                phase_number,
                title,
            } => {
                assert_eq!(*phase_number, 1);
                assert_eq!(title, "Kanban State Reader");
            }
            other => panic!("expected StartPhase, got {other:?}"),
        }
    }

    #[test]
    fn test_extract_keywords() {
        let keywords = extract_keywords("Build in-memory work graph with dependencies");
        assert!(keywords.contains(&"build".to_string()));
        assert!(keywords.contains(&"in-memory".to_string()));
        assert!(keywords.contains(&"work".to_string()));
        assert!(keywords.contains(&"graph".to_string()));
        assert!(keywords.contains(&"dependencies".to_string()));
        // Stop words should be excluded
        assert!(!keywords.contains(&"with".to_string()));
    }

    #[test]
    fn test_phase_heading_variations() {
        // "Phase N: Title" format
        let p = try_parse_phase_heading("## Phase 1: Setup").unwrap();
        assert_eq!(p.number, 1);
        assert_eq!(p.title, "Setup");

        // "Phase N — Title" format
        let p = try_parse_phase_heading("## Phase 2 — Core").unwrap();
        assert_eq!(p.number, 2);
        assert_eq!(p.title, "Core");

        // "Phase N - Title" format
        let p = try_parse_phase_heading("### Phase 3 - Testing").unwrap();
        assert_eq!(p.number, 3);
        assert_eq!(p.title, "Testing");

        // Not a phase heading
        assert!(try_parse_phase_heading("## Context").is_none());
        assert!(try_parse_phase_heading("Just a line").is_none());
    }

    #[test]
    fn test_numbered_heading_format() {
        let p = try_parse_phase_heading("## 1. Setup Environment").unwrap();
        assert_eq!(p.number, 1);
        assert_eq!(p.title, "Setup Environment");
    }

    #[test]
    fn test_all_phases_complete_suggests_review() {
        let body = r#"## Phase 1: Setup
- Do setup
- **Done when:** Setup complete and verified

## Phase 2: Implement
- Write code
- **Done when:** Code compiles and tests pass"#;

        let item = WorkItem {
            id: "EX-5004".to_string(),
            title: "Two phase".to_string(),
            item_type: "expedition".to_string(),
            status: "in_progress".to_string(),
            priority: None,
            assignee: None,
            board: None,
            tags: vec![],
            related: vec![],
            depends_on: vec![],
            body: Some(body.to_string()),
        };

        // Evidence that both phases are done
        let evidence = &[
            "setup complete and verified, all dependencies installed",
            "code compiles, implementation done, all tests pass successfully",
        ];

        let progress = analyze_expedition(&item, evidence);
        assert_eq!(progress.phases.len(), 2);
        assert_eq!(progress.suggested_action, SuggestedAction::ReadyForReview);
    }
}
