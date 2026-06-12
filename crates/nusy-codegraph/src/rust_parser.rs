//! tree-sitter Rust parser — extract CodeNodes from Rust source files.
//!
//! Parses Rust source into a tree of CodeNodes representing files, functions,
//! structs, enums, traits, impl blocks, methods, macros, use declarations,
//! const/static items, type aliases, and modules. Builds containment hierarchy
//! via parent_id and extracts import information from use declarations.

use crate::parser::{ImportInfo, ParseError, ParseResult, sha256_hex};
use crate::schema::{CodeNode, CodeNodeKind};
use std::path::Path;

/// Parse a Rust source file into CodeNodes.
///
/// Returns nodes for the file and all top-level and nested items:
/// functions, structs, enums, traits, impl blocks (with methods),
/// macros, use declarations, const/static items, type aliases, and modules.
pub fn parse_rust_file(path: &Path, source: &str) -> Result<ParseResult, ParseError> {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_rust::LANGUAGE;
    parser.set_language(&language.into())?;

    let tree = parser
        .parse(source, None)
        .ok_or_else(|| ParseError::ParseFailed {
            path: path.display().to_string(),
        })?;

    let path_str = path.display().to_string();
    let file_id = format!("file:{path_str}");

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
        docstring: None,
        body_hash: Some(sha256_hex(source.as_bytes())),
        body: None, // File-level body omitted (too large)
        loc: Some(source.lines().count() as i32),
        file_path: Some(path_str.clone()),
        ..Default::default()
    };

    let mut nodes = vec![file_node];
    let mut imports = Vec::new();

    let root = tree.root_node();
    let src = source.as_bytes();

    // Walk top-level children
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        extract_item(&child, src, &path_str, &file_id, &mut nodes, &mut imports);
    }

    Ok(ParseResult { nodes, imports })
}

// ---- Extraction dispatcher --------------------------------------------------

fn extract_item(
    node: &tree_sitter::Node,
    src: &[u8],
    path: &str,
    parent_id: &str,
    nodes: &mut Vec<CodeNode>,
    imports: &mut Vec<ImportInfo>,
) {
    match node.kind() {
        "function_item" => {
            let is_test = has_test_attribute(node, src);
            let extracted = extract_function(node, src, path, parent_id, is_test);
            nodes.push(extracted);
        }
        "struct_item" => {
            nodes.push(extract_struct(node, src, path, parent_id));
        }
        "enum_item" => {
            nodes.push(extract_enum(node, src, path, parent_id));
        }
        "impl_item" => {
            let impl_nodes = extract_impl(node, src, path, parent_id);
            nodes.extend(impl_nodes);
        }
        "trait_item" => {
            let trait_nodes = extract_trait(node, src, path, parent_id);
            nodes.extend(trait_nodes);
        }
        "mod_item" => {
            let mod_nodes = extract_mod(node, src, path, parent_id, imports);
            nodes.extend(mod_nodes);
        }
        "macro_definition" => {
            nodes.push(extract_macro(node, src, path, parent_id));
        }
        "use_declaration" => {
            let (use_node, import) = extract_use(node, src, path, parent_id);
            nodes.push(use_node);
            if let Some(imp) = import {
                imports.push(imp);
            }
        }
        "const_item" => {
            nodes.push(extract_const(node, src, path, parent_id));
        }
        "static_item" => {
            nodes.push(extract_static(node, src, path, parent_id));
        }
        "type_item" => {
            nodes.push(extract_type_alias(node, src, path, parent_id));
        }
        // Handle attributed items (e.g., #[test] fn ...)
        "attribute_item" => {
            // Attributes are handled by peeking at siblings in the parent context;
            // nothing to extract standalone.
        }
        _ => {}
    }
}

// ---- Individual extractors --------------------------------------------------

fn extract_function(
    node: &tree_sitter::Node,
    src: &[u8],
    path: &str,
    parent_id: &str,
    is_test: bool,
) -> CodeNode {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let kind = if is_test {
        CodeNodeKind::RustTest
    } else {
        CodeNodeKind::RustFn
    };

    let id = if is_test {
        format!("rust_test:{path}::{name}")
    } else {
        format!("rust_fn:{path}::{name}")
    };

    let signature = extract_rust_fn_signature(node, src);
    let body = node_text(node, src);
    let body_hash = sha256_hex(body.as_bytes());
    let loc = (node.end_position().row - node.start_position().row + 1) as i32;

    CodeNode {
        id,
        kind,
        parent_id: Some(parent_id.to_string()),
        name,
        signature: Some(signature),
        docstring: extract_rust_doc_comment(node, src),
        body_hash: Some(body_hash),
        body: Some(body),
        loc: Some(loc),
        start_line: Some(node.start_position().row as u32 + 1),
        end_line: Some(node.end_position().row as u32 + 1),
        start_col: Some(node.start_position().column as u32),
        end_col: Some(node.end_position().column as u32),
        file_path: Some(path.to_string()),
        byte_offset: Some(node.start_byte() as u64),
        ..Default::default()
    }
}

fn extract_struct(node: &tree_sitter::Node, src: &[u8], path: &str, parent_id: &str) -> CodeNode {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let id = format!("rust_struct:{path}::{name}");
    let body = node_text(node, src);
    let body_hash = sha256_hex(body.as_bytes());
    let loc = (node.end_position().row - node.start_position().row + 1) as i32;

    // Signature: first line up to opening brace or semicolon
    let signature = extract_first_line_signature(&body);

    CodeNode {
        id,
        kind: CodeNodeKind::RustStruct,
        parent_id: Some(parent_id.to_string()),
        name,
        signature: Some(signature),
        docstring: extract_rust_doc_comment(node, src),
        body_hash: Some(body_hash),
        body: Some(body),
        loc: Some(loc),
        start_line: Some(node.start_position().row as u32 + 1),
        end_line: Some(node.end_position().row as u32 + 1),
        start_col: Some(node.start_position().column as u32),
        end_col: Some(node.end_position().column as u32),
        file_path: Some(path.to_string()),
        byte_offset: Some(node.start_byte() as u64),
        ..Default::default()
    }
}

fn extract_enum(node: &tree_sitter::Node, src: &[u8], path: &str, parent_id: &str) -> CodeNode {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let id = format!("rust_enum:{path}::{name}");
    let body = node_text(node, src);
    let body_hash = sha256_hex(body.as_bytes());
    let loc = (node.end_position().row - node.start_position().row + 1) as i32;
    let signature = extract_first_line_signature(&body);

    CodeNode {
        id,
        kind: CodeNodeKind::RustEnum,
        parent_id: Some(parent_id.to_string()),
        name,
        signature: Some(signature),
        docstring: extract_rust_doc_comment(node, src),
        body_hash: Some(body_hash),
        body: Some(body),
        loc: Some(loc),
        start_line: Some(node.start_position().row as u32 + 1),
        end_line: Some(node.end_position().row as u32 + 1),
        start_col: Some(node.start_position().column as u32),
        end_col: Some(node.end_position().column as u32),
        file_path: Some(path.to_string()),
        byte_offset: Some(node.start_byte() as u64),
        ..Default::default()
    }
}

fn extract_impl(
    node: &tree_sitter::Node,
    src: &[u8],
    path: &str,
    parent_id: &str,
) -> Vec<CodeNode> {
    let body_text = node_text(node, src);

    // Build impl name: "impl Foo" or "impl Trait for Foo"
    let impl_name = extract_impl_name(node, src);
    let id = format!("rust_impl:{path}::{impl_name}");

    let body_hash = sha256_hex(body_text.as_bytes());
    let loc = (node.end_position().row - node.start_position().row + 1) as i32;
    let signature = extract_first_line_signature(&body_text);

    let impl_node = CodeNode {
        id: id.clone(),
        kind: CodeNodeKind::RustImpl,
        parent_id: Some(parent_id.to_string()),
        name: impl_name,
        signature: Some(signature),
        docstring: extract_rust_doc_comment(node, src),
        body_hash: Some(body_hash),
        body: Some(body_text),
        loc: Some(loc),
        start_line: Some(node.start_position().row as u32 + 1),
        end_line: Some(node.end_position().row as u32 + 1),
        start_col: Some(node.start_position().column as u32),
        end_col: Some(node.end_position().column as u32),
        file_path: Some(path.to_string()),
        byte_offset: Some(node.start_byte() as u64),
        ..Default::default()
    };

    let mut nodes = vec![impl_node];

    // Extract methods from the impl body
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if child.kind() == "function_item" {
                let is_test = has_test_attribute(&child, src);
                let method_name = node_child_text(&child, "name", src).unwrap_or_default();
                let kind = if is_test {
                    CodeNodeKind::RustTest
                } else {
                    CodeNodeKind::RustMethod
                };
                let method_id = if is_test {
                    format!("rust_test:{path}::{method_name}")
                } else {
                    format!("rust_method:{path}::{method_name}")
                };

                let method_sig = extract_rust_fn_signature(&child, src);
                let method_body = node_text(&child, src);
                let method_hash = sha256_hex(method_body.as_bytes());
                let method_loc = (child.end_position().row - child.start_position().row + 1) as i32;

                nodes.push(CodeNode {
                    id: method_id,
                    kind,
                    parent_id: Some(id.clone()),
                    name: method_name,
                    signature: Some(method_sig),
                    docstring: extract_rust_doc_comment(&child, src),
                    body_hash: Some(method_hash),
                    body: Some(method_body),
                    loc: Some(method_loc),
                    start_line: Some(child.start_position().row as u32 + 1),
                    end_line: Some(child.end_position().row as u32 + 1),
                    start_col: Some(child.start_position().column as u32),
                    end_col: Some(child.end_position().column as u32),
                    file_path: Some(path.to_string()),
                    byte_offset: Some(child.start_byte() as u64),
                    ..Default::default()
                });
            }
        }
    }

    nodes
}

fn extract_trait(
    node: &tree_sitter::Node,
    src: &[u8],
    path: &str,
    parent_id: &str,
) -> Vec<CodeNode> {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let id = format!("rust_trait:{path}::{name}");
    let body_text = node_text(node, src);
    let body_hash = sha256_hex(body_text.as_bytes());
    let loc = (node.end_position().row - node.start_position().row + 1) as i32;
    let signature = extract_first_line_signature(&body_text);

    let trait_node = CodeNode {
        id: id.clone(),
        kind: CodeNodeKind::RustTrait,
        parent_id: Some(parent_id.to_string()),
        name,
        signature: Some(signature),
        docstring: extract_rust_doc_comment(node, src),
        body_hash: Some(body_hash),
        body: Some(body_text),
        loc: Some(loc),
        start_line: Some(node.start_position().row as u32 + 1),
        end_line: Some(node.end_position().row as u32 + 1),
        start_col: Some(node.start_position().column as u32),
        end_col: Some(node.end_position().column as u32),
        file_path: Some(path.to_string()),
        byte_offset: Some(node.start_byte() as u64),
        ..Default::default()
    };

    let mut nodes = vec![trait_node];

    // Extract methods from the trait body
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if child.kind() == "function_item" || child.kind() == "function_signature_item" {
                let method_name = node_child_text(&child, "name", src).unwrap_or_default();
                let method_id = format!("rust_method:{path}::{method_name}");
                let method_sig = extract_rust_fn_signature(&child, src);
                let method_body = node_text(&child, src);
                let method_hash = sha256_hex(method_body.as_bytes());
                let method_loc = (child.end_position().row - child.start_position().row + 1) as i32;

                nodes.push(CodeNode {
                    id: method_id,
                    kind: CodeNodeKind::RustMethod,
                    parent_id: Some(id.clone()),
                    name: method_name,
                    signature: Some(method_sig),
                    docstring: extract_rust_doc_comment(&child, src),
                    body_hash: Some(method_hash),
                    body: Some(method_body),
                    loc: Some(method_loc),
                    start_line: Some(child.start_position().row as u32 + 1),
                    end_line: Some(child.end_position().row as u32 + 1),
                    start_col: Some(child.start_position().column as u32),
                    end_col: Some(child.end_position().column as u32),
                    file_path: Some(path.to_string()),
                    byte_offset: Some(child.start_byte() as u64),
                    ..Default::default()
                });
            }
        }
    }

    nodes
}

fn extract_mod(
    node: &tree_sitter::Node,
    src: &[u8],
    path: &str,
    parent_id: &str,
    imports: &mut Vec<ImportInfo>,
) -> Vec<CodeNode> {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let id = format!("rust_mod:{path}::{name}");
    let body_text = node_text(node, src);
    let body_hash = sha256_hex(body_text.as_bytes());
    let loc = (node.end_position().row - node.start_position().row + 1) as i32;

    let mod_node = CodeNode {
        id: id.clone(),
        kind: CodeNodeKind::RustMod,
        parent_id: Some(parent_id.to_string()),
        name,
        signature: Some(extract_first_line_signature(&body_text)),
        docstring: extract_rust_doc_comment(node, src),
        body_hash: Some(body_hash),
        body: Some(body_text),
        loc: Some(loc),
        start_line: Some(node.start_position().row as u32 + 1),
        end_line: Some(node.end_position().row as u32 + 1),
        start_col: Some(node.start_position().column as u32),
        end_col: Some(node.end_position().column as u32),
        file_path: Some(path.to_string()),
        byte_offset: Some(node.start_byte() as u64),
        ..Default::default()
    };

    let mut nodes = vec![mod_node];

    // If the mod has a body (inline module), extract its children
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            extract_item(&child, src, path, &id, &mut nodes, imports);
        }
    }

    nodes
}

fn extract_macro(node: &tree_sitter::Node, src: &[u8], path: &str, parent_id: &str) -> CodeNode {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let id = format!("rust_macro:{path}::{name}");
    let body = node_text(node, src);
    let body_hash = sha256_hex(body.as_bytes());
    let loc = (node.end_position().row - node.start_position().row + 1) as i32;

    CodeNode {
        id,
        kind: CodeNodeKind::RustMacro,
        parent_id: Some(parent_id.to_string()),
        name,
        signature: Some(format!(
            "macro_rules! {}",
            node_child_text(node, "name", src).unwrap_or_default()
        )),
        docstring: extract_rust_doc_comment(node, src),
        body_hash: Some(body_hash),
        body: Some(body),
        loc: Some(loc),
        start_line: Some(node.start_position().row as u32 + 1),
        end_line: Some(node.end_position().row as u32 + 1),
        start_col: Some(node.start_position().column as u32),
        end_col: Some(node.end_position().column as u32),
        file_path: Some(path.to_string()),
        byte_offset: Some(node.start_byte() as u64),
        ..Default::default()
    }
}

fn extract_use(
    node: &tree_sitter::Node,
    src: &[u8],
    path: &str,
    parent_id: &str,
) -> (CodeNode, Option<ImportInfo>) {
    let full_text = node_text(node, src);
    let use_path = full_text
        .trim()
        .strip_prefix("use ")
        .unwrap_or(&full_text)
        .trim_end_matches(';')
        .trim()
        .to_string();

    // Parse the use path for ImportInfo
    let (module, names) = parse_use_path(&use_path);

    let id = format!("rust_use:{path}::{use_path}");
    let body_hash = sha256_hex(full_text.as_bytes());

    let code_node = CodeNode {
        id,
        kind: CodeNodeKind::RustUse,
        parent_id: Some(parent_id.to_string()),
        name: use_path.clone(),
        signature: Some(full_text.trim().to_string()),
        docstring: None,
        body_hash: Some(body_hash),
        body: Some(full_text),
        loc: Some(1),
        start_line: Some(node.start_position().row as u32 + 1),
        end_line: Some(node.end_position().row as u32 + 1),
        start_col: Some(node.start_position().column as u32),
        end_col: Some(node.end_position().column as u32),
        file_path: Some(path.to_string()),
        byte_offset: Some(node.start_byte() as u64),
        ..Default::default()
    };

    let import = ImportInfo {
        file_node_id: format!("file:{path}"),
        module,
        names,
        is_relative: false,
    };

    (code_node, Some(import))
}

fn extract_const(node: &tree_sitter::Node, src: &[u8], path: &str, parent_id: &str) -> CodeNode {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let id = format!("rust_const:{path}::{name}");
    let body = node_text(node, src);
    let body_hash = sha256_hex(body.as_bytes());
    let loc = (node.end_position().row - node.start_position().row + 1) as i32;

    CodeNode {
        id,
        kind: CodeNodeKind::RustConst,
        parent_id: Some(parent_id.to_string()),
        name,
        signature: Some(extract_first_line_signature(&body)),
        docstring: extract_rust_doc_comment(node, src),
        body_hash: Some(body_hash),
        body: Some(body),
        loc: Some(loc),
        start_line: Some(node.start_position().row as u32 + 1),
        end_line: Some(node.end_position().row as u32 + 1),
        start_col: Some(node.start_position().column as u32),
        end_col: Some(node.end_position().column as u32),
        file_path: Some(path.to_string()),
        byte_offset: Some(node.start_byte() as u64),
        ..Default::default()
    }
}

fn extract_static(node: &tree_sitter::Node, src: &[u8], path: &str, parent_id: &str) -> CodeNode {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let id = format!("rust_static:{path}::{name}");
    let body = node_text(node, src);
    let body_hash = sha256_hex(body.as_bytes());
    let loc = (node.end_position().row - node.start_position().row + 1) as i32;

    CodeNode {
        id,
        kind: CodeNodeKind::RustStatic,
        parent_id: Some(parent_id.to_string()),
        name,
        signature: Some(extract_first_line_signature(&body)),
        docstring: extract_rust_doc_comment(node, src),
        body_hash: Some(body_hash),
        body: Some(body),
        loc: Some(loc),
        start_line: Some(node.start_position().row as u32 + 1),
        end_line: Some(node.end_position().row as u32 + 1),
        start_col: Some(node.start_position().column as u32),
        end_col: Some(node.end_position().column as u32),
        file_path: Some(path.to_string()),
        byte_offset: Some(node.start_byte() as u64),
        ..Default::default()
    }
}

fn extract_type_alias(
    node: &tree_sitter::Node,
    src: &[u8],
    path: &str,
    parent_id: &str,
) -> CodeNode {
    let name = node_child_text(node, "name", src).unwrap_or_default();
    let id = format!("rust_type_alias:{path}::{name}");
    let body = node_text(node, src);
    let body_hash = sha256_hex(body.as_bytes());
    let loc = (node.end_position().row - node.start_position().row + 1) as i32;

    CodeNode {
        id,
        kind: CodeNodeKind::RustTypeAlias,
        parent_id: Some(parent_id.to_string()),
        name,
        signature: Some(body.trim().to_string()),
        docstring: extract_rust_doc_comment(node, src),
        body_hash: Some(body_hash),
        body: Some(body),
        loc: Some(loc),
        start_line: Some(node.start_position().row as u32 + 1),
        end_line: Some(node.end_position().row as u32 + 1),
        start_col: Some(node.start_position().column as u32),
        end_col: Some(node.end_position().column as u32),
        file_path: Some(path.to_string()),
        byte_offset: Some(node.start_byte() as u64),
        ..Default::default()
    }
}

// ---- AST utility helpers ----------------------------------------------------

fn node_text(node: &tree_sitter::Node, src: &[u8]) -> String {
    node.utf8_text(src).unwrap_or("").to_string()
}

fn node_child_text(node: &tree_sitter::Node, field: &str, src: &[u8]) -> Option<String> {
    node.child_by_field_name(field).map(|n| node_text(&n, src))
}

/// Extract function signature: `fn name(params) -> ReturnType`
fn extract_rust_fn_signature(node: &tree_sitter::Node, src: &[u8]) -> String {
    let full_text = node_text(node, src);
    // Signature is everything up to the opening brace (or semicolon for trait methods)
    if let Some(brace_pos) = full_text.find('{') {
        full_text[..brace_pos].trim().to_string()
    } else if let Some(semi_pos) = full_text.find(';') {
        full_text[..semi_pos].trim().to_string()
    } else {
        full_text.lines().next().unwrap_or("").trim().to_string()
    }
}

/// Extract the first line of a body as a signature (for structs, enums, etc.)
fn extract_first_line_signature(body: &str) -> String {
    let first_line = body.lines().next().unwrap_or("").trim();
    // Strip trailing brace if present
    let sig = first_line.trim_end_matches('{').trim();
    sig.to_string()
}

/// Extract impl name: "Foo" for `impl Foo { ... }` or "Trait for Foo" for `impl Trait for Foo { ... }`
fn extract_impl_name(node: &tree_sitter::Node, src: &[u8]) -> String {
    let full_text = node_text(node, src);
    // Extract text between "impl" and "{"
    let after_impl = full_text
        .trim()
        .strip_prefix("impl")
        .unwrap_or(&full_text)
        .trim();

    if let Some(brace_pos) = after_impl.find('{') {
        after_impl[..brace_pos].trim().to_string()
    } else {
        // External impl (no body)
        after_impl.trim().to_string()
    }
}

/// Check if a function has a `#[test]` attribute by looking at preceding siblings.
fn has_test_attribute(node: &tree_sitter::Node, src: &[u8]) -> bool {
    // Check preceding siblings for attribute_item containing "test"
    let mut sibling = node.prev_sibling();
    while let Some(sib) = sibling {
        if sib.kind() == "attribute_item" {
            let text = node_text(&sib, src);
            if text.contains("#[test]") || text.contains("#[tokio::test]") {
                return true;
            }
        } else if sib.kind() != "line_comment" && sib.kind() != "block_comment" {
            // Stop at non-attribute, non-comment nodes
            break;
        }
        sibling = sib.prev_sibling();
    }
    false
}

/// Extract Rust doc comments (/// or //!) preceding a node.
fn extract_rust_doc_comment(node: &tree_sitter::Node, src: &[u8]) -> Option<String> {
    let mut doc_lines = Vec::new();
    let mut sibling = node.prev_sibling();

    while let Some(sib) = sibling {
        if sib.kind() == "line_comment" {
            let text = node_text(&sib, src);
            if let Some(doc) = text.strip_prefix("///") {
                doc_lines.push(doc.trim_start_matches(' ').to_string());
            } else if let Some(doc) = text.strip_prefix("//!") {
                doc_lines.push(doc.trim_start_matches(' ').to_string());
            } else {
                break;
            }
        } else if sib.kind() == "attribute_item" {
            // Skip attributes (they can appear between doc comments and the item)
            sibling = sib.prev_sibling();
            continue;
        } else {
            break;
        }
        sibling = sib.prev_sibling();
    }

    if doc_lines.is_empty() {
        return None;
    }

    doc_lines.reverse();
    Some(doc_lines.join("\n").trim().to_string())
}

/// Parse a Rust use path into module and imported names.
///
/// Examples:
/// - `std::collections::HashMap` -> ("std::collections", ["HashMap"])
/// - `std::collections::{HashMap, BTreeMap}` -> ("std::collections", ["HashMap", "BTreeMap"])
/// - `crate::parser::ParseResult` -> ("crate::parser", ["ParseResult"])
/// - `std::io` -> ("std", ["io"])
fn parse_use_path(path: &str) -> (String, Vec<String>) {
    let path = path.trim();

    // Handle glob imports: `use std::io::*;`
    if path.ends_with("::*") {
        let module = path.strip_suffix("::*").unwrap_or(path).to_string();
        return (module, vec!["*".to_string()]);
    }

    // Handle group imports: `use std::collections::{HashMap, BTreeMap};`
    if let Some(brace_start) = path.find("::{") {
        let module = path[..brace_start].to_string();
        let group = &path[brace_start + 3..];
        let group = group.trim_end_matches('}');
        let names: Vec<String> = group
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        return (module, names);
    }

    // Simple path: `use std::collections::HashMap;`
    if let Some(last_sep) = path.rfind("::") {
        let module = path[..last_sep].to_string();
        let name = path[last_sep + 2..].to_string();
        (module, vec![name])
    } else {
        // Single segment: `use std;`
        (String::new(), vec![path.to_string()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_parse_simple_function() {
        let source = r#"fn foo() -> i32 {
    42
}"#;
        let path = PathBuf::from("src/lib.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        // Should have: file + function
        assert!(
            result.nodes.len() >= 2,
            "Expected >= 2 nodes, got {}",
            result.nodes.len()
        );

        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustFn && n.name == "foo");
        assert!(func.is_some(), "should have RustFn node for foo");
        let func = func.unwrap();
        assert!(func.signature.as_ref().unwrap().contains("fn foo() -> i32"));
        assert!(func.body.as_ref().unwrap().contains("42"));
        assert!(func.body_hash.is_some());
    }

    #[test]
    fn test_parse_struct_with_fields() {
        let source = r#"struct Foo {
    x: i32,
    y: String,
}"#;
        let path = PathBuf::from("src/lib.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        let st = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustStruct && n.name == "Foo");
        assert!(st.is_some(), "should have RustStruct node for Foo");
        let st = st.unwrap();
        assert!(st.signature.as_ref().unwrap().contains("struct Foo"));
    }

    #[test]
    fn test_parse_enum() {
        let source = r#"enum Color {
    Red,
    Blue,
}"#;
        let path = PathBuf::from("src/lib.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        let en = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustEnum && n.name == "Color");
        assert!(en.is_some(), "should have RustEnum node for Color");
    }

    #[test]
    fn test_parse_impl_block_with_methods() {
        let source = r#"struct Foo;

impl Foo {
    fn bar(&self) -> i32 {
        42
    }

    fn baz(&mut self, x: i32) {
        self.x = x;
    }
}"#;
        let path = PathBuf::from("src/lib.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        // Check impl node
        let imp = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustImpl);
        assert!(imp.is_some(), "should have RustImpl node");
        let imp = imp.unwrap();
        assert!(imp.name.contains("Foo"), "impl name should contain Foo");

        // Check methods
        let methods: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == CodeNodeKind::RustMethod)
            .collect();
        assert_eq!(methods.len(), 2, "should have 2 methods");

        let bar = methods.iter().find(|n| n.name == "bar");
        assert!(bar.is_some(), "should have method bar");
        let bar = bar.unwrap();
        assert_eq!(
            bar.parent_id.as_deref(),
            Some(imp.id.as_str()),
            "method parent should be impl"
        );
        assert!(
            bar.signature
                .as_ref()
                .unwrap()
                .contains("fn bar(&self) -> i32")
        );
    }

    #[test]
    fn test_parse_trait() {
        let source = r#"trait MyTrait {
    fn required(&self) -> i32;

    fn provided(&self) -> bool {
        true
    }
}"#;
        let path = PathBuf::from("src/lib.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        let tr = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustTrait && n.name == "MyTrait");
        assert!(tr.is_some(), "should have RustTrait node for MyTrait");

        // Check trait methods
        let methods: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == CodeNodeKind::RustMethod)
            .collect();
        assert!(methods.len() >= 1, "should have at least 1 method in trait");
    }

    #[test]
    fn test_parse_use_declaration() {
        let source = "use std::collections::HashMap;\n";
        let path = PathBuf::from("src/lib.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        let use_node = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustUse);
        assert!(use_node.is_some(), "should have RustUse node");

        assert!(!result.imports.is_empty(), "should have imports");
        let imp = &result.imports[0];
        assert_eq!(imp.module, "std::collections");
        assert_eq!(imp.names, vec!["HashMap"]);
    }

    #[test]
    fn test_parse_test_function() {
        let source = r#"#[test]
fn test_foo() {
    assert_eq!(1 + 1, 2);
}"#;
        let path = PathBuf::from("src/lib.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        let test_fn = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustTest && n.name == "test_foo");
        assert!(test_fn.is_some(), "should have RustTest node for test_foo");
    }

    #[test]
    fn test_parse_macro() {
        let source = r#"macro_rules! my_macro {
    ($x:expr) => {
        $x + 1
    };
}"#;
        let path = PathBuf::from("src/lib.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        let mac = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustMacro && n.name == "my_macro");
        assert!(mac.is_some(), "should have RustMacro node for my_macro");
    }

    #[test]
    fn test_parse_const_and_static() {
        let source = r#"const X: i32 = 5;
static Y: i32 = 10;"#;
        let path = PathBuf::from("src/lib.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        let const_node = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustConst && n.name == "X");
        assert!(const_node.is_some(), "should have RustConst node for X");

        let static_node = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustStatic && n.name == "Y");
        assert!(static_node.is_some(), "should have RustStatic node for Y");
    }

    #[test]
    fn test_position_metadata_populated() {
        let source = r#"fn hello() -> &'static str {
    "world"
}"#;
        let path = PathBuf::from("src/main.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustFn && n.name == "hello")
            .expect("should have function hello");

        assert!(func.start_line.is_some(), "start_line should be set");
        assert!(func.end_line.is_some(), "end_line should be set");
        assert!(func.start_col.is_some(), "start_col should be set");
        assert!(func.end_col.is_some(), "end_col should be set");
        assert_eq!(func.file_path.as_deref(), Some("src/main.rs"));
        assert!(func.byte_offset.is_some(), "byte_offset should be set");

        // start_line should be 1 (1-indexed)
        assert_eq!(func.start_line, Some(1));
        assert!(func.end_line.unwrap() >= 1);
    }

    #[test]
    fn test_containment_hierarchy() {
        let source = r#"mod inner {
    struct Data {
        value: i32,
    }

    impl Data {
        fn get_value(&self) -> i32 {
            self.value
        }
    }
}"#;
        let path = PathBuf::from("src/lib.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        let file_id = "file:src/lib.rs";

        // Module's parent is file
        let mod_node = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustMod && n.name == "inner")
            .expect("should have mod inner");
        assert_eq!(mod_node.parent_id.as_deref(), Some(file_id));

        // Struct's parent is module
        let struct_node = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustStruct && n.name == "Data")
            .expect("should have struct Data");
        assert_eq!(struct_node.parent_id.as_deref(), Some(mod_node.id.as_str()));

        // Impl's parent is module
        let impl_node = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustImpl)
            .expect("should have impl Data");
        assert_eq!(impl_node.parent_id.as_deref(), Some(mod_node.id.as_str()));

        // Method's parent is impl
        let method = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustMethod && n.name == "get_value")
            .expect("should have method get_value");
        assert_eq!(method.parent_id.as_deref(), Some(impl_node.id.as_str()));
    }

    #[test]
    fn test_parse_use_group() {
        let source = "use std::collections::{HashMap, BTreeMap};\n";
        let path = PathBuf::from("src/lib.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        assert!(!result.imports.is_empty(), "should have imports");
        let imp = &result.imports[0];
        assert_eq!(imp.module, "std::collections");
        assert!(imp.names.contains(&"HashMap".to_string()));
        assert!(imp.names.contains(&"BTreeMap".to_string()));
    }

    #[test]
    fn test_parse_type_alias() {
        let source = "type Result<T> = std::result::Result<T, MyError>;\n";
        let path = PathBuf::from("src/lib.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        let ta = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustTypeAlias && n.name == "Result");
        assert!(ta.is_some(), "should have RustTypeAlias node for Result");
    }

    #[test]
    fn test_parse_impl_test_method() {
        let source = r#"struct Foo;

impl Foo {
    fn normal(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_foo() {
        assert!(true);
    }
}"#;
        let path = PathBuf::from("src/lib.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        let test_fn = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustTest && n.name == "test_foo");
        assert!(test_fn.is_some(), "should have RustTest for test_foo");
    }

    #[test]
    fn test_parse_doc_comments() {
        let source = r#"/// This is a documented function.
/// It does important things.
fn documented() -> bool {
    true
}"#;
        let path = PathBuf::from("src/lib.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed");

        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == CodeNodeKind::RustFn && n.name == "documented")
            .expect("should have documented function");

        assert!(func.docstring.is_some(), "should have docstring");
        let doc = func.docstring.as_ref().unwrap();
        assert!(
            doc.contains("documented function"),
            "docstring should contain 'documented function', got: {doc}"
        );
    }

    #[test]
    fn test_parse_empty_file() {
        let source = "";
        let path = PathBuf::from("src/empty.rs");
        let result = parse_rust_file(&path, source).expect("parse should succeed on empty file");

        // Should have at least the file node
        assert_eq!(
            result.nodes.len(),
            1,
            "empty file should have just the file node"
        );
        assert_eq!(result.nodes[0].kind, CodeNodeKind::File);
    }

    #[test]
    fn test_parse_malformed_rust() {
        let source = "fn { broken syntax ;;;";
        let path = PathBuf::from("src/broken.rs");
        // tree-sitter is lenient — should not panic
        let result = parse_rust_file(&path, source);
        assert!(result.is_ok(), "malformed Rust should not panic");
    }

    #[test]
    fn test_use_path_parsing() {
        let (module, names) = parse_use_path("std::collections::HashMap");
        assert_eq!(module, "std::collections");
        assert_eq!(names, vec!["HashMap"]);

        let (module, names) = parse_use_path("std::collections::{HashMap, BTreeMap}");
        assert_eq!(module, "std::collections");
        assert_eq!(names, vec!["HashMap", "BTreeMap"]);

        let (module, names) = parse_use_path("std::io::*");
        assert_eq!(module, "std::io");
        assert_eq!(names, vec!["*"]);

        let (module, names) = parse_use_path("crate::parser::ParseResult");
        assert_eq!(module, "crate::parser");
        assert_eq!(names, vec!["ParseResult"]);
    }
}
