//! Edge extraction — build CodeEdges from parsed CodeNodes.
//!
//! Extracts containment, import, inheritance, and call edges
//! from the parsed code graph. Uses name resolution to map
//! references to CodeNode IDs.

use crate::parser::{ImportInfo, ParseResult};
use crate::schema::{CodeEdge, CodeEdgePredicate, CodeNode, CodeNodeKind};
use crate::scip_calls;
use std::collections::HashMap;
use std::path::Path;

/// Name resolution table: maps fully-qualified names to CodeNode IDs.
#[derive(Debug, Default)]
pub struct NameResolver {
    /// Exact name → node ID.
    name_to_id: HashMap<String, String>,
    /// Short name → node ID (for unqualified references).
    short_name_to_id: HashMap<String, String>,
}

impl NameResolver {
    /// Build a resolver from a set of CodeNodes.
    pub fn from_nodes(nodes: &[CodeNode]) -> Self {
        let mut resolver = Self::default();
        for node in nodes {
            // Register by full ID
            resolver.name_to_id.insert(node.id.clone(), node.id.clone());

            // Register by module-qualified name (e.g. "brain.perception.signal_fusion.fuse")
            let qualified = node_to_qualified_name(node);
            if !qualified.is_empty() {
                resolver.name_to_id.insert(qualified, node.id.clone());
            }

            // Register by short name (last component)
            if !node.name.is_empty() {
                resolver
                    .short_name_to_id
                    .entry(node.name.clone())
                    .or_insert_with(|| node.id.clone());
            }
        }
        resolver
    }

    /// Resolve a name to a CodeNode ID.
    pub fn resolve(&self, name: &str) -> Option<&String> {
        self.name_to_id
            .get(name)
            .or_else(|| self.short_name_to_id.get(name))
    }
}

/// Extract all edges from parsed results.
///
/// Returns edges for:
/// - Containment (parent→child via parent_id)
/// - Imports (file→module)
/// - Inheritance (class→base class)
/// - Calls (detected via text scanning, best-effort)
pub fn extract_edges(parse_results: &[ParseResult], resolver: &NameResolver) -> Vec<CodeEdge> {
    let mut edges = Vec::new();

    // Collect all nodes for cross-referencing
    let all_nodes: Vec<&CodeNode> = parse_results.iter().flat_map(|r| &r.nodes).collect();

    // 1. Containment edges (from parent_id)
    for node in &all_nodes {
        if let Some(parent_id) = &node.parent_id {
            edges.push(CodeEdge {
                source_id: parent_id.clone(),
                target_id: node.id.clone(),
                predicate: CodeEdgePredicate::Contains,
                weight: None,
                commit_id: None,
            });
        }
    }

    // 2. Import edges
    let all_imports: Vec<&ImportInfo> = parse_results.iter().flat_map(|r| &r.imports).collect();

    for imp in &all_imports {
        // Try to resolve the imported module to a known node
        let target = resolve_import(imp, resolver);
        let target_id = target.unwrap_or_else(|| format!("ext:{}", imp.module));

        edges.push(CodeEdge {
            source_id: imp.file_node_id.clone(),
            target_id,
            predicate: CodeEdgePredicate::Imports,
            weight: None,
            commit_id: None,
        });
    }

    // 3. Inheritance edges (from class signatures)
    for node in &all_nodes {
        if node.kind == CodeNodeKind::Class
            && let Some(sig) = &node.signature
        {
            let bases = extract_bases_from_signature(sig);
            for base in bases {
                let target_id = resolver
                    .resolve(&base)
                    .cloned()
                    .unwrap_or_else(|| format!("ext:{base}"));
                edges.push(CodeEdge {
                    source_id: node.id.clone(),
                    target_id,
                    predicate: CodeEdgePredicate::InheritsFrom,
                    weight: None,
                    commit_id: None,
                });
            }
        }
    }

    edges
}

/// Extract call edges, preferring SCIP data when available.
///
/// When `crate_dir` is `Some`, this first attempts `extract_scip_call_edges`
/// (which uses rust-analyzer for compiler-quality call detection). If rust-analyzer
/// is unavailable or fails, falls back to text-scanning via `text_scan_call_edges`.
///
/// Pass `crate_dir: None` to always use text scanning (e.g. for Python).
pub fn extract_call_edges(
    parse_results: &[ParseResult],
    _resolver: &NameResolver,
    source_texts: &HashMap<String, String>,
    crate_dir: Option<&Path>,
) -> Vec<CodeEdge> {
    // Collect all nodes for both SCIP and text-scan paths.
    let all_nodes: Vec<CodeNode> = parse_results.iter().flat_map(|r| r.nodes.clone()).collect();

    // Try SCIP first (Rust only — requires rust-analyzer).
    if let Some(dir) = crate_dir {
        let result = scip_calls::extract_scip_call_edges(dir, &all_nodes);
        if !result.edges.is_empty() {
            tracing::info!(
                "SCIP call edges: {} resolved, {} unresolved ({} docs)",
                result.symbols_resolved,
                result.unresolved_references,
                result.documents_processed
            );
            return result.edges;
        } else if !result.warnings.is_empty() {
            tracing::debug!(
                "SCIP unavailable or failed, falling back to text scan: {}",
                result.warnings.join("; ")
            );
        }
    }

    // Fall back to text scanning.
    text_scan_call_edges(&all_nodes, source_texts)
}

/// Text-based call edge extraction (best-effort).
///
/// Scans function/method bodies for identifiers that match known callable names.
/// Not precise — misses indirect calls, dynamic dispatch, and variable-mediated calls.
/// Use `extract_scip_call_edges` instead when rust-analyzer is available.
fn text_scan_call_edges(
    all_nodes: &[CodeNode],
    source_texts: &HashMap<String, String>,
) -> Vec<CodeEdge> {
    let mut edges = Vec::new();

    // Build set of callable names
    let callable_names: HashMap<&str, &str> = all_nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                CodeNodeKind::Function | CodeNodeKind::Method | CodeNodeKind::Test
            )
        })
        .map(|n| (n.name.as_str(), n.id.as_str()))
        .collect();

    // Scan each function/method body for calls
    for node in all_nodes {
        if !matches!(
            node.kind,
            CodeNodeKind::Function | CodeNodeKind::Method | CodeNodeKind::Test
        ) {
            continue;
        }

        // Get the source file for this node
        let file_path = node_id_to_path(&node.id);
        let Some(source) = source_texts.get(&file_path) else {
            continue;
        };

        // Simple heuristic: look for `name(` patterns in the source
        // This is imprecise but catches most direct calls
        for (name, target_id) in &callable_names {
            if *target_id == node.id {
                continue; // Skip self-references
            }

            let call_pattern = format!("{name}(");
            // Count occurrences as a rough weight
            let count = source.matches(&call_pattern).count();
            if count > 0 {
                // Verify we're not just matching substring
                // (e.g., "test_fuse(" shouldn't match "fuse(")
                let valid = source.contains(&format!(".{call_pattern}"))
                    || source.contains(&format!(" {call_pattern}"))
                    || source.contains(&format!("={call_pattern}"))
                    || source.contains(&format!("({call_pattern}"))
                    || source.starts_with(&call_pattern);

                if valid {
                    edges.push(CodeEdge {
                        source_id: node.id.clone(),
                        target_id: target_id.to_string(),
                        predicate: CodeEdgePredicate::Calls,
                        weight: Some(count as f32),
                        commit_id: None,
                    });
                }
            }
        }
    }

    edges
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Convert a CodeNode to its module-qualified name.
fn node_to_qualified_name(node: &CodeNode) -> String {
    // Extract path from ID: "func:brain/perception/signal_fusion.py::fuse" → "brain.perception.signal_fusion.fuse"
    let id = &node.id;
    let after_colon = id.split_once(':').map(|(_, rest)| rest).unwrap_or(id);
    let (path, name) = after_colon.split_once("::").unwrap_or((after_colon, ""));

    let mod_path = path.strip_suffix(".py").unwrap_or(path).replace('/', ".");

    if name.is_empty() {
        mod_path
    } else if name.contains("::") {
        // method: "Class::method" → "module.Class.method"
        format!("{mod_path}.{}", name.replace("::", "."))
    } else {
        format!("{mod_path}.{name}")
    }
}

/// Extract file path from a CodeNode ID.
fn node_id_to_path(id: &str) -> String {
    let after_colon = id.split_once(':').map(|(_, rest)| rest).unwrap_or(id);
    after_colon
        .split_once("::")
        .map(|(path, _)| path.to_string())
        .unwrap_or_else(|| after_colon.to_string())
}

/// Resolve an import to a CodeNode ID.
fn resolve_import(imp: &ImportInfo, resolver: &NameResolver) -> Option<String> {
    // Try exact module match (e.g. "brain.utils.helper" → "mod:brain/utils/helper.py")
    let mod_id = format!("mod:{}.py", imp.module.replace('.', "/"));
    if let Some(id) = resolver.resolve(&mod_id) {
        return Some(id.clone());
    }

    // Try file ID
    let file_id = format!("file:{}.py", imp.module.replace('.', "/"));
    if let Some(id) = resolver.resolve(&file_id) {
        return Some(id.clone());
    }

    // Try module name directly
    resolver.resolve(&imp.module).cloned()
}

/// Extract base class names from a class signature like "class Child(Parent, Mixin)".
fn extract_bases_from_signature(sig: &str) -> Vec<String> {
    let Some(start) = sig.find('(') else {
        return Vec::new();
    };
    let Some(end) = sig.rfind(')') else {
        return Vec::new();
    };
    if start >= end {
        return Vec::new();
    }

    sig[start + 1..end]
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Extract cross-file edges using the RustModuleResolver.
///
/// Creates edges for:
/// - Resolved `use` imports → specific CodeNode (replaces generic module edges)
/// - `impl Trait for Type` → ImplementsTrait edge
/// - `pub use` re-exports → ReExports edge
/// - `#[test]` functions → TestTargets edge (naming convention: test_X targets X)
pub fn extract_cross_file_edges(
    parse_results: &[ParseResult],
    resolver: &crate::module_resolver::RustModuleResolver,
) -> Vec<CodeEdge> {
    let mut edges = Vec::new();

    let all_nodes: Vec<&CodeNode> = parse_results.iter().flat_map(|r| &r.nodes).collect();

    // 1. Resolved import edges (use statements → specific targets)
    let all_imports: Vec<&ImportInfo> = parse_results.iter().flat_map(|r| &r.imports).collect();
    for imp in &all_imports {
        let resolved = resolver.resolve_use(&imp.module, &imp.names);
        for (name, target_id) in &resolved {
            edges.push(CodeEdge {
                source_id: imp.file_node_id.clone(),
                target_id: target_id.clone(),
                predicate: CodeEdgePredicate::Imports,
                weight: None,
                commit_id: None,
            });
            // Check if this is a re-export (pub use)
            // We detect re-exports by checking if the importing node is a RustUse with "pub" in signature
            let use_node = all_nodes.iter().find(|n| {
                n.kind == CodeNodeKind::RustUse
                    && n.name == *name
                    && n.parent_id.as_deref() == Some(&imp.file_node_id)
            });
            if let Some(use_node) = use_node
                && use_node
                    .signature
                    .as_ref()
                    .is_some_and(|s| s.starts_with("pub"))
            {
                edges.push(CodeEdge {
                    source_id: imp.file_node_id.clone(),
                    target_id: target_id.clone(),
                    predicate: CodeEdgePredicate::ReExports,
                    weight: None,
                    commit_id: None,
                });
            }
        }
    }

    // 2. ImplementsTrait edges (from RustImpl nodes)
    for node in &all_nodes {
        if node.kind != CodeNodeKind::RustImpl {
            continue;
        }
        // Signature pattern: "impl TraitName for TypeName" or "impl TypeName"
        if let Some(sig) = &node.signature
            && let Some(trait_name) = extract_trait_from_impl(sig)
        {
            let target_id = resolver
                .resolve_name(&trait_name)
                .unwrap_or_else(|| format!("ext:{trait_name}"));
            edges.push(CodeEdge {
                source_id: node.id.clone(),
                target_id,
                predicate: CodeEdgePredicate::ImplementsTrait,
                weight: None,
                commit_id: None,
            });
        }
    }

    // 3. TestTargets edges (naming convention: test_foo → foo)
    for node in &all_nodes {
        if node.kind != CodeNodeKind::RustTest && node.kind != CodeNodeKind::Test {
            continue;
        }
        if let Some(target_name) = node.name.strip_prefix("test_")
            && let Some(target_id) = resolver.resolve_name(target_name)
        {
            edges.push(CodeEdge {
                source_id: node.id.clone(),
                target_id,
                predicate: CodeEdgePredicate::TestTargets,
                weight: None,
                commit_id: None,
            });
        }
    }

    edges
}

/// Extract the trait name from an `impl Trait for Type` signature.
///
/// Returns `Some("TraitName")` for `impl TraitName for TypeName`,
/// Returns `None` for `impl TypeName` (inherent impl).
fn extract_trait_from_impl(sig: &str) -> Option<String> {
    let sig = sig.strip_prefix("impl").map(|s| s.trim())?;
    if let Some(for_idx) = sig.find(" for ") {
        let trait_part = sig[..for_idx].trim();
        // Handle generic bounds: "Display" from "impl Display for Foo<T>"
        let name = trait_part
            .split('<')
            .next()
            .unwrap_or(trait_part)
            .trim()
            .to_string();
        if name.is_empty() { None } else { Some(name) }
    } else {
        None // inherent impl, no trait
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_python_file;
    use std::path::PathBuf;

    const SAMPLE: &str = r#"
import os
from pathlib import Path

class Parent:
    """Base class."""
    def greet(self):
        return "hello"

class Child(Parent):
    """Child class."""
    def greet(self):
        return "hi"

def helper():
    """A helper."""
    return 42

def caller():
    """Calls helper."""
    x = helper()
    return x
"#;

    #[test]
    fn test_containment_edges() {
        let path = PathBuf::from("test.py");
        let result = parse_python_file(&path, SAMPLE).unwrap();
        let resolver = NameResolver::from_nodes(&result.nodes);
        let edges = extract_edges(&[result], &resolver);

        let containment: Vec<_> = edges
            .iter()
            .filter(|e| e.predicate == CodeEdgePredicate::Contains)
            .collect();

        // Should have containment edges for module, classes, methods, functions
        assert!(
            containment.len() >= 5,
            "Expected >= 5 containment edges, got {}",
            containment.len()
        );
    }

    #[test]
    fn test_import_edges() {
        let path = PathBuf::from("test.py");
        let result = parse_python_file(&path, SAMPLE).unwrap();
        let resolver = NameResolver::from_nodes(&result.nodes);
        let edges = extract_edges(&[result], &resolver);

        let imports: Vec<_> = edges
            .iter()
            .filter(|e| e.predicate == CodeEdgePredicate::Imports)
            .collect();

        assert!(
            imports.len() >= 2,
            "Expected >= 2 import edges, got {}",
            imports.len()
        );

        // External imports should have ext: prefix
        let ext_imports: Vec<_> = imports
            .iter()
            .filter(|e| e.target_id.starts_with("ext:"))
            .collect();
        assert!(
            !ext_imports.is_empty(),
            "External imports should have ext: prefix"
        );
    }

    #[test]
    fn test_inheritance_edges() {
        let path = PathBuf::from("test.py");
        let result = parse_python_file(&path, SAMPLE).unwrap();
        let resolver = NameResolver::from_nodes(&result.nodes);
        let edges = extract_edges(&[result], &resolver);

        let inheritance: Vec<_> = edges
            .iter()
            .filter(|e| e.predicate == CodeEdgePredicate::InheritsFrom)
            .collect();

        assert_eq!(
            inheritance.len(),
            1,
            "Should have 1 inheritance edge (Child→Parent)"
        );
        assert!(inheritance[0].source_id.contains("Child"));
    }

    #[test]
    fn test_extract_bases_from_signature() {
        assert_eq!(
            extract_bases_from_signature("class Child(Parent, Mixin)"),
            vec!["Parent", "Mixin"]
        );
        assert_eq!(
            extract_bases_from_signature("class Foo"),
            Vec::<String>::new()
        );
        assert_eq!(
            extract_bases_from_signature("class Bar()"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn test_name_resolver() {
        let nodes = vec![CodeNode {
            id: "func:brain/utils.py::helper".to_string(),
            kind: CodeNodeKind::Function,
            parent_id: None,
            name: "helper".to_string(),
            signature: None,
            docstring: None,
            body_hash: None,
            body: None,
            loc: None,
            cyclomatic_complexity: None,
            coverage_pct: None,
            last_modified: None,
            ..Default::default()
        }];

        let resolver = NameResolver::from_nodes(&nodes);

        // Resolve by short name
        assert_eq!(
            resolver.resolve("helper"),
            Some(&"func:brain/utils.py::helper".to_string())
        );

        // Resolve by full ID
        assert_eq!(
            resolver.resolve("func:brain/utils.py::helper"),
            Some(&"func:brain/utils.py::helper".to_string())
        );
    }
}
