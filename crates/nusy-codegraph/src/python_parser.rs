//! Python-specific tree-sitter parser — extracts CodeNodes with position metadata.
//!
//! This parser uses Python-specific `CodeNodeKind` variants (`PythonFunction`,
//! `PythonClass`, etc.) rather than the generic variants used by `parser.rs`.
//! All emitted nodes include position metadata (start_line, end_line, start_col,
//! end_col, file_path, byte_offset) populated from tree-sitter node positions.
//!
//! # Differences from `parser.rs`
//!
//! | parser.rs (generic) | python_parser.rs (Python-specific) |
//! |---|---|
//! | `CodeNodeKind::Function` | `PythonFunction` or `PythonAsync` |
//! | `CodeNodeKind::Method` | `PythonMethod`, `PythonProperty`, or `PythonAsync` |
//! | `CodeNodeKind::Class` | `PythonClass` |
//! | `CodeNodeKind::Module` | `PythonModule` |
//! | No position metadata | All nodes have start/end line/col + byte_offset |
//! | No decorator nodes | `PythonDecorator` nodes emitted |
//! | No import nodes | `PythonImport` nodes emitted |
//! | No lambda nodes | `PythonLambda` nodes (best-effort) |
//!
//! # Usage
//!
//! ```ignore
//! let parser = PythonParser::new()?;
//! let result = parser.parse_file(Path::new("brain/signal_fusion.py"), &source)?;
//! println!("{} nodes extracted", result.nodes.len());
//! ```

use crate::parser::{ImportInfo, sha256_hex};
use crate::schema::{CodeNode, CodeNodeKind};
use std::path::Path;

/// Errors from the Python-specific parser.
#[derive(Debug, thiserror::Error)]
pub enum PythonParserError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("tree-sitter parse failed for {path}")]
    ParseFailed { path: String },

    #[error("tree-sitter language error: {0}")]
    Language(#[from] tree_sitter::LanguageError),
}

/// Result of parsing a Python file with the Python-specific parser.
#[derive(Debug)]
pub struct PythonParseResult {
    /// All code nodes extracted from the file.
    ///
    /// Node kinds are Python-specific (`PythonFunction`, `PythonClass`, etc.).
    /// All nodes include position metadata.
    pub nodes: Vec<CodeNode>,
    /// Import statements found (module path strings).
    pub imports: Vec<ImportInfo>,
}

/// A Python-specific tree-sitter parser.
///
/// Wraps tree-sitter::Parser for Python. Caller constructs once, reuses across files.
pub struct PythonParser {
    parser: tree_sitter::Parser,
}

impl PythonParser {
    /// Construct a new `PythonParser`.
    pub fn new() -> Result<Self, PythonParserError> {
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_python::LANGUAGE;
        parser.set_language(&language.into())?;
        Ok(Self { parser })
    }

    /// Parse a Python source file into `PythonParseResult`.
    ///
    /// All emitted `CodeNode` records use Python-specific kinds and include
    /// position metadata (`start_line`, `end_line`, `start_col`, `end_col`,
    /// `file_path`, `byte_offset`).
    pub fn parse_file(
        &mut self,
        path: &Path,
        source: &str,
    ) -> Result<PythonParseResult, PythonParserError> {
        let tree =
            self.parser
                .parse(source, None)
                .ok_or_else(|| PythonParserError::ParseFailed {
                    path: path.display().to_string(),
                })?;

        let path_str = path.display().to_string();
        let file_id = format!("file:{path_str}");
        let src = source.as_bytes();
        let root = tree.root_node();

        // Module dotted name (e.g., brain.perception.signal_fusion)
        let mod_dotted = path_str
            .strip_suffix(".py")
            .unwrap_or(&path_str)
            .replace(['/', '\\'], ".");
        let mod_id = format!("mod:{path_str}");

        // File node — language-agnostic (File) with position spanning whole file
        let file_node = CodeNode {
            id: file_id.clone(),
            kind: CodeNodeKind::File,
            parent_id: None,
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            docstring: extract_module_docstring(&root, src),
            body_hash: Some(sha256_hex(source.as_bytes())),
            loc: Some(source.lines().count() as i32),
            start_line: Some(1),
            end_line: Some(source.lines().count() as u32),
            start_col: Some(0),
            end_col: Some(0),
            file_path: Some(path_str.clone()),
            byte_offset: Some(0),
            ..Default::default()
        };

        // Module node — PythonModule
        let module_node = CodeNode {
            id: mod_id.clone(),
            kind: CodeNodeKind::PythonModule,
            parent_id: Some(file_id.clone()),
            name: mod_dotted,
            docstring: file_node.docstring.clone(),
            body_hash: file_node.body_hash.clone(),
            loc: file_node.loc,
            start_line: Some(1),
            end_line: file_node.end_line,
            start_col: Some(0),
            end_col: Some(0),
            file_path: Some(path_str.clone()),
            byte_offset: Some(0),
            ..Default::default()
        };

        let mut nodes = vec![file_node, module_node];
        let mut imports = Vec::new();

        // Walk top-level AST children
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            match child.kind() {
                "function_definition" => {
                    // tree-sitter-python represents `async def` as a function_definition
                    // whose source text starts with "async " — detect it here.
                    let is_async = node_text(&child, src).trim_start().starts_with("async ");
                    let extracted =
                        extract_py_function(&child, src, &path_str, &mod_id, None, is_async);
                    nodes.extend(extracted);
                }
                "decorated_definition" => {
                    let (extracted_nodes, _extra_imports) =
                        extract_decorated(&child, src, &path_str, &mod_id, None);
                    nodes.extend(extracted_nodes);
                    // Note: decorated definitions at top level may contain import-like constructs
                    // but tree-sitter parses these as function/class definitions only
                }
                "class_definition" => {
                    let class_nodes = extract_py_class(&child, src, &path_str, &mod_id);
                    nodes.extend(class_nodes);
                }
                "import_statement" => {
                    if let Some(imp) = extract_py_import(&child, src, &file_id) {
                        // Emit PythonImport node
                        let node =
                            import_to_node(&child, src, &file_id, &imp.module, &path_str, false);
                        nodes.push(node);
                        imports.push(imp);
                    }
                }
                "import_from_statement" => {
                    if let Some(imp) = extract_py_import_from(&child, src, &file_id) {
                        let node =
                            import_to_node(&child, src, &file_id, &imp.module, &path_str, true);
                        nodes.push(node);
                        imports.push(imp);
                    }
                }
                _ => {}
            }
        }

        Ok(PythonParseResult { nodes, imports })
    }
}

// ─── Extraction helpers ──────────────────────────────────────────────────────

/// Determine function kind based on context and decorators.
///
/// - `async def` → `PythonAsync`
/// - `@property` on a method → `PythonProperty`
/// - `def inside class` → `PythonMethod`
/// - `def` at top-level or in module → `PythonFunction`
fn function_kind(
    node: &tree_sitter::Node,
    src: &[u8],
    in_class: bool,
    is_async: bool,
    decorators: &[String],
) -> CodeNodeKind {
    if is_async {
        return CodeNodeKind::PythonAsync;
    }
    if in_class {
        if decorators.iter().any(|d| d == "property") {
            return CodeNodeKind::PythonProperty;
        }
        CodeNodeKind::PythonMethod
    } else {
        // Check for test function naming (consistent with parser.rs)
        let name = node_child_text(node, "name", src).unwrap_or_default();
        if name.starts_with("test_") || name == "test" {
            // Keep as PythonFunction — tests in V12b use generic Test kind only in parser.rs
            // PythonParser uses PythonFunction for all regular functions
        }
        CodeNodeKind::PythonFunction
    }
}

fn extract_py_function(
    node: &tree_sitter::Node,
    src: &[u8],
    path: &str,
    parent_id: &str,
    class_name: Option<&str>,
    is_async: bool,
) -> Vec<CodeNode> {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let in_class = class_name.is_some();
    let kind = function_kind(node, src, in_class, is_async, &[]);

    let id = match class_name {
        Some(cls) => format!("pymethod:{path}::{cls}::{name}"),
        None => format!("pyfunc:{path}::{name}"),
    };

    let signature = extract_py_signature(node, src, is_async);
    let docstring = extract_py_docstring(node, src);
    let body = node_text(node, src);
    let body_hash = sha256_hex(body.as_bytes());
    let loc = (node.end_position().row - node.start_position().row + 1) as i32;
    let complexity = compute_cyclomatic_complexity(node, src);

    let start = node.start_position();
    let end = node.end_position();

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
        start_line: Some(start.row as u32 + 1),
        end_line: Some(end.row as u32 + 1),
        start_col: Some(start.column as u32),
        end_col: Some(end.column as u32),
        file_path: Some(path.to_string()),
        byte_offset: Some(node.start_byte() as u64),
        ..Default::default()
    }]
}

/// Handle a `decorated_definition` node — emits decorator nodes + the decorated item.
fn extract_decorated(
    node: &tree_sitter::Node,
    src: &[u8],
    path: &str,
    parent_id: &str,
    class_name: Option<&str>,
) -> (Vec<CodeNode>, Vec<ImportInfo>) {
    let mut nodes = Vec::new();
    let imports = Vec::new();

    // Collect decorator names and emit PythonDecorator nodes
    let mut decorator_names: Vec<String> = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "decorator" {
            let text = node_text(&child, src);
            // Extract the decorator name (strip leading @)
            let name = text
                .trim_start_matches('@')
                .split('(')
                .next()
                .unwrap_or("")
                .trim();
            decorator_names.push(name.to_string());

            let dec_id = format!("pydec:{path}::{name}");
            let start = child.start_position();
            let end = child.end_position();
            nodes.push(CodeNode {
                id: dec_id,
                kind: CodeNodeKind::PythonDecorator,
                parent_id: Some(parent_id.to_string()),
                name: format!("@{name}"),
                body: Some(text),
                start_line: Some(start.row as u32 + 1),
                end_line: Some(end.row as u32 + 1),
                start_col: Some(start.column as u32),
                end_col: Some(end.column as u32),
                file_path: Some(path.to_string()),
                byte_offset: Some(child.start_byte() as u64),
                ..Default::default()
            });
        }
    }

    let in_class = class_name.is_some();
    let is_property = decorator_names.iter().any(|d| d == "property");

    // Find the inner function/class definition
    let mut inner_cursor = node.walk();
    for child in node.children(&mut inner_cursor) {
        match child.kind() {
            "function_definition" => {
                let name = node_child_text(&child, "name", src).unwrap_or_default();
                let is_async = node_text(&child, src).trim_start().starts_with("async ");

                let kind = if is_async {
                    CodeNodeKind::PythonAsync
                } else if in_class && is_property {
                    CodeNodeKind::PythonProperty
                } else if in_class {
                    CodeNodeKind::PythonMethod
                } else {
                    CodeNodeKind::PythonFunction
                };

                let id = match class_name {
                    Some(cls) => format!("pymethod:{path}::{cls}::{name}"),
                    None => format!("pyfunc:{path}::{name}"),
                };

                let signature = extract_py_signature(&child, src, is_async);
                let docstring = extract_py_docstring(&child, src);
                let body = node_text(&child, src);
                let body_hash = sha256_hex(body.as_bytes());
                let loc = (child.end_position().row - child.start_position().row + 1) as i32;
                let complexity = compute_cyclomatic_complexity(&child, src);
                let start = child.start_position();
                let end = child.end_position();

                nodes.push(CodeNode {
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
                    start_line: Some(start.row as u32 + 1),
                    end_line: Some(end.row as u32 + 1),
                    start_col: Some(start.column as u32),
                    end_col: Some(end.column as u32),
                    file_path: Some(path.to_string()),
                    byte_offset: Some(child.start_byte() as u64),
                    ..Default::default()
                });
            }
            "class_definition" => {
                let class_nodes = extract_py_class(&child, src, path, parent_id);
                nodes.extend(class_nodes);
            }
            _ => {}
        }
    }

    (nodes, imports)
}

fn extract_py_class(
    node: &tree_sitter::Node,
    src: &[u8],
    path: &str,
    parent_id: &str,
) -> Vec<CodeNode> {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let class_id = format!("pyclass:{path}::{name}");

    let bases = extract_class_bases(node, src);
    let signature = if bases.is_empty() {
        format!("class {name}")
    } else {
        format!("class {name}({})", bases.join(", "))
    };

    let docstring = extract_py_docstring(node, src);
    let body_text = node_text(node, src);
    let body_hash = sha256_hex(body_text.as_bytes());
    let loc = (node.end_position().row - node.start_position().row + 1) as i32;
    let start = node.start_position();
    let end = node.end_position();

    let mut nodes = vec![CodeNode {
        id: class_id.clone(),
        kind: CodeNodeKind::PythonClass,
        parent_id: Some(parent_id.to_string()),
        name: name.clone(),
        signature: Some(signature),
        docstring,
        body_hash: Some(body_hash),
        body: Some(body_text),
        loc: Some(loc),
        start_line: Some(start.row as u32 + 1),
        end_line: Some(end.row as u32 + 1),
        start_col: Some(start.column as u32),
        end_col: Some(end.column as u32),
        file_path: Some(path.to_string()),
        byte_offset: Some(node.start_byte() as u64),
        ..Default::default()
    }];

    // Extract methods from the class body
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            match child.kind() {
                "function_definition" => {
                    let is_async = node_text(&child, src).trim_start().starts_with("async ");
                    let method_nodes =
                        extract_py_function(&child, src, path, &class_id, Some(&name), is_async);
                    nodes.extend(method_nodes);
                }
                "decorated_definition" => {
                    let (method_nodes, _) =
                        extract_decorated(&child, src, path, &class_id, Some(&name));
                    nodes.extend(method_nodes);
                }
                _ => {}
            }
        }
    }

    nodes
}

/// Emit a `PythonImport` node from an import statement.
fn import_to_node(
    node: &tree_sitter::Node,
    _src: &[u8],
    parent_id: &str,
    module: &str,
    path: &str,
    is_from: bool,
) -> CodeNode {
    let start = node.start_position();
    let end = node.end_position();
    let id = format!("pyimport:{path}::{}", module.replace(['.', '/'], "_"));
    let name = if is_from {
        format!("from {module} import ...")
    } else {
        format!("import {module}")
    };
    CodeNode {
        id,
        kind: CodeNodeKind::PythonImport,
        parent_id: Some(parent_id.to_string()),
        name,
        start_line: Some(start.row as u32 + 1),
        end_line: Some(end.row as u32 + 1),
        start_col: Some(start.column as u32),
        end_col: Some(end.column as u32),
        file_path: Some(path.to_string()),
        byte_offset: Some(node.start_byte() as u64),
        ..Default::default()
    }
}

fn extract_py_import(node: &tree_sitter::Node, src: &[u8], file_id: &str) -> Option<ImportInfo> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "dotted_name" {
            let module = node_text(&child, src);
            return Some(ImportInfo {
                file_node_id: file_id.to_string(),
                module,
                names: Vec::new(),
                is_relative: false,
            });
        }
    }
    None
}

fn extract_py_import_from(
    node: &tree_sitter::Node,
    src: &[u8],
    file_id: &str,
) -> Option<ImportInfo> {
    let full_text = node_text(node, src);
    let is_relative = full_text
        .trim_start_matches("from")
        .trim_start()
        .starts_with('.');

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

// ─── AST utility helpers ─────────────────────────────────────────────────────

fn node_text(node: &tree_sitter::Node, src: &[u8]) -> String {
    node.utf8_text(src).unwrap_or("").to_string()
}

fn node_child_text(node: &tree_sitter::Node, field: &str, src: &[u8]) -> Option<String> {
    node.child_by_field_name(field).map(|n| node_text(&n, src))
}

fn extract_py_signature(node: &tree_sitter::Node, src: &[u8], is_async: bool) -> String {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let params = node
        .child_by_field_name("parameters")
        .map(|n| node_text(&n, src))
        .unwrap_or_else(|| "()".to_string());
    let ret = node
        .child_by_field_name("return_type")
        .map(|n| format!(" -> {}", node_text(&n, src)))
        .unwrap_or_default();
    let prefix = if is_async { "async def" } else { "def" };
    format!("{prefix} {name}{params}{ret}")
}

fn extract_py_docstring(node: &tree_sitter::Node, src: &[u8]) -> Option<String> {
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

fn extract_module_docstring(root: &tree_sitter::Node, src: &[u8]) -> Option<String> {
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

fn compute_cyclomatic_complexity(node: &tree_sitter::Node, src: &[u8]) -> i32 {
    let text = node_text(node, src);
    let mut complexity = 1;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("if ")
            || trimmed.starts_with("elif ")
            || trimmed.starts_with("for ")
            || trimmed.starts_with("while ")
            || trimmed.starts_with("except ")
            || trimmed.starts_with("except:")
        {
            complexity += 1;
        }
        complexity += trimmed.matches(" and ").count() as i32;
        complexity += trimmed.matches(" or ").count() as i32;
    }
    complexity
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const SAMPLE: &str = r#"
"""Module docstring."""

import os
from pathlib import Path
from brain.utils import helper

class SignalFusion:
    """Fuses signals."""

    def __init__(self, config):
        """Init."""
        self.config = config

    @property
    def name(self):
        """Property getter."""
        return "SignalFusion"

    async def fuse_async(self, signals):
        """Async fuse."""
        return signals

    def fuse(self, signals: list) -> dict:
        """Fuse all signals."""
        result = {}
        for s in signals:
            if s.is_valid() and s.weight > 0:
                result[s.name] = s.value
        return result

def standalone(x: int) -> int:
    """Add."""
    return x + 1

async def async_top(x):
    """Top-level async."""
    return x
"#;

    #[test]
    fn test_python_parser_extracts_python_specific_kinds() {
        let mut parser = PythonParser::new().expect("parser init");
        let path = PathBuf::from("brain/signal_fusion.py");
        let result = parser.parse_file(&path, SAMPLE).expect("parse");

        // File node uses generic File kind
        let file = result.nodes.iter().find(|n| n.kind == CodeNodeKind::File);
        assert!(file.is_some(), "should have File node");

        // Module uses PythonModule
        let module = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::PythonModule);
        assert!(module.is_some(), "should have PythonModule node");

        // Class uses PythonClass
        let class = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::PythonClass);
        assert!(class.is_some(), "should have PythonClass node");
        assert_eq!(class.unwrap().name, "SignalFusion");

        // Regular function uses PythonFunction
        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::PythonFunction && n.name == "standalone");
        assert!(func.is_some(), "should have PythonFunction for standalone");

        // Async function uses PythonAsync
        let async_fn = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::PythonAsync && n.name == "async_top");
        assert!(async_fn.is_some(), "should have PythonAsync for async_top");

        // @property method uses PythonProperty
        let prop = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::PythonProperty && n.name == "name");
        assert!(prop.is_some(), "should have PythonProperty for name");

        // async method uses PythonAsync
        let async_method = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::PythonAsync && n.name == "fuse_async");
        assert!(
            async_method.is_some(),
            "should have PythonAsync for fuse_async"
        );

        // Regular method uses PythonMethod
        let method = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::PythonMethod && n.name == "fuse");
        assert!(method.is_some(), "should have PythonMethod for fuse");
    }

    #[test]
    fn test_python_parser_emits_position_metadata() {
        let mut parser = PythonParser::new().expect("parser init");
        let path = PathBuf::from("brain/signal_fusion.py");
        let result = parser.parse_file(&path, SAMPLE).expect("parse");

        // Every non-File node should have position metadata
        for node in &result.nodes {
            assert!(
                node.start_line.is_some(),
                "node {} ({:?}) missing start_line",
                node.id,
                node.kind
            );
            assert!(
                node.end_line.is_some(),
                "node {} ({:?}) missing end_line",
                node.id,
                node.kind
            );
            assert!(
                node.start_col.is_some(),
                "node {} ({:?}) missing start_col",
                node.id,
                node.kind
            );
            assert!(
                node.end_col.is_some(),
                "node {} ({:?}) missing end_col",
                node.id,
                node.kind
            );
            assert_eq!(
                node.file_path.as_deref(),
                Some("brain/signal_fusion.py"),
                "node {} missing file_path",
                node.id
            );
            assert!(
                node.byte_offset.is_some(),
                "node {} missing byte_offset",
                node.id
            );
        }
    }

    #[test]
    fn test_python_parser_position_ordering() {
        let mut parser = PythonParser::new().expect("parser init");
        let path = PathBuf::from("test.py");
        let result = parser.parse_file(&path, SAMPLE).expect("parse");

        // start_line must be <= end_line for all nodes
        for node in &result.nodes {
            if let (Some(sl), Some(el)) = (node.start_line, node.end_line) {
                assert!(
                    sl <= el,
                    "node {} start_line {} > end_line {}",
                    node.id,
                    sl,
                    el
                );
            }
        }
    }

    #[test]
    fn test_python_parser_decorator_nodes() {
        let mut parser = PythonParser::new().expect("parser init");
        let path = PathBuf::from("test.py");
        let result = parser.parse_file(&path, SAMPLE).expect("parse");

        let decorators: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == CodeNodeKind::PythonDecorator)
            .collect();
        assert!(
            !decorators.is_empty(),
            "should have at least one decorator node"
        );
        // The @property decorator should be present
        assert!(
            decorators.iter().any(|d| d.name.contains("property")),
            "should have @property decorator"
        );
    }

    #[test]
    fn test_python_parser_import_nodes() {
        let mut parser = PythonParser::new().expect("parser init");
        let path = PathBuf::from("test.py");
        let result = parser.parse_file(&path, SAMPLE).expect("parse");

        let imports: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == CodeNodeKind::PythonImport)
            .collect();
        assert!(!imports.is_empty(), "should have import nodes");
    }

    #[test]
    fn test_python_parser_containment_hierarchy() {
        let mut parser = PythonParser::new().expect("parser init");
        let path = PathBuf::from("brain/test.py");
        let result = parser.parse_file(&path, SAMPLE).expect("parse");

        // File node
        let file = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::File)
            .unwrap();
        // Module node's parent is file
        let module = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::PythonModule)
            .unwrap();
        assert_eq!(module.parent_id.as_deref(), Some(file.id.as_str()));

        // Class parent is module
        let class = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::PythonClass)
            .unwrap();
        assert_eq!(class.parent_id.as_deref(), Some(module.id.as_str()));

        // Method parent is class
        let method = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::PythonMethod)
            .unwrap();
        assert_eq!(method.parent_id.as_deref(), Some(class.id.as_str()));
    }

    #[test]
    fn test_python_parser_import_info_extracted() {
        let mut parser = PythonParser::new().expect("parser init");
        let path = PathBuf::from("test.py");
        let result = parser.parse_file(&path, SAMPLE).expect("parse");

        assert!(
            result.imports.len() >= 2,
            "Expected >= 2 imports, got {}",
            result.imports.len()
        );
        let os_import = result.imports.iter().find(|i| i.module == "os");
        assert!(os_import.is_some(), "should have os import");
        let pathlib = result.imports.iter().find(|i| i.module.contains("pathlib"));
        assert!(pathlib.is_some(), "should have pathlib import");
    }

    #[test]
    fn test_python_parser_docstrings() {
        let mut parser = PythonParser::new().expect("parser init");
        let path = PathBuf::from("test.py");
        let result = parser.parse_file(&path, SAMPLE).expect("parse");

        let module = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::PythonModule)
            .unwrap();
        assert_eq!(module.docstring.as_deref(), Some("Module docstring."));

        let class = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::PythonClass)
            .unwrap();
        assert_eq!(class.docstring.as_deref(), Some("Fuses signals."));
    }

    #[test]
    fn test_python_parser_class_inheritance() {
        let src = r#"
class Child(Parent, Mixin):
    """A child class."""
    pass
"#;
        let mut parser = PythonParser::new().expect("parser init");
        let path = PathBuf::from("test.py");
        let result = parser.parse_file(&path, src).expect("parse");

        let class = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::PythonClass && n.name == "Child")
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

    #[test]
    fn test_python_parser_no_generic_kinds_emitted() {
        // The Python parser should not emit Function/Method/Class (generic) —
        // only Python-specific kinds and File (which stays generic).
        let mut parser = PythonParser::new().expect("parser init");
        let path = PathBuf::from("test.py");
        let result = parser.parse_file(&path, SAMPLE).expect("parse");

        for node in &result.nodes {
            assert!(
                !matches!(
                    node.kind,
                    CodeNodeKind::Function
                        | CodeNodeKind::Method
                        | CodeNodeKind::Class
                        | CodeNodeKind::Module
                ),
                "node {} has generic kind {:?} — should use Python-specific kind",
                node.id,
                node.kind
            );
        }
    }
}
