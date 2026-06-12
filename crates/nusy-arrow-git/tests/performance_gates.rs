//! Performance gate benchmarks for the NuSy Arrow substrate.
//!
//! These benchmarks establish the performance gates that must be met
//! before the Arrow substrate can be used in production:
//!
//! | Gate | Metric | Target | Measured On |
//! |------|--------|--------|-------------|
//! | H-019 | Query latency @10K triples | ≤ 2ms | Per namespace query |
//! | H-GIT-1 | Commit+checkout @10K triples | < 50ms | Full round-trip |
//! | M-SAVE | Save/restore @10K triples | < 100ms | Full round-trip |
//! | M-119 | Being awakening | < 200ms | Restore + 4 queries |
//!
//! Run with: `cargo test --package nusy-arrow-core --test performance_gates`
//!
//! Note: These are implemented as `#[test]` rather than criterion benchmarks
//! to avoid adding a dependency. Timing assertions use generous CI margins
//! (5×) while the target is documented above.

use nusy_arrow_core::{ArrowGraphStore, Namespace, QuerySpec, Triple, YLayer};

fn triple(subj: &str) -> Triple {
    Triple {
        subject: subj.to_string(),
        predicate: "rdf:type".to_string(),
        object: "Entity".to_string(),
        graph: None,
        confidence: Some(0.9),
        source_document: None,
        source_chunk_id: None,
        extracted_by: Some("bench".to_string()),
        caused_by: None,
        derived_from: None,
        consolidated_at: None,
        certifiability_class: None,
        object_datatype: None,
    }
}

/// Build a store with 2500 triples per namespace × all namespaces × 7 Y-layers.
fn build_10k_store() -> ArrowGraphStore {
    let mut store = ArrowGraphStore::new();
    let layers = [
        (YLayer::Prose, 500),
        (YLayer::Semantic, 800),
        (YLayer::Reasoning, 300),
        (YLayer::Experience, 400),
        (YLayer::Journal, 200),
        (YLayer::Procedural, 200),
        (YLayer::Metacognitive, 100),
    ];
    for ns in Namespace::ALL {
        for (layer, count) in &layers {
            let triples: Vec<Triple> = (0..*count)
                .map(|i| triple(&format!("{}:e{}", ns.as_str(), i)))
                .collect();
            store.add_batch(&triples, ns, *layer).unwrap();
        }
    }
    assert_eq!(store.len(), 2500 * Namespace::ALL.len());
    store
}

/// Build a large store for H-019 final validation (70K per namespace × all namespaces).
fn build_350k_store() -> ArrowGraphStore {
    let mut store = ArrowGraphStore::new();
    let batch_size = 70_000;
    for ns in Namespace::ALL {
        let triples: Vec<Triple> = (0..batch_size)
            .map(|i| triple(&format!("{}:e{}", ns.as_str(), i)))
            .collect();
        store.add_batch(&triples, ns, YLayer::Semantic).unwrap();
    }
    assert_eq!(store.len(), batch_size * Namespace::ALL.len());
    store
}

/// H-019: Query latency at 10K triples ≤ 2ms per namespace query.
#[test]
fn gate_h019_query_latency_10k() {
    let store = build_10k_store();

    // Warm up
    for ns in Namespace::ALL {
        let _ = store
            .query(&QuerySpec {
                namespace: Some(ns),
                ..Default::default()
            })
            .unwrap();
    }

    // Measure: 4 individual namespace queries
    let mut max_query_ms = 0u128;
    for ns in Namespace::ALL {
        let start = std::time::Instant::now();
        let results = store
            .query(&QuerySpec {
                namespace: Some(ns),
                ..Default::default()
            })
            .unwrap();
        let elapsed = start.elapsed().as_millis();
        let count: usize = results.iter().map(|b| b.num_rows()).sum();
        assert!(count > 0, "Namespace {} should have triples", ns.as_str());
        if elapsed > max_query_ms {
            max_query_ms = elapsed;
        }
    }

    eprintln!(
        "H-019 @10K: max namespace query = {}ms (target: ≤2ms)",
        max_query_ms
    );
    // CI margin: 10ms (target is 2ms)
    assert!(
        max_query_ms <= 10,
        "H-019 FAIL: query latency {}ms > 10ms (target ≤2ms)",
        max_query_ms
    );
}

/// H-019 stretch: Query latency at 350K triples ≤ 2ms per namespace query.
#[test]
fn gate_h019_query_latency_350k() {
    let store = build_350k_store();

    // Warm up
    for ns in Namespace::ALL {
        let _ = store
            .query(&QuerySpec {
                namespace: Some(ns),
                ..Default::default()
            })
            .unwrap();
    }

    // Measure
    let mut max_query_ms = 0u128;
    for ns in Namespace::ALL {
        let start = std::time::Instant::now();
        let results = store
            .query(&QuerySpec {
                namespace: Some(ns),
                ..Default::default()
            })
            .unwrap();
        let elapsed = start.elapsed().as_millis();
        let count: usize = results.iter().map(|b| b.num_rows()).sum();
        assert_eq!(count, 70_000);
        if elapsed > max_query_ms {
            max_query_ms = elapsed;
        }
    }

    eprintln!(
        "H-019 @350K: max namespace query = {}ms (target: ≤2ms)",
        max_query_ms
    );
    // CI margin: 50ms (target is 2ms, 350K is large)
    assert!(
        max_query_ms <= 50,
        "H-019 FAIL @350K: query latency {}ms > 50ms (target ≤2ms)",
        max_query_ms
    );
}

/// H-GIT-1: Commit+checkout at 10K triples < 50ms.
#[test]
fn gate_hgit1_commit_checkout_10k() {
    use nusy_arrow_git::{CommitsTable, GitObjectStore, checkout, create_commit};

    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();

    // Populate
    let layers = [
        (YLayer::Prose, 500),
        (YLayer::Semantic, 800),
        (YLayer::Reasoning, 300),
        (YLayer::Experience, 400),
        (YLayer::Journal, 200),
        (YLayer::Procedural, 200),
        (YLayer::Metacognitive, 100),
    ];
    for ns in Namespace::ALL {
        for (layer, count) in &layers {
            let triples: Vec<Triple> = (0..*count)
                .map(|i| triple(&format!("{}:e{}", ns.as_str(), i)))
                .collect();
            obj.store.add_batch(&triples, ns, *layer).unwrap();
        }
    }
    assert_eq!(obj.store.len(), 2500 * Namespace::ALL.len());

    // Benchmark commit
    let start = std::time::Instant::now();
    let c1 = create_commit(&obj, &mut commits, vec![], "bench", "DGX").unwrap();
    let commit_ms = start.elapsed().as_millis();

    // Benchmark checkout
    obj.store.clear();
    let start = std::time::Instant::now();
    checkout(&mut obj, &commits, &c1.commit_id).unwrap();
    let checkout_ms = start.elapsed().as_millis();

    assert_eq!(obj.store.len(), 2500 * Namespace::ALL.len());

    let total = commit_ms + checkout_ms;
    eprintln!(
        "H-GIT-1: commit={}ms, checkout={}ms, total={}ms (target: <50ms)",
        commit_ms, checkout_ms, total
    );
    // CI margin: 250ms (target is 50ms)
    assert!(
        total <= 250,
        "H-GIT-1 FAIL: commit+checkout {}ms > 250ms (target <50ms)",
        total
    );
}

/// M-SAVE: Save/restore at 10K triples < 100ms.
#[test]
fn gate_msave_save_restore_10k() {
    use nusy_arrow_git::{GitObjectStore, restore, save};

    let tmp = tempfile::tempdir().unwrap();
    let save_dir = tmp.path().join("save");
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snap"));

    // Populate 10K
    let layers = [
        (YLayer::Prose, 500),
        (YLayer::Semantic, 800),
        (YLayer::Reasoning, 300),
        (YLayer::Experience, 400),
        (YLayer::Journal, 200),
        (YLayer::Procedural, 200),
        (YLayer::Metacognitive, 100),
    ];
    for ns in Namespace::ALL {
        for (layer, count) in &layers {
            let triples: Vec<Triple> = (0..*count)
                .map(|i| triple(&format!("{}:e{}", ns.as_str(), i)))
                .collect();
            obj.store.add_batch(&triples, ns, *layer).unwrap();
        }
    }

    // Benchmark save
    let start = std::time::Instant::now();
    save(&obj, &save_dir).unwrap();
    let save_ms = start.elapsed().as_millis();

    // Benchmark restore
    obj.store.clear();
    let start = std::time::Instant::now();
    restore(&mut obj, &save_dir).unwrap();
    let restore_ms = start.elapsed().as_millis();

    assert_eq!(obj.store.len(), 2500 * Namespace::ALL.len());

    let total = save_ms + restore_ms;
    eprintln!(
        "M-SAVE: save={}ms, restore={}ms, total={}ms (target: <100ms)",
        save_ms, restore_ms, total
    );
    // CI margin: 500ms (target is 100ms)
    assert!(
        total <= 500,
        "M-SAVE FAIL: save+restore {}ms > 500ms (target <100ms)",
        total
    );
}

/// M-119: Simulated being awakening < 200ms.
/// Awakening = restore from save + 4 namespace queries.
#[test]
fn gate_m119_awakening_latency() {
    use nusy_arrow_git::{GitObjectStore, restore, save};

    let tmp = tempfile::tempdir().unwrap();
    let save_dir = tmp.path().join("being-state");
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snap"));

    // Populate a realistic being state (10K triples)
    let layers = [
        (YLayer::Prose, 500),
        (YLayer::Semantic, 800),
        (YLayer::Reasoning, 300),
        (YLayer::Experience, 400),
        (YLayer::Journal, 200),
        (YLayer::Procedural, 200),
        (YLayer::Metacognitive, 100),
    ];
    for ns in Namespace::ALL {
        for (layer, count) in &layers {
            let triples: Vec<Triple> = (0..*count)
                .map(|i| triple(&format!("{}:e{}", ns.as_str(), i)))
                .collect();
            obj.store.add_batch(&triples, ns, *layer).unwrap();
        }
    }
    save(&obj, &save_dir).unwrap();
    obj.store.clear();

    // Benchmark: awakening = restore + queries
    let start = std::time::Instant::now();

    // Step 1: Restore state from disk
    restore(&mut obj, &save_dir).unwrap();

    // Step 2: Initial queries (what a being does on awakening)
    for ns in Namespace::ALL {
        let _ = obj
            .store
            .query(&QuerySpec {
                namespace: Some(ns),
                ..Default::default()
            })
            .unwrap();
    }

    let awakening_ms = start.elapsed().as_millis();
    eprintln!("M-119: awakening = {}ms (target: <200ms)", awakening_ms);

    // CI margin: 1000ms (target is 200ms)
    assert!(
        awakening_ms <= 1000,
        "M-119 FAIL: awakening {}ms > 1000ms (target <200ms)",
        awakening_ms
    );
}
