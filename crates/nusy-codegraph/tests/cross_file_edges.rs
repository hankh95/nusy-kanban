//! Integration tests for cross-file Rust edge extraction (EX-3169).

use nusy_codegraph::ingest::ingest_directory;
use nusy_codegraph::schema::CodeEdgePredicate;
use std::path::PathBuf;

fn codegraph_crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn test_cross_file_imports_resolved() {
    let result = ingest_directory(&codegraph_crate_root()).expect("ingest");

    // Should have Imports edges from files that use items from other files
    let import_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.predicate == CodeEdgePredicate::Imports)
        .collect();

    assert!(
        !import_edges.is_empty(),
        "should have import edges from cross-file use statements"
    );

    // At least some should resolve to internal CodeNode IDs (not ext: prefix)
    let internal_imports = import_edges
        .iter()
        .filter(|e| !e.target_id.starts_with("ext:"))
        .count();

    assert!(
        internal_imports > 0,
        "at least some imports should resolve to internal nodes, got 0 out of {} total",
        import_edges.len()
    );
}

#[test]
fn test_impl_trait_edges_extracted() {
    let result = ingest_directory(&codegraph_crate_root()).expect("ingest");

    let impl_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.predicate == CodeEdgePredicate::ImplementsTrait)
        .collect();

    // nusy-codegraph has `impl Display for ...`, `impl Default for ...`, etc.
    assert!(
        !impl_edges.is_empty(),
        "should extract ImplementsTrait edges from impl blocks"
    );
}

#[test]
fn test_contains_edges_from_parent_id() {
    let result = ingest_directory(&codegraph_crate_root()).expect("ingest");

    let contains_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.predicate == CodeEdgePredicate::Contains)
        .collect();

    // Files contain functions/structs → many Contains edges expected
    assert!(
        contains_edges.len() > 50,
        "should have many Contains edges, got {}",
        contains_edges.len()
    );
}

#[test]
fn test_edge_predicate_variants_used() {
    let result = ingest_directory(&codegraph_crate_root()).expect("ingest");

    let predicates: std::collections::HashSet<CodeEdgePredicate> =
        result.edges.iter().map(|e| e.predicate).collect();

    // Should use at least: Contains, Imports, ImplementsTrait
    assert!(
        predicates.contains(&CodeEdgePredicate::Contains),
        "missing Contains edges"
    );
    assert!(
        predicates.contains(&CodeEdgePredicate::Imports),
        "missing Imports edges"
    );
    assert!(
        predicates.contains(&CodeEdgePredicate::ImplementsTrait),
        "missing ImplementsTrait edges"
    );
}

#[test]
fn test_module_resolver_finds_crate_modules() {
    let resolver =
        nusy_codegraph::RustModuleResolver::from_crate(&codegraph_crate_root()).expect("resolver");

    // Should find at least 5 modules (schema, parser, ingest, edges, rust_parser, ...)
    assert!(
        resolver.module_count() >= 5,
        "should find at least 5 modules, got {}",
        resolver.module_count()
    );
}
