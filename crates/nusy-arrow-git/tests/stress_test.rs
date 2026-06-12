//! Stress tests for nusy-arrow-git.
//!
//! These tests push beyond normal usage to find edge cases and verify
//! graceful degradation at scale. Run with:
//!   cargo test --package nusy-arrow-git --test stress_test -- --nocapture
//!
//! Some tests are marked #[ignore] because they take >10s. Run them with:
//!   cargo test --package nusy-arrow-git --test stress_test -- --ignored --nocapture

use nusy_arrow_core::{Namespace, QuerySpec, Triple, YLayer};
use nusy_arrow_git::{
    CommitsTable, GitObjectStore, checkout, create_commit, diff, merge, restore, save,
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
        extracted_by: Some("stress".to_string()),
        caused_by: None,
        derived_from: None,
        consolidated_at: None,
        certifiability_class: None,
        object_datatype: None,
    }
}

fn simple_triple(subj: &str) -> Triple {
    triple(subj, "rdf:type", "Entity")
}

/// Populate a store with `n` triples per namespace (total = n * namespace count).
fn populate(obj: &mut GitObjectStore, n: usize) {
    for ns in Namespace::ALL {
        let triples: Vec<Triple> = (0..n)
            .map(|i| simple_triple(&format!("{}:e{}", ns.as_str(), i)))
            .collect();
        obj.store.add_batch(&triples, ns, YLayer::Semantic).unwrap();
    }
}

// ─── Large Graph Tests ──────────────────────────────────────────

/// Large graph: 20K per namespace → commit → checkout → verify.
#[test]
fn stress_100k_commit_checkout() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    let per_ns = 20_000;
    populate(&mut obj, per_ns);
    let expected = per_ns * Namespace::ALL.len();
    assert_eq!(obj.store.len(), expected);

    let start = std::time::Instant::now();
    let c1 = create_commit(&obj, &mut commits, vec![], "large commit", "DGX").unwrap();
    let commit_ms = start.elapsed().as_millis();

    obj.store.clear();
    assert_eq!(obj.store.len(), 0);

    let start = std::time::Instant::now();
    checkout(&mut obj, &commits, &c1.commit_id).unwrap();
    let checkout_ms = start.elapsed().as_millis();

    assert_eq!(obj.store.len(), expected);
    eprintln!(
        "{}K: commit={}ms, checkout={}ms",
        expected / 1000,
        commit_ms,
        checkout_ms
    );
}

/// 500K triples → save → restore → verify.
#[test]
#[ignore] // Takes ~5s, run with --ignored
fn stress_500k_save_restore() {
    let tmp = tempfile::tempdir().unwrap();
    let save_dir = tmp.path().join("save");
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snap"));

    let per_ns = 100_000;
    populate(&mut obj, per_ns);
    let expected = per_ns * Namespace::ALL.len();
    assert_eq!(obj.store.len(), expected);

    let start = std::time::Instant::now();
    save(&obj, &save_dir).unwrap();
    let save_ms = start.elapsed().as_millis();

    obj.store.clear();

    let start = std::time::Instant::now();
    restore(&mut obj, &save_dir).unwrap();
    let restore_ms = start.elapsed().as_millis();

    assert_eq!(obj.store.len(), expected);
    eprintln!(
        "{}K: save={}ms, restore={}ms",
        expected / 1000,
        save_ms,
        restore_ms
    );
}

// ─── Deep History Tests ─────────────────────────────────────────

/// 100 sequential commits → history traversal → log correct.
#[test]
fn stress_100_sequential_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    // Start with base data
    let base_triples: Vec<Triple> = (0..100)
        .map(|i| simple_triple(&format!("base:e{}", i)))
        .collect();
    obj.store
        .add_batch(&base_triples, Namespace::World, YLayer::Semantic)
        .unwrap();

    let mut parent_ids: Vec<String> = vec![];

    for i in 0..100 {
        // Add one triple per commit
        obj.store
            .add_triple(
                &simple_triple(&format!("commit{}:entity", i)),
                Namespace::Work,
                YLayer::Experience,
            )
            .unwrap();
        let c = create_commit(
            &obj,
            &mut commits,
            parent_ids,
            &format!("commit {}", i),
            "DGX",
        )
        .unwrap();
        parent_ids = vec![c.commit_id.clone()];
    }

    // History should show all 100 commits
    let log = nusy_arrow_git::log(&commits, &parent_ids[0], 200);
    assert_eq!(log.len(), 100, "Should have 100 commits in history");

    // Checkout the last commit should have 100 base + 100 incremental = 200
    obj.store.clear();
    checkout(&mut obj, &commits, &parent_ids[0]).unwrap();
    assert_eq!(obj.store.len(), 200);

    // Checkout the first commit
    let first = &log[log.len() - 1];
    obj.store.clear();
    checkout(&mut obj, &commits, &first.commit_id).unwrap();
    // First commit: 100 base + 1 incremental = 101
    assert_eq!(obj.store.len(), 101);
}

// ─── Branch & Merge Stress ──────────────────────────────────────

/// Multiple branches diverging and merging back sequentially.
#[test]
fn stress_branch_merge_cascade() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();
    let mut refs = nusy_arrow_git::RefsTable::new();

    // Base commit
    let base_triples: Vec<Triple> = (0..100)
        .map(|i| simple_triple(&format!("base:e{}", i)))
        .collect();
    obj.store
        .add_batch(&base_triples, Namespace::World, YLayer::Semantic)
        .unwrap();
    let base = create_commit(&obj, &mut commits, vec![], "base", "DGX").unwrap();
    refs.init_main(&base.commit_id);

    // Create 10 branches, each adding unique data
    let mut branch_heads = Vec::new();
    for b in 0..10 {
        obj.store.clear();
        checkout(&mut obj, &commits, &base.commit_id).unwrap();

        // Add unique triples for this branch
        for i in 0..50 {
            obj.store
                .add_triple(
                    &triple(
                        &format!("branch{}:e{}", b, i),
                        "rdf:type",
                        &format!("Branch{}Entity", b),
                    ),
                    Namespace::Work,
                    YLayer::Experience,
                )
                .unwrap();
        }

        let bc = create_commit(
            &obj,
            &mut commits,
            vec![base.commit_id.clone()],
            &format!("branch-{}", b),
            "DGX",
        )
        .unwrap();
        refs.create_branch(&format!("branch-{}", b), &bc.commit_id)
            .unwrap();
        branch_heads.push(bc.commit_id);
    }

    // Merge all branches sequentially into main
    let mut current_main = base.commit_id.clone();
    for (b, head) in branch_heads.iter().enumerate() {
        let result = merge(&mut obj, &mut commits, &current_main, head, "DGX").unwrap();
        match result {
            nusy_arrow_git::MergeResult::Clean(c) => {
                current_main = c.commit_id;
            }
            nusy_arrow_git::MergeResult::Conflict(conflicts) => {
                panic!(
                    "Unexpected conflict merging branch {}: {:?}",
                    b,
                    conflicts.iter().map(|c| &c.subject).collect::<Vec<_>>()
                );
            }
            nusy_arrow_git::MergeResult::NoCommonAncestor => {
                panic!("No common ancestor for branch {}", b);
            }
        }
    }

    // Verify final state has all data
    obj.store.clear();
    checkout(&mut obj, &commits, &current_main).unwrap();
    // 100 base + 10 branches × 50 = 600
    assert_eq!(obj.store.len(), 600);
}

// ─── Rapid Save/Restore Cycles ──────────────────────────────────

/// 100 rapid save/restore cycles → no corruption.
#[test]
fn stress_rapid_save_restore_cycles() {
    let tmp = tempfile::tempdir().unwrap();
    let save_dir = tmp.path().join("save");
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snap"));

    // Start with triples across all namespaces
    populate(&mut obj, 250);
    let initial_count = obj.store.len();
    assert_eq!(initial_count, 250 * Namespace::ALL.len());

    for i in 0..100 {
        save(&obj, &save_dir).unwrap();
        obj.store.clear();
        restore(&mut obj, &save_dir).unwrap();
        assert_eq!(
            obj.store.len(),
            initial_count,
            "Data loss at cycle {}: expected {}, got {}",
            i,
            initial_count,
            obj.store.len()
        );
    }
}

// ─── Conflict Stress ────────────────────────────────────────────

/// Many conflicts → all reported, no crash.
#[test]
fn stress_many_conflicts() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    // Base commit
    let base_triples: Vec<Triple> = (0..100)
        .map(|i| simple_triple(&format!("entity:{}", i)))
        .collect();
    obj.store
        .add_batch(&base_triples, Namespace::World, YLayer::Semantic)
        .unwrap();
    let base = create_commit(&obj, &mut commits, vec![], "base", "DGX").unwrap();

    // Branch A: modify 500 entities
    obj.store.clear();
    checkout(&mut obj, &commits, &base.commit_id).unwrap();
    for i in 0..500 {
        obj.store
            .add_triple(
                &triple(&format!("conflict:e{}", i), "value", "branch_a_value"),
                Namespace::Work,
                YLayer::Experience,
            )
            .unwrap();
    }
    let ca = create_commit(
        &obj,
        &mut commits,
        vec![base.commit_id.clone()],
        "branch-a",
        "DGX",
    )
    .unwrap();

    // Branch B: modify same 500 entities with different values
    obj.store.clear();
    checkout(&mut obj, &commits, &base.commit_id).unwrap();
    for i in 0..500 {
        obj.store
            .add_triple(
                &triple(&format!("conflict:e{}", i), "value", "branch_b_value"),
                Namespace::Work,
                YLayer::Experience,
            )
            .unwrap();
    }
    let cb = create_commit(
        &obj,
        &mut commits,
        vec![base.commit_id.clone()],
        "branch-b",
        "DGX",
    )
    .unwrap();

    // Merge should report all 500 conflicts
    let result = merge(&mut obj, &mut commits, &ca.commit_id, &cb.commit_id, "DGX").unwrap();
    match result {
        nusy_arrow_git::MergeResult::Conflict(conflicts) => {
            assert_eq!(
                conflicts.len(),
                500,
                "Expected 500 conflicts, got {}",
                conflicts.len()
            );
            // Verify all conflicts have correct values
            for c in &conflicts {
                assert_eq!(c.object_a, "branch_a_value");
                assert_eq!(c.object_b, "branch_b_value");
            }
        }
        other => panic!("Expected Conflict, got {:?}", other),
    }
}

// ─── Multi-Namespace Integrity Under Churn ──────────────────────

/// Repeated add/commit/checkout across all namespaces — data stays consistent.
#[test]
fn stress_namespace_integrity_under_churn() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    let mut parent_ids: Vec<String> = vec![];
    let namespaces = Namespace::ALL;

    for round in 0..20 {
        // Add 50 triples to each namespace per round
        for ns in namespaces {
            let triples: Vec<Triple> = (0..50)
                .map(|i| simple_triple(&format!("r{}:{}:e{}", round, ns.as_str(), i)))
                .collect();
            obj.store.add_batch(&triples, ns, YLayer::Semantic).unwrap();
        }

        let c = create_commit(
            &obj,
            &mut commits,
            parent_ids,
            &format!("round {}", round),
            "DGX",
        )
        .unwrap();
        parent_ids = vec![c.commit_id.clone()];
    }

    // Checkout should restore all data
    let expected = 20 * 50 * Namespace::ALL.len();
    obj.store.clear();
    checkout(&mut obj, &commits, &parent_ids[0]).unwrap();
    assert_eq!(obj.store.len(), expected);

    // Verify each namespace has the right count
    for ns in namespaces {
        let results = obj
            .store
            .query(&QuerySpec {
                namespace: Some(ns),
                ..Default::default()
            })
            .unwrap();
        let count: usize = results.iter().map(|b| b.num_rows()).sum();
        assert_eq!(
            count,
            20 * 50,
            "Namespace {} should have {} triples, got {}",
            ns.as_str(),
            20 * 50,
            count
        );
    }
}

// ─── Diff Stress ────────────────────────────────────────────────

/// Large diff between commits with many changes.
#[test]
fn stress_large_diff() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    // Commit 1: 1000 triples
    let triples1: Vec<Triple> = (0..1000)
        .map(|i| simple_triple(&format!("entity:{}", i)))
        .collect();
    obj.store
        .add_batch(&triples1, Namespace::World, YLayer::Semantic)
        .unwrap();
    let c1 = create_commit(&obj, &mut commits, vec![], "v1", "DGX").unwrap();

    // Commit 2: add 500 more, different subjects
    let triples2: Vec<Triple> = (1000..1500)
        .map(|i| simple_triple(&format!("entity:{}", i)))
        .collect();
    obj.store
        .add_batch(&triples2, Namespace::World, YLayer::Semantic)
        .unwrap();
    let c2 = create_commit(&obj, &mut commits, vec![c1.commit_id.clone()], "v2", "DGX").unwrap();

    let d = diff(&mut obj, &commits, &c1.commit_id, &c2.commit_id).unwrap();
    assert_eq!(d.added.len(), 500, "Should show 500 additions");
    assert_eq!(d.removed.len(), 0, "Should show 0 removals");
}

// ─── Full State Persistence Under Load ──────────────────────────

/// Save/restore full state (commits + refs + data) after many operations.
#[test]
fn stress_full_state_persistence_after_churn() {
    let tmp = tempfile::tempdir().unwrap();
    let save_dir = tmp.path().join("full-save");
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snap"));
    let mut commits = CommitsTable::new();
    let mut refs = nusy_arrow_git::RefsTable::new();

    // Build up state with multiple commits and branches
    populate(&mut obj, 500); // 2K triples
    let c1 = create_commit(&obj, &mut commits, vec![], "initial", "DGX").unwrap();
    refs.init_main(&c1.commit_id);

    // Add more data and create branch
    let more: Vec<Triple> = (0..500)
        .map(|i| simple_triple(&format!("extra:e{}", i)))
        .collect();
    obj.store
        .add_batch(&more, Namespace::Work, YLayer::Experience)
        .unwrap();
    let c2 = create_commit(
        &obj,
        &mut commits,
        vec![c1.commit_id.clone()],
        "more data",
        "DGX",
    )
    .unwrap();
    refs.update_ref("main", &c2.commit_id).unwrap();
    refs.create_branch("feature", &c2.commit_id).unwrap();

    let original_len = obj.store.len();
    let original_commit_count = commits.all().len();

    // Full save
    nusy_arrow_git::save_full(&obj, Some(&commits), Some(&refs), &save_dir).unwrap();

    // Clear everything
    obj.store.clear();

    // Full restore
    let (restored_commits, restored_refs) =
        nusy_arrow_git::restore_full(&mut obj, &save_dir).unwrap();

    assert_eq!(obj.store.len(), original_len);

    let rc = restored_commits.expect("commits should be restored");
    assert_eq!(rc.all().len(), original_commit_count);

    let rr = restored_refs.expect("refs should be restored");
    assert_eq!(
        rr.get("main").map(|r| r.commit_id.as_str()),
        Some(c2.commit_id.as_str())
    );
    assert_eq!(
        rr.get("feature").map(|r| r.commit_id.as_str()),
        Some(c2.commit_id.as_str())
    );
}
