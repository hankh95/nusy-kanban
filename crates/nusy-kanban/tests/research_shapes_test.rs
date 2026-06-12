//! Tests for crates/nusy-kanban/ontology/shapes/research/*.ttl
//!
//! EX-3219: SHACL Shapes — Research Board Item Types (6 types)
//!
//! Validates that each shape file:
//! - exists and is readable
//! - declares the correct sh:NodeShape targeting the right kb: class
//! - has sh:minCount 1 on required fields with sh:message
//! - has sh:order on all properties (deterministic template generation)
//! - has ID pattern validation matching actual ID format
//! - has kb:requiredSection entries with kb:templateHint
//! - has kb:hasComment reference
//! - has kb:TurtleBlockShape on types that require turtle blocks (5 of 6)
//! - cross-type links: hypothesis→paper (kb:tests), experiment→hypothesis (kb:validates)
//! - groups.ttl defines all expected PropertyGroup instances

const SHAPES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ontology/shapes/research");

fn shape_path(filename: &str) -> String {
    format!("{SHAPES_DIR}/{filename}")
}

fn load_shape(filename: &str) -> String {
    let path = shape_path(filename);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read shape file {path}: {e}"))
}

/// All 6 research board shape files.
const RESEARCH_SHAPE_FILES: &[(&str, &str, &str, &str)] = &[
    // (filename,  NodeShape,            targetClass,        ID prefix)
    (
        "hypothesis.ttl",
        "kb:HypothesisShape",
        "kb:Hypothesis",
        "H-",
    ),
    (
        "experiment.ttl",
        "kb:ExperimentShape",
        "kb:Experiment",
        "EXPR-",
    ),
    ("paper.ttl", "kb:PaperShape", "kb:Paper", "PAPER-"),
    ("measure.ttl", "kb:MeasureShape", "kb:Measure", "M-"),
    ("idea.ttl", "kb:IdeaShape", "kb:Idea", "IDEA-"),
    (
        "literature.ttl",
        "kb:LiteratureShape",
        "kb:Literature",
        "LIT-",
    ),
];

/// Research types that require turtle blocks (5 of 6 — all except idea).
const TURTLE_BLOCK_TYPES: &[&str] = &[
    "hypothesis.ttl",
    "experiment.ttl",
    "paper.ttl",
    "measure.ttl",
    "literature.ttl",
];

// ---------------------------------------------------------------------------
// Existence and basic structure
// ---------------------------------------------------------------------------

#[test]
fn all_research_shape_files_exist_and_are_readable() {
    for (file, _, _, _) in RESEARCH_SHAPE_FILES {
        let content = load_shape(file);
        assert!(!content.is_empty(), "{file} must not be empty");
    }
    assert!(
        !load_shape("groups.ttl").is_empty(),
        "groups.ttl must not be empty"
    );
}

#[test]
fn all_shapes_declare_kb_prefix() {
    for (file, _, _, _) in RESEARCH_SHAPE_FILES {
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
    for (file, _, _, _) in RESEARCH_SHAPE_FILES {
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
    for (file, shape_name, _, _) in RESEARCH_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains(&format!("{shape_name} a sh:NodeShape")),
            "{file}: missing '{shape_name} a sh:NodeShape'"
        );
    }
}

#[test]
fn each_shape_targets_correct_class() {
    for (file, shape_name, target_class, _) in RESEARCH_SHAPE_FILES {
        let content = load_shape(file);
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
    for (file, _, _, _) in RESEARCH_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("sh:description"),
            "{file}: missing sh:description on shape"
        );
    }
}

// ---------------------------------------------------------------------------
// Property shapes: sh:order, sh:minCount, sh:message
// ---------------------------------------------------------------------------

#[test]
fn all_shapes_have_sh_order_on_properties() {
    for (file, _, _, _) in RESEARCH_SHAPE_FILES {
        let content = load_shape(file);
        let prop_count =
            content.matches("sh:property [").count() + content.matches("sh:property\n").count();
        let order_count = content.matches("sh:order").count();
        assert!(
            order_count >= prop_count,
            "{file}: found {prop_count} sh:property blocks but only {order_count} sh:order entries"
        );
    }
}

#[test]
fn body_property_is_required_in_all_shapes() {
    for (file, _, _, _) in RESEARCH_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("sh:path kb:body"),
            "{file}: missing sh:path kb:body"
        );
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

// ---------------------------------------------------------------------------
// ID pattern validation
// ---------------------------------------------------------------------------

#[test]
fn each_shape_has_id_pattern_matching_actual_format() {
    for (file, _, _, id_prefix) in RESEARCH_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("sh:path kb:id"),
            "{file}: missing sh:path kb:id property"
        );
        assert!(
            content.contains("sh:pattern"),
            "{file}: kb:id must have sh:pattern for format validation"
        );
        // The pattern must contain the correct prefix
        let prefix_chars = id_prefix.trim_end_matches('-');
        assert!(
            content.contains(prefix_chars),
            "{file}: sh:pattern must reference the correct ID prefix '{prefix_chars}'"
        );
    }
}

#[test]
fn id_is_exactly_one_per_item() {
    for (file, _, _, _) in RESEARCH_SHAPE_FILES {
        let content = load_shape(file);
        let id_pos = content.find("sh:path kb:id").expect("kb:id not found");
        let block_end = (id_pos + 200).min(content.len());
        let block = &content[id_pos..block_end];
        assert!(
            block.contains("sh:minCount 1") && block.contains("sh:maxCount 1"),
            "{file}: kb:id must have sh:minCount 1 and sh:maxCount 1"
        );
    }
}

// ---------------------------------------------------------------------------
// Cross-type links (hypothesis→paper, experiment→hypothesis)
// ---------------------------------------------------------------------------

#[test]
fn hypothesis_requires_kb_tests_linking_to_paper() {
    let content = load_shape("hypothesis.ttl");
    assert!(
        content.contains("sh:path kb:tests"),
        "hypothesis.ttl: missing sh:path kb:tests"
    );
    let pos = content.find("sh:path kb:tests").unwrap();
    let block_end = (pos + 300).min(content.len());
    let block = &content[pos..block_end];
    assert!(
        block.contains("sh:class kb:Paper"),
        "hypothesis.ttl: kb:tests must have sh:class kb:Paper"
    );
    assert!(
        block.contains("sh:minCount 1"),
        "hypothesis.ttl: kb:tests must have sh:minCount 1"
    );
    assert!(
        block.contains("sh:message"),
        "hypothesis.ttl: kb:tests must have sh:message"
    );
}

#[test]
fn experiment_requires_kb_validates_linking_to_hypothesis() {
    let content = load_shape("experiment.ttl");
    assert!(
        content.contains("sh:path kb:validates"),
        "experiment.ttl: missing sh:path kb:validates"
    );
    let pos = content.find("sh:path kb:validates").unwrap();
    let block_end = (pos + 300).min(content.len());
    let block = &content[pos..block_end];
    assert!(
        block.contains("sh:class kb:Hypothesis"),
        "experiment.ttl: kb:validates must have sh:class kb:Hypothesis"
    );
    assert!(
        block.contains("sh:minCount 1"),
        "experiment.ttl: kb:validates must have sh:minCount 1"
    );
}

// ---------------------------------------------------------------------------
// Turtle block shapes (5 of 6 types)
// ---------------------------------------------------------------------------

#[test]
fn turtle_block_types_have_turtle_block_shape() {
    for file in TURTLE_BLOCK_TYPES {
        let content = load_shape(file);
        assert!(
            content.contains("kb:TurtleBlockShape"),
            "{file}: missing kb:TurtleBlockShape — required for research types with turtle blocks"
        );
        assert!(
            content.contains("kb:requiredPrefix"),
            "{file}: kb:TurtleBlockShape must list kb:requiredPrefix"
        );
        assert!(
            content.contains("kb:requiredPredicate"),
            "{file}: kb:TurtleBlockShape must list kb:requiredPredicate"
        );
        assert!(
            content.contains("kb:templateHint"),
            "{file}: kb:TurtleBlockShape must have kb:templateHint with example turtle"
        );
    }
}

#[test]
fn idea_has_no_turtle_block() {
    let content = load_shape("idea.ttl");
    assert!(
        !content.contains("kb:TurtleBlockShape"),
        "idea.ttl: must NOT have TurtleBlockShape — ideas are lightweight"
    );
}

#[test]
fn experiment_turtle_block_requires_run_status() {
    let content = load_shape("experiment.ttl");
    assert!(
        content.contains("expr:runStatus"),
        "experiment.ttl: turtle block must reference expr:runStatus for DGX queue integration"
    );
}

#[test]
fn hypothesis_turtle_block_requires_claim_and_tested_by() {
    let content = load_shape("hypothesis.ttl");
    assert!(
        content.contains("hyp:claim"),
        "hypothesis.ttl: turtle block must require hyp:claim predicate"
    );
    assert!(
        content.contains("hyp:testedBy"),
        "hypothesis.ttl: turtle block must require hyp:testedBy predicate"
    );
}

#[test]
fn measure_turtle_block_requires_unit_and_category() {
    let content = load_shape("measure.ttl");
    assert!(
        content.contains("measure:unit"),
        "measure.ttl: turtle block must require measure:unit"
    );
    assert!(
        content.contains("measure:category"),
        "measure.ttl: turtle block must require measure:category"
    );
    assert!(
        content.contains("measure:collectionMethod"),
        "measure.ttl: turtle block must require measure:collectionMethod"
    );
}

// ---------------------------------------------------------------------------
// Body section structure (template generation annotations)
// ---------------------------------------------------------------------------

#[test]
fn all_shapes_have_required_sections_with_template_hints() {
    for (file, _, _, _) in RESEARCH_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("kb:requiredSection"),
            "{file}: missing kb:requiredSection"
        );
        assert!(
            content.contains("kb:templateHint"),
            "{file}: missing kb:templateHint"
        );
        assert!(
            content.contains("kb:sectionName"),
            "{file}: missing kb:sectionName"
        );
    }
}

#[test]
fn all_shapes_have_section_ordering() {
    for (file, _, _, _) in RESEARCH_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("kb:sectionOrder"),
            "{file}: missing kb:sectionOrder"
        );
    }
}

#[test]
fn hypothesis_has_claim_and_falsifiable_sections() {
    let content = load_shape("hypothesis.ttl");
    assert!(
        content.contains("\"Claim\""),
        "hypothesis.ttl: missing Claim section"
    );
    assert!(
        content.contains("\"Falsifiable By\""),
        "hypothesis.ttl: missing Falsifiable By section"
    );
    assert!(
        content.contains("\"Rationale\""),
        "hypothesis.ttl: missing Rationale section"
    );
}

#[test]
fn experiment_has_method_subsections() {
    let content = load_shape("experiment.ttl");
    assert!(
        content.contains("Method — Participants"),
        "experiment.ttl: missing Method — Participants subsection"
    );
    assert!(
        content.contains("Method — Materials"),
        "experiment.ttl: missing Method — Materials subsection"
    );
    assert!(
        content.contains("Method — Procedure"),
        "experiment.ttl: missing Method — Procedure subsection"
    );
    assert!(
        content.contains("Method — Configuration"),
        "experiment.ttl: missing Method — Configuration subsection"
    );
    assert!(
        content.contains("Data Location"),
        "experiment.ttl: missing Data Location section"
    );
    // sectionOrder must list all Method sub-sections, not just "Method"
    assert!(
        content.contains("\"Method — Participants\""),
        "experiment.ttl: sectionOrder must include 'Method — Participants' — not just 'Method'"
    );
}

#[test]
fn measure_has_specification_and_historical_values() {
    let content = load_shape("measure.ttl");
    assert!(
        content.contains("\"Specification\""),
        "measure.ttl: missing Specification section"
    );
    assert!(
        content.contains("\"Historical Values\""),
        "measure.ttl: missing Historical Values section"
    );
}

// ---------------------------------------------------------------------------
// Comment shape reference
// ---------------------------------------------------------------------------

#[test]
fn all_research_shapes_reference_comment_shape() {
    for (file, _, _, _) in RESEARCH_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("kb:hasComment"),
            "{file}: missing kb:hasComment"
        );
        assert!(
            content.contains("kb:CommentShape"),
            "{file}: kb:hasComment must reference kb:CommentShape"
        );
    }
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
fn groups_file_has_turtle_block_group() {
    let content = load_shape("groups.ttl");
    assert!(
        content.contains("kb:TurtleBlockGroup"),
        "groups.ttl: missing kb:TurtleBlockGroup — shared by 5 research types"
    );
    assert!(
        content.contains("Turtle Block"),
        "groups.ttl: TurtleBlockGroup must have 'Turtle Block' label"
    );
}

#[test]
fn groups_file_has_hypothesis_groups() {
    let content = load_shape("groups.ttl");
    let groups = &[
        "kb:ClaimGroup",
        "kb:RationaleGroup",
        "kb:VariablesGroup",
        "kb:FalsifiableGroup",
        "kb:ACFConnectionGroup",
    ];
    for group in groups {
        assert!(
            content.contains(group),
            "groups.ttl: missing {group} (required by hypothesis.ttl)"
        );
    }
}

#[test]
fn groups_file_has_experiment_groups() {
    let content = load_shape("groups.ttl");
    let groups = &[
        "kb:PurposeGroup",
        "kb:MethodParticipantsGroup",
        "kb:MethodMaterialsGroup",
        "kb:MethodProcedureGroup",
        "kb:MethodConfigGroup",
        "kb:AnalysisPlanGroup",
        "kb:ExpectedResultsGroup",
        "kb:DataLocationGroup",
    ];
    for group in groups {
        assert!(
            content.contains(group),
            "groups.ttl: missing {group} (required by experiment.ttl)"
        );
    }
}

#[test]
fn groups_file_has_paper_groups() {
    let content = load_shape("groups.ttl");
    let groups = &[
        "kb:AbstractGroup",
        "kb:HypothesesTestedGroup",
        "kb:KeyExperimentsGroup",
        "kb:OutlineGroup",
    ];
    for group in groups {
        assert!(
            content.contains(group),
            "groups.ttl: missing {group} (required by paper.ttl)"
        );
    }
}

#[test]
fn groups_file_has_measure_groups() {
    let content = load_shape("groups.ttl");
    let groups = &["kb:SpecificationGroup", "kb:HistoricalValuesGroup"];
    for group in groups {
        assert!(
            content.contains(group),
            "groups.ttl: missing {group} (required by measure.ttl)"
        );
    }
}

#[test]
fn groups_file_has_idea_groups() {
    let content = load_shape("groups.ttl");
    let groups = &["kb:OriginGroup", "kb:DomainGroup", "kb:NextStepsGroup"];
    for group in groups {
        assert!(
            content.contains(group),
            "groups.ttl: missing {group} (required by idea.ttl)"
        );
    }
}

#[test]
fn groups_file_has_literature_groups() {
    let content = load_shape("groups.ttl");
    let groups = &[
        "kb:TopicGroup",
        "kb:SearchStrategyGroup",
        "kb:KeyFindingsGroup",
        "kb:GapsGroup",
        "kb:ReferencesGroup",
    ];
    for group in groups {
        assert!(
            content.contains(group),
            "groups.ttl: missing {group} (required by literature.ttl)"
        );
    }
}

#[test]
fn all_groups_have_rdfs_label_and_sh_order() {
    let content = load_shape("groups.ttl");
    assert!(
        content.contains("rdfs:label"),
        "groups.ttl: groups must have rdfs:label"
    );
    assert!(
        content.contains("sh:order"),
        "groups.ttl: groups must have sh:order"
    );
}

// ---------------------------------------------------------------------------
// Scope constraints — no state transition SPARQL
// ---------------------------------------------------------------------------

#[test]
fn research_shapes_contain_no_state_transition_sparql() {
    for (file, _, _, _) in RESEARCH_SHAPE_FILES {
        let content = load_shape(file);
        assert!(
            !content.contains("sh:SPARQLConstraint") && !content.contains("sh:sparql"),
            "{file}: must NOT contain SPARQL constraints — state transitions belong in EX-3220"
        );
    }
}
