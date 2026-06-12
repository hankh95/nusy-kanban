//! Tests for crates/nusy-kanban/ontology/shapes/dev/*.ttl
//!
//! EX-3218: SHACL Shapes — Dev Board Item Types (6 types)
//!
//! Validates that each shape file:
//! - exists and is readable
//! - declares the correct sh:NodeShape targeting the right kb: class
//! - has sh:minCount 1 on required fields with sh:message
//! - has sh:order on all properties (deterministic template generation)
//! - has sh:in on enum fields (priority, assignee)
//! - has ID pattern validation matching actual ID format
//! - has kb:requiredSection entries with kb:templateHint
//! - has kb:hasComment reference
//! - groups.ttl defines all expected PropertyGroup instances
//! - comment.ttl has all 7 CommentsTable properties

const SHAPES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ontology/shapes/dev");

fn shape_path(filename: &str) -> String {
    format!("{SHAPES_DIR}/{filename}")
}

fn load_shape(filename: &str) -> String {
    let path = shape_path(filename);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read shape file {path}: {e}"))
}

/// All 6 dev board shape files (not groups or comment).
const DEV_SHAPE_FILES: &[(&str, &str, &str, &str)] = &[
    // (filename,  NodeShape,          targetClass,       ID prefix)
    (
        "expedition.ttl",
        "kb:ExpeditionShape",
        "kb:Expedition",
        "EX-",
    ),
    ("voyage.ttl", "kb:VoyageShape", "kb:Voyage", "VY-"),
    ("chore.ttl", "kb:ChoreShape", "kb:Chore", "CH-"),
    ("hazard.ttl", "kb:HazardShape", "kb:Hazard", "HZ-"),
    ("signal.ttl", "kb:SignalShape", "kb:Signal", "SG-"),
    ("feature.ttl", "kb:FeatureShape", "kb:Feature", "FT-"),
];

// ---------------------------------------------------------------------------
// Existence and basic structure
// ---------------------------------------------------------------------------

#[test]
fn all_dev_shape_files_exist_and_are_readable() {
    for (file, _, _, _) in DEV_SHAPE_FILES {
        let content = load_shape(file);
        assert!(!content.is_empty(), "{file} must not be empty");
    }
    // Also check groups and comment
    assert!(
        !load_shape("groups.ttl").is_empty(),
        "groups.ttl must not be empty"
    );
    assert!(
        !load_shape("comment.ttl").is_empty(),
        "comment.ttl must not be empty"
    );
}

#[test]
fn all_shapes_declare_kb_prefix() {
    for (file, _, _, _) in DEV_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("@prefix kb:"),
            "{file}: missing @prefix kb: declaration"
        );
        assert!(
            content.contains("https://nusy.dev/kanban/"),
            "{file}: kb: must point to <https://nusy.dev/kanban/>"
        );
    }
}

#[test]
fn all_shapes_declare_sh_prefix() {
    for (file, _, _, _) in DEV_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("@prefix sh:"),
            "{file}: missing @prefix sh: declaration"
        );
        assert!(
            content.contains("http://www.w3.org/ns/shacl#"),
            "{file}: sh: must point to SHACL namespace"
        );
    }
}

// ---------------------------------------------------------------------------
// Shape declarations and targeting
// ---------------------------------------------------------------------------

#[test]
fn each_shape_declares_correct_node_shape() {
    for (file, shape_name, _, _) in DEV_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains(&format!("{shape_name} a sh:NodeShape")),
            "{file}: missing '{shape_name} a sh:NodeShape'"
        );
    }
}

#[test]
fn each_shape_targets_correct_class() {
    for (file, shape_name, target_class, _) in DEV_SHAPE_FILES {
        let content = load_shape(file);
        // sh:targetClass must appear in the shape's block
        let shape_pos = content
            .find(&format!("{shape_name} a sh:NodeShape"))
            .unwrap_or_else(|| panic!("{file}: NodeShape declaration not found"));
        let block = &content[shape_pos..];
        assert!(
            block.contains(&format!("sh:targetClass {target_class}")),
            "{file}: {shape_name} missing sh:targetClass {target_class}"
        );
    }
}

#[test]
fn each_shape_has_sh_description() {
    for (file, _, _, _) in DEV_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("sh:description"),
            "{file}: missing sh:description on shape"
        );
    }
}

// ---------------------------------------------------------------------------
// Property shapes: sh:order, sh:minCount, sh:message, sh:in
// ---------------------------------------------------------------------------

#[test]
fn all_shapes_have_sh_order_on_properties() {
    for (file, _, _, _) in DEV_SHAPE_FILES {
        let content = load_shape(file);
        // Every sh:property block must have sh:order
        let prop_count =
            content.matches("sh:property [").count() + content.matches("sh:property\n").count();
        let order_count = content.matches("sh:order").count();
        assert!(
            order_count >= prop_count,
            "{file}: found {prop_count} sh:property blocks but only {order_count} sh:order entries — all properties need sh:order"
        );
    }
}

#[test]
fn body_property_is_required_in_all_shapes() {
    for (file, _, _, _) in DEV_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("sh:path kb:body"),
            "{file}: missing sh:path kb:body"
        );
        // kb:body must have sh:minCount 1
        let body_pos = content
            .find("sh:path kb:body")
            .expect("body path not found");
        let block_end = (body_pos + 200).min(content.len());
        let block = &content[body_pos..block_end];
        assert!(
            block.contains("sh:minCount 1"),
            "{file}: kb:body must have sh:minCount 1"
        );
        assert!(
            block.contains("sh:message"),
            "{file}: kb:body must have sh:message when minCount 1"
        );
    }
}

#[test]
fn priority_has_sh_in_on_shapes_that_require_it() {
    // Expedition, chore, hazard, feature — priority is sh:minCount 1
    let required_priority = &["expedition.ttl", "chore.ttl", "hazard.ttl", "feature.ttl"];
    for file in required_priority {
        let content = load_shape(file);
        assert!(
            content.contains("sh:path kb:priority"),
            "{file}: missing sh:path kb:priority"
        );
        let prio_pos = content.find("sh:path kb:priority").unwrap();
        let block_end = (prio_pos + 300).min(content.len());
        let block = &content[prio_pos..block_end];
        assert!(
            block.contains("sh:in"),
            "{file}: kb:priority must have sh:in enum constraint"
        );
        assert!(
            block.contains("\"low\"") && block.contains("\"medium\"") && block.contains("\"high\""),
            "{file}: kb:priority sh:in must include low, medium, high"
        );
        assert!(
            block.contains("sh:minCount 1"),
            "{file}: kb:priority must be sh:minCount 1 for this type"
        );
    }
}

#[test]
fn assignee_has_sh_in_on_shapes_that_require_it() {
    let required_assignee = &["expedition.ttl", "chore.ttl", "feature.ttl"];
    for file in required_assignee {
        let content = load_shape(file);
        assert!(
            content.contains("sh:path kb:assignee"),
            "{file}: missing sh:path kb:assignee"
        );
        let pos = content.find("sh:path kb:assignee").unwrap();
        let block_end = (pos + 300).min(content.len());
        let block = &content[pos..block_end];
        assert!(
            block.contains("sh:in"),
            "{file}: kb:assignee must have sh:in enum constraint"
        );
        assert!(
            block.contains("\"M5\"") && block.contains("\"DGX\"") && block.contains("\"Mini\""),
            "{file}: kb:assignee sh:in must include M5, DGX, Mini"
        );
        assert!(
            block.contains("sh:minCount 1"),
            "{file}: kb:assignee must be sh:minCount 1 for this type"
        );
    }
}

// ---------------------------------------------------------------------------
// ID pattern validation
// ---------------------------------------------------------------------------

#[test]
fn each_shape_has_id_pattern_matching_actual_format() {
    for (file, _, _, id_prefix) in DEV_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("sh:path kb:id"),
            "{file}: missing sh:path kb:id property"
        );
        assert!(
            content.contains("sh:pattern"),
            "{file}: kb:id must have sh:pattern for format validation"
        );
        // The pattern must contain the correct prefix characters
        let prefix_chars = &id_prefix[..2]; // e.g. "EX", "VY", "CH"
        assert!(
            content.contains(prefix_chars),
            "{file}: sh:pattern must reference the correct ID prefix '{prefix_chars}'"
        );
    }
}

#[test]
fn id_is_exactly_one_per_item() {
    for (file, _, _, _) in DEV_SHAPE_FILES {
        let content = load_shape(file);
        let id_pos = content.find("sh:path kb:id").expect("kb:id not found");
        let block_end = (id_pos + 200).min(content.len());
        let block = &content[id_pos..block_end];
        assert!(
            block.contains("sh:minCount 1") && block.contains("sh:maxCount 1"),
            "{file}: kb:id must have sh:minCount 1 and sh:maxCount 1 (exactly one ID per item)"
        );
    }
}

// ---------------------------------------------------------------------------
// Body section structure (template generation annotations)
// ---------------------------------------------------------------------------

#[test]
fn all_shapes_have_required_sections_with_template_hints() {
    for (file, _, _, _) in DEV_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("kb:requiredSection"),
            "{file}: missing kb:requiredSection — required for template generation"
        );
        assert!(
            content.contains("kb:templateHint"),
            "{file}: missing kb:templateHint — required for nk templates output"
        );
        assert!(
            content.contains("kb:sectionName"),
            "{file}: missing kb:sectionName — required for section headers"
        );
    }
}

#[test]
fn all_shapes_have_section_ordering() {
    for (file, _, _, _) in DEV_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("kb:sectionOrder"),
            "{file}: missing kb:sectionOrder — required for deterministic template output"
        );
    }
}

#[test]
fn expedition_has_parity_check_and_dod_sections() {
    let content = load_shape("expedition.ttl");
    assert!(
        content.contains("V12/V13 Parity Check"),
        "expedition.ttl: missing 'V12/V13 Parity Check' section (required per project conventions)"
    );
    assert!(
        content.contains("Definition of Done"),
        "expedition.ttl: missing 'Definition of Done' section"
    );
    assert!(
        content.contains("Constraints"),
        "expedition.ttl: missing 'Constraints' section (prevents over-engineering)"
    );
}

#[test]
fn voyage_has_expeditions_table_section() {
    let content = load_shape("voyage.ttl");
    assert!(
        content.contains("Expeditions"),
        "voyage.ttl: missing 'Expeditions' section for child expedition table"
    );
    assert!(
        content.contains("Done When"),
        "voyage.ttl: missing 'Done When' section"
    );
}

// ---------------------------------------------------------------------------
// Comment shape reference
// ---------------------------------------------------------------------------

#[test]
fn all_dev_shapes_reference_comment_shape() {
    for (file, _, _, _) in DEV_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("kb:hasComment"),
            "{file}: missing kb:hasComment — all item types support comments"
        );
        assert!(
            content.contains("kb:CommentShape"),
            "{file}: kb:hasComment must reference kb:CommentShape"
        );
    }
}

// ---------------------------------------------------------------------------
// comment.ttl — CommentShape
// ---------------------------------------------------------------------------

#[test]
fn comment_shape_has_all_7_properties() {
    let content = load_shape("comment.ttl");
    let required_paths = &[
        "kb:comment_id",
        "kb:item_id",
        "kb:author",
        "kb:body",
        "kb:created_at",
        "kb:parent_comment_id",
        "kb:resolved",
    ];
    for path in required_paths {
        assert!(
            content.contains(path),
            "comment.ttl: missing property {path} (required by CommentsTable schema from EX-3244)"
        );
    }
}

#[test]
fn comment_required_fields_have_min_count_1() {
    let content = load_shape("comment.ttl");
    let required = &[
        "kb:comment_id",
        "kb:item_id",
        "kb:author",
        "kb:body",
        "kb:created_at",
    ];
    for path in required {
        let pos = content
            .find(path)
            .unwrap_or_else(|| panic!("comment.ttl: {path} not found"));
        let block_end = (pos + 200).min(content.len());
        let block = &content[pos..block_end];
        assert!(
            block.contains("sh:minCount 1"),
            "comment.ttl: {path} must have sh:minCount 1"
        );
    }
}

#[test]
fn comment_id_has_cmt_pattern() {
    let content = load_shape("comment.ttl");
    let pos = content
        .find("kb:comment_id")
        .expect("kb:comment_id not found");
    let block_end = (pos + 300).min(content.len());
    let block = &content[pos..block_end];
    assert!(
        block.contains("sh:pattern"),
        "comment.ttl: kb:comment_id must have sh:pattern"
    );
    assert!(
        block.contains("CMT"),
        "comment.ttl: comment_id pattern must reference CMT prefix"
    );
}

#[test]
fn comment_shape_targets_kb_comment_class() {
    let content = load_shape("comment.ttl");
    assert!(
        content.contains("sh:targetClass kb:Comment"),
        "comment.ttl: missing sh:targetClass kb:Comment"
    );
}

// ---------------------------------------------------------------------------
// groups.ttl — PropertyGroup definitions
// ---------------------------------------------------------------------------

#[test]
fn groups_file_declares_sh_property_group_type() {
    let content = load_shape("groups.ttl");
    assert!(
        content.contains("a sh:PropertyGroup"),
        "groups.ttl: must declare items as sh:PropertyGroup"
    );
}

#[test]
fn groups_file_has_all_expedition_groups() {
    let content = load_shape("groups.ttl");
    let expedition_groups = &[
        "kb:ParityCheckGroup",
        "kb:ContextGroup",
        "kb:PhasesGroup",
        "kb:TestsGroup",
        "kb:DoDGroup",
        "kb:ConstraintsGroup",
    ];
    for group in expedition_groups {
        assert!(
            content.contains(group),
            "groups.ttl: missing {group} (required by expedition.ttl)"
        );
    }
}

#[test]
fn groups_file_has_voyage_groups() {
    let content = load_shape("groups.ttl");
    let voyage_groups = &["kb:ProblemGroup", "kb:GoalGroup", "kb:DoneWhenGroup"];
    for group in voyage_groups {
        assert!(
            content.contains(group),
            "groups.ttl: missing {group} (required by voyage.ttl)"
        );
    }
}

#[test]
fn groups_file_has_hazard_and_signal_groups() {
    let content = load_shape("groups.ttl");
    let groups = &[
        "kb:RiskGroup",
        "kb:ImpactGroup",
        "kb:MitigationGroup",
        "kb:ObservationGroup",
        "kb:ConditionsGroup",
        "kb:RawDataGroup",
    ];
    for group in groups {
        assert!(content.contains(group), "groups.ttl: missing {group}");
    }
}

#[test]
fn all_groups_have_rdfs_label_and_sh_order() {
    let content = load_shape("groups.ttl");
    assert!(
        content.contains("rdfs:label"),
        "groups.ttl: groups must have rdfs:label (used as section header text)"
    );
    assert!(
        content.contains("sh:order"),
        "groups.ttl: groups must have sh:order for deterministic ordering"
    );
}

// ---------------------------------------------------------------------------
// No SHACL state transition constraints (those belong in EX-3220)
// ---------------------------------------------------------------------------

#[test]
fn dev_shapes_contain_no_state_transition_sparql() {
    for (file, _, _, _) in DEV_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            !content.contains("sh:SPARQLConstraint") && !content.contains("sh:sparql"),
            "{file}: must NOT contain SPARQL constraints — state transitions belong in EX-3220 (workflow shapes)"
        );
    }
}
