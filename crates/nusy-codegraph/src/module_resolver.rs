//! Rust module tree resolution for cross-file edge extraction.
//!
//! Builds a map from Rust module paths (`crate::graph::GraphStore`) to file paths
//! and CodeNode IDs. Used by the ingest pipeline to resolve `use` statements and
//! function calls into cross-file edges.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::schema::CodeNode;

/// Maps Rust module paths to file paths within a crate.
pub struct RustModuleResolver {
    /// Module path → file path (e.g., "crate::graph" → "src/graph.rs")
    module_to_file: HashMap<String, PathBuf>,
    /// Fully qualified name → CodeNode ID (e.g., "crate::graph::GraphStore" → "rust_struct:src/graph.rs::GraphStore")
    name_to_node_id: HashMap<String, String>,
    /// Crate name (e.g., "nusy_arrow_core")
    crate_name: String,
}

impl RustModuleResolver {
    /// Build a module resolver from a crate root directory.
    ///
    /// Walks `src/` to build the module tree from file structure.
    /// The crate name is derived from Cargo.toml or the directory name.
    pub fn from_crate(crate_root: &Path) -> Option<Self> {
        let src_dir = crate_root.join("src");
        if !src_dir.exists() {
            return None;
        }

        let crate_name = detect_crate_name(crate_root);
        let mut module_to_file = HashMap::new();

        // Root module
        if src_dir.join("lib.rs").exists() {
            module_to_file.insert("crate".to_string(), src_dir.join("lib.rs"));
        } else if src_dir.join("main.rs").exists() {
            module_to_file.insert("crate".to_string(), src_dir.join("main.rs"));
        }

        // Walk src/ for module files
        walk_modules(&src_dir, "crate", &mut module_to_file);

        Some(Self {
            module_to_file,
            name_to_node_id: HashMap::new(),
            crate_name,
        })
    }

    /// Populate the name→ID index from parsed CodeNodes.
    ///
    /// Call this after parsing all files to enable cross-file name resolution.
    pub fn index_nodes(&mut self, nodes: &[CodeNode]) {
        for node in nodes {
            if let Some(ref file_path) = node.file_path {
                // Build qualified name: "crate::module::ItemName"
                let module_path = self.file_to_module(Path::new(file_path));
                if let Some(mod_path) = module_path {
                    let qualified = format!("{}::{}", mod_path, node.name);
                    self.name_to_node_id.insert(qualified, node.id.clone());
                }
                // Also index by crate name (e.g., "nusy_arrow_core::graph::GraphStore")
                let module_path = self.file_to_module(Path::new(file_path));
                if let Some(mod_path) = module_path {
                    let external = mod_path.replacen("crate", &self.crate_name, 1);
                    let qualified = format!("{}::{}", external, node.name);
                    self.name_to_node_id.insert(qualified, node.id.clone());
                }
                // Direct name lookup (for same-module references)
                self.name_to_node_id
                    .entry(node.name.clone())
                    .or_insert_with(|| node.id.clone());
            }
        }
    }

    /// Resolve a `use` path to a CodeNode ID.
    ///
    /// Handles `crate::`, `self::`, `super::`, and external crate paths.
    /// Returns None if the target can't be resolved (e.g., external crate).
    pub fn resolve_use(&self, module: &str, names: &[String]) -> Vec<(String, String)> {
        let mut resolved = Vec::new();

        for name in names {
            if name == "*" {
                continue; // Skip glob imports
            }

            // Try: module::name (e.g., "crate::graph::GraphStore")
            let qualified = format!("{module}::{name}");
            if let Some(node_id) = self.name_to_node_id.get(&qualified) {
                resolved.push((name.clone(), node_id.clone()));
                continue;
            }

            // Try with crate name substitution
            let external = qualified.replacen("crate", &self.crate_name, 1);
            if let Some(node_id) = self.name_to_node_id.get(&external) {
                resolved.push((name.clone(), node_id.clone()));
                continue;
            }

            // Try just the name (unqualified)
            if let Some(node_id) = self.name_to_node_id.get(name.as_str()) {
                resolved.push((name.clone(), node_id.clone()));
            }
            // Silently skip unresolvable imports (external crates, etc.)
        }

        resolved
    }

    /// Resolve a type name or function call to a CodeNode ID.
    ///
    /// Tries active imports first, then same-module lookup.
    pub fn resolve_name(&self, name: &str) -> Option<String> {
        self.name_to_node_id.get(name).cloned()
    }

    /// Get the module path for a file.
    fn file_to_module(&self, file_path: &Path) -> Option<String> {
        for (mod_path, path) in &self.module_to_file {
            if path.ends_with(file_path) || file_path.ends_with(path) {
                return Some(mod_path.clone());
            }
        }
        // Try to derive from file path structure
        let path_str = file_path.to_string_lossy();
        if let Some(src_idx) = path_str.find("src/") {
            let relative = &path_str[src_idx + 4..];
            let module = relative
                .trim_end_matches(".rs")
                .trim_end_matches("/mod")
                .replace('/', "::");
            if module == "lib" || module == "main" {
                return Some("crate".to_string());
            }
            return Some(format!("crate::{module}"));
        }
        None
    }

    /// Number of indexed module paths.
    pub fn module_count(&self) -> usize {
        self.module_to_file.len()
    }

    /// Number of indexed names.
    pub fn name_count(&self) -> usize {
        self.name_to_node_id.len()
    }
}

/// Walk a directory tree to find Rust module files.
fn walk_modules(dir: &Path, parent_module: &str, map: &mut HashMap<String, PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if path.is_file() && name.ends_with(".rs") && name != "lib.rs" && name != "main.rs" {
            let mod_name = name.trim_end_matches(".rs");
            let module_path = format!("{parent_module}::{mod_name}");
            map.insert(module_path.clone(), path.clone());

            // Check for submodule directory
            let subdir = dir.join(mod_name);
            if subdir.is_dir() {
                walk_modules(&subdir, &module_path, map);
            }
        } else if path.is_dir() && path.join("mod.rs").exists() {
            let module_path = format!("{parent_module}::{name}");
            map.insert(module_path.clone(), path.join("mod.rs"));
            walk_modules(&path, &module_path, map);
        }
    }
}

/// Detect crate name from Cargo.toml or directory name.
fn detect_crate_name(crate_root: &Path) -> String {
    let cargo_toml = crate_root.join("Cargo.toml");
    if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
        for line in content.lines() {
            if let Some(name) = line.strip_prefix("name") {
                let name = name.trim().trim_start_matches('=').trim().trim_matches('"');
                return name.replace('-', "_");
            }
        }
    }
    crate_root
        .file_name()
        .map(|n| n.to_string_lossy().replace('-', "_"))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{CodeNode, CodeNodeKind};

    #[test]
    fn test_from_crate_finds_modules() {
        // Use the nusy-codegraph crate itself as test data
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let resolver = RustModuleResolver::from_crate(&crate_root).expect("should resolve");

        assert!(resolver.module_count() > 0);
        // Should find at least: crate, crate::schema, crate::parser, crate::ingest, crate::rust_parser
    }

    #[test]
    fn test_index_nodes_and_resolve() {
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut resolver = RustModuleResolver::from_crate(&crate_root).expect("should resolve");

        let nodes = vec![CodeNode {
            id: "rust_struct:src/schema.rs::CodeNode".to_string(),
            kind: CodeNodeKind::RustStruct,
            name: "CodeNode".to_string(),
            file_path: Some("src/schema.rs".to_string()),
            ..Default::default()
        }];

        resolver.index_nodes(&nodes);
        assert!(resolver.name_count() > 0);

        // Should resolve by name
        let resolved = resolver.resolve_name("CodeNode");
        assert!(resolved.is_some());
        assert!(resolved.unwrap().contains("CodeNode"));
    }

    #[test]
    fn test_resolve_use_crate_path() {
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut resolver = RustModuleResolver::from_crate(&crate_root).expect("should resolve");

        let nodes = vec![CodeNode {
            id: "rust_fn:src/ingest.rs::ingest_directory".to_string(),
            kind: CodeNodeKind::RustFn,
            name: "ingest_directory".to_string(),
            file_path: Some("src/ingest.rs".to_string()),
            ..Default::default()
        }];

        resolver.index_nodes(&nodes);

        let resolved = resolver.resolve_use("crate::ingest", &["ingest_directory".to_string()]);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, "ingest_directory");
    }

    #[test]
    fn test_resolve_external_crate_returns_empty() {
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let resolver = RustModuleResolver::from_crate(&crate_root).expect("should resolve");

        // External crate references should silently return empty
        let resolved = resolver.resolve_use("std::collections", &["HashMap".to_string()]);
        assert!(resolved.is_empty());
    }

    #[test]
    fn test_detect_crate_name() {
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let name = detect_crate_name(&crate_root);
        assert_eq!(name, "nusy_codegraph");
    }

    #[test]
    fn test_file_to_module() {
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let resolver = RustModuleResolver::from_crate(&crate_root).expect("should resolve");

        let module = resolver.file_to_module(Path::new("src/schema.rs"));
        assert_eq!(module, Some("crate::schema".to_string()));

        let module = resolver.file_to_module(Path::new("src/lib.rs"));
        assert_eq!(module, Some("crate".to_string()));
    }

    #[test]
    fn test_glob_imports_skipped() {
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let resolver = RustModuleResolver::from_crate(&crate_root).expect("should resolve");

        let resolved = resolver.resolve_use("crate::schema", &["*".to_string()]);
        assert!(resolved.is_empty());
    }
}
