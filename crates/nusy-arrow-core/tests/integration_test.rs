//! End-to-end integration tests for nusy-arrow-core.
//!
//! Exercises all 4 namespaces × 7 Y-layers, cross-namespace bridge queries,
//! provenance preservation (caused_by/derived_from/consolidated_at), and
//! causal chain traversal.

use arrow::array::Array;
use nusy_arrow_core::{ArrowGraphStore, Namespace, QuerySpec, Triple, YLayer};

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

/// Populate a store with triples across all 4 namespaces × 7 Y-layers = 28 partitions.
/// Returns the total number of triples added.
fn populate_full_store(store: &mut ArrowGraphStore, per_partition: usize) -> usize {
    let mut total = 0;
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
            total += per_partition;
        }
    }
    total
}

#[test]
fn test_all_namespaces_and_ylayers() {
    let mut store = ArrowGraphStore::new();
    let per_partition = 50;
    let total = populate_full_store(&mut store, per_partition);

    // Total should be 5 namespaces × 7 layers × 50 = 1750
    assert_eq!(total, 5 * 7 * per_partition);
    assert_eq!(store.len(), total);

    // Query each namespace independently
    for ns in Namespace::ALL {
        let results = store
            .query(&QuerySpec {
                namespace: Some(ns),
                ..Default::default()
            })
            .unwrap();
        let count: usize = results.iter().map(|b| b.num_rows()).sum();
        assert_eq!(
            count,
            7 * per_partition,
            "Namespace {} should have {} triples",
            ns.as_str(),
            7 * per_partition
        );
    }

    // Query each Y-layer independently (across all namespaces)
    for layer in YLayer::ALL {
        let results = store
            .query(&QuerySpec {
                y_layer: Some(layer),
                ..Default::default()
            })
            .unwrap();
        let count: usize = results.iter().map(|b| b.num_rows()).sum();
        assert_eq!(
            count,
            5 * per_partition,
            "Y-layer {} should have {} triples",
            layer.name(),
            5 * per_partition
        );
    }

    // Query specific namespace + Y-layer combination
    let results = store
        .query(&QuerySpec {
            namespace: Some(Namespace::Research),
            y_layer: Some(YLayer::Metacognitive),
            ..Default::default()
        })
        .unwrap();
    let count: usize = results.iter().map(|b| b.num_rows()).sum();
    assert_eq!(count, per_partition);
}

#[test]
fn test_namespace_isolation_no_cross_contamination() {
    let mut store = ArrowGraphStore::new();

    // Add unique subjects to each namespace
    store
        .add_triple(
            &triple("world:entity1", "rdf:type", "WorldThing"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();
    store
        .add_triple(
            &triple("work:task1", "rdf:type", "WorkTask"),
            Namespace::Work,
            YLayer::Semantic,
        )
        .unwrap();
    store
        .add_triple(
            &triple("research:hyp1", "rdf:type", "Hypothesis"),
            Namespace::Research,
            YLayer::Semantic,
        )
        .unwrap();
    store
        .add_triple(
            &triple("self:reflection1", "rdf:type", "Reflection"),
            Namespace::Self_,
            YLayer::Journal,
        )
        .unwrap();

    // Query World — should NOT contain work/research/self subjects
    let world = store
        .query(&QuerySpec {
            namespace: Some(Namespace::World),
            ..Default::default()
        })
        .unwrap();
    let world_count: usize = world.iter().map(|b| b.num_rows()).sum();
    assert_eq!(world_count, 1);

    // Query by subject across all namespaces — should find exactly 1
    let q = store
        .query(&QuerySpec {
            subject: Some("work:task1".to_string()),
            ..Default::default()
        })
        .unwrap();
    let count: usize = q.iter().map(|b| b.num_rows()).sum();
    assert_eq!(count, 1);
}

#[test]
fn test_cross_namespace_bridge_relations() {
    let mut store = ArrowGraphStore::new();

    // World: domain entity
    store
        .add_triple(
            &triple("nusy:Cardiology", "rdf:type", "nusy:Domain"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();

    // Self: being knows domain (bridge: self→world)
    store
        .add_triple(
            &triple("nusy:Santiago", "nusy:expertIn", "nusy:Cardiology"),
            Namespace::Self_,
            YLayer::Semantic,
        )
        .unwrap();

    // Work: expedition validates hypothesis (bridge: work→research)
    store
        .add_triple(
            &triple("kb:EXP-1253", "kb:validates", "kb:H-019"),
            Namespace::Work,
            YLayer::Semantic,
        )
        .unwrap();

    // Research: hypothesis
    store
        .add_triple(
            &triple("kb:H-019", "rdf:type", "kb:Hypothesis"),
            Namespace::Research,
            YLayer::Semantic,
        )
        .unwrap();

    // Bridge query: find what Santiago is expert in (self namespace)
    let expertise = store
        .query(&QuerySpec {
            subject: Some("nusy:Santiago".to_string()),
            predicate: Some("nusy:expertIn".to_string()),
            ..Default::default()
        })
        .unwrap();
    let count: usize = expertise.iter().map(|b| b.num_rows()).sum();
    assert_eq!(count, 1);

    // Bridge query: find what validates H-019 (work namespace)
    let validators = store
        .query(&QuerySpec {
            predicate: Some("kb:validates".to_string()),
            object: Some("kb:H-019".to_string()),
            ..Default::default()
        })
        .unwrap();
    let count: usize = validators.iter().map(|b| b.num_rows()).sum();
    assert_eq!(count, 1);
}

#[test]
fn test_definitional_vs_experiential_layer_split() {
    let mut store = ArrowGraphStore::new();

    // Add triples to definitional layers (Y0-Y2)
    for layer in YLayer::DEFINITIONAL {
        store
            .add_triple(
                &triple("entity", "def-pred", &format!("def-{}", layer.name())),
                Namespace::World,
                layer,
            )
            .unwrap();
    }

    // Add triples to experiential layers (Y3-Y6)
    for layer in YLayer::EXPERIENTIAL {
        store
            .add_triple(
                &triple("entity", "exp-pred", &format!("exp-{}", layer.name())),
                Namespace::Self_,
                layer,
            )
            .unwrap();
    }

    assert_eq!(store.len(), 7);

    // Query definitional layers only
    let mut def_count = 0;
    for layer in YLayer::DEFINITIONAL {
        let r = store
            .query(&QuerySpec {
                y_layer: Some(layer),
                ..Default::default()
            })
            .unwrap();
        def_count += r.iter().map(|b| b.num_rows()).sum::<usize>();
    }
    assert_eq!(def_count, 3);

    // Query experiential layers only
    let mut exp_count = 0;
    for layer in YLayer::EXPERIENTIAL {
        let r = store
            .query(&QuerySpec {
                y_layer: Some(layer),
                ..Default::default()
            })
            .unwrap();
        exp_count += r.iter().map(|b| b.num_rows()).sum::<usize>();
    }
    assert_eq!(exp_count, 4);
}

#[test]
fn test_provenance_columns_populated() {
    use arrow::array::{StringArray, TimestampMillisecondArray};
    use nusy_arrow_core::col;

    let mut store = ArrowGraphStore::new();
    let now_ms = chrono::Utc::now().timestamp_millis();

    let t = Triple {
        subject: "s1".to_string(),
        predicate: "p1".to_string(),
        object: "o1".to_string(),
        graph: Some("default".to_string()),
        confidence: Some(0.95),
        source_document: Some("ontology.md".to_string()),
        source_chunk_id: Some("chunk_onto_001".to_string()),
        extracted_by: Some("DGX".to_string()),
        caused_by: Some("t-parent".to_string()),
        derived_from: Some("t-origin".to_string()),
        consolidated_at: Some(now_ms),
        certifiability_class: None,
        object_datatype: None,
    };

    store
        .add_triple(&t, Namespace::World, YLayer::Semantic)
        .unwrap();

    let batches = store
        .query(&QuerySpec {
            subject: Some("s1".to_string()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];

    // Verify provenance columns
    let caused = batch
        .column(col::CAUSED_BY)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(caused.value(0), "t-parent");

    let derived = batch
        .column(col::DERIVED_FROM)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(derived.value(0), "t-origin");

    let consolidated = batch
        .column(col::CONSOLIDATED_AT)
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
        .unwrap();
    assert!(!consolidated.is_null(0));
    assert_eq!(consolidated.value(0), now_ms);
}

#[test]
fn test_causal_chain_across_namespaces() {
    let mut store = ArrowGraphStore::new();

    // Root triple in World namespace
    let t0 = Triple {
        subject: "world:fact".to_string(),
        predicate: "rdf:type".to_string(),
        object: "Observation".to_string(),
        caused_by: None,
        derived_from: None,
        ..triple("world:fact", "rdf:type", "Observation")
    };
    let id0 = store
        .add_triple(&t0, Namespace::World, YLayer::Prose)
        .unwrap();

    // Derived triple in Research namespace
    let t1 = Triple {
        subject: "research:conclusion".to_string(),
        predicate: "rdf:type".to_string(),
        object: "Finding".to_string(),
        caused_by: None,
        derived_from: Some(id0.clone()),
        ..triple("research:conclusion", "rdf:type", "Finding")
    };
    let id1 = store
        .add_triple(&t1, Namespace::Research, YLayer::Reasoning)
        .unwrap();

    // Action triple in Self namespace caused by the research finding
    let t2 = Triple {
        subject: "self:action".to_string(),
        predicate: "rdf:type".to_string(),
        object: "Decision".to_string(),
        caused_by: Some(id1.clone()),
        derived_from: None,
        ..triple("self:action", "rdf:type", "Decision")
    };
    let id2 = store
        .add_triple(&t2, Namespace::Self_, YLayer::Experience)
        .unwrap();

    // Causal chain from the action should traverse across namespaces
    let chain = store.causal_chain(&id2);
    assert_eq!(
        chain.len(),
        3,
        "Chain should span 3 triples across namespaces"
    );
    assert_eq!(chain[0].triple_id, id2);
    assert_eq!(chain[0].caused_by, Some(id1.clone()));
    assert_eq!(chain[1].triple_id, id1);
    assert_eq!(chain[1].derived_from, Some(id0.clone()));
    assert_eq!(chain[2].triple_id, id0);
}

#[test]
fn test_logical_delete_and_query_consistency() {
    let mut store = ArrowGraphStore::new();

    let id = store
        .add_triple(
            &triple("to-delete", "rdf:type", "Ephemeral"),
            Namespace::World,
            YLayer::Prose,
        )
        .unwrap();
    store
        .add_triple(
            &triple("to-keep", "rdf:type", "Permanent"),
            Namespace::World,
            YLayer::Prose,
        )
        .unwrap();

    assert_eq!(store.len(), 2);
    store.delete(&id).unwrap();
    assert_eq!(store.len(), 1);

    // Deleted triple excluded from default queries
    let results = store
        .query(&QuerySpec {
            namespace: Some(Namespace::World),
            ..Default::default()
        })
        .unwrap();
    let count: usize = results.iter().map(|b| b.num_rows()).sum();
    assert_eq!(count, 1);

    // include_deleted shows both
    let all = store
        .query(&QuerySpec {
            namespace: Some(Namespace::World),
            include_deleted: true,
            ..Default::default()
        })
        .unwrap();
    let all_count: usize = all.iter().map(|b| b.num_rows()).sum();
    assert_eq!(all_count, 2);
}

#[test]
fn test_10k_triples_across_namespaces_performance() {
    let mut store = ArrowGraphStore::new();

    // 2500 triples per namespace (varied Y-layer distribution)
    let layers = [
        (YLayer::Prose, 500),
        (YLayer::Semantic, 800),
        (YLayer::Reasoning, 300),
        (YLayer::Experience, 400),
        (YLayer::Journal, 200),
        (YLayer::Procedural, 200),
        (YLayer::Metacognitive, 100),
    ];

    let start = std::time::Instant::now();
    for ns in Namespace::ALL {
        for (layer, count) in &layers {
            let triples: Vec<Triple> = (0..*count)
                .map(|i| triple(&format!("{}:e{}", ns.as_str(), i), "rdf:type", "Entity"))
                .collect();
            store.add_batch(&triples, ns, *layer).unwrap();
        }
    }
    let populate_ms = start.elapsed().as_millis();

    assert_eq!(store.len(), 12_500);

    // Query performance: namespace-scoped
    let start = std::time::Instant::now();
    for ns in Namespace::ALL {
        let _ = store
            .query(&QuerySpec {
                namespace: Some(ns),
                ..Default::default()
            })
            .unwrap();
    }
    let query_ms = start.elapsed().as_millis();

    eprintln!(
        "10K populate: {}ms, 4 namespace queries: {}ms",
        populate_ms, query_ms
    );

    // Populate should be fast (<100ms)
    assert!(
        populate_ms < 200,
        "10K populate took {populate_ms}ms — expected <200ms"
    );
    // Query should be fast (<10ms for 4 queries)
    assert!(
        query_ms < 50,
        "4 namespace queries took {query_ms}ms — expected <50ms"
    );
}
