//! Integration test — Arrow-native triple store with realistic data.
//!
//! EX-3099 Phase 3: Load ~1000 triples, run query patterns used by
//! kg_store's dependents, verify correctness, benchmark performance.

use nusy_arrow_core::kg_store::KgStore;
use nusy_arrow_core::triple_store::SimpleTripleStore;

/// Generate realistic being knowledge (domain: children's literature).
fn populate_being_knowledge(store: &mut KgStore) {
    // Ontology triples (rdf:type declarations)
    let types = [
        ("santiago:Character", "rdf:type", "owl:Class"),
        ("santiago:Animal", "rdf:type", "owl:Class"),
        ("santiago:Place", "rdf:type", "owl:Class"),
        ("santiago:Book", "rdf:type", "owl:Class"),
        ("santiago:Theme", "rdf:type", "owl:Class"),
    ];
    for (s, p, o) in &types {
        store.add_triple(s, p, o, Some("ontology"), 1.0).unwrap();
    }

    // Entity declarations
    let entities = [
        ("santiago:Pooh", "rdf:type", "santiago:Character"),
        ("santiago:Piglet", "rdf:type", "santiago:Character"),
        ("santiago:Eeyore", "rdf:type", "santiago:Character"),
        ("santiago:Tigger", "rdf:type", "santiago:Character"),
        ("santiago:Owl", "rdf:type", "santiago:Character"),
        ("santiago:Rabbit", "rdf:type", "santiago:Character"),
        (
            "santiago:ChristopherRobin",
            "rdf:type",
            "santiago:Character",
        ),
        ("santiago:HundredAcreWood", "rdf:type", "santiago:Place"),
        ("santiago:WinnieThePooh", "rdf:type", "santiago:Book"),
    ];
    for (s, p, o) in &entities {
        store.add_triple(s, p, o, Some("curriculum"), 0.95).unwrap();
    }

    // Relationships (bulk)
    let relationships = [
        (
            "santiago:Pooh",
            "santiago:friendOf",
            "santiago:Piglet",
            0.95,
        ),
        ("santiago:Pooh", "santiago:friendOf", "santiago:Eeyore", 0.9),
        (
            "santiago:Pooh",
            "santiago:friendOf",
            "santiago:Tigger",
            0.85,
        ),
        (
            "santiago:Pooh",
            "santiago:livesIn",
            "santiago:HundredAcreWood",
            1.0,
        ),
        (
            "santiago:Piglet",
            "santiago:livesIn",
            "santiago:HundredAcreWood",
            1.0,
        ),
        (
            "santiago:Eeyore",
            "santiago:livesIn",
            "santiago:HundredAcreWood",
            1.0,
        ),
        ("santiago:Pooh", "santiago:likes", "santiago:Honey", 1.0),
        (
            "santiago:Pooh",
            "santiago:appearsIn",
            "santiago:WinnieThePooh",
            1.0,
        ),
        (
            "santiago:Piglet",
            "santiago:appearsIn",
            "santiago:WinnieThePooh",
            1.0,
        ),
        (
            "santiago:Tigger",
            "santiago:appearsIn",
            "santiago:WinnieThePooh",
            1.0,
        ),
    ];
    store
        .add_triples(
            &relationships
                .iter()
                .map(|(s, p, o, c)| (*s, *p, *o, *c))
                .collect::<Vec<_>>(),
            Some("voyage_learning"),
        )
        .unwrap();

    // Generate ~980 more triples (varied domains)
    let domains = ["medical", "ethics", "literature", "science", "history"];
    let predicates = [
        "rdf:type",
        "rdfs:subClassOf",
        "santiago:relatedTo",
        "santiago:partOf",
        "santiago:hasProperty",
    ];
    for domain in domains {
        for i in 0..196 {
            let s = format!("santiago:entity_{}_{}", domain, i);
            let p = predicates[i % predicates.len()];
            let o = format!("santiago:target_{}_{}", domain, i % 50);
            let confidence = 0.5 + (i as f64 % 50.0) / 100.0;
            store
                .add_triple(&s, p, &o, Some(domain), confidence)
                .unwrap();
        }
    }
}

#[test]
fn test_realistic_load_and_query() {
    let mut store = KgStore::new();
    populate_being_knowledge(&mut store);

    // Verify total count (~1004 triples: 5 + 9 + 10 + 980)
    let total = store.len();
    assert!(total >= 1000, "should have ~1000+ triples, got {total}");

    // Query pattern 1: Find all Characters (used by unified_engine.py)
    let characters = store
        .query(None, Some("rdf:type"), Some("santiago:Character"))
        .unwrap();
    assert_eq!(characters.len(), 7, "should find 7 characters");

    // Query pattern 2: Find Pooh's friends (used by nusy_reasoner.py)
    let friends = store
        .query(Some("santiago:Pooh"), Some("santiago:friendOf"), None)
        .unwrap();
    assert_eq!(friends.len(), 3, "Pooh has 3 friends");

    // Query pattern 3: Find all entities in a place (used by cascading_reasoner.py)
    let in_wood = store
        .query(
            None,
            Some("santiago:livesIn"),
            Some("santiago:HundredAcreWood"),
        )
        .unwrap();
    assert_eq!(in_wood.len(), 3, "3 characters live in the wood");

    // Query pattern 4: Wildcard query (all triples for a subject)
    let pooh_all = store.query(Some("santiago:Pooh"), None, None).unwrap();
    assert!(
        pooh_all.len() >= 5,
        "Pooh should have 5+ triples, got {}",
        pooh_all.len()
    );
}

#[test]
fn test_keyword_search_on_realistic_data() {
    let mut store = KgStore::new();
    populate_being_knowledge(&mut store);

    // Keyword search (used by kg_store.search_by_keywords)
    let results = store.search_by_keywords(&["Pooh"]);
    assert!(
        results.len() >= 5,
        "keyword 'Pooh' should match 5+ triples, got {}",
        results.len()
    );

    let results = store.search_by_keywords(&["medical"]);
    assert!(
        results.len() >= 100,
        "keyword 'medical' should match 100+ triples (domain entities)"
    );
}

#[test]
fn test_statistics_on_realistic_data() {
    let mut store = KgStore::new();
    populate_being_knowledge(&mut store);

    let stats = store.statistics();
    assert!(stats.total_triples >= 1000);
    assert!(stats.unique_subjects > 100);
    assert!(stats.unique_predicates >= 5);
}

#[test]
fn test_simple_triple_store_batch_performance() {
    let mut store = SimpleTripleStore::new();

    // Batch add 1000 triples and measure time
    let triples: Vec<(&str, &str, &str, f64, &str)> = (0..1000)
        .map(|_| ("subject", "predicate", "object", 1.0, "bench"))
        .collect();

    let start = std::time::Instant::now();
    store.add_batch(&triples).unwrap();
    let add_elapsed = start.elapsed();

    assert_eq!(store.len(), 1000);
    assert!(
        add_elapsed.as_millis() < 50,
        "batch add of 1000 triples took {:?} (target: <50ms)",
        add_elapsed
    );

    // Query all 1000 and measure time
    let start = std::time::Instant::now();
    let results = store.query(None, None, None).unwrap();
    let query_elapsed = start.elapsed();

    assert_eq!(results.len(), 1000);
    assert!(
        query_elapsed.as_millis() < 50,
        "full scan of 1000 triples took {:?} (target: <50ms)",
        query_elapsed
    );
}

#[test]
fn test_kg_store_query_performance() {
    let mut store = KgStore::new();
    populate_being_knowledge(&mut store);

    // Pattern query on 1000+ triples
    let start = std::time::Instant::now();
    for _ in 0..100 {
        let _ = store.query(None, Some("rdf:type"), None);
    }
    let elapsed = start.elapsed();

    // 100 pattern queries on debug build — just verify it completes in reasonable time
    // Release build is ~10x faster. rdflib does ~5ms/query; we target ≤2x = 10ms.
    // Debug build penalty: allow up to 20ms/query = 2000ms total.
    assert!(
        elapsed.as_millis() < 3000,
        "100 pattern queries on 1000 triples took {:?} (target: <3s debug build)",
        elapsed
    );
}

#[test]
fn test_prefix_round_trip() {
    let store = KgStore::new();

    // Expand and compact should be inverse operations
    let prefixed = "santiago:Alice";
    let expanded = store.expand_uri(prefixed);
    let compacted = store.compact_uri(&expanded);
    assert_eq!(compacted, prefixed, "expand then compact should round-trip");

    // Test all default prefixes
    for (prefix, uri) in store.prefixes() {
        let test_uri = format!("{uri}TestEntity");
        let compacted = store.compact_uri(&test_uri);
        assert_eq!(
            compacted,
            format!("{prefix}:TestEntity"),
            "prefix {prefix} should round-trip"
        );
    }
}
