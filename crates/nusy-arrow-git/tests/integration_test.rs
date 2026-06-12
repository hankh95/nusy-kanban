//! End-to-end integration tests for nusy-arrow-git.
//!
//! Exercises the full EXP-1251 + EXP-1252 stack together:
//! - Create a graph with all 4 namespaces
//! - Populate each with triples across Y-layers
//! - Commit, branch, modify, merge
//! - Query across namespaces via bridges
//! - Parquet durability (save/restore)
//! - Provenance preservation through commit/checkout/merge cycles
//! - Performance benchmark: 10K triples full commit cycle

use arrow::array::Array;
use nusy_arrow_core::{Namespace, QuerySpec, Triple, YLayer, col};
use nusy_arrow_git::{
    CommitsTable, GitObjectStore, MergeResult, RefsTable, checkout, create_commit, merge,
};

fn triple(subj: &str, pred: &str, obj: &str) -> Triple {
    Triple {
        subject: subj.to_string(),
        predicate: pred.to_string(),
        object: obj.to_string(),
        graph: None,
        confidence: Some(0.9),
        source_document: None,
        source_chunk_id: None,
        extracted_by: Some("integration-test".to_string()),
        caused_by: None,
        derived_from: None,
        consolidated_at: None,
        certifiability_class: None,
        object_datatype: None,
    }
}

fn triple_with_provenance(
    subj: &str,
    pred: &str,
    obj: &str,
    caused_by: Option<&str>,
    derived_from: Option<&str>,
) -> Triple {
    Triple {
        caused_by: caused_by.map(String::from),
        derived_from: derived_from.map(String::from),
        consolidated_at: Some(chrono::Utc::now().timestamp_millis()),
        certifiability_class: None,
        object_datatype: None,
        ..triple(subj, pred, obj)
    }
}

/// Populate store with triples across all 4 namespaces × 7 Y-layers.
fn populate_full(store: &mut nusy_arrow_core::ArrowGraphStore, per_partition: usize) {
    for ns in Namespace::ALL {
        for layer in YLayer::ALL {
            let triples: Vec<Triple> = (0..per_partition)
                .map(|i| {
                    triple(
                        &format!("{}:{}-{}", ns.as_str(), layer.name(), i),
                        "rdf:type",
                        &format!("{}-Entity", layer.name()),
                    )
                })
                .collect();
            store.add_batch(&triples, ns, layer).unwrap();
        }
    }
}

#[test]
fn test_full_commit_branch_merge_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    // 1. Populate all namespaces × 7 Y-layers
    populate_full(&mut obj.store, 10);
    assert_eq!(obj.store.len(), Namespace::ALL.len() * 7 * 10);

    // 2. Initial commit
    let base_count = Namespace::ALL.len() * 7 * 10;
    let c0 = create_commit(&obj, &mut commits, vec![], "initial triples", "DGX").unwrap();

    // 3. Branch A: add research triples
    obj.store
        .add_triple(
            &triple("research:finding-a", "validates", "H-019"),
            Namespace::Research,
            YLayer::Reasoning,
        )
        .unwrap();
    let ca = create_commit(
        &obj,
        &mut commits,
        vec![c0.commit_id.clone()],
        "branch-a: research finding",
        "DGX",
    )
    .unwrap();

    // 4. Checkout base, Branch B: add work triples
    checkout(&mut obj, &commits, &c0.commit_id).unwrap();
    assert_eq!(obj.store.len(), base_count);

    obj.store
        .add_triple(
            &triple("work:task-b", "kb:status", "done"),
            Namespace::Work,
            YLayer::Procedural,
        )
        .unwrap();
    let cb = create_commit(
        &obj,
        &mut commits,
        vec![c0.commit_id.clone()],
        "branch-b: work task",
        "DGX",
    )
    .unwrap();

    // 5. Merge (non-conflicting — different namespaces)
    let result = merge(&mut obj, &mut commits, &ca.commit_id, &cb.commit_id, "DGX").unwrap();

    match result {
        MergeResult::Clean(mc) => {
            assert_eq!(mc.parent_ids.len(), 2);
            // After merge: base + 1 research + 1 work = base + 2
            assert!(
                obj.store.len() >= base_count + 2,
                "Expected >= {} triples after merge, got {}",
                base_count + 2,
                obj.store.len()
            );
        }
        MergeResult::Conflict(c) => panic!("Expected clean merge, got {} conflicts", c.len()),
        MergeResult::NoCommonAncestor => panic!("Expected common ancestor"),
    }
}

#[test]
fn test_conflict_detection_same_namespace() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    // Base commit
    obj.store
        .add_triple(
            &triple("entity", "rdf:type", "Base"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();
    let c0 = create_commit(&obj, &mut commits, vec![], "base", "DGX").unwrap();

    // Branch A: set entity status to "active"
    obj.store
        .add_triple(
            &triple("entity", "status", "active"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();
    let ca = create_commit(
        &obj,
        &mut commits,
        vec![c0.commit_id.clone()],
        "a: active",
        "DGX",
    )
    .unwrap();

    // Branch B: set entity status to "deprecated"
    checkout(&mut obj, &commits, &c0.commit_id).unwrap();
    obj.store
        .add_triple(
            &triple("entity", "status", "deprecated"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();
    let cb = create_commit(
        &obj,
        &mut commits,
        vec![c0.commit_id.clone()],
        "b: deprecated",
        "DGX",
    )
    .unwrap();

    // Merge should detect conflict
    let result = merge(&mut obj, &mut commits, &ca.commit_id, &cb.commit_id, "DGX").unwrap();

    match result {
        MergeResult::Conflict(conflicts) => {
            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].subject, "entity");
            assert_eq!(conflicts[0].predicate, "status");
            assert_eq!(conflicts[0].object_a, "active");
            assert_eq!(conflicts[0].object_b, "deprecated");
        }
        _ => panic!("Expected conflict"),
    }
}

#[test]
fn test_provenance_survives_commit_checkout() {
    use arrow::array::{StringArray, TimestampMillisecondArray};

    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();
    let now_ms = chrono::Utc::now().timestamp_millis();

    // Add triple with full provenance
    let t = Triple {
        subject: "s1".to_string(),
        predicate: "p1".to_string(),
        object: "o1".to_string(),
        graph: Some("prov-test".to_string()),
        confidence: Some(0.99),
        source_document: Some("source.md".to_string()),
        source_chunk_id: None,
        extracted_by: Some("DGX".to_string()),
        caused_by: Some("t-cause".to_string()),
        derived_from: Some("t-origin".to_string()),
        consolidated_at: Some(now_ms),
        certifiability_class: None,
        object_datatype: None,
    };
    obj.store
        .add_triple(&t, Namespace::World, YLayer::Reasoning)
        .unwrap();

    // Commit
    let c1 = create_commit(&obj, &mut commits, vec![], "with provenance", "DGX").unwrap();

    // Clear and checkout
    obj.store.clear();
    assert_eq!(obj.store.len(), 0);
    checkout(&mut obj, &commits, &c1.commit_id).unwrap();
    assert_eq!(obj.store.len(), 1);

    // Verify all provenance columns survived
    let batches = obj
        .store
        .query(&QuerySpec {
            subject: Some("s1".to_string()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];

    let caused = batch
        .column(col::CAUSED_BY)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(
        caused.value(0),
        "t-cause",
        "caused_by should survive commit/checkout"
    );

    let derived = batch
        .column(col::DERIVED_FROM)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(
        derived.value(0),
        "t-origin",
        "derived_from should survive commit/checkout"
    );

    let consolidated = batch
        .column(col::CONSOLIDATED_AT)
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
        .unwrap();
    assert!(!consolidated.is_null(0));
    assert_eq!(
        consolidated.value(0),
        now_ms,
        "consolidated_at should survive commit/checkout"
    );

    let source = batch
        .column(col::SOURCE_DOCUMENT)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(source.value(0), "source.md");
}

#[test]
fn test_provenance_survives_merge() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    // Base commit
    obj.store
        .add_triple(
            &triple("base", "rdf:type", "Base"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();
    let c0 = create_commit(&obj, &mut commits, vec![], "base", "DGX").unwrap();

    // Branch A: add triple with causal provenance
    let t_a = triple_with_provenance(
        "research:finding",
        "validates",
        "H-019",
        Some("t-root"),
        None,
    );
    obj.store
        .add_triple(&t_a, Namespace::Research, YLayer::Reasoning)
        .unwrap();
    let ca = create_commit(
        &obj,
        &mut commits,
        vec![c0.commit_id.clone()],
        "a: finding",
        "DGX",
    )
    .unwrap();

    // Branch B: add different triple (no conflict)
    checkout(&mut obj, &commits, &c0.commit_id).unwrap();
    let t_b = triple_with_provenance("work:task", "status", "done", None, Some("t-derived"));
    obj.store
        .add_triple(&t_b, Namespace::Work, YLayer::Procedural)
        .unwrap();
    let cb = create_commit(
        &obj,
        &mut commits,
        vec![c0.commit_id.clone()],
        "b: task",
        "DGX",
    )
    .unwrap();

    // Merge
    let result = merge(&mut obj, &mut commits, &ca.commit_id, &cb.commit_id, "DGX").unwrap();
    assert!(
        matches!(result, MergeResult::Clean(_)),
        "Expected clean merge"
    );

    // Verify provenance on branch A's triple survives merge
    let findings = obj
        .store
        .query(&QuerySpec {
            subject: Some("research:finding".to_string()),
            ..Default::default()
        })
        .unwrap();
    assert!(
        !findings.is_empty(),
        "Branch A's triple should exist after merge"
    );
    let batch = &findings[0];
    use arrow::array::StringArray;
    let caused = batch
        .column(col::CAUSED_BY)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(caused.value(0), "t-root", "caused_by should survive merge");
}

#[test]
fn test_causal_chain_survives_commit_checkout() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    // Create a causal chain
    let id0 = obj
        .store
        .add_triple(
            &triple("root", "rdf:type", "Origin"),
            Namespace::World,
            YLayer::Prose,
        )
        .unwrap();

    let t1 = Triple {
        caused_by: Some(id0.clone()),
        ..triple("derived", "rdf:type", "Conclusion")
    };
    let id1 = obj
        .store
        .add_triple(&t1, Namespace::Research, YLayer::Reasoning)
        .unwrap();

    // Commit
    let c1 = create_commit(&obj, &mut commits, vec![], "causal chain", "DGX").unwrap();

    // Clear, checkout, verify chain
    obj.store.clear();
    checkout(&mut obj, &commits, &c1.commit_id).unwrap();

    let chain = obj.store.causal_chain(&id1);
    assert_eq!(
        chain.len(),
        2,
        "Causal chain should survive commit/checkout"
    );
    assert_eq!(chain[0].triple_id, id1);
    assert_eq!(chain[0].caused_by, Some(id0.clone()));
    assert_eq!(chain[1].triple_id, id0);
}

#[test]
fn test_save_restore_with_provenance() {
    use nusy_arrow_git::{restore, save};

    let tmp = tempfile::tempdir().unwrap();
    let save_dir = tmp.path().join("savepoint");
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snapshots"));
    let now_ms = chrono::Utc::now().timestamp_millis();

    // Add triples with provenance to multiple namespaces
    let t = Triple {
        subject: "s1".to_string(),
        predicate: "p1".to_string(),
        object: "o1".to_string(),
        graph: None,
        confidence: Some(0.95),
        source_document: Some("doc.md".to_string()),
        source_chunk_id: None,
        extracted_by: Some("DGX".to_string()),
        caused_by: Some("cause-id".to_string()),
        derived_from: Some("derive-id".to_string()),
        consolidated_at: Some(now_ms),
        certifiability_class: None,
        object_datatype: None,
    };
    obj.store
        .add_triple(&t, Namespace::World, YLayer::Semantic)
        .unwrap();

    save(&obj, &save_dir).unwrap();

    // Clear and restore
    obj.store.clear();
    restore(&mut obj, &save_dir).unwrap();

    assert_eq!(obj.store.len(), 1);

    // Verify provenance
    let batches = obj
        .store
        .query(&QuerySpec {
            subject: Some("s1".to_string()),
            ..Default::default()
        })
        .unwrap();
    let batch = &batches[0];
    use arrow::array::StringArray;
    let caused = batch
        .column(col::CAUSED_BY)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(caused.value(0), "cause-id");
}

#[test]
fn test_refs_branch_management() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();
    let mut refs = RefsTable::new();

    // Initial commit
    obj.store
        .add_triple(
            &triple("s1", "rdf:type", "Thing"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();
    let c0 = create_commit(&obj, &mut commits, vec![], "init", "DGX").unwrap();
    refs.init_main(&c0.commit_id);

    // Create feature branch
    refs.create_branch("feature", &c0.commit_id).unwrap();
    refs.switch_head("feature").unwrap();
    assert_eq!(refs.head().unwrap().ref_name, "feature");
    assert_eq!(refs.resolve("feature"), Some(c0.commit_id.as_str()));

    // Add work on feature branch
    obj.store
        .add_triple(
            &triple("s2", "rdf:type", "Feature"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();
    let c1 = create_commit(
        &obj,
        &mut commits,
        vec![c0.commit_id.clone()],
        "feature work",
        "DGX",
    )
    .unwrap();
    refs.update_ref("feature", &c1.commit_id).unwrap();

    // Switch back to main
    refs.switch_head("main").unwrap();
    assert_eq!(refs.head().unwrap().ref_name, "main");
    assert_eq!(refs.resolve("main"), Some(c0.commit_id.as_str()));

    // Branches list
    assert_eq!(refs.branches().len(), 2);
}

#[test]
fn test_full_save_restore_with_commits_and_refs() {
    use nusy_arrow_git::{restore_full, save_full};

    let tmp = tempfile::tempdir().unwrap();
    let save_dir = tmp.path().join("full-save");
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snapshots"));
    let mut commits = CommitsTable::new();
    let mut refs = RefsTable::new();

    // Build state
    populate_full(&mut obj.store, 5);
    let c0 = create_commit(&obj, &mut commits, vec![], "full state", "DGX").unwrap();
    refs.init_main(&c0.commit_id);
    refs.create_branch("dev", &c0.commit_id).unwrap();

    // Save everything
    save_full(&obj, Some(&commits), Some(&refs), &save_dir).unwrap();

    // Restore into new store
    let mut obj2 = GitObjectStore::with_snapshot_dir(tmp.path().join("snapshots2"));
    let (rc, rr) = restore_full(&mut obj2, &save_dir).unwrap();

    assert_eq!(obj2.store.len(), Namespace::ALL.len() * 7 * 5);
    assert_eq!(rc.unwrap().len(), 1);
    assert_eq!(rr.unwrap().branches().len(), 2);
}

#[test]
fn test_10k_triples_full_commit_cycle_benchmark() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    // Populate 10K triples across 4 namespaces with varied Y-layer distribution
    let layers = [
        (YLayer::Prose, 500),
        (YLayer::Semantic, 800),
        (YLayer::Reasoning, 300),
        (YLayer::Experience, 400),
        (YLayer::Journal, 200),
        (YLayer::Procedural, 200),
        (YLayer::Metacognitive, 100),
    ];
    // Total per namespace = 2500, × number of namespaces

    for ns in Namespace::ALL {
        for (layer, count) in &layers {
            let triples: Vec<Triple> = (0..*count)
                .map(|i| triple(&format!("{}:e{}", ns.as_str(), i), "rdf:type", "Entity"))
                .collect();
            obj.store.add_batch(&triples, ns, *layer).unwrap();
        }
    }
    assert_eq!(obj.store.len(), 2500 * Namespace::ALL.len());

    // Benchmark: full commit cycle
    let start = std::time::Instant::now();
    let c1 = create_commit(&obj, &mut commits, vec![], "10K commit", "DGX").unwrap();
    let commit_ms = start.elapsed().as_millis();

    // Benchmark: checkout
    obj.store.clear();
    let start = std::time::Instant::now();
    checkout(&mut obj, &commits, &c1.commit_id).unwrap();
    let checkout_ms = start.elapsed().as_millis();

    assert_eq!(obj.store.len(), 2500 * Namespace::ALL.len());

    // Benchmark: query
    let start = std::time::Instant::now();
    for ns in Namespace::ALL {
        let _ = obj
            .store
            .query(&QuerySpec {
                namespace: Some(ns),
                ..Default::default()
            })
            .unwrap();
    }
    let query_ms = start.elapsed().as_millis();

    let total = commit_ms + checkout_ms;
    eprintln!(
        "10K benchmark — commit: {}ms, checkout: {}ms, total: {}ms, 4 queries: {}ms",
        commit_ms, checkout_ms, total, query_ms
    );

    // Gate: full commit+checkout cycle < 50ms (generous for CI)
    assert!(
        total < 500,
        "10K commit+checkout took {total}ms — target <50ms (CI margin 500ms)"
    );

    // Gate: query latency
    assert!(
        query_ms < 50,
        "4 namespace queries at 10K took {query_ms}ms — target <10ms"
    );
}
