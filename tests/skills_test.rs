//! Tests for crates/nusy-kanban/skills/*/SKILL.md
//!
//! EX-3212: Claude Code Skills Auto-Install for nusy-kanban
//!
//! Validates that each skill file:
//! - exists and is readable
//! - has YAML frontmatter with name, description, disable-model-invocation
//! - has allowed-tools listing
//! - references nk or nusy-kanban commands
//! - core workflow skills are user-invocable (disable-model-invocation: true)
//! - board skills can be agent-invocable (disable-model-invocation: false)

const SKILLS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/skills");

fn load_skill(name: &str) -> String {
    let path = format!("{SKILLS_DIR}/{name}/SKILL.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read skill file {path}: {e}"))
}

/// Core workflow skills (user-invocable).
const CORE_SKILLS: &[&str] = &["work", "done", "review", "blocked"];

const ALL_SKILLS: &[&str] = &[
    "work", "done", "review", "blocked", "sync", "status", "handoff",
];

// ---------------------------------------------------------------------------
// Existence
// ---------------------------------------------------------------------------

#[test]
fn all_skill_files_exist() {
    for name in ALL_SKILLS {
        let content = load_skill(name);
        assert!(
            !content.is_empty(),
            "skills/{name}/SKILL.md must not be empty"
        );
    }
}

// ---------------------------------------------------------------------------
// YAML frontmatter
// ---------------------------------------------------------------------------

#[test]
fn all_skills_have_yaml_frontmatter() {
    for name in ALL_SKILLS {
        let content = load_skill(name);
        assert!(
            content.starts_with("---"),
            "skills/{name}/SKILL.md must start with YAML frontmatter (---)"
        );
        // Must have closing ---
        let rest = &content[3..];
        assert!(
            rest.contains("---"),
            "skills/{name}/SKILL.md must have closing --- for frontmatter"
        );
    }
}

#[test]
fn all_skills_have_name_field() {
    for name in ALL_SKILLS {
        let content = load_skill(name);
        assert!(
            content.contains(&format!("name: {name}")),
            "skills/{name}/SKILL.md frontmatter must contain 'name: {name}'"
        );
    }
}

#[test]
fn all_skills_have_description() {
    for name in ALL_SKILLS {
        let content = load_skill(name);
        assert!(
            content.contains("description:"),
            "skills/{name}/SKILL.md must have a description field"
        );
    }
}

#[test]
fn all_skills_have_disable_model_invocation() {
    for name in ALL_SKILLS {
        let content = load_skill(name);
        assert!(
            content.contains("disable-model-invocation:"),
            "skills/{name}/SKILL.md must specify disable-model-invocation"
        );
    }
}

#[test]
fn all_skills_have_allowed_tools() {
    for name in ALL_SKILLS {
        let content = load_skill(name);
        assert!(
            content.contains("allowed-tools:"),
            "skills/{name}/SKILL.md must specify allowed-tools"
        );
    }
}

// ---------------------------------------------------------------------------
// Core skills are user-invocable (disable-model-invocation: true)
// ---------------------------------------------------------------------------

#[test]
fn core_skills_are_user_invocable() {
    for name in CORE_SKILLS {
        let content = load_skill(name);
        assert!(
            content.contains("disable-model-invocation: true"),
            "skills/{name}/SKILL.md: core workflow skills must be user-invocable (disable-model-invocation: true)"
        );
    }
}

// ---------------------------------------------------------------------------
// Board skills can be agent-invocable
// ---------------------------------------------------------------------------

#[test]
fn sync_and_status_are_agent_invocable() {
    for name in &["sync", "status"] {
        let content = load_skill(name);
        assert!(
            content.contains("disable-model-invocation: false"),
            "skills/{name}/SKILL.md: board skills should be agent-invocable (disable-model-invocation: false)"
        );
    }
}

// ---------------------------------------------------------------------------
// Skills reference nk commands
// ---------------------------------------------------------------------------

#[test]
fn all_skills_reference_nk_commands() {
    for name in ALL_SKILLS {
        let content = load_skill(name);
        assert!(
            content.contains("nk ") || content.contains("nusy-kanban"),
            "skills/{name}/SKILL.md must reference nk or nusy-kanban commands"
        );
    }
}

#[test]
fn work_skill_claims_item() {
    let content = load_skill("work");
    assert!(
        content.contains("nk move") || content.contains("move"),
        "work skill must claim the item via nk move"
    );
    assert!(
        content.contains("nk show") || content.contains("show"),
        "work skill must read the item via nk show"
    );
}

#[test]
fn done_skill_runs_tests() {
    let content = load_skill("done");
    assert!(
        content.contains("cargo test") || content.contains("pytest"),
        "done skill must run tests before completing"
    );
    assert!(
        content.contains("nk pr create") || content.contains("pr create"),
        "done skill must create a proposal"
    );
}

#[test]
fn status_skill_shows_board() {
    let content = load_skill("status");
    assert!(
        content.contains("nk board") || content.contains("nk stats"),
        "status skill must show board or stats"
    );
}

#[test]
fn handoff_skill_shows_recent_work() {
    let content = load_skill("handoff");
    assert!(
        content.contains("git log"),
        "handoff skill must show recent commits"
    );
}
