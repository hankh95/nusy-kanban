//! tree-sitter Python parser — extract CodeNodes from Python source files.
//!
//! Parses Python source into a tree of CodeNodes representing files, classes,
//! functions, methods, and imports. Builds containment hierarchy via parent_id.

use crate::schema::{CodeNode, CodeNodeKind};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Errors from parsing operations.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("tree-sitter parse failed for {path}")]
    ParseFailed { path: String },

    #[error("tree-sitter language error: {0}")]
    Language(#[from] tree_sitter::LanguageError),
}

/// Result of parsing a Python file.
#[derive(Debug)]
pub struct ParseResult {
    /// All code nodes extracted from the file.
    pub nodes: Vec<CodeNode>,
    /// Import statements found (module path strings).
    pub imports: Vec<ImportInfo>,
}

/// An import statement extracted from source.
#[derive(Debug, Clone)]
pub struct ImportInfo {
    /// The file node ID that contains this import.
    pub file_node_id: String,
    /// The module being imported (e.g. "brain.perception.signal_fusion").
    pub module: String,
    /// Specific names imported (empty for `import X` style).
    pub names: Vec<String>,
    /// Whether this is a relative import.
    pub is_relative: bool,
}

/// Parse a Python source file into CodeNodes.
///
/// Returns nodes for the file, all classes, functions, methods,
/// and import information.
pub fn parse_python_file(path: &Path, source: &str) -> Result<ParseResult, ParseError> {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    parser.set_language(&language.into())?;

    let tree = parser
        .parse(source, None)
        .ok_or_else(|| ParseError::ParseFailed {
            path: path.display().to_string(),
        })?;

    let path_str = path.display().to_string();
    let file_id = format!("file:{path_str}");

    // Module ID (file without extension)
    let mod_path = path_str
        .strip_suffix(".py")
        .unwrap_or(&path_str)
        .replace('/', ".");
    let mod_id = format!("mod:{path_str}");

    // File node
    let file_node = CodeNode {
        id: file_id.clone(),
        kind: CodeNodeKind::File,
        parent_id: None,
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        signature: None,
        docstring: extract_module_docstring(source, &tree),
        body_hash: Some(sha256_hex(source.as_bytes())),
        body: None, // File-level body omitted (too large)
        loc: Some(source.lines().count() as i32),
        cyclomatic_complexity: None,
        coverage_pct: None,
        last_modified: None,
        ..Default::default()
    };

    // Module node (parent for top-level definitions)
    let module_node = CodeNode {
        id: mod_id.clone(),
        kind: CodeNodeKind::Module,
        parent_id: Some(file_id.clone()),
        name: mod_path,
        signature: None,
        docstring: file_node.docstring.clone(),
        body_hash: file_node.body_hash.clone(),
        body: None, // Module-level body omitted (too large)
        loc: file_node.loc,
        cyclomatic_complexity: None,
        coverage_pct: None,
        last_modified: None,
        ..Default::default()
    };

    let mut nodes = vec![file_node, module_node];
    let mut imports = Vec::new();

    let root = tree.root_node();
    let src = source.as_bytes();

    // Walk top-level children
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match child.kind() {
            "function_definition" | "decorated_definition" => {
                let func_node = child_with_kind(&child, "function_definition").unwrap_or(child);
                let extracted = extract_function(&func_node, src, &path_str, &mod_id, None);
                nodes.extend(extracted);
            }
            "class_definition" => {
                let class_nodes = extract_class(&child, src, &path_str, &mod_id);
                nodes.extend(class_nodes);
            }
            "import_statement" => {
                if let Some(imp) = extract_import(&child, src, &file_id, false) {
                    imports.push(imp);
                }
            }
            "import_from_statement" => {
                if let Some(imp) = extract_import_from(&child, src, &file_id) {
                    imports.push(imp);
                }
            }
            _ => {}
        }
    }

    Ok(ParseResult { nodes, imports })
}

// ─── Extraction helpers ─────────────────────────────────────────────────────

fn extract_function(
    node: &tree_sitter::Node,
    src: &[u8],
    path: &str,
    parent_id: &str,
    class_name: Option<&str>,
) -> Vec<CodeNode> {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let is_test = name.starts_with("test_") || name.starts_with("test");

    let kind = if class_name.is_some() {
        CodeNodeKind::Method
    } else if is_test {
        CodeNodeKind::Test
    } else {
        CodeNodeKind::Function
    };

    let id = match class_name {
        Some(cls) => format!("method:{path}::{cls}::{name}"),
        None => format!("func:{path}::{name}"),
    };

    let signature = extract_signature(node, src);
    let docstring = extract_docstring(node, src);
    let body = node_text(node, src);
    let body_hash = sha256_hex(body.as_bytes());
    let loc = (node.end_position().row - node.start_position().row + 1) as i32;
    let complexity = compute_cyclomatic_complexity(node, src);

    vec![CodeNode {
        id,
        kind,
        parent_id: Some(parent_id.to_string()),
        name,
        signature: Some(signature),
        docstring,
        body_hash: Some(body_hash),
        body: Some(body),
        loc: Some(loc),
        cyclomatic_complexity: Some(complexity),
        coverage_pct: None,
        last_modified: None,
        ..Default::default()
    }]
}

fn extract_class(
    node: &tree_sitter::Node,
    src: &[u8],
    path: &str,
    parent_id: &str,
) -> Vec<CodeNode> {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let class_id = format!("class:{path}::{name}");

    let bases = extract_class_bases(node, src);
    let signature = if bases.is_empty() {
        format!("class {name}")
    } else {
        format!("class {name}({})", bases.join(", "))
    };

    let docstring = extract_docstring(node, src);
    let body_text = node_text(node, src);
    let body_hash = sha256_hex(body_text.as_bytes());
    let loc = (node.end_position().row - node.start_position().row + 1) as i32;

    let mut nodes = vec![CodeNode {
        id: class_id.clone(),
        kind: CodeNodeKind::Class,
        parent_id: Some(parent_id.to_string()),
        name: name.clone(),
        signature: Some(signature),
        docstring,
        body_hash: Some(body_hash),
        body: Some(body_text),
        loc: Some(loc),
        cyclomatic_complexity: None,
        coverage_pct: None,
        last_modified: None,
        ..Default::default()
    }];

    // Extract methods from the class body
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            match child.kind() {
                "function_definition" | "decorated_definition" => {
                    let func_node = child_with_kind(&child, "function_definition").unwrap_or(child);
                    let method_nodes =
                        extract_function(&func_node, src, path, &class_id, Some(&name));
                    nodes.extend(method_nodes);
                }
                _ => {}
            }
        }
    }

    nodes
}

fn extract_import(
    node: &tree_sitter::Node,
    src: &[u8],
    file_id: &str,
    is_relative: bool,
) -> Option<ImportInfo> {
    // `import X` or `import X.Y.Z`
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "dotted_name" {
            let module = node_text(&child, src);
            return Some(ImportInfo {
                file_node_id: file_id.to_string(),
                module,
                names: Vec::new(),
                is_relative,
            });
        }
    }
    None
}

fn extract_import_from(node: &tree_sitter::Node, src: &[u8], file_id: &str) -> Option<ImportInfo> {
    // Parse `from X import Y, Z` using the full text approach
    // tree-sitter's child structure varies, so text parsing is more reliable
    let full_text = node_text(node, src);
    let is_relative = full_text
        .trim_start_matches("from")
        .trim_start()
        .starts_with('.');

    // Split on "import" keyword
    let parts: Vec<&str> = full_text.splitn(2, "import").collect();
    if parts.len() < 2 {
        return None;
    }

    let module = parts[0]
        .trim()
        .strip_prefix("from")
        .unwrap_or(parts[0])
        .trim()
        .to_string();

    let names: Vec<String> = parts[1]
        .split(',')
        .map(|s| {
            s.trim()
                .split(" as ")
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect();

    if module.is_empty() && names.is_empty() {
        return None;
    }

    Some(ImportInfo {
        file_node_id: file_id.to_string(),
        module,
        names,
        is_relative,
    })
}

// ─── AST utility helpers ────────────────────────────────────────────────────

fn node_text(node: &tree_sitter::Node, src: &[u8]) -> String {
    node.utf8_text(src).unwrap_or("").to_string()
}

fn node_child_text(node: &tree_sitter::Node, field: &str, src: &[u8]) -> Option<String> {
    node.child_by_field_name(field).map(|n| node_text(&n, src))
}

fn child_with_kind<'a>(node: &tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).find(|c| c.kind() == kind)
}

fn extract_signature(node: &tree_sitter::Node, src: &[u8]) -> String {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let params = node
        .child_by_field_name("parameters")
        .map(|n| node_text(&n, src))
        .unwrap_or_else(|| "()".to_string());
    let ret = node
        .child_by_field_name("return_type")
        .map(|n| format!(" -> {}", node_text(&n, src)))
        .unwrap_or_default();

    // Check for async
    let is_async = node
        .parent()
        .is_some_and(|p| p.kind() == "decorated_definition")
        || node_text(node, src).starts_with("async ");

    let prefix = if is_async { "async def" } else { "def" };
    format!("{prefix} {name}{params}{ret}")
}

fn extract_docstring(node: &tree_sitter::Node, src: &[u8]) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    let first_stmt = body.children(&mut cursor).next()?;

    if first_stmt.kind() == "expression_statement" {
        let mut inner_cursor = first_stmt.walk();
        let expr = first_stmt.children(&mut inner_cursor).next()?;
        if expr.kind() == "string" || expr.kind() == "concatenated_string" {
            let text = node_text(&expr, src);
            return Some(strip_docstring_quotes(&text));
        }
    }
    None
}

fn extract_module_docstring(source: &str, tree: &tree_sitter::Tree) -> Option<String> {
    let root = tree.root_node();
    let src = source.as_bytes();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if child.kind() == "expression_statement" {
            let mut inner = child.walk();
            if let Some(expr) = child.children(&mut inner).next()
                && (expr.kind() == "string" || expr.kind() == "concatenated_string")
            {
                return Some(strip_docstring_quotes(&node_text(&expr, src)));
            }
        }
        // Skip comments but stop at non-docstring statements
        if child.kind() != "comment" && child.kind() != "expression_statement" {
            break;
        }
    }
    None
}

fn strip_docstring_quotes(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(inner) = trimmed
        .strip_prefix("\"\"\"")
        .and_then(|s| s.strip_suffix("\"\"\""))
    {
        inner.trim().to_string()
    } else if let Some(inner) = trimmed
        .strip_prefix("'''")
        .and_then(|s| s.strip_suffix("'''"))
    {
        inner.trim().to_string()
    } else if let Some(inner) = trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        inner.trim().to_string()
    } else if let Some(inner) = trimmed
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
    {
        inner.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

fn extract_class_bases(node: &tree_sitter::Node, src: &[u8]) -> Vec<String> {
    let Some(superclasses) = node.child_by_field_name("superclasses") else {
        return Vec::new();
    };
    let mut bases = Vec::new();
    let mut cursor = superclasses.walk();
    for child in superclasses.children(&mut cursor) {
        if child.kind() == "identifier" || child.kind() == "attribute" {
            bases.push(node_text(&child, src));
        }
    }
    bases
}

/// Simple cyclomatic complexity: count branching keywords.
fn compute_cyclomatic_complexity(node: &tree_sitter::Node, src: &[u8]) -> i32 {
    let text = node_text(node, src);
    let mut complexity = 1; // base

    for line in text.lines() {
        let trimmed = line.trim();
        // Count branching constructs
        if trimmed.starts_with("if ")
            || trimmed.starts_with("elif ")
            || trimmed.starts_with("for ")
            || trimmed.starts_with("while ")
            || trimmed.starts_with("except ")
            || trimmed.starts_with("except:")
        {
            complexity += 1;
        }
        // Count boolean operators
        complexity += trimmed.matches(" and ").count() as i32;
        complexity += trimmed.matches(" or ").count() as i32;
    }

    complexity
}

/// Compute SHA-256 hex digest.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const SAMPLE_PYTHON: &str = r#"
"""Module docstring."""

import os
from pathlib import Path
from brain.utils import helper

class SignalFusion:
    """Fuses signals from multiple sources."""

    def __init__(self, config):
        """Initialize with config."""
        self.config = config

    def fuse(self, signals: list) -> dict:
        """Fuse all signals.

        Returns merged result.
        """
        result = {}
        for s in signals:
            if s.is_valid() and s.weight > 0:
                result[s.name] = s.value
            elif s.fallback:
                result[s.name] = s.fallback
        return result

    async def fuse_async(self, signals):
        """Async version."""
        return self.fuse(signals)

def standalone_function(x: int, y: int) -> int:
    """Add two numbers."""
    return x + y

def test_something():
    """A test function."""
    assert True
"#;

    #[test]
    fn test_parse_python_file_extracts_nodes() {
        let path = PathBuf::from("brain/perception/signal_fusion.py");
        let result = parse_python_file(&path, SAMPLE_PYTHON).expect("parse should succeed");

        // Should have: file, module, class, 3 methods (__init__, fuse, fuse_async),
        // standalone_function, test_something
        assert!(
            result.nodes.len() >= 7,
            "Expected >= 7 nodes, got {}",
            result.nodes.len()
        );

        // Check file node
        let file = result.nodes.iter().find(|n| n.kind == CodeNodeKind::File);
        assert!(file.is_some(), "should have file node");
        assert_eq!(file.unwrap().name, "signal_fusion.py");

        // Check module node
        let module = result.nodes.iter().find(|n| n.kind == CodeNodeKind::Module);
        assert!(module.is_some(), "should have module node");
        assert_eq!(module.unwrap().name, "brain.perception.signal_fusion");

        // Check class
        let class = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::Class && n.name == "SignalFusion");
        assert!(class.is_some(), "should have SignalFusion class");
        let cls = class.unwrap();
        assert!(cls.docstring.as_deref() == Some("Fuses signals from multiple sources."));

        // Check methods
        let fuse = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::Method && n.name == "fuse");
        assert!(fuse.is_some(), "should have fuse method");
        let fuse = fuse.unwrap();
        assert!(fuse.signature.as_ref().unwrap().contains("def fuse"));
        assert!(
            fuse.cyclomatic_complexity.unwrap() >= 3,
            "fuse has branches"
        );

        // Check standalone function
        let standalone = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::Function && n.name == "standalone_function");
        assert!(standalone.is_some(), "should have standalone_function");

        // Check test function
        let test_fn = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::Test && n.name == "test_something");
        assert!(test_fn.is_some(), "should have test_something as Test kind");
    }

    #[test]
    fn test_parse_extracts_imports() {
        let path = PathBuf::from("test.py");
        let result = parse_python_file(&path, SAMPLE_PYTHON).expect("parse should succeed");

        assert!(
            result.imports.len() >= 2,
            "Expected >= 2 imports, got {}",
            result.imports.len()
        );

        // Check for `import os`
        let os_import = result.imports.iter().find(|i| i.module == "os");
        assert!(os_import.is_some(), "should have os import");

        // Check for `from pathlib import Path`
        let pathlib = result.imports.iter().find(|i| i.module.contains("pathlib"));
        assert!(pathlib.is_some(), "should have pathlib import");
    }

    #[test]
    fn test_parse_extracts_docstrings() {
        let path = PathBuf::from("test.py");
        let result = parse_python_file(&path, SAMPLE_PYTHON).expect("parse should succeed");

        // Module docstring
        let module = result.nodes.iter().find(|n| n.kind == CodeNodeKind::Module);
        assert_eq!(
            module.unwrap().docstring.as_deref(),
            Some("Module docstring.")
        );
    }

    #[test]
    fn test_parse_computes_body_hash() {
        let path = PathBuf::from("test.py");
        let result = parse_python_file(&path, SAMPLE_PYTHON).expect("parse should succeed");

        // File node should have a body hash
        let file = result.nodes.iter().find(|n| n.kind == CodeNodeKind::File);
        assert!(file.unwrap().body_hash.is_some());

        // All functions/methods should have body hashes
        for node in &result.nodes {
            if matches!(
                node.kind,
                CodeNodeKind::Function | CodeNodeKind::Method | CodeNodeKind::Test
            ) {
                assert!(
                    node.body_hash.is_some(),
                    "{} should have body_hash",
                    node.id
                );
            }
        }
    }

    #[test]
    fn test_parse_containment_hierarchy() {
        let path = PathBuf::from("brain/test.py");
        let result = parse_python_file(&path, SAMPLE_PYTHON).expect("parse should succeed");

        // Module's parent is file
        let module = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::Module)
            .unwrap();
        let file = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::File)
            .unwrap();
        assert_eq!(module.parent_id.as_deref(), Some(file.id.as_str()));

        // Class parent is module
        let class = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::Class)
            .unwrap();
        assert_eq!(class.parent_id.as_deref(), Some(module.id.as_str()));

        // Method parent is class
        let method = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::Method)
            .unwrap();
        assert_eq!(method.parent_id.as_deref(), Some(class.id.as_str()));
    }

    #[test]
    fn test_cyclomatic_complexity() {
        let path = PathBuf::from("test.py");
        let result = parse_python_file(&path, SAMPLE_PYTHON).expect("parse should succeed");

        // standalone_function has no branches → complexity 1
        let standalone = result
            .nodes
            .iter()
            .find(|n| n.name == "standalone_function")
            .unwrap();
        assert_eq!(standalone.cyclomatic_complexity, Some(1));

        // fuse has for, if+and, elif → complexity >= 4
        let fuse = result.nodes.iter().find(|n| n.name == "fuse").unwrap();
        assert!(
            fuse.cyclomatic_complexity.unwrap() >= 4,
            "fuse complexity should be >= 4, got {}",
            fuse.cyclomatic_complexity.unwrap()
        );
    }

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex(b"hello");
        assert_eq!(hash.len(), 64);
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_parse_class_inheritance() {
        let src = r#"
class Child(Parent, Mixin):
    """A child class."""
    pass
"#;
        let path = PathBuf::from("test.py");
        let result = parse_python_file(&path, src).expect("parse should succeed");

        let class = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::Class && n.name == "Child")
            .unwrap();
        assert!(
            class.signature.as_ref().unwrap().contains("Parent"),
            "signature should include bases"
        );
        assert!(
            class.signature.as_ref().unwrap().contains("Mixin"),
            "signature should include all bases"
        );
    }
}
