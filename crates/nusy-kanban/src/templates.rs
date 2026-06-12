//! Shape-driven template generation for all 12 nusy-kanban item types.
//!
//! Reads SHACL shapes from `ontology/shapes/{dev,research}/` and generates
//! rich body templates with section headers, field guidance, and turtle block
//! skeletons for research types.
//!
//! Override hierarchy: project (`.yurtle-kanban/shapes/`) → built-in.

use crate::item_type::ItemType;
use std::path::{Path, PathBuf};

/// A parsed section from a SHACL shape's `kb:requiredSection`.
#[derive(Debug, Clone)]
pub struct ShapeSection {
    pub name: String,
    pub order: u32,
    pub heading_level: u32,
    pub template_hint: String,
    pub required: bool,
}

/// A parsed turtle block template from `kb:TurtleBlockShape`.
#[derive(Debug, Clone)]
pub struct TurtleBlock {
    pub template_hint: String,
}

/// A parsed SHACL shape for one item type.
#[derive(Debug, Clone)]
pub struct Shape {
    pub item_type: String,
    pub description: String,
    pub sections: Vec<ShapeSection>,
    pub turtle_block: Option<TurtleBlock>,
}

/// Summary of an item type for `nk templates` listing.
#[derive(Debug, Clone)]
pub struct TypeSummary {
    pub item_type: ItemType,
    pub description: String,
}

/// Loads SHACL shapes with override hierarchy.
pub struct ShapeLoader {
    project_shapes_dir: Option<PathBuf>,
    builtin_shapes_dir: PathBuf,
}

impl ShapeLoader {
    /// Create a loader with optional project-level overrides.
    pub fn new(root: &Path) -> Self {
        let project_dir = root.join(".yurtle-kanban/shapes");
        Self {
            project_shapes_dir: if project_dir.exists() {
                Some(project_dir)
            } else {
                None
            },
            builtin_shapes_dir: root.join("crates/nusy-kanban/ontology/shapes"),
        }
    }

    /// Create a loader pointing directly at a shapes directory (for testing).
    pub fn from_dir(shapes_dir: &Path) -> Self {
        Self {
            project_shapes_dir: None,
            builtin_shapes_dir: shapes_dir.to_path_buf(),
        }
    }

    /// Load the shape for an item type.
    pub fn load_shape(&self, item_type: &ItemType) -> Option<Shape> {
        let board_dir = if item_type.is_research() {
            "research"
        } else {
            "dev"
        };
        let filename = format!("{}.ttl", item_type.as_str());

        // Project override first
        if let Some(ref project_dir) = self.project_shapes_dir {
            let path = project_dir.join(board_dir).join(&filename);
            if let Ok(content) = std::fs::read_to_string(&path) {
                return Some(parse_shape(&content, item_type.as_str()));
            }
        }

        // Built-in
        let path = self.builtin_shapes_dir.join(board_dir).join(&filename);
        if let Ok(content) = std::fs::read_to_string(path) {
            return Some(parse_shape(&content, item_type.as_str()));
        }

        None
    }
}

/// Template generator driven by SHACL shapes.
pub struct TemplateGenerator {
    loader: ShapeLoader,
}

impl TemplateGenerator {
    pub fn new(loader: ShapeLoader) -> Self {
        Self { loader }
    }

    /// Generate a full body template for the given item type and title.
    pub fn generate(&self, item_type: &ItemType, title: &str) -> String {
        let shape = match self.loader.load_shape(item_type) {
            Some(s) => s,
            None => return format!("# {title}\n"),
        };

        let mut lines = Vec::new();
        lines.push(format!("# {title}"));
        lines.push(String::new());

        // Emit sections ordered by sh:order
        let mut sections = shape.sections.clone();
        sections.sort_by_key(|s| s.order);

        for section in &sections {
            let heading = "#".repeat(section.heading_level as usize);
            lines.push(format!("{heading} {}", section.name));
            lines.push(String::new());

            if !section.template_hint.is_empty() {
                // Unescape \n in template hints
                let hint = section.template_hint.replace("\\n", "\n");
                for hint_line in hint.lines() {
                    lines.push(hint_line.to_string());
                }
            } else if section.required {
                lines.push("<!-- TODO -->".to_string());
            } else {
                lines.push("<!-- Optional -->".to_string());
            }
            lines.push(String::new());
        }

        // Append turtle block for research types
        if let Some(tb) = &shape.turtle_block {
            let hint = tb.template_hint.replace("\\n", "\n");
            lines.push(hint);
            lines.push(String::new());
        }

        lines.join("\n")
    }

    /// List all item types with their descriptions.
    pub fn list_all(&self) -> Vec<TypeSummary> {
        let all_types = [
            ItemType::Expedition,
            ItemType::Voyage,
            ItemType::Chore,
            ItemType::Hazard,
            ItemType::Signal,
            ItemType::Feature,
            ItemType::Paper,
            ItemType::Hypothesis,
            ItemType::Experiment,
            ItemType::Measure,
            ItemType::Idea,
            ItemType::Literature,
        ];

        all_types
            .iter()
            .map(|it| {
                let desc = self
                    .loader
                    .load_shape(it)
                    .map(|s| s.description)
                    .unwrap_or_default();
                TypeSummary {
                    item_type: *it,
                    description: desc,
                }
            })
            .collect()
    }
}

// ─── Shape Parser ──────────────────────────────────────────────────────────

/// Parse a TTL shape file into a Shape struct.
///
/// This is a minimal parser — not a full Turtle parser. It extracts:
/// - `sh:description` on the NodeShape
/// - `kb:requiredSection` blocks with their properties
/// - `kb:TurtleBlockShape` block
fn parse_shape(content: &str, type_name: &str) -> Shape {
    let description = extract_node_description(content);
    let sections = extract_sections(content);
    let turtle_block = extract_turtle_block(content);

    Shape {
        item_type: type_name.to_string(),
        description,
        sections,
        turtle_block,
    }
}

/// Extract `sh:description "..."` from the NodeShape declaration.
fn extract_node_description(content: &str) -> String {
    // Look for sh:description on the NodeShape (first occurrence)
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("sh:description") {
            return extract_quoted_string(trimmed);
        }
    }
    String::new()
}

/// Extract all `kb:requiredSection` blocks.
fn extract_sections(content: &str) -> Vec<ShapeSection> {
    let mut sections = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        if lines[i].trim().starts_with("kb:requiredSection") {
            let mut name = String::new();
            let mut order = 0u32;
            let mut heading_level = 2u32;
            let mut template_hint = String::new();
            let mut required = false;

            // Parse until closing ] ;
            let mut j = i;
            while j < lines.len() {
                let t = lines[j].trim();

                if t.contains("kb:sectionName") {
                    name = extract_quoted_string(t);
                }
                if t.contains("sh:order") {
                    order = extract_number(t);
                }
                if t.contains("kb:headingLevel") {
                    heading_level = extract_number(t);
                }
                if t.contains("kb:templateHint") {
                    template_hint = extract_quoted_string(t);
                }
                if t.contains("kb:required") && !t.contains("requiredSection") {
                    required = t.contains("true");
                }
                if t.ends_with("] ;") || t.ends_with("] .") {
                    break;
                }
                j += 1;
            }

            if !name.is_empty() {
                sections.push(ShapeSection {
                    name,
                    order,
                    heading_level,
                    template_hint,
                    required,
                });
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }

    sections
}

/// Extract `kb:TurtleBlockShape` block.
fn extract_turtle_block(content: &str) -> Option<TurtleBlock> {
    let lines: Vec<&str> = content.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.trim().starts_with("kb:TurtleBlockShape") {
            let mut hint = String::new();
            let mut j = i;
            while j < lines.len() {
                let t = lines[j].trim();
                if t.contains("kb:templateHint") {
                    hint = extract_quoted_string(t);
                }
                if t.ends_with("] ;") || t.ends_with("] .") {
                    break;
                }
                j += 1;
            }
            if !hint.is_empty() {
                return Some(TurtleBlock {
                    template_hint: hint,
                });
            }
        }
    }
    None
}

/// Extract a quoted string value from a TTL property line.
fn extract_quoted_string(line: &str) -> String {
    if let Some(start) = line.find('"') {
        let rest = &line[start + 1..];
        if let Some(end) = rest.rfind('"') {
            return rest[..end].to_string();
        }
    }
    String::new()
}

/// Extract an integer value from a TTL property line.
fn extract_number(line: &str) -> u32 {
    line.split_whitespace()
        .filter_map(|w| {
            w.trim_end_matches(';')
                .trim_end_matches('.')
                .parse::<u32>()
                .ok()
        })
        .next()
        .unwrap_or(0)
}

/// Format the `nk templates` listing output.
pub fn format_type_listing(summaries: &[TypeSummary]) -> String {
    let mut lines = Vec::new();
    lines.push("Available item types:".to_string());
    lines.push(String::new());

    for s in summaries {
        let desc = if s.description.is_empty() {
            "(no description)".to_string()
        } else {
            s.description.clone()
        };
        lines.push(format!("  {:12} {}", s.item_type.as_str(), desc));
    }

    lines.push(String::new());
    lines.push("Usage: nk templates <type>".to_string());

    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_shapes_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ontology/shapes")
    }

    fn make_loader() -> ShapeLoader {
        ShapeLoader::from_dir(&test_shapes_dir())
    }

    #[test]
    fn test_expedition_template_has_sections() {
        let generator = TemplateGenerator::new(make_loader());
        let template = generator.generate(&ItemType::Expedition, "Test Expedition");

        assert!(template.contains("# Test Expedition"));
        assert!(template.contains("## V12/V13 Parity Check"));
        assert!(template.contains("## Context"));
        assert!(template.contains("## Phase 1:"));
        assert!(template.contains("## Tests"));
        assert!(template.contains("## Definition of Done"));
        assert!(template.contains("## Constraints"));
    }

    #[test]
    fn test_hypothesis_template_has_turtle_block() {
        let generator = TemplateGenerator::new(make_loader());
        let template = generator.generate(&ItemType::Hypothesis, "Test Hypothesis");

        assert!(template.contains("# Test Hypothesis"));
        assert!(template.contains("## Claim"));
        assert!(template.contains("## Rationale"));
        assert!(template.contains("@prefix hyp:"));
        assert!(template.contains("hyp:Hypothesis"));
    }

    #[test]
    fn test_list_all_has_12_types() {
        let generator = TemplateGenerator::new(make_loader());
        let summaries = generator.list_all();
        assert_eq!(summaries.len(), 12);
    }

    #[test]
    fn test_list_all_has_descriptions() {
        let generator = TemplateGenerator::new(make_loader());
        let summaries = generator.list_all();

        let expedition = summaries
            .iter()
            .find(|s| s.item_type == ItemType::Expedition)
            .expect("expedition in list");
        assert!(
            !expedition.description.is_empty(),
            "expedition should have a description"
        );
    }

    #[test]
    fn test_all_12_types_produce_non_trivial_templates() {
        let generator = TemplateGenerator::new(make_loader());
        let types = [
            ItemType::Expedition,
            ItemType::Voyage,
            ItemType::Chore,
            ItemType::Hazard,
            ItemType::Signal,
            ItemType::Feature,
            ItemType::Paper,
            ItemType::Hypothesis,
            ItemType::Experiment,
            ItemType::Measure,
            ItemType::Idea,
            ItemType::Literature,
        ];

        for it in &types {
            let template = generator.generate(it, "Test");
            assert!(
                template.contains("## "),
                "{} template should have section headers",
                it.as_str()
            );
            assert!(
                template.len() > 50,
                "{} template should be non-trivial (got {} bytes)",
                it.as_str(),
                template.len()
            );
        }
    }

    #[test]
    fn test_research_types_have_turtle_blocks() {
        let generator = TemplateGenerator::new(make_loader());
        let research_types = [ItemType::Hypothesis, ItemType::Experiment, ItemType::Paper];

        for it in &research_types {
            let template = generator.generate(it, "Test");
            assert!(
                template.contains("@prefix") || template.contains("```turtle"),
                "{} should include turtle block",
                it.as_str()
            );
        }
    }

    #[test]
    fn test_project_override_takes_precedence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let shapes = dir.path().join("dev");
        std::fs::create_dir_all(&shapes).expect("create shapes dir");

        // Write a custom expedition shape
        std::fs::write(
            shapes.join("expedition.ttl"),
            r#"
kb:ExpeditionShape a sh:NodeShape ;
    sh:description "Custom project expedition" ;

    kb:requiredSection [
        kb:sectionName "Custom Section" ; sh:order 1 ;
        kb:headingLevel 2 ;
        kb:templateHint "This is a custom template" ;
        kb:required true ] .
"#,
        )
        .expect("write shape");

        let loader = ShapeLoader {
            project_shapes_dir: Some(dir.path().to_path_buf()),
            builtin_shapes_dir: test_shapes_dir(),
        };
        let generator = TemplateGenerator::new(loader);
        let template = generator.generate(&ItemType::Expedition, "Override Test");

        assert!(template.contains("## Custom Section"));
        assert!(template.contains("This is a custom template"));
    }

    #[test]
    fn test_shape_description_extraction() {
        let content = r#"
kb:TestShape a sh:NodeShape ;
    sh:description "A test shape for validation" ;
    sh:property [ sh:path kb:id ] .
"#;
        let shape = parse_shape(content, "test");
        assert_eq!(shape.description, "A test shape for validation");
    }

    #[test]
    fn test_format_type_listing() {
        let summaries = vec![
            TypeSummary {
                item_type: ItemType::Expedition,
                description: "Multi-phase feature work".to_string(),
            },
            TypeSummary {
                item_type: ItemType::Chore,
                description: "Routine maintenance".to_string(),
            },
        ];

        let output = format_type_listing(&summaries);
        assert!(output.contains("expedition"));
        assert!(output.contains("Multi-phase feature work"));
        assert!(output.contains("chore"));
        assert!(output.contains("nk templates <type>"));
    }

    #[test]
    fn test_unknown_type_falls_back() {
        let loader = ShapeLoader::from_dir(Path::new("/nonexistent"));
        let generator = TemplateGenerator::new(loader);
        let template = generator.generate(&ItemType::Expedition, "Fallback");
        assert_eq!(template, "# Fallback\n");
    }
}
