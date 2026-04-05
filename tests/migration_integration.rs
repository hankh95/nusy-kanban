//! Integration tests for migration — validates against real-world kanban file structures.
//!
//! These tests use synthetic directory trees that mirror the NuSy project's
//! actual file layout and content patterns.

use nusy_kanban::config::BoardConfig;
use nusy_kanban::migrate::{
    MigrateResult, extract_relations, migrate_board, migrate_boards, parse_markdown_file,
    parse_turtle_blocks,
};
use std::collections::HashMap;
use std::path::Path;

/// Helper: create a realistic development board config.
fn dev_board_config() -> BoardConfig {
    BoardConfig {
        name: "development".to_string(),
        preset: "nautical".to_string(),
        path: "kanban-work/".to_string(),
        scan_paths: vec![
            "kanban-work/expeditions/".to_string(),
            "kanban-work/voyages/".to_string(),
            "kanban-work/chores/".to_string(),
        ],
        ignore: vec!["**/archive/**".to_string(), "**/templates/**".to_string()],
        wip_exempt_types: vec!["voyage".to_string()],
        wip_limits: HashMap::from([
            ("provisioning".to_string(), 50),
            ("underway".to_string(), 4),
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

/// Helper: create a research board config.
fn research_board_config() -> BoardConfig {
    BoardConfig {
        name: "research".to_string(),
        preset: "hdd".to_string(),
        path: "research/".to_string(),
        scan_paths: vec![
            "research/hypotheses/".to_string(),
            "research/experiments/".to_string(),
            "research/papers/".to_string(),
            "research/measures/".to_string(),
        ],
        ignore: vec!["**/archive/**".to_string()],
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

/// Create a realistic project tree with multiple item types and verify migration counts.
#[test]
fn test_full_dual_board_migration() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = dir.path();

    // --- Development board ---
    let exp_dir = root.join("kanban-work/expeditions");
    let voy_dir = root.join("kanban-work/voyages");
    let chore_dir = root.join("kanban-work/chores");
    let archive_dir = exp_dir.join("archive");
    std::fs::create_dir_all(&exp_dir).unwrap();
    std::fs::create_dir_all(&voy_dir).unwrap();
    std::fs::create_dir_all(&chore_dir).unwrap();
    std::fs::create_dir_all(&archive_dir).unwrap();

    // Expeditions
    for i in 1..=5 {
        let status = if i <= 3 { "done" } else { "in_progress" };
        std::fs::write(
            exp_dir.join(format!("EXP-{i}-Test.md")),
            format!(
                r#"---
id: EXP-{i}
title: "Expedition {i}"
type: expedition
status: {status}
priority: medium
created: 2026-01-0{i}
tags: [v14]
related: []
depends_on: []
---

# EXP-{i}: Expedition {i}

Body content for expedition {i}.
"#
            ),
        )
        .unwrap();
    }

    // Add a turtle block to EXP-1
    std::fs::write(
        exp_dir.join("EXP-1-Test.md"),
        r#"---
id: EXP-1
title: "Expedition 1"
type: expedition
status: done
priority: medium
created: 2026-01-01
tags: [v14]
related: [EXP-2]
depends_on: []
---

# EXP-1: Expedition 1

Body.

```yurtle
@prefix kb: <https://yurtle.dev/kanban/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

<> kb:statusChange [
    kb:status kb:in_progress ;
    kb:at "2026-01-05T10:00:00"^^xsd:dateTime ;
    kb:by "DGX" ;
  ],
  [
    kb:status kb:done ;
    kb:at "2026-01-10T15:30:00"^^xsd:dateTime ;
    kb:by "DGX" ;
  ] .
```
"#,
    )
    .unwrap();

    // Archived expedition (should be ignored)
    std::fs::write(
        archive_dir.join("EXP-old.md"),
        "---\nid: EXP-old\ntitle: Old\ntype: expedition\nstatus: done\n---\nBody\n",
    )
    .unwrap();

    // Voyage
    std::fs::write(
        voy_dir.join("VOY-1-Campaign.md"),
        r#"---
id: VOY-1
title: "Test Campaign"
type: voyage
status: in_progress
priority: high
created: 2026-01-01
tags: [v14]
related: [EXP-1, EXP-2, EXP-3]
depends_on: []
---

# VOY-1: Test Campaign
"#,
    )
    .unwrap();

    // Chore
    std::fs::write(
        chore_dir.join("CHORE-1-Cleanup.md"),
        r#"---
id: CHORE-1
title: "Cleanup task"
type: chore
status: done
created: 2026-02-01
tags: []
related: []
depends_on: []
---

# CHORE-1: Cleanup
"#,
    )
    .unwrap();

    // --- Research board ---
    let hyp_dir = root.join("research/hypotheses");
    let expr_dir = root.join("research/experiments");
    let paper_dir = root.join("research/papers");
    let measure_dir = root.join("research/measures");
    std::fs::create_dir_all(&hyp_dir).unwrap();
    std::fs::create_dir_all(&expr_dir).unwrap();
    std::fs::create_dir_all(&paper_dir).unwrap();
    std::fs::create_dir_all(&measure_dir).unwrap();

    std::fs::write(
        paper_dir.join("PAPER-130-Signal-Fusion.md"),
        r#"---
id: PAPER-130
title: "Signal Fusion Paper"
type: paper
status: active
created: 2026-01-01
tags: [v12]
related: []
depends_on: []
---

# PAPER-130
"#,
    )
    .unwrap();

    std::fs::write(
        hyp_dir.join("H130.1-Test-Hyp.md"),
        r#"---
id: H130.1
title: "Test Hypothesis"
type: hypothesis
status: active
created: 2026-01-02
tags: [v12]
related: [PAPER-130]
depends_on: []
---

# H130.1
"#,
    )
    .unwrap();

    std::fs::write(
        expr_dir.join("EXPR-130.1-Test-Expr.md"),
        r#"---
id: EXPR-130.1
title: "Test Experiment"
type: experiment
status: active
created: 2026-01-03
tags: [v12]
related: [H130.1]
depends_on: []
---

# EXPR-130.1
"#,
    )
    .unwrap();

    std::fs::write(
        measure_dir.join("M-001-Latency.md"),
        r#"---
id: M-001
title: "Latency Metric"
type: measure
status: active
created: 2026-01-04
tags: []
related: [EXPR-130.1]
depends_on: []
---

# M-001
"#,
    )
    .unwrap();

    // Write config
    let config_dir = root.join(".yurtle-kanban");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.yaml"),
        r#"version: "2.0"
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
    wip_exempt_types: [voyage]
    wip_limits:
      underway: 4
    states: [backlog, in_progress, done]
  - name: research
    preset: hdd
    path: research/
    scan_paths:
      - "research/hypotheses/"
      - "research/experiments/"
      - "research/papers/"
      - "research/measures/"
    ignore: []
    wip_limits:
      active: 5
    states: [draft, active, complete]
namespace: "https://nusy.dev/"
default_board: development
"#,
    )
    .unwrap();

    // --- Run full migration ---
    let config = nusy_kanban::config::ConfigFile::from_path(&config_dir.join("config.yaml"))
        .expect("load config");
    let result = migrate_boards(root, &config).expect("migrate");

    // Verify counts
    assert_eq!(
        result.items.len(),
        11,
        "5 expeditions + 1 voyage + 1 chore + 1 paper + 1 hyp + 1 expr + 1 measure = 11 (archived excluded)"
    );

    // Verify dev items
    let dev_items: Vec<_> = result
        .items
        .iter()
        .filter(|i| i.board == "development")
        .collect();
    assert_eq!(dev_items.len(), 7); // 5 exp + 1 voy + 1 chore

    // Verify research items
    let research_items: Vec<_> = result
        .items
        .iter()
        .filter(|i| i.board == "research")
        .collect();
    assert_eq!(research_items.len(), 4); // paper + hyp + expr + measure

    // Verify runs (turtle blocks) — EXP-1 has 2 status changes
    assert_eq!(result.runs.len(), 2);
    assert!(result.runs.iter().all(|r| r.item_id == "EXP-1"));

    // Verify relations
    assert!(result.relations.len() >= 5); // related + depends_on from frontmatter

    // Verify archived file was excluded
    assert!(
        !result.items.iter().any(|i| i.id == "EXP-old"),
        "Archived items should be excluded"
    );

    // Verify errors are empty
    assert!(
        result.errors.is_empty(),
        "Should have no errors: {:?}",
        result.errors
    );

    // Verify batch building works
    let items_batch = result.items_batch().expect("items batch");
    assert_eq!(items_batch.num_rows(), 11);
    assert_eq!(items_batch.num_columns(), 18);

    let runs_batch = result.runs_batch().expect("runs batch");
    assert_eq!(runs_batch.num_rows(), 2);

    let rel_batch = result.relations_batch().expect("relations batch");
    assert!(rel_batch.num_rows() >= 5);

    // Verify into_stores works
    let (store, rel_store) = result.into_stores().expect("into stores");
    assert_eq!(store.active_item_count(), 11);
    assert!(rel_store.active_count() >= 5);

    // Verify summary output
    let summary = result.summary();
    assert!(summary.contains("Items migrated: 11"));
    assert!(summary.contains("Development board: 7"));
    assert!(summary.contains("Research board: 4"));
}

/// Verify that files without frontmatter are gracefully skipped.
#[test]
fn test_migration_handles_non_kanban_files() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let exp_dir = dir.path().join("kanban-work/expeditions");
    std::fs::create_dir_all(&exp_dir).unwrap();

    // Valid kanban file
    std::fs::write(
        exp_dir.join("EXP-1-Good.md"),
        "---\nid: EXP-1\ntitle: Good\ntype: expedition\nstatus: backlog\n---\nBody\n",
    )
    .unwrap();

    // README-style file (no frontmatter)
    std::fs::write(
        exp_dir.join("README.md"),
        "# Expedition Index\n\nThis is a readme.\n",
    )
    .unwrap();

    // Python init file (not .md, should be ignored)
    std::fs::write(exp_dir.join("__init__.py"), "").unwrap();

    let board = dev_board_config();
    let result = migrate_board(dir.path(), &board).expect("migrate");

    // Only the valid kanban file should be parsed
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].id, "EXP-1");

    // README.md should produce an error (no frontmatter)
    assert_eq!(result.errors.len(), 1);
}

/// Verify that spot-checking individual fields preserves all data.
#[test]
fn test_field_preservation_spot_check() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let exp_dir = dir.path().join("kanban-work/expeditions");
    std::fs::create_dir_all(&exp_dir).unwrap();

    std::fs::write(
        exp_dir.join("EXP-42-Arrow.md"),
        r#"---
id: EXP-42
title: "Arrow-Kanban Migration — 959 Items to Arrow + Strangler Activation"
type: expedition
status: in_progress
priority: high
created: 2026-03-14
assignee: DGX
tags: [v14, arrow, rust, kanban, migration]
related: [VOY-145, EXP-1258, EXP-1259]
depends_on: [EXP-1258, EXP-1259]
---

# EXP-42: Arrow-Kanban Migration

Complex body with multiple sections.
"#,
    )
    .unwrap();

    let (item, _content) =
        parse_markdown_file(&exp_dir.join("EXP-42-Arrow.md"), "development").expect("parse");

    assert_eq!(item.id, "EXP-42");
    assert_eq!(
        item.title,
        "Arrow-Kanban Migration — 959 Items to Arrow + Strangler Activation"
    );
    assert_eq!(item.status, "in_progress");
    assert_eq!(item.priority.as_deref(), Some("high"));
    assert_eq!(item.created.as_deref(), Some("2026-03-14"));
    assert_eq!(item.assignee.as_deref(), Some("DGX"));
    assert_eq!(item.tags.len(), 5);
    assert!(item.tags.contains(&"v14".to_string()));
    assert!(item.tags.contains(&"migration".to_string()));
    assert_eq!(item.related.len(), 3);
    assert_eq!(item.depends_on.len(), 2);
    assert!(!item.body_hash.is_empty());
    assert_eq!(item.board, "development");
}

/// Verify Parquet round-trip: migrate → save → load → verify counts match.
#[test]
fn test_parquet_round_trip() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let exp_dir = dir.path().join("kanban-work/expeditions");
    std::fs::create_dir_all(&exp_dir).unwrap();

    for i in 1..=10 {
        std::fs::write(
            exp_dir.join(format!("EXP-{i}-Test.md")),
            format!(
                "---\nid: EXP-{i}\ntitle: Exp {i}\ntype: expedition\nstatus: backlog\ncreated: 2026-01-01\ntags: []\nrelated: []\ndepends_on: []\n---\nBody {i}\n"
            ),
        )
        .unwrap();
    }

    let board = BoardConfig {
        name: "development".to_string(),
        preset: "nautical".to_string(),
        path: "kanban-work/".to_string(),
        scan_paths: vec!["kanban-work/expeditions/".to_string()],
        ignore: vec![],
        wip_exempt_types: vec![],
        wip_limits: HashMap::new(),
        states: vec!["backlog".to_string(), "done".to_string()],
        phases: vec![],
        type_states: HashMap::new(),
    };

    let result = migrate_board(dir.path(), &board).expect("migrate");
    assert_eq!(result.items.len(), 10);

    // Save to Parquet
    let (store, _) = result.into_stores().expect("into stores");
    nusy_kanban::persist::save_store(dir.path(), &store).expect("save");

    // Load back
    let loaded = nusy_kanban::persist::load_store(dir.path()).expect("load");
    assert_eq!(loaded.active_item_count(), 10);

    // Verify a specific item
    let item = loaded.get_item("EXP-5").expect("get EXP-5");
    assert_eq!(item.num_rows(), 1);
}
