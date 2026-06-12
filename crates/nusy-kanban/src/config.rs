//! Board configuration — parse `.yurtle-kanban/config.yaml`.
//!
//! Supports two boards (development/nautical, research/hdd) with
//! configurable item types, state graphs, WIP limits, and scan paths.

/// Default config YAML for `nusy-kanban init`.
pub fn default_config_yaml() -> &'static str {
    r#"version: "2.0"

boards:
  - name: development
    preset: nautical
    path: kanban-work/
    scan_paths:
      - "kanban-work/expeditions/"
      - "kanban-work/voyages/"
      - "kanban-work/chores/"
    wip_exempt_types:
      - voyage
    wip_limits:
      in_progress: 4
      review: 3
    states:
      - backlog
      - in_progress
      - review
      - done

  - name: research
    preset: hdd
    path: research/
    scan_paths:
      - "research/"
    states:
      - draft
      - captured
      - planned
      - outline
      - active
      - writing
      - running
      - review
      - formalized
      - complete
      - abandoned
      - retired
    type_states:
      hypothesis:
        - draft
        - active
        - retired
      measure:
        - draft
        - active
        - retired
      paper:
        - draft
        - outline
        - writing
        - review
        - complete
        - abandoned
      experiment:
        - planned
        - running
        - complete
        - abandoned
      literature:
        - draft
        - active
        - complete
      idea:
        - captured
        - formalized
        - abandoned
"#
}

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Errors from config parsing.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("Board not found: {0}")]
    BoardNotFound(String),

    #[error("Invalid config: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, ConfigError>;

/// Top-level config file structure.
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigFile {
    pub version: String,
    pub boards: Vec<BoardConfig>,
    pub namespace: Option<String>,
    pub default_board: Option<String>,
    pub relationships: Option<HashMap<String, RelationshipConfig>>,
    pub critical_path: Option<CriticalPathConfig>,
}

/// A single board's configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct BoardConfig {
    pub name: String,
    pub preset: String,
    pub path: String,
    pub scan_paths: Vec<String>,
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default)]
    pub wip_exempt_types: Vec<String>,
    #[serde(default)]
    pub wip_limits: HashMap<String, u32>,
    pub states: Vec<String>,
    #[serde(default)]
    pub phases: Vec<String>,
    /// Per-type state overrides. If a type has an entry here, its valid states
    /// are these instead of the board-level `states`. This enables research
    /// items (measures, hypotheses, papers, etc.) to have distinct lifecycles.
    #[serde(default)]
    pub type_states: HashMap<String, Vec<String>>,
}

/// Cross-board relationship config.
#[derive(Debug, Clone, Deserialize)]
pub struct RelationshipConfig {
    pub from_board: String,
    pub to_board: String,
    pub predicate: String,
}

/// Critical path tracking config.
#[derive(Debug, Clone, Deserialize)]
pub struct CriticalPathConfig {
    pub enabled: bool,
    #[serde(default)]
    pub boost_priority: bool,
}

impl ConfigFile {
    /// Load config from a YAML file path.
    pub fn from_path(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        Self::from_yaml(&contents)
    }

    /// Parse config from a YAML string.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let config: ConfigFile = serde_yaml::from_str(yaml)?;
        config.validate()?;
        Ok(config)
    }

    /// Get a board config by name.
    pub fn board(&self, name: &str) -> Result<&BoardConfig> {
        self.boards
            .iter()
            .find(|b| b.name == name)
            .ok_or_else(|| ConfigError::BoardNotFound(name.to_string()))
    }

    /// Get the default board config.
    pub fn default_board(&self) -> Result<&BoardConfig> {
        let name = self.default_board.as_deref().unwrap_or("development");
        self.board(name)
    }

    /// Validate the config for internal consistency.
    fn validate(&self) -> Result<()> {
        if self.boards.is_empty() {
            return Err(ConfigError::Invalid("No boards defined".to_string()));
        }
        for board in &self.boards {
            if board.states.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "Board '{}' has no states defined",
                    board.name
                )));
            }
            if board.scan_paths.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "Board '{}' has no scan_paths defined",
                    board.name
                )));
            }
        }
        Ok(())
    }
}

impl BoardConfig {
    /// Get the WIP limit for a given state category, or None if unlimited.
    pub fn wip_limit(&self, category: &str) -> Option<u32> {
        self.wip_limits.get(category).copied()
    }

    /// Check if a type is WIP-exempt (e.g., voyages).
    pub fn is_wip_exempt(&self, item_type: &str) -> bool {
        self.wip_exempt_types
            .iter()
            .any(|t| t.eq_ignore_ascii_case(item_type))
    }

    /// Check if a state is valid for this board (board-level states).
    pub fn is_valid_state(&self, state: &str) -> bool {
        self.states.iter().any(|s| s == state)
    }

    /// Check if a state is valid for a specific item type on this board.
    /// Falls back to board-level states if no type-specific override exists.
    pub fn is_valid_state_for_type(&self, state: &str, item_type: &str) -> bool {
        if let Some(type_states) = self.type_states.get(item_type) {
            type_states.iter().any(|s| s == state)
        } else {
            self.is_valid_state(state)
        }
    }

    /// Get the valid states for a specific item type.
    /// Returns type-specific states if defined, otherwise board-level states.
    pub fn states_for_type(&self, item_type: &str) -> &[String] {
        if let Some(type_states) = self.type_states.get(item_type) {
            type_states
        } else {
            &self.states
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CONFIG: &str = r#"
version: "2.0"

boards:
  - name: development
    preset: nautical
    path: kanban-work/
    scan_paths:
      - "kanban-work/expeditions/"
      - "kanban-work/voyages/"
      - "kanban-work/chores/"
    ignore:
      - "**/archive/**"
    wip_exempt_types:
      - voyage
    wip_limits:
      provisioning: 50
      underway: 4
      approaching: 3
    states:
      - backlog
      - planning
      - ready
      - in_progress
      - review
      - done

  - name: research
    preset: hdd
    path: research/
    scan_paths:
      - "research/hypotheses/"
      - "research/experiments/"
      - "research/papers/"
    wip_limits:
      active: 5
    states:
      - draft
      - active
      - complete
      - abandoned
    phases:
      - discovery
      - design
      - execution
      - analysis
      - writing

namespace: "https://nusy.dev/"
default_board: development

relationships:
  implements:
    from_board: development
    to_board: research
    predicate: "expr:implements"
  spawns:
    from_board: research
    to_board: development
    predicate: "expr:spawns"

critical_path:
  enabled: true
  boost_priority: true
"#;

    #[test]
    fn test_parse_config() {
        let config = ConfigFile::from_yaml(SAMPLE_CONFIG).unwrap();
        assert_eq!(config.version, "2.0");
        assert_eq!(config.boards.len(), 2);
        assert_eq!(config.default_board.as_deref(), Some("development"));
    }

    #[test]
    fn test_board_lookup() {
        let config = ConfigFile::from_yaml(SAMPLE_CONFIG).unwrap();
        let dev = config.board("development").unwrap();
        assert_eq!(dev.preset, "nautical");
        assert_eq!(dev.states.len(), 6);

        let research = config.board("research").unwrap();
        assert_eq!(research.preset, "hdd");
        assert_eq!(research.states.len(), 4);
        assert_eq!(research.phases.len(), 5);
    }

    #[test]
    fn test_default_board() {
        let config = ConfigFile::from_yaml(SAMPLE_CONFIG).unwrap();
        let default = config.default_board().unwrap();
        assert_eq!(default.name, "development");
    }

    #[test]
    fn test_board_not_found() {
        let config = ConfigFile::from_yaml(SAMPLE_CONFIG).unwrap();
        assert!(config.board("nonexistent").is_err());
    }

    #[test]
    fn test_wip_limits() {
        let config = ConfigFile::from_yaml(SAMPLE_CONFIG).unwrap();
        let dev = config.board("development").unwrap();
        assert_eq!(dev.wip_limit("underway"), Some(4));
        assert_eq!(dev.wip_limit("approaching"), Some(3));
        assert_eq!(dev.wip_limit("nonexistent"), None);
    }

    #[test]
    fn test_wip_exempt() {
        let config = ConfigFile::from_yaml(SAMPLE_CONFIG).unwrap();
        let dev = config.board("development").unwrap();
        assert!(dev.is_wip_exempt("voyage"));
        assert!(dev.is_wip_exempt("Voyage")); // case insensitive
        assert!(!dev.is_wip_exempt("expedition"));
    }

    #[test]
    fn test_valid_states() {
        let config = ConfigFile::from_yaml(SAMPLE_CONFIG).unwrap();
        let dev = config.board("development").unwrap();
        assert!(dev.is_valid_state("backlog"));
        assert!(dev.is_valid_state("in_progress"));
        assert!(!dev.is_valid_state("archived"));
    }

    #[test]
    fn test_relationships() {
        let config = ConfigFile::from_yaml(SAMPLE_CONFIG).unwrap();
        let rels = config.relationships.as_ref().unwrap();
        assert_eq!(rels.len(), 2);
        let implements = &rels["implements"];
        assert_eq!(implements.from_board, "development");
        assert_eq!(implements.to_board, "research");
    }

    #[test]
    fn test_critical_path() {
        let config = ConfigFile::from_yaml(SAMPLE_CONFIG).unwrap();
        let cp = config.critical_path.as_ref().unwrap();
        assert!(cp.enabled);
        assert!(cp.boost_priority);
    }

    #[test]
    fn test_loads_real_config() {
        let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.yurtle-kanban/config.yaml");
        if config_path.exists() {
            let config = ConfigFile::from_path(&config_path).unwrap();
            assert_eq!(config.boards.len(), 2);
            assert!(config.board("development").is_ok());
            assert!(config.board("research").is_ok());
        }
    }

    #[test]
    fn test_type_states() {
        let config = ConfigFile::from_yaml(SAMPLE_CONFIG_WITH_TYPE_STATES).unwrap();
        let research = config.board("research").unwrap();

        // Board-level states
        assert!(research.is_valid_state("draft"));
        assert!(research.is_valid_state("active"));

        // Type-specific states
        assert!(research.is_valid_state_for_type("planned", "experiment"));
        assert!(research.is_valid_state_for_type("running", "experiment"));
        assert!(!research.is_valid_state_for_type("outline", "experiment")); // outline is paper-only

        assert!(research.is_valid_state_for_type("outline", "paper"));
        assert!(research.is_valid_state_for_type("writing", "paper"));
        assert!(!research.is_valid_state_for_type("running", "paper")); // running is experiment-only

        assert!(research.is_valid_state_for_type("captured", "idea"));
        assert!(research.is_valid_state_for_type("formalized", "idea"));

        // Hypothesis and measure: draft → active → retired
        assert!(research.is_valid_state_for_type("retired", "hypothesis"));
        assert!(research.is_valid_state_for_type("retired", "measure"));
        assert!(!research.is_valid_state_for_type("complete", "hypothesis")); // hypotheses don't "complete"
        assert!(!research.is_valid_state_for_type("complete", "measure")); // measures don't "complete"

        // Fallback: unknown type uses board-level states
        assert!(research.is_valid_state_for_type("draft", "unknown_type"));
        assert!(research.is_valid_state_for_type("active", "unknown_type"));
    }

    #[test]
    fn test_states_for_type() {
        let config = ConfigFile::from_yaml(SAMPLE_CONFIG_WITH_TYPE_STATES).unwrap();
        let research = config.board("research").unwrap();

        let exp_states = research.states_for_type("experiment");
        assert_eq!(exp_states, &["planned", "running", "complete", "abandoned"]);

        let hyp_states = research.states_for_type("hypothesis");
        assert_eq!(hyp_states, &["draft", "active", "retired"]);

        // Unknown type falls back to board states
        let unknown_states = research.states_for_type("widget");
        assert_eq!(unknown_states, research.states.as_slice());
    }

    const SAMPLE_CONFIG_WITH_TYPE_STATES: &str = r#"
version: "2.0"

boards:
  - name: development
    preset: nautical
    path: kanban-work/
    scan_paths:
      - "kanban-work/expeditions/"
    states:
      - backlog
      - in_progress
      - review
      - done

  - name: research
    preset: hdd
    path: research/
    scan_paths:
      - "research/"
    states:
      - draft
      - captured
      - planned
      - outline
      - active
      - writing
      - running
      - review
      - formalized
      - complete
      - abandoned
      - retired
    type_states:
      hypothesis:
        - draft
        - active
        - retired
      measure:
        - draft
        - active
        - retired
      paper:
        - draft
        - outline
        - writing
        - review
        - complete
        - abandoned
      experiment:
        - planned
        - running
        - complete
        - abandoned
      literature:
        - draft
        - active
        - complete
      idea:
        - captured
        - formalized
        - abandoned
"#;

    #[test]
    fn test_invalid_config_no_boards() {
        let yaml = r#"
version: "1.0"
boards: []
"#;
        assert!(ConfigFile::from_yaml(yaml).is_err());
    }

    #[test]
    fn test_invalid_config_no_states() {
        let yaml = r#"
version: "1.0"
boards:
  - name: test
    preset: nautical
    path: test/
    scan_paths: ["test/"]
    states: []
"#;
        assert!(ConfigFile::from_yaml(yaml).is_err());
    }
}
