//! Tests for crates/nusy-kanban/ontology/kanban.ttl
//!
//! EX-3217: kanban.ttl — Core OWL Ontology for All 12 Item Types
//!
//! Validates that the ontology file:
//! - exists and is readable (required for SHACL shapes to load it)
//! - declares all 12 item type classes as rdfs:subClassOf kb:Item
//! - declares all 7 relation predicates with domain/range
//! - uses only the canonical kb: namespace
//! - contains the installation path note for project-local override

const ONTOLOGY_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ontology/kanban.ttl");

const KB_NS: &str = "https://nusy.dev/kanban/";

/// All 12 item type class names as they appear in the ontology.
const ITEM_TYPES: &[&str] = &[
    "Expedition",
    "Voyage",
    "Chore",
    "Hazard",
    "Signal",
    "Feature",
    "Paper",
    "Hypothesis",
    "Experiment",
    "Measure",
    "Idea",
    "Literature",
];

/// All 7 relation predicate names.
const RELATION_PREDICATES: &[&str] = &[
    "dependsOn",
    "related",
    "implements",
    "spawns",
    "tests",
    "validates",
    "measures",
];

/// Core datatype property names (5 total).
const DATATYPE_PROPERTIES: &[&str] = &["status", "priority", "assignee", "body", "tags"];

fn load_ontology() -> String {
    std::fs::read_to_string(ONTOLOGY_PATH)
        .unwrap_or_else(|e| panic!("Failed to read ontology at {ONTOLOGY_PATH}: {e}"))
}

#[test]
fn ontology_file_exists_and_is_readable() {
    let content = load_ontology();
    assert!(
        !content.is_empty(),
        "kanban.ttl must not be empty — file path: {ONTOLOGY_PATH}"
    );
}

#[test]
fn ontology_declares_kb_prefix() {
    let content = load_ontology();
    assert!(
        content.contains(KB_NS),
        "ontology must declare kb: namespace <{KB_NS}>"
    );
    // Must have the @prefix declaration
    assert!(
        content.contains("@prefix kb:"),
        "ontology must have @prefix kb: declaration"
    );
}

#[test]
fn ontology_declares_owl_ontology() {
    let content = load_ontology();
    assert!(
        content.contains("a owl:Ontology"),
        "ontology must declare itself as owl:Ontology"
    );
}

#[test]
fn all_12_item_types_declared_as_owl_classes() {
    let content = load_ontology();
    for type_name in ITEM_TYPES {
        let class_decl = format!("kb:{type_name} a owl:Class");
        assert!(
            content.contains(&class_decl),
            "kanban.ttl missing OWL class declaration for kb:{type_name}"
        );
    }
}

#[test]
fn all_12_item_types_are_subclass_of_kb_item() {
    let content = load_ontology();
    for type_name in ITEM_TYPES {
        // Each type must have rdfs:subClassOf (either directly of kb:Item,
        // or of kb:DevItem/kb:ResearchItem which are themselves subClassOf kb:Item).
        // We verify both the class declaration and the subClassOf triple exist.
        let subclass_decl = format!("kb:{type_name}");
        let subclass_of = "rdfs:subClassOf";
        assert!(
            content.contains(&subclass_decl) && content.contains(subclass_of),
            "kanban.ttl missing rdfs:subClassOf for kb:{type_name}"
        );
        // Verify the specific subClassOf kb:DevItem or kb:ResearchItem or kb:Item
        let has_dev = content.contains(&format!(
            "kb:{type_name} a owl:Class ;\n    rdfs:subClassOf kb:DevItem"
        )) || content.contains(&format!(
            "kb:{type_name} a owl:Class ;\n    rdfs:subClassOf kb:ResearchItem"
        )) || content.contains(&format!(
            "kb:{type_name} a owl:Class ;\n    rdfs:subClassOf kb:Item"
        ));
        assert!(
            has_dev,
            "kb:{type_name} must be rdfs:subClassOf kb:DevItem, kb:ResearchItem, or kb:Item"
        );
    }
}

#[test]
fn dev_board_types_are_subclass_of_kb_dev_item() {
    let content = load_ontology();
    let dev_types = &[
        "Expedition",
        "Voyage",
        "Chore",
        "Hazard",
        "Signal",
        "Feature",
    ];
    for type_name in dev_types {
        assert!(
            content.contains(&format!(
                "kb:{type_name} a owl:Class ;\n    rdfs:subClassOf kb:DevItem"
            )),
            "dev board type kb:{type_name} must be rdfs:subClassOf kb:DevItem"
        );
    }
}

#[test]
fn research_board_types_are_subclass_of_kb_research_item() {
    let content = load_ontology();
    let research_types = &[
        "Paper",
        "Hypothesis",
        "Experiment",
        "Measure",
        "Idea",
        "Literature",
    ];
    for type_name in research_types {
        assert!(
            content.contains(&format!(
                "kb:{type_name} a owl:Class ;\n    rdfs:subClassOf kb:ResearchItem"
            )),
            "research board type kb:{type_name} must be rdfs:subClassOf kb:ResearchItem"
        );
    }
}

#[test]
fn all_7_relation_predicates_declared() {
    let content = load_ontology();
    for predicate in RELATION_PREDICATES {
        let prop_decl = format!("kb:{predicate} a owl:ObjectProperty");
        assert!(
            content.contains(&prop_decl),
            "kanban.ttl missing owl:ObjectProperty declaration for kb:{predicate}"
        );
    }
}

#[test]
fn relation_predicates_have_domain_and_range() {
    let content = load_ontology();
    for predicate in RELATION_PREDICATES {
        let block_start = format!("kb:{predicate} a owl:ObjectProperty");
        assert!(
            content.contains(&block_start),
            "kb:{predicate} missing owl:ObjectProperty declaration"
        );
        // domain and range must appear after the declaration
        let pos = content
            .find(&block_start)
            .expect("predicate declaration not found");
        let end = (pos + 400).min(content.len());
        let block = &content[pos..end];
        assert!(
            block.contains("rdfs:domain"),
            "kb:{predicate} missing rdfs:domain"
        );
        assert!(
            block.contains("rdfs:range"),
            "kb:{predicate} missing rdfs:range"
        );
    }
}

#[test]
fn all_5_datatype_properties_declared() {
    let content = load_ontology();
    for prop in DATATYPE_PROPERTIES {
        let prop_decl = format!("kb:{prop} a owl:DatatypeProperty");
        assert!(
            content.contains(&prop_decl),
            "kanban.ttl missing owl:DatatypeProperty for kb:{prop}"
        );
    }
}

#[test]
fn installation_path_note_present() {
    let content = load_ontology();
    assert!(
        content.contains(".yurtle-kanban/kanban.ttl"),
        "kanban.ttl header must document the project-local installation path (.yurtle-kanban/kanban.ttl)"
    );
}

#[test]
fn board_classes_declared() {
    let content = load_ontology();
    for class in &["kb:Board", "kb:DevBoard", "kb:ResearchBoard", "kb:Relation"] {
        assert!(
            content.contains(class),
            "kanban.ttl missing structural class {class}"
        );
    }
}

#[test]
fn no_shacl_in_ontology() {
    // Shapes belong in EX-3218/3219/3220, not in the base ontology
    let content = load_ontology();
    assert!(
        !content.contains("sh:NodeShape") && !content.contains("sh:property"),
        "kanban.ttl must NOT contain SHACL shapes — those belong in ontology/shapes/ (EX-3218/3219/3220)"
    );
}
