//! TurtleBlockBuilder — auto-generates fenced turtle blocks for research items.
//!
//! When creating research board items, this module reads the SHACL TurtleBlockShape
//! from the item type's shape file and generates a turtle block stub with correct
//! namespace prefixes and predicate placeholders.
//!
//! Used by `nk create hypothesis "Title"` to auto-populate the body with a turtle
//! block linking to the parent paper, experiment, etc.

use crate::item_type::ItemType;
use crate::templates::ShapeLoader;

/// Build a turtle block for a research item, driven by its SHACL TurtleBlockShape.
///
/// Returns `None` for dev board types or types without a TurtleBlockShape (e.g., Idea).
/// Returns `Some(block)` with the fenced turtle block ready to append to the body.
pub fn build_turtle_block(
    loader: &ShapeLoader,
    item_type: &ItemType,
    item_id: &str,
) -> Option<String> {
    if !item_type.is_research() {
        return None;
    }

    let shape = loader.load_shape(item_type)?;
    let tb = shape.turtle_block.as_ref()?;

    // Use the template hint from the shape, substituting the actual item ID
    let block = tb
        .template_hint
        .replace("\\n", "\n")
        .replace("H-XXX", item_id)
        .replace("EXPR-XXX.X", item_id)
        .replace("EXPR-XXX", item_id)
        .replace("PAPER-XXX", item_id)
        .replace("M-XXX", item_id)
        .replace("LIT-XXX", item_id);

    Some(block)
}

/// Append a turtle block to an item body if the type supports it.
///
/// If the body already contains a fenced turtle block (`` ```turtle ``), no block is added.
/// Returns the potentially modified body.
pub fn append_turtle_block_if_needed(
    loader: &ShapeLoader,
    item_type: &ItemType,
    item_id: &str,
    body: &str,
) -> String {
    // Don't double-add if body already has a turtle block
    if body.contains("```turtle") {
        return body.to_string();
    }

    match build_turtle_block(loader, item_type, item_id) {
        Some(block) => {
            let mut result = body.to_string();
            if !result.ends_with('\n') {
                result.push('\n');
            }
            result.push('\n');
            result.push_str(&block);
            result.push('\n');
            result
        }
        None => body.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_shapes_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ontology/shapes")
    }

    fn make_loader() -> ShapeLoader {
        ShapeLoader::from_dir(&test_shapes_dir())
    }

    #[test]
    fn test_hypothesis_gets_turtle_block() {
        let loader = make_loader();
        let block = build_turtle_block(&loader, &ItemType::Hypothesis, "H-042");
        assert!(block.is_some());
        let block = block.unwrap();
        assert!(block.contains("@prefix hyp:"));
        assert!(block.contains("H-042"));
        assert!(block.contains("hyp:Hypothesis"));
    }

    #[test]
    fn test_experiment_gets_turtle_block_with_run_status() {
        let loader = make_loader();
        let block = build_turtle_block(&loader, &ItemType::Experiment, "EXPR-131.1");
        assert!(block.is_some());
        let block = block.unwrap();
        assert!(block.contains("@prefix expr:"));
        assert!(block.contains("runStatus"));
    }

    #[test]
    fn test_idea_has_no_turtle_block() {
        let loader = make_loader();
        let block = build_turtle_block(&loader, &ItemType::Idea, "IDEA-029");
        assert!(block.is_none());
    }

    #[test]
    fn test_dev_types_have_no_turtle_block() {
        let loader = make_loader();
        for it in &[
            ItemType::Expedition,
            ItemType::Chore,
            ItemType::Voyage,
            ItemType::Signal,
        ] {
            let block = build_turtle_block(&loader, it, "EX-3001");
            assert!(block.is_none(), "{:?} should not have turtle block", it);
        }
    }

    #[test]
    fn test_append_skips_if_already_present() {
        let loader = make_loader();
        let body = "# Test\n\n```turtle\n@prefix hyp: ...\n```\n";
        let result = append_turtle_block_if_needed(&loader, &ItemType::Hypothesis, "H-042", body);
        assert_eq!(result, body, "should not double-add turtle block");
    }

    #[test]
    fn test_append_adds_to_empty_body() {
        let loader = make_loader();
        let body = "# Test Hypothesis\n\n## Claim\n\nTODO";
        let result = append_turtle_block_if_needed(&loader, &ItemType::Hypothesis, "H-042", body);
        assert!(result.contains("```turtle") || result.contains("@prefix hyp:"));
        assert!(result.starts_with("# Test Hypothesis"));
    }

    #[test]
    fn test_measure_gets_turtle_block() {
        let loader = make_loader();
        let block = build_turtle_block(&loader, &ItemType::Measure, "M-098");
        assert!(block.is_some());
        let block = block.unwrap();
        assert!(block.contains("@prefix measure:"));
        assert!(block.contains("M-098"));
    }

    #[test]
    fn test_literature_gets_turtle_block() {
        let loader = make_loader();
        let block = build_turtle_block(&loader, &ItemType::Literature, "LIT-001");
        assert!(block.is_some());
        let block = block.unwrap();
        assert!(block.contains("@prefix lit:"));
    }
}
