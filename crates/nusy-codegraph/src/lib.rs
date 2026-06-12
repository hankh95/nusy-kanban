//! nusy-codegraph — Code as a live Arrow object graph.
//!
//! Parses Python source files via tree-sitter into CodeNodes (functions, classes,
//! modules) and CodeEdges (calls, imports, inheritance, containment). All data
//! stored as Arrow RecordBatches for zero-copy query and versioning via nusy-arrow-git.
//!
//! # Architecture
//!
//! ```text
//! Python source → tree-sitter CST → CodeNodes + CodeEdges → Arrow RecordBatches
//!                                     ↓
//!                              NameResolver (cross-file edge extraction)
//! ```

pub mod build_cache;
pub mod cargo_parser;
pub mod crate_graph;
pub mod crate_schema;
pub mod edges;
pub mod embeddings;
pub mod executor;
pub mod git_tools;
pub mod impact;
pub mod ingest;
pub mod ingest_pipeline;
pub mod mcp_bridge;
pub mod mcp_tools;
pub mod metrics;
pub mod module_resolver;
pub mod nats_service;
pub mod nats_sync;
pub mod parser;
pub mod python_parser;
pub mod python_resolver;
pub mod rename;
pub mod rust_parser;
pub mod schema;
pub mod scip_calls;
pub mod search;
pub mod semantic_diff;
pub mod test_discovery;
pub mod topo_sort;

// Re-export primary types
pub use edges::{NameResolver, extract_call_edges, extract_cross_file_edges, extract_edges};
pub use embeddings::{
    EmbeddingProvider, HashEmbeddingProvider, SearchResult, attach_embeddings, cosine_similarity,
    embed_nodes, semantic_search,
};
pub use executor::{ExecutionResult, ExecutorError, execute_object};
pub use git_tools::{
    CodeConflict, CodeDiffChangeType, CodeDiffEntry, CodeDiffResult, CodeMergeResult, MergeWarning,
    SmartMergeResult, codegraph_diff, codegraph_merge, smart_merge,
};
pub use impact::{ChangedNode, ImpactReport, ImpactStats, format_impact_report, impact_analysis};
pub use ingest::{
    IngestResult, Language, callers_of, ingest_directory, ingest_files, ingest_python_directory,
    nodes_in_file,
};
pub use ingest_pipeline::{
    GraphViolations, WorkspaceIngestResult, discover_workspace_crates, ingest_workspace,
    verify_graph, write_graph_parquet,
};
pub use mcp_tools::{
    McpToolError, NodeUpdate, QueryFilter, QueryResult, codegraph_add_edge,
    codegraph_query_objects, codegraph_remove_edge, codegraph_update_object,
};
pub use metrics::{
    CodebaseMetrics, FileCoverage, compute_codebase_metrics, enrich_with_coverage,
    enrich_with_git_timestamps, high_complexity_nodes, largest_nodes, low_coverage_nodes,
    parse_coverage_json,
};
pub use module_resolver::RustModuleResolver;
pub use parser::{ImportInfo, ParseError, ParseResult, parse_python_file};
pub use python_parser::{PythonParseResult, PythonParser, PythonParserError};
pub use python_resolver::PythonModuleResolver;
pub use rename::{RenameError, rename_node};
pub use rust_parser::parse_rust_file;
pub use schema::{
    CodeEdge, CodeEdgePredicate, CodeNode, CodeNodeKind, build_code_edges_batch,
    build_code_nodes_batch, code_edges_schema, code_nodes_schema, extract_file_path,
};
pub use scip_calls::{ScipCallResult, extract_scip_call_edges};
pub use search::{
    CodeSearch, callees, callers, children_of, find_sources, find_targets, search_nodes, tests_for,
    transitive_callers, transitive_deps,
};
pub use semantic_diff::{
    AffectedEdge, AffectedReason, ChangeClassification, DiffStats, SemanticDiff, SemanticDiffEntry,
    format_semantic_diff, semantic_diff,
};
pub use topo_sort::{
    ParallelismStats, crate_parallelism_stats, sort_crates, sort_crates_parallel,
    sort_functions_in_crate, sort_functions_parallel,
};
