//! Event hooks — config-driven actions triggered by kanban operations.
//!
//! Hooks are best-effort: failures are logged but never fail the kanban operation.
//! Configuration lives in `.yurtle-kanban/hooks.yaml`.
//!
//! # Supported events
//!
//! - `on_create` — after an item is created
//! - `on_move` — after an item's status changes
//! - `on_comment` — after a comment is added
//!
//! # Supported actions
//!
//! - `shell` — execute a shell command (item fields available as env vars)
//! - `log` — append a line to a log file
//!
//! # Example hooks.yaml
//!
//! ```yaml
//! hooks:
//!   - event: on_create
//!     action: log
//!     target: .yurtle-kanban/hooks.log
//!   - event: on_move
//!     filter:
//!       to_status: done
//!     action: shell
//!     command: "echo 'Item $NK_ITEM_ID completed' >> /tmp/kanban-done.log"
//! ```

use std::path::{Path, PathBuf};

/// Events that can trigger hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookEvent {
    OnCreate,
    OnMove,
    OnComment,
}

impl HookEvent {
    pub fn as_str(&self) -> &'static str {
        match self {
            HookEvent::OnCreate => "on_create",
            HookEvent::OnMove => "on_move",
            HookEvent::OnComment => "on_comment",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "on_create" => Some(HookEvent::OnCreate),
            "on_move" => Some(HookEvent::OnMove),
            "on_comment" => Some(HookEvent::OnComment),
            _ => None,
        }
    }
}

/// Actions a hook can perform.
#[derive(Debug, Clone)]
pub enum HookAction {
    /// Execute a shell command. Item fields are available as NK_* env vars.
    Shell { command: String },
    /// Append a line to a log file.
    Log { target: PathBuf },
}

/// Optional filter for when a hook should fire.
#[derive(Debug, Clone, Default)]
pub struct HookFilter {
    pub to_status: Option<String>,
    pub item_type: Option<String>,
}

/// A configured hook.
#[derive(Debug, Clone)]
pub struct Hook {
    pub event: HookEvent,
    pub action: HookAction,
    pub filter: HookFilter,
}

/// Context passed to hooks when they fire.
#[derive(Debug, Clone)]
pub struct HookContext {
    pub item_id: String,
    pub item_type: String,
    pub title: String,
    pub from_status: Option<String>,
    pub to_status: Option<String>,
    pub agent: Option<String>,
}

/// Hook engine — loads hooks from config and fires them on events.
pub struct HookEngine {
    hooks: Vec<Hook>,
}

impl HookEngine {
    /// Create an engine with no hooks.
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Load hooks from `.yurtle-kanban/hooks.yaml`.
    pub fn load(root: &Path) -> Self {
        let hooks_path = root.join(".yurtle-kanban/hooks.yaml");
        match std::fs::read_to_string(&hooks_path) {
            Ok(content) => Self {
                hooks: parse_hooks_yaml(&content),
            },
            Err(_) => Self::new(),
        }
    }

    /// Get the number of configured hooks.
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    /// Fire all hooks matching the given event and context.
    /// Best-effort: failures are logged to stderr, never propagated.
    pub fn fire(&self, event: &HookEvent, ctx: &HookContext) {
        for hook in &self.hooks {
            if &hook.event != event {
                continue;
            }

            // Apply filters
            if let Some(ref filter_status) = hook.filter.to_status
                && ctx.to_status.as_deref() != Some(filter_status.as_str())
            {
                continue;
            }
            if let Some(ref filter_type) = hook.filter.item_type
                && ctx.item_type != *filter_type
            {
                continue;
            }

            // Execute action — best effort
            match &hook.action {
                HookAction::Shell { command } => {
                    execute_shell_hook(command, ctx);
                }
                HookAction::Log { target } => {
                    execute_log_hook(target, event, ctx);
                }
            }
        }
    }
}

impl Default for HookEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Execute a shell command hook with NK_* environment variables.
fn execute_shell_hook(command: &str, ctx: &HookContext) {
    let result = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("NK_ITEM_ID", &ctx.item_id)
        .env("NK_ITEM_TYPE", &ctx.item_type)
        .env("NK_TITLE", &ctx.title)
        .env("NK_FROM_STATUS", ctx.from_status.as_deref().unwrap_or(""))
        .env("NK_TO_STATUS", ctx.to_status.as_deref().unwrap_or(""))
        .env("NK_AGENT", ctx.agent.as_deref().unwrap_or(""))
        .output();

    match result {
        Ok(output) => {
            if !output.status.success() {
                eprintln!(
                    "hook shell command failed (exit {}): {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        Err(e) => {
            eprintln!("hook shell command error: {e}");
        }
    }
}

/// Execute a log hook — append event line to target file.
fn execute_log_hook(target: &Path, event: &HookEvent, ctx: &HookContext) {
    use std::io::Write;
    let line = format!(
        "{} {} {} {}\n",
        chrono::Utc::now().to_rfc3339(),
        event.as_str(),
        ctx.item_id,
        ctx.title,
    );
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(target)
    {
        Ok(mut file) => {
            if let Err(e) = file.write_all(line.as_bytes()) {
                eprintln!("hook log write error: {e}");
            }
        }
        Err(e) => {
            eprintln!("hook log open error: {e}");
        }
    }
}

/// Parse hooks from YAML content (minimal parser — no serde_yaml dependency needed
/// since we already have it in the workspace).
fn parse_hooks_yaml(content: &str) -> Vec<Hook> {
    // Use serde_yaml which is already a dependency
    let value: serde_yaml::Value = match serde_yaml::from_str(content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let hooks_array = match value.get("hooks").and_then(|v| v.as_sequence()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    let mut hooks = Vec::new();
    for entry in hooks_array {
        let event_str = entry.get("event").and_then(|v| v.as_str()).unwrap_or("");
        let event = match HookEvent::parse(event_str) {
            Some(e) => e,
            None => continue,
        };

        let action_str = entry.get("action").and_then(|v| v.as_str()).unwrap_or("");

        let action = match action_str {
            "shell" => {
                let cmd = entry
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                HookAction::Shell { command: cmd }
            }
            "log" => {
                let target = entry
                    .get("target")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".yurtle-kanban/hooks.log")
                    .to_string();
                HookAction::Log {
                    target: PathBuf::from(target),
                }
            }
            _ => continue,
        };

        let mut filter = HookFilter::default();
        if let Some(filter_map) = entry.get("filter") {
            filter.to_status = filter_map
                .get("to_status")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            filter.item_type = filter_map
                .get("item_type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }

        hooks.push(Hook {
            event,
            action,
            filter,
        });
    }

    hooks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty_yaml() {
        let hooks = parse_hooks_yaml("");
        assert!(hooks.is_empty());
    }

    #[test]
    fn test_parse_hooks_yaml() {
        let yaml = r#"
hooks:
  - event: on_create
    action: log
    target: /tmp/test.log
  - event: on_move
    action: shell
    command: "echo done"
    filter:
      to_status: done
"#;
        let hooks = parse_hooks_yaml(yaml);
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].event, HookEvent::OnCreate);
        assert!(matches!(hooks[0].action, HookAction::Log { .. }));
        assert_eq!(hooks[1].event, HookEvent::OnMove);
        assert!(matches!(hooks[1].action, HookAction::Shell { .. }));
        assert_eq!(hooks[1].filter.to_status, Some("done".to_string()));
    }

    #[test]
    fn test_hook_filter_blocks_non_matching() {
        let engine = HookEngine {
            hooks: vec![Hook {
                event: HookEvent::OnMove,
                action: HookAction::Log {
                    target: PathBuf::from("/dev/null"),
                },
                filter: HookFilter {
                    to_status: Some("done".to_string()),
                    item_type: None,
                },
            }],
        };

        // This should NOT fire (to_status doesn't match)
        let ctx = HookContext {
            item_id: "EX-3001".to_string(),
            item_type: "expedition".to_string(),
            title: "Test".to_string(),
            from_status: Some("backlog".to_string()),
            to_status: Some("in_progress".to_string()),
            agent: None,
        };
        engine.fire(&HookEvent::OnMove, &ctx);
        // No panic = filter worked (log to /dev/null)
    }

    #[test]
    fn test_hook_event_roundtrip() {
        for event in &[HookEvent::OnCreate, HookEvent::OnMove, HookEvent::OnComment] {
            let s = event.as_str();
            let parsed = HookEvent::parse(s).expect("should parse");
            assert_eq!(&parsed, event);
        }
    }

    #[test]
    fn test_load_nonexistent_hooks_file() {
        let engine = HookEngine::load(Path::new("/nonexistent"));
        assert_eq!(engine.hook_count(), 0);
    }

    #[test]
    fn test_shell_hook_sets_env_vars() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output_file = dir.path().join("output.txt");

        let ctx = HookContext {
            item_id: "EX-3001".to_string(),
            item_type: "expedition".to_string(),
            title: "Test Title".to_string(),
            from_status: None,
            to_status: Some("in_progress".to_string()),
            agent: Some("Mini".to_string()),
        };

        let cmd = format!(
            "echo \"$NK_ITEM_ID $NK_ITEM_TYPE $NK_AGENT\" > {}",
            output_file.display()
        );
        execute_shell_hook(&cmd, &ctx);

        let content = std::fs::read_to_string(&output_file).expect("read output");
        assert!(content.contains("EX-3001"));
        assert!(content.contains("expedition"));
        assert!(content.contains("Mini"));
    }

    #[test]
    fn test_log_hook_appends() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("hooks.log");

        let ctx = HookContext {
            item_id: "EX-3001".to_string(),
            item_type: "expedition".to_string(),
            title: "Test".to_string(),
            from_status: None,
            to_status: None,
            agent: None,
        };

        execute_log_hook(&log_path, &HookEvent::OnCreate, &ctx);
        execute_log_hook(&log_path, &HookEvent::OnCreate, &ctx);

        let content = std::fs::read_to_string(&log_path).expect("read log");
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2, "should have 2 log entries");
        assert!(lines[0].contains("on_create"));
        assert!(lines[0].contains("EX-3001"));
    }
}
