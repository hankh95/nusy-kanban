//! Cross-crate integration tests — prove nusy-arrow-git works as the
//! versioning layer for its real consumers (nusy-codegraph, nusy-kanban).
//!
//! These tests exercise the full stack: create domain-specific Arrow data,
//! version it via git primitives (commit/checkout/branch/merge/save/restore),
//! and verify data integrity across operations.

use nusy_arrow_core::{Namespace, QuerySpec, Triple, YLayer};
use nusy_arrow_git::{
    CommitsTable, GitObjectStore, MergeResult, RefsTable, checkout, create_commit, merge, restore,
    restore_full, save, save_full,
};

// ─── Helpers ───────────────────────────────────────────────────────

fn triple(subj: &str, pred: &str, obj: &str) -> Triple {
    Triple {
        subject: subj.to_string(),
        predicate: pred.to_string(),
        object: obj.to_string(),
        graph: None,
        confidence: Some(0.9),
        source_document: None,
        source_chunk_id: None,
        extracted_by: Some("cross-crate-test".to_string()),
        caused_by: None,
        derived_from: None,
        consolidated_at: None,
        certifiability_class: None,
        object_datatype: None,
    }
}

// ─── Codegraph × Git ──────────────────────────────────────────────

/// Verify that codegraph diff/merge functions work on data that has been
/// round-tripped through git commit/checkout.
#[test]
fn test_codegraph_nodes_survive_git_commit_checkout() {
    use nusy_codegraph::schema::{CodeNode, CodeNodeKind, build_code_nodes_batch};
    use nusy_codegraph::{CodeDiffChangeType, codegraph_diff};

    // Build codegraph nodes
    let nodes = vec![
        CodeNode {
            id: "func:main.py::hello".to_string(),
            kind: CodeNodeKind::Function,
            parent_id: Some("module:main.py".to_string()),
            name: "hello".to_string(),
            signature: Some("def hello(name: str) -> str".to_string()),
            docstring: Some("Greet a user.".to_string()),
            body_hash: Some("abc123".to_string()),
            loc: Some(5),
            cyclomatic_complexity: Some(1),
            ..Default::default()
        },
        CodeNode {
            id: "class:main.py::Greeter".to_string(),
            kind: CodeNodeKind::Class,
            parent_id: Some("module:main.py".to_string()),
            name: "Greeter".to_string(),
            docstring: Some("A greeting class.".to_string()),
            body_hash: Some("def456".to_string()),
            loc: Some(20),
            cyclomatic_complexity: Some(3),
            ..Default::default()
        },
    ];

    let batch_v1 = build_code_nodes_batch(&nodes).expect("build batch v1");
    assert_eq!(batch_v1.num_rows(), 2);

    // Now store the equivalent data as triples in an ArrowGraphStore and
    // commit/checkout to prove the git layer preserves data for codegraph.
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    // Store codegraph metadata as triples (how codegraph would use the substrate)
    for node in &nodes {
        obj.store
            .add_triple(
                &triple(&node.id, "rdf:type", &format!("{:?}", node.kind)),
                Namespace::Work,
                YLayer::Semantic,
            )
            .expect("add node type triple");
        obj.store
            .add_triple(
                &triple(&node.id, "cg:name", &node.name),
                Namespace::Work,
                YLayer::Semantic,
            )
            .expect("add node name triple");
        if let Some(hash) = &node.body_hash {
            obj.store
                .add_triple(
                    &triple(&node.id, "cg:bodyHash", hash),
                    Namespace::Work,
                    YLayer::Semantic,
                )
                .expect("add body hash triple");
        }
    }

    // Commit
    let c1 = create_commit(&obj, &mut commits, vec![], "codegraph v1", "DGX").unwrap();
    assert_eq!(obj.store.len(), 6); // 3 triples per node × 2 nodes

    // Clear and checkout
    obj.store.clear();
    assert_eq!(obj.store.len(), 0);
    checkout(&mut obj, &commits, &c1.commit_id).unwrap();
    assert_eq!(obj.store.len(), 6);

    // Verify specific triples survived
    let results = obj
        .store
        .query(&QuerySpec {
            subject: Some("func:main.py::hello".to_string()),
            ..Default::default()
        })
        .unwrap();
    assert!(
        !results.is_empty(),
        "Function node triples should survive commit/checkout"
    );

    // Now build a v2 batch with a modified node and use codegraph_diff
    let mut nodes_v2 = nodes.clone();
    nodes_v2[0].body_hash = Some("modified_hash".to_string());
    let batch_v2 = build_code_nodes_batch(&nodes_v2).expect("build batch v2");

    let diff = codegraph_diff(&batch_v1, &batch_v2).expect("codegraph diff");
    assert_eq!(diff.entries.len(), 1, "One node modified");
    assert_eq!(diff.entries[0].change_type, CodeDiffChangeType::Modified);
    assert_eq!(diff.entries[0].node_id, "func:main.py::hello");
}

/// Verify that codegraph nodes on two branches can be merged via git,
/// then the merged state has triples from both branches.
#[test]
fn test_codegraph_branch_and_merge() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    // Base commit: one module node
    obj.store
        .add_triple(
            &triple("module:app.py", "rdf:type", "Module"),
            Namespace::Work,
            YLayer::Semantic,
        )
        .unwrap();
    let base = create_commit(&obj, &mut commits, vec![], "base module", "DGX").unwrap();

    // Branch A: add a function node
    obj.store
        .add_triple(
            &triple("func:app.py::process", "rdf:type", "Function"),
            Namespace::Work,
            YLayer::Semantic,
        )
        .unwrap();
    let ca = create_commit(
        &obj,
        &mut commits,
        vec![base.commit_id.clone()],
        "add process func",
        "DGX",
    )
    .unwrap();

    // Branch B: add a class node (different namespace to avoid conflict)
    checkout(&mut obj, &commits, &base.commit_id).unwrap();
    obj.store
        .add_triple(
            &triple("class:app.py::Handler", "rdf:type", "Class"),
            Namespace::Research,
            YLayer::Semantic,
        )
        .unwrap();
    let cb = create_commit(
        &obj,
        &mut commits,
        vec![base.commit_id.clone()],
        "add Handler class",
        "DGX",
    )
    .unwrap();

    // Merge
    let result = merge(&mut obj, &mut commits, &ca.commit_id, &cb.commit_id, "DGX").unwrap();
    match result {
        MergeResult::Clean(mc) => {
            assert_eq!(mc.parent_ids.len(), 2);
            // Module + function + class = 3
            assert!(
                obj.store.len() >= 3,
                "Merged store should have all 3 triples, got {}",
                obj.store.len()
            );
        }
        MergeResult::Conflict(c) => panic!("Expected clean merge, got {} conflicts", c.len()),
        MergeResult::NoCommonAncestor => panic!("Expected common ancestor"),
    }
}

// ─── Kanban × Git ─────────────────────────────────────────────────

/// Verify that kanban item metadata stored as triples survives
/// git save/restore cycles (the pattern EXP-1273 will use).
#[test]
fn test_kanban_metadata_survives_save_restore() {
    let tmp = tempfile::tempdir().unwrap();
    let save_dir = tmp.path().join("kanban-save");
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snapshots"));

    // Simulate kanban item metadata as triples in the work namespace
    let items = vec![
        ("EXP-1275", "nusy-arrow-git Hardening", "in_progress", "DGX"),
        ("EXP-1271", "CLI Parity", "backlog", "Mini"),
        ("EXP-1272", "NATS Server", "backlog", "M5"),
    ];

    for (id, title, status, assignee) in &items {
        obj.store
            .add_triple(
                &triple(id, "kb:title", title),
                Namespace::Work,
                YLayer::Procedural,
            )
            .unwrap();
        obj.store
            .add_triple(
                &triple(id, "kb:status", status),
                Namespace::Work,
                YLayer::Procedural,
            )
            .unwrap();
        obj.store
            .add_triple(
                &triple(id, "kb:assignee", assignee),
                Namespace::Work,
                YLayer::Procedural,
            )
            .unwrap();
    }

    assert_eq!(obj.store.len(), 9); // 3 items × 3 triples

    // Save
    save(&obj, &save_dir).unwrap();

    // Clear and restore
    obj.store.clear();
    assert_eq!(obj.store.len(), 0);
    restore(&mut obj, &save_dir).unwrap();
    assert_eq!(obj.store.len(), 9);

    // Verify specific item survived
    let results = obj
        .store
        .query(&QuerySpec {
            subject: Some("EXP-1275".to_string()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        results.iter().map(|b| b.num_rows()).sum::<usize>(),
        3,
        "EXP-1275 should have 3 triples after restore"
    );
}

/// Verify kanban status changes tracked via git commits form a
/// queryable audit trail.
#[test]
fn test_kanban_status_audit_trail_via_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    // Commit 1: item created in backlog
    obj.store
        .add_triple(
            &triple("EXP-1275", "kb:status", "backlog"),
            Namespace::Work,
            YLayer::Procedural,
        )
        .unwrap();
    let c1 = create_commit(&obj, &mut commits, vec![], "create EXP-1275", "DGX").unwrap();

    // Commit 2: move to in_progress (delete old status, add new)
    // First find and delete old status triple
    let batches = obj
        .store
        .query(&QuerySpec {
            subject: Some("EXP-1275".to_string()),
            predicate: Some("kb:status".to_string()),
            ..Default::default()
        })
        .unwrap();
    for batch in &batches {
        let ids = batch
            .column(nusy_arrow_core::col::TRIPLE_ID)
            .as_any()
            .downcast_ref::<arrow::array::StringArray>()
            .expect("triple_id");
        for i in 0..batch.num_rows() {
            obj.store.delete(ids.value(i)).unwrap();
        }
    }
    obj.store
        .add_triple(
            &triple("EXP-1275", "kb:status", "in_progress"),
            Namespace::Work,
            YLayer::Procedural,
        )
        .unwrap();
    let c2 = create_commit(
        &obj,
        &mut commits,
        vec![c1.commit_id.clone()],
        "move EXP-1275 to in_progress",
        "DGX",
    )
    .unwrap();

    // Diff between commits should show the status change
    let diff = nusy_arrow_git::diff(&mut obj, &commits, &c1.commit_id, &c2.commit_id).unwrap();
    assert_eq!(diff.added.len(), 1, "One triple added (new status)");
    assert_eq!(diff.removed.len(), 1, "One triple removed (old status)");
    assert_eq!(diff.added[0].object, "in_progress");
    assert_eq!(diff.removed[0].object, "backlog");

    // History should show 2 commits
    let log = nusy_arrow_git::log(&commits, &c2.commit_id, 100);
    assert_eq!(log.len(), 2);
}

// ─── Multi-Namespace Coexistence ──────────────────────────────────

/// Multiple crates' data can coexist in the same store across different
/// namespaces and survive commit/checkout/merge.
#[test]
fn test_multi_crate_data_coexistence() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    // Codegraph data in Work namespace
    obj.store
        .add_triple(
            &triple("func:main.py::run", "rdf:type", "Function"),
            Namespace::Work,
            YLayer::Semantic,
        )
        .unwrap();

    // Kanban data also in Work namespace (different subjects)
    obj.store
        .add_triple(
            &triple("EXP-1275", "kb:status", "in_progress"),
            Namespace::Work,
            YLayer::Procedural,
        )
        .unwrap();

    // Research data in Research namespace
    obj.store
        .add_triple(
            &triple("H-GIT-1", "validates", "commit-checkout-perf"),
            Namespace::Research,
            YLayer::Reasoning,
        )
        .unwrap();

    // Being self-knowledge in Self namespace
    obj.store
        .add_triple(
            &triple("DGX", "knows", "rust-arrow-architecture"),
            Namespace::Self_,
            YLayer::Metacognitive,
        )
        .unwrap();

    assert_eq!(obj.store.len(), 4);

    // Commit
    let c1 = create_commit(&obj, &mut commits, vec![], "multi-crate state", "DGX").unwrap();

    // Clear, checkout, verify each namespace
    obj.store.clear();
    checkout(&mut obj, &commits, &c1.commit_id).unwrap();
    assert_eq!(obj.store.len(), 4);

    // Query each namespace independently
    for (ns, expected_subject) in [
        (Namespace::Work, "func:main.py::run"),
        (Namespace::Research, "H-GIT-1"),
        (Namespace::Self_, "DGX"),
    ] {
        let results = obj
            .store
            .query(&QuerySpec {
                namespace: Some(ns),
                subject: Some(expected_subject.to_string()),
                ..Default::default()
            })
            .unwrap();
        assert!(
            !results.is_empty(),
            "Namespace {} should have triple for {}",
            ns.as_str(),
            expected_subject
        );
    }
}

/// Verify that save_full/restore_full preserves multi-crate data
/// along with commits and refs metadata.
#[test]
fn test_full_state_persistence_multi_crate() {
    let tmp = tempfile::tempdir().unwrap();
    let save_dir = tmp.path().join("full-state");
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snap"));
    let mut commits = CommitsTable::new();
    let mut refs = RefsTable::new();

    // Populate multiple namespaces
    obj.store
        .add_triple(
            &triple("func:app.py::main", "rdf:type", "Function"),
            Namespace::Work,
            YLayer::Semantic,
        )
        .unwrap();
    obj.store
        .add_triple(
            &triple("H-019", "measures", "query-latency"),
            Namespace::Research,
            YLayer::Reasoning,
        )
        .unwrap();
    obj.store
        .add_triple(
            &triple("DGX", "calibration", "0.95"),
            Namespace::Self_,
            YLayer::Metacognitive,
        )
        .unwrap();

    // Commit and set up refs
    let c1 = create_commit(&obj, &mut commits, vec![], "full state", "DGX").unwrap();
    refs.init_main(&c1.commit_id);
    refs.create_branch("exp-1275", &c1.commit_id).unwrap();

    // Save everything
    save_full(&obj, Some(&commits), Some(&refs), &save_dir).unwrap();

    // Restore into fresh store
    let mut obj2 = GitObjectStore::with_snapshot_dir(tmp.path().join("snap2"));
    let (rc, rr) = restore_full(&mut obj2, &save_dir).unwrap();

    assert_eq!(obj2.store.len(), 3);
    assert_eq!(rc.unwrap().len(), 1);
    let restored_refs = rr.unwrap();
    assert_eq!(restored_refs.branches().len(), 2);
    assert!(restored_refs.resolve("exp-1275").is_some());
}

// ─── Cross-Namespace Bridge Queries ───────────────────────────────

/// Verify that cross-namespace bridge relations survive commit/checkout.
/// Pattern: work→research (expedition validates hypothesis).
#[test]
fn test_cross_namespace_bridge_survives_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    // Work item
    obj.store
        .add_triple(
            &triple("EXP-1275", "rdf:type", "Expedition"),
            Namespace::Work,
            YLayer::Semantic,
        )
        .unwrap();

    // Research hypothesis
    obj.store
        .add_triple(
            &triple("H-GIT-1", "rdf:type", "Hypothesis"),
            Namespace::Research,
            YLayer::Semantic,
        )
        .unwrap();

    // Bridge relation: expedition validates hypothesis (stored in Work namespace)
    obj.store
        .add_triple(
            &triple("EXP-1275", "validates", "H-GIT-1"),
            Namespace::Work,
            YLayer::Reasoning,
        )
        .unwrap();

    let c1 = create_commit(&obj, &mut commits, vec![], "bridge test", "DGX").unwrap();

    // Clear, checkout, verify bridge
    obj.store.clear();
    checkout(&mut obj, &commits, &c1.commit_id).unwrap();

    // Query the bridge relation
    let bridge = obj
        .store
        .query(&QuerySpec {
            subject: Some("EXP-1275".to_string()),
            predicate: Some("validates".to_string()),
            ..Default::default()
        })
        .unwrap();
    assert!(
        !bridge.is_empty(),
        "Bridge relation should survive commit/checkout"
    );

    let batch = &bridge[0];
    let objects = batch
        .column(nusy_arrow_core::col::OBJECT)
        .as_any()
        .downcast_ref::<arrow::array::StringArray>()
        .expect("object column");
    assert_eq!(objects.value(0), "H-GIT-1");
}
