//! State machine — valid transitions and WIP limit enforcement.
//!
//! Each board has its own state graph. Transitions are validated
//! against the graph. WIP limits are enforced per-state-category.

use crate::config::BoardConfig;

/// Errors from state machine operations.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("Invalid transition: '{from}' → '{to}' is not allowed on board '{board}'")]
    InvalidTransition {
        from: String,
        to: String,
        board: String,
    },

    #[error("Invalid state '{state}' for board '{board}'")]
    InvalidState { state: String, board: String },

    #[error("WIP limit reached: {current}/{limit} items at '{status}' (use --force to override)")]
    WipLimitReached {
        status: String,
        current: u32,
        limit: u32,
    },

    #[error(
        "Invalid resolution '{resolution}'. Valid values: completed, superseded, wont_do, duplicate, obsolete, merged"
    )]
    InvalidResolution { resolution: String },

    #[error(
        "Resolution can only be set on terminal states (done, complete, abandoned, retired), not '{status}'"
    )]
    ResolutionOnNonTerminal { status: String },
}

pub type Result<T> = std::result::Result<T, StateError>;

/// Check if a state transition is valid for the given board.
///
/// Valid transitions: state[i] → state[j] where j > i (forward only).
/// Exception: any state → "done"/"complete"/"abandoned" is always allowed.
pub fn validate_transition(board: &BoardConfig, from: &str, to: &str) -> Result<()> {
    validate_transition_for_type(board, from, to, None)
}

/// Check if a state transition is valid for a specific item type on the board.
///
/// If `item_type` is provided and the board has `type_states` for that type,
/// validation uses the type-specific state list. Otherwise falls back to
/// board-level states.
pub fn validate_transition_for_type(
    board: &BoardConfig,
    from: &str,
    to: &str,
    item_type: Option<&str>,
) -> Result<()> {
    let states = match item_type {
        Some(t) => board.states_for_type(t),
        None => &board.states,
    };

    let from_valid = states.iter().any(|s| s == from);
    let to_valid = states.iter().any(|s| s == to);

    if !from_valid {
        return Err(StateError::InvalidState {
            state: from.to_string(),
            board: board.name.clone(),
        });
    }
    if !to_valid {
        return Err(StateError::InvalidState {
            state: to.to_string(),
            board: board.name.clone(),
        });
    }

    let from_idx = states
        .iter()
        .position(|s| s == from)
        .expect("already validated");
    let to_idx = states
        .iter()
        .position(|s| s == to)
        .expect("already validated");

    // Forward transitions are always valid
    if to_idx > from_idx {
        return Ok(());
    }

    // Backward transitions are invalid
    Err(StateError::InvalidTransition {
        from: from.to_string(),
        to: to.to_string(),
        board: board.name.clone(),
    })
}

/// Check WIP limits for a target status.
///
/// Returns Ok if under limit, Err if at or over limit.
/// `item_type` is checked against `wip_exempt_types` (e.g., voyages).
pub fn check_wip_limit(
    board: &BoardConfig,
    target_status: &str,
    current_count: u32,
    item_type: &str,
) -> Result<()> {
    // WIP-exempt types (e.g., voyages) bypass limits
    if board.is_wip_exempt(item_type) {
        return Ok(());
    }

    // Map status to WIP category
    let category = status_to_wip_category(target_status, &board.name);

    if let Some(limit) = board.wip_limit(category)
        && current_count >= limit
    {
        return Err(StateError::WipLimitReached {
            status: target_status.to_string(),
            current: current_count,
            limit,
        });
    }

    Ok(())
}

/// Map a status to its WIP limit category.
///
/// Development board:
///   - backlog, planning, ready → "provisioning"
///   - in_progress → "underway"
///   - review → "approaching"
///   - done → no limit
///
/// Research board:
///   - active → "active"
///   - others → no limit
fn status_to_wip_category<'a>(status: &str, board_name: &str) -> &'a str {
    match board_name {
        "development" => match status {
            "backlog" | "planning" | "ready" => "provisioning",
            "in_progress" => "underway",
            "review" => "approaching",
            _ => "",
        },
        "research" => match status {
            "active" => "active",
            _ => "",
        },
        _ => "",
    }
}

/// Valid resolution values (ported from yurtle-kanban).
const VALID_RESOLUTIONS: &[&str] = &[
    "completed",
    "superseded",
    "wont_do",
    "duplicate",
    "obsolete",
    "merged",
];

/// Terminal states where resolution can be set.
const TERMINAL_STATES: &[&str] = &["done", "complete", "abandoned", "retired"];

/// Validate a resolution value. Returns Ok if valid or None.
pub fn validate_resolution(resolution: Option<&str>, target_status: &str) -> Result<()> {
    let Some(res) = resolution else {
        return Ok(());
    };

    if !VALID_RESOLUTIONS.contains(&res) {
        return Err(StateError::InvalidResolution {
            resolution: res.to_string(),
        });
    }

    if !TERMINAL_STATES.contains(&target_status) {
        return Err(StateError::ResolutionOnNonTerminal {
            status: target_status.to_string(),
        });
    }

    Ok(())
}

/// Check if a status is terminal (resolution-eligible).
pub fn is_terminal_state(status: &str) -> bool {
    TERMINAL_STATES.contains(&status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn dev_board() -> BoardConfig {
        BoardConfig {
            name: "development".to_string(),
            preset: "nautical".to_string(),
            path: "kanban-work/".to_string(),
            scan_paths: vec!["kanban-work/expeditions/".to_string()],
            ignore: vec![],
            wip_exempt_types: vec!["voyage".to_string()],
            wip_limits: HashMap::from([
                ("provisioning".to_string(), 50),
                ("underway".to_string(), 4),
                ("approaching".to_string(), 3),
            ]),
            states: vec![
                "backlog".to_string(),
                "planning".to_string(),
                "ready".to_string(),
                "in_progress".to_string(),
                "review".to_string(),
                "done".to_string(),
            ],
            phases: vec![],
            type_states: HashMap::new(),
        }
    }

    fn research_board() -> BoardConfig {
        BoardConfig {
            name: "research".to_string(),
            preset: "hdd".to_string(),
            path: "research/".to_string(),
            scan_paths: vec!["research/hypotheses/".to_string()],
            ignore: vec![],
            wip_exempt_types: vec![],
            wip_limits: HashMap::from([("active".to_string(), 5)]),
            states: vec![
                "draft".to_string(),
                "active".to_string(),
                "complete".to_string(),
                "abandoned".to_string(),
            ],
            phases: vec![],
            type_states: HashMap::new(),
        }
    }

    #[test]
    fn test_valid_forward_transitions() {
        let board = dev_board();
        assert!(validate_transition(&board, "backlog", "in_progress").is_ok());
        assert!(validate_transition(&board, "in_progress", "review").is_ok());
        assert!(validate_transition(&board, "review", "done").is_ok());
        assert!(validate_transition(&board, "backlog", "done").is_ok()); // skip allowed (forward)
    }

    #[test]
    fn test_invalid_backward_transition() {
        let board = dev_board();
        assert!(validate_transition(&board, "done", "backlog").is_err());
        assert!(validate_transition(&board, "review", "in_progress").is_err());
        assert!(validate_transition(&board, "in_progress", "backlog").is_err());
    }

    #[test]
    fn test_invalid_state() {
        let board = dev_board();
        let err = validate_transition(&board, "nonexistent", "done");
        assert!(err.is_err());
        match err.unwrap_err() {
            StateError::InvalidState { state, .. } => assert_eq!(state, "nonexistent"),
            _ => panic!("Expected InvalidState"),
        }
    }

    #[test]
    fn test_research_transitions() {
        let board = research_board();
        assert!(validate_transition(&board, "draft", "active").is_ok());
        assert!(validate_transition(&board, "active", "complete").is_ok());
        assert!(validate_transition(&board, "draft", "abandoned").is_ok());
        assert!(validate_transition(&board, "complete", "draft").is_err());
    }

    #[test]
    fn test_wip_limit_under() {
        let board = dev_board();
        assert!(check_wip_limit(&board, "in_progress", 3, "expedition").is_ok());
    }

    #[test]
    fn test_wip_limit_at_capacity() {
        let board = dev_board();
        let err = check_wip_limit(&board, "in_progress", 4, "expedition");
        assert!(err.is_err());
        match err.unwrap_err() {
            StateError::WipLimitReached { current, limit, .. } => {
                assert_eq!(current, 4);
                assert_eq!(limit, 4);
            }
            _ => panic!("Expected WipLimitReached"),
        }
    }

    #[test]
    fn test_wip_exempt_voyage() {
        let board = dev_board();
        // Voyages bypass WIP limits even when at capacity
        assert!(check_wip_limit(&board, "in_progress", 4, "voyage").is_ok());
        assert!(check_wip_limit(&board, "in_progress", 100, "voyage").is_ok());
    }

    #[test]
    fn test_wip_no_limit_for_done() {
        let board = dev_board();
        // No WIP limit on "done" status
        assert!(check_wip_limit(&board, "done", 1000, "expedition").is_ok());
    }

    fn research_board_with_type_states() -> BoardConfig {
        BoardConfig {
            name: "research".to_string(),
            preset: "hdd".to_string(),
            path: "research/".to_string(),
            scan_paths: vec!["research/".to_string()],
            ignore: vec![],
            wip_exempt_types: vec![],
            wip_limits: HashMap::from([("active".to_string(), 5)]),
            states: vec![
                "draft".to_string(),
                "active".to_string(),
                "complete".to_string(),
                "abandoned".to_string(),
                "retired".to_string(),
            ],
            phases: vec![],
            type_states: HashMap::from([
                (
                    "hypothesis".to_string(),
                    vec![
                        "draft".to_string(),
                        "active".to_string(),
                        "retired".to_string(),
                    ],
                ),
                (
                    "measure".to_string(),
                    vec![
                        "draft".to_string(),
                        "active".to_string(),
                        "retired".to_string(),
                    ],
                ),
                (
                    "experiment".to_string(),
                    vec![
                        "planned".to_string(),
                        "running".to_string(),
                        "complete".to_string(),
                        "abandoned".to_string(),
                    ],
                ),
                (
                    "paper".to_string(),
                    vec![
                        "draft".to_string(),
                        "outline".to_string(),
                        "writing".to_string(),
                        "review".to_string(),
                        "complete".to_string(),
                        "abandoned".to_string(),
                    ],
                ),
                (
                    "idea".to_string(),
                    vec![
                        "captured".to_string(),
                        "formalized".to_string(),
                        "abandoned".to_string(),
                    ],
                ),
            ]),
        }
    }

    #[test]
    fn test_hypothesis_cannot_complete() {
        let board = research_board_with_type_states();
        // Hypotheses go draft → active → retired, never "complete"
        assert!(
            validate_transition_for_type(&board, "draft", "active", Some("hypothesis")).is_ok()
        );
        assert!(
            validate_transition_for_type(&board, "active", "retired", Some("hypothesis")).is_ok()
        );
        assert!(
            validate_transition_for_type(&board, "active", "complete", Some("hypothesis")).is_err()
        );
    }

    #[test]
    fn test_measure_cannot_complete() {
        let board = research_board_with_type_states();
        // Measures go draft → active → retired, never "complete"
        assert!(validate_transition_for_type(&board, "draft", "active", Some("measure")).is_ok());
        assert!(validate_transition_for_type(&board, "active", "retired", Some("measure")).is_ok());
        assert!(
            validate_transition_for_type(&board, "active", "complete", Some("measure")).is_err()
        );
    }

    #[test]
    fn test_experiment_follows_run_lifecycle() {
        let board = research_board_with_type_states();
        assert!(
            validate_transition_for_type(&board, "planned", "running", Some("experiment")).is_ok()
        );
        assert!(
            validate_transition_for_type(&board, "running", "complete", Some("experiment")).is_ok()
        );
        assert!(
            validate_transition_for_type(&board, "planned", "abandoned", Some("experiment"))
                .is_ok()
        );
        // Can't go backward
        assert!(
            validate_transition_for_type(&board, "complete", "running", Some("experiment"))
                .is_err()
        );
    }

    #[test]
    fn test_paper_follows_work_lifecycle() {
        let board = research_board_with_type_states();
        assert!(validate_transition_for_type(&board, "draft", "outline", Some("paper")).is_ok());
        assert!(validate_transition_for_type(&board, "outline", "writing", Some("paper")).is_ok());
        assert!(validate_transition_for_type(&board, "writing", "review", Some("paper")).is_ok());
        assert!(validate_transition_for_type(&board, "review", "complete", Some("paper")).is_ok());
        assert!(validate_transition_for_type(&board, "draft", "abandoned", Some("paper")).is_ok());
    }

    #[test]
    fn test_idea_captured_to_formalized() {
        let board = research_board_with_type_states();
        assert!(
            validate_transition_for_type(&board, "captured", "formalized", Some("idea")).is_ok()
        );
        assert!(
            validate_transition_for_type(&board, "captured", "abandoned", Some("idea")).is_ok()
        );
        // Can't go backward
        assert!(
            validate_transition_for_type(&board, "formalized", "captured", Some("idea")).is_err()
        );
    }

    #[test]
    fn test_unknown_type_uses_board_states() {
        let board = research_board_with_type_states();
        // Unknown type falls back to board-level states
        assert!(validate_transition_for_type(&board, "draft", "active", Some("unknown")).is_ok());
        assert!(
            validate_transition_for_type(&board, "active", "complete", Some("unknown")).is_ok()
        );
    }

    #[test]
    fn test_research_wip_limit() {
        let board = research_board();
        assert!(check_wip_limit(&board, "active", 4, "hypothesis").is_ok());
        assert!(check_wip_limit(&board, "active", 5, "hypothesis").is_err());
    }

    // ── Resolution validation tests ──

    #[test]
    fn test_valid_resolutions_on_terminal_states() {
        assert!(validate_resolution(Some("completed"), "done").is_ok());
        assert!(validate_resolution(Some("superseded"), "done").is_ok());
        assert!(validate_resolution(Some("wont_do"), "done").is_ok());
        assert!(validate_resolution(Some("duplicate"), "done").is_ok());
        assert!(validate_resolution(Some("obsolete"), "done").is_ok());
        assert!(validate_resolution(Some("merged"), "done").is_ok());
        assert!(validate_resolution(Some("completed"), "complete").is_ok());
        assert!(validate_resolution(Some("wont_do"), "abandoned").is_ok());
        assert!(validate_resolution(Some("completed"), "retired").is_ok());
    }

    #[test]
    fn test_none_resolution_always_ok() {
        assert!(validate_resolution(None, "done").is_ok());
        assert!(validate_resolution(None, "in_progress").is_ok());
        assert!(validate_resolution(None, "backlog").is_ok());
    }

    #[test]
    fn test_invalid_resolution_value() {
        let err = validate_resolution(Some("cancelled"), "done");
        assert!(err.is_err());
        match err.unwrap_err() {
            StateError::InvalidResolution { resolution } => {
                assert_eq!(resolution, "cancelled");
            }
            _ => panic!("Expected InvalidResolution"),
        }
    }

    #[test]
    fn test_resolution_on_non_terminal_state() {
        let err = validate_resolution(Some("completed"), "in_progress");
        assert!(err.is_err());
        match err.unwrap_err() {
            StateError::ResolutionOnNonTerminal { status } => {
                assert_eq!(status, "in_progress");
            }
            _ => panic!("Expected ResolutionOnNonTerminal"),
        }
    }

    #[test]
    fn test_is_terminal_state() {
        assert!(is_terminal_state("done"));
        assert!(is_terminal_state("complete"));
        assert!(is_terminal_state("abandoned"));
        assert!(is_terminal_state("retired"));
        assert!(!is_terminal_state("in_progress"));
        assert!(!is_terminal_state("backlog"));
        assert!(!is_terminal_state("review"));
    }
}
