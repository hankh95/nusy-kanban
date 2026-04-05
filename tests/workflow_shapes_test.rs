//! Tests for crates/nusy-kanban/ontology/shapes/workflow/*.ttl
//!
//! EX-3220: SHACL Shapes — Board + Workflow (WIP limits, state machine, terminal states)
//!
//! Validates:
//! - boards.ttl: WIP SPARQLConstraints for both boards
//! - states.ttl: valid-status sh:in for all 12 types
//! - terminal.ttl: resolution requirement for terminal states
//! - comments.ttl: comment creation and resolution lifecycle shapes
//! - mutations.ttl: updated_at auto-set rule documentation
//! - relations.ttl: valid predicate domain/range constraints including blocks

const SHAPES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ontology/shapes/workflow");

fn shape_path(filename: &str) -> String {
    format!("{SHAPES_DIR}/{filename}")
}

fn load_shape(filename: &str) -> String {
    let path = shape_path(filename);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read shape file {path}: {e}"))
}

const WORKFLOW_FILES: &[&str] = &[
    "boards.ttl",
    "states.ttl",
    "terminal.ttl",
    "comments.ttl",
    "mutations.ttl",
    "relations.ttl",
];

// ---------------------------------------------------------------------------
// Existence and basic structure
// ---------------------------------------------------------------------------

#[test]
fn all_workflow_shape_files_exist() {
    for file in WORKFLOW_FILES {
        let content = load_shape(file);
        assert!(!content.is_empty(), "{file} must not be empty");
    }
}

#[test]
fn all_files_declare_kb_and_sh_prefixes() {
    for file in WORKFLOW_FILES {
        let content = load_shape(file);
        assert!(
            content.contains("@prefix kb:"),
            "{file}: missing @prefix kb:"
        );
        assert!(
            content.contains("@prefix sh:"),
            "{file}: missing @prefix sh:"
        );
    }
}

// ---------------------------------------------------------------------------
// boards.ttl — WIP SPARQLConstraints
// ---------------------------------------------------------------------------

#[test]
fn boards_has_dev_wip_shape() {
    let content = load_shape("boards.ttl");
    assert!(
        content.contains("kb:DevBoardWIPShape"),
        "boards.ttl: missing kb:DevBoardWIPShape"
    );
    assert!(
        content.contains("sh:targetClass kb:DevBoard"),
        "boards.ttl: DevBoardWIPShape must target kb:DevBoard"
    );
}

#[test]
fn boards_has_research_wip_shape() {
    let content = load_shape("boards.ttl");
    assert!(
        content.contains("kb:ResearchBoardWIPShape"),
        "boards.ttl: missing kb:ResearchBoardWIPShape"
    );
    assert!(
        content.contains("sh:targetClass kb:ResearchBoard"),
        "boards.ttl: ResearchBoardWIPShape must target kb:ResearchBoard"
    );
}

#[test]
fn boards_uses_sparql_constraints() {
    let content = load_shape("boards.ttl");
    assert!(
        content.contains("sh:sparql"),
        "boards.ttl: WIP shapes must use sh:sparql constraints"
    );
    assert!(
        content.contains("sh:select"),
        "boards.ttl: SPARQL constraints must have sh:select"
    );
    assert!(
        content.contains("sh:message"),
        "boards.ttl: SPARQL constraints must have sh:message"
    );
}

#[test]
fn boards_dev_wip_limit_is_4() {
    let content = load_shape("boards.ttl");
    assert!(
        content.contains("?count > 4"),
        "boards.ttl: dev board WIP limit must be 4 (FILTER(?count > 4))"
    );
}

#[test]
fn boards_research_wip_limit_is_5() {
    let content = load_shape("boards.ttl");
    assert!(
        content.contains("?count > 5"),
        "boards.ttl: research board WIP limit must be 5 (FILTER(?count > 5))"
    );
}

// ---------------------------------------------------------------------------
// states.ttl — Per-type valid status
// ---------------------------------------------------------------------------

#[test]
fn states_has_all_12_type_shapes() {
    let content = load_shape("states.ttl");
    let types = &[
        "kb:ExpeditionStatusShape",
        "kb:VoyageStatusShape",
        "kb:ChoreStatusShape",
        "kb:HazardStatusShape",
        "kb:FeatureStatusShape",
        "kb:SignalStatusShape",
        "kb:HypothesisStatusShape",
        "kb:ExperimentStatusShape",
        "kb:PaperStatusShape",
        "kb:MeasureStatusShape",
        "kb:IdeaStatusShape",
        "kb:LiteratureStatusShape",
    ];
    for t in types {
        assert!(content.contains(t), "states.ttl: missing {t}");
    }
}

#[test]
fn states_all_shapes_have_sh_in_for_status() {
    let content = load_shape("states.ttl");
    // Every StatusShape must have sh:in
    let shape_count = content.matches("StatusShape a sh:NodeShape").count();
    let sh_in_count = content.matches("sh:in (").count() + content.matches("sh:in(").count();
    assert_eq!(
        shape_count, sh_in_count,
        "states.ttl: {shape_count} StatusShapes but only {sh_in_count} sh:in constraints"
    );
}

#[test]
fn states_hypothesis_cannot_be_complete() {
    let content = load_shape("states.ttl");
    let pos = content
        .find("kb:HypothesisStatusShape")
        .expect("HypothesisStatusShape not found");
    let block_end = (pos + 300).min(content.len());
    let block = &content[pos..block_end];
    assert!(
        !block.contains("\"complete\""),
        "states.ttl: hypothesis must NOT have 'complete' as valid status — hypotheses are never complete"
    );
    assert!(
        block.contains("\"draft\"")
            && block.contains("\"active\"")
            && block.contains("\"retired\""),
        "states.ttl: hypothesis must have draft, active, retired"
    );
}

#[test]
fn states_signal_is_minimal() {
    let content = load_shape("states.ttl");
    let pos = content
        .find("kb:SignalStatusShape")
        .expect("SignalStatusShape not found");
    let block_end = (pos + 300).min(content.len());
    let block = &content[pos..block_end];
    assert!(
        block.contains("\"backlog\"") && block.contains("\"done\""),
        "states.ttl: signal must have backlog and done"
    );
    assert!(
        !block.contains("\"in_progress\""),
        "states.ttl: signal should NOT have in_progress — signals are capture-and-done"
    );
}

#[test]
fn states_experiment_has_planned_running_complete_abandoned() {
    let content = load_shape("states.ttl");
    let pos = content
        .find("kb:ExperimentStatusShape")
        .expect("ExperimentStatusShape not found");
    let block_end = (pos + 300).min(content.len());
    let block = &content[pos..block_end];
    assert!(
        block.contains("\"planned\"")
            && block.contains("\"running\"")
            && block.contains("\"complete\"")
            && block.contains("\"abandoned\""),
        "states.ttl: experiment must have planned, running, complete, abandoned"
    );
}

#[test]
fn states_paper_has_writing_pipeline() {
    let content = load_shape("states.ttl");
    let pos = content
        .find("kb:PaperStatusShape")
        .expect("PaperStatusShape not found");
    let block_end = (pos + 400).min(content.len());
    let block = &content[pos..block_end];
    assert!(
        block.contains("\"draft\"")
            && block.contains("\"outline\"")
            && block.contains("\"writing\"")
            && block.contains("\"review\""),
        "states.ttl: paper must have draft, outline, writing, review pipeline"
    );
}

#[test]
fn states_idea_has_captured_formalized_abandoned() {
    let content = load_shape("states.ttl");
    let pos = content
        .find("kb:IdeaStatusShape")
        .expect("IdeaStatusShape not found");
    let block_end = (pos + 300).min(content.len());
    let block = &content[pos..block_end];
    assert!(
        block.contains("\"captured\"")
            && block.contains("\"formalized\"")
            && block.contains("\"abandoned\""),
        "states.ttl: idea must have captured, formalized, abandoned"
    );
}

// ---------------------------------------------------------------------------
// terminal.ttl — Resolution requirements
// ---------------------------------------------------------------------------

#[test]
fn terminal_has_resolution_shape() {
    let content = load_shape("terminal.ttl");
    assert!(
        content.contains("kb:TerminalResolutionShape"),
        "terminal.ttl: missing kb:TerminalResolutionShape"
    );
}

#[test]
fn terminal_resolution_checks_terminal_states() {
    let content = load_shape("terminal.ttl");
    assert!(
        content.contains("\"done\"")
            && content.contains("\"complete\"")
            && content.contains("\"abandoned\"")
            && content.contains("\"retired\""),
        "terminal.ttl: must check all terminal states (done, complete, abandoned, retired)"
    );
}

#[test]
fn terminal_resolution_values_are_valid() {
    let content = load_shape("terminal.ttl");
    assert!(
        content.contains("kb:ResolutionValuesShape"),
        "terminal.ttl: missing kb:ResolutionValuesShape"
    );
    assert!(
        content.contains("\"completed\"")
            && content.contains("\"superseded\"")
            && content.contains("\"wont_do\"")
            && content.contains("\"duplicate\"")
            && content.contains("\"obsolete\"")
            && content.contains("\"merged\""),
        "terminal.ttl: resolution sh:in must include all 6 valid values"
    );
}

#[test]
fn terminal_has_closed_by_shape() {
    let content = load_shape("terminal.ttl");
    assert!(
        content.contains("kb:ClosedByShape"),
        "terminal.ttl: missing kb:ClosedByShape for provenance tracking"
    );
    assert!(
        content.contains("kb:closedBy"),
        "terminal.ttl: ClosedByShape must reference kb:closedBy"
    );
}

// ---------------------------------------------------------------------------
// comments.ttl — Comment lifecycle
// ---------------------------------------------------------------------------

#[test]
fn comments_has_creation_shape() {
    let content = load_shape("comments.ttl");
    assert!(
        content.contains("kb:CommentCreationShape"),
        "comments.ttl: missing kb:CommentCreationShape"
    );
    assert!(
        content.contains("sh:targetClass kb:Comment"),
        "comments.ttl: CommentCreationShape must target kb:Comment"
    );
}

#[test]
fn comments_creation_requires_item_id_author_body() {
    let content = load_shape("comments.ttl");
    let required = &["kb:item_id", "kb:author", "kb:body", "kb:created_at"];
    for path in required {
        assert!(
            content.contains(path),
            "comments.ttl: missing required property {path}"
        );
    }
}

#[test]
fn comments_has_resolution_shape() {
    let content = load_shape("comments.ttl");
    assert!(
        content.contains("kb:CommentResolutionShape"),
        "comments.ttl: missing kb:CommentResolutionShape"
    );
}

#[test]
fn comments_has_threading_shape() {
    let content = load_shape("comments.ttl");
    assert!(
        content.contains("kb:CommentThreadingShape"),
        "comments.ttl: missing kb:CommentThreadingShape for parent_comment_id validation"
    );
    assert!(
        content.contains("kb:parent_comment_id"),
        "comments.ttl: threading shape must reference parent_comment_id"
    );
}

// ---------------------------------------------------------------------------
// mutations.ttl — updated_at rule
// ---------------------------------------------------------------------------

#[test]
fn mutations_has_timestamp_shape() {
    let content = load_shape("mutations.ttl");
    assert!(
        content.contains("kb:MutationTimestampShape"),
        "mutations.ttl: missing kb:MutationTimestampShape"
    );
    assert!(
        content.contains("kb:updated_at"),
        "mutations.ttl: must reference kb:updated_at"
    );
}

#[test]
fn mutations_documents_implementation_reference() {
    let content = load_shape("mutations.ttl");
    assert!(
        content.contains("EX-3244") || content.contains("touch_updated_at"),
        "mutations.ttl: should reference EX-3244 or touch_updated_at (the implementation)"
    );
}

// ---------------------------------------------------------------------------
// relations.ttl — Predicate domain/range
// ---------------------------------------------------------------------------

#[test]
fn relations_has_generic_predicates() {
    let content = load_shape("relations.ttl");
    assert!(
        content.contains("kb:DependsOnShape"),
        "relations.ttl: missing kb:DependsOnShape"
    );
    assert!(
        content.contains("kb:RelatedShape"),
        "relations.ttl: missing kb:RelatedShape"
    );
    assert!(
        content.contains("kb:BlocksShape"),
        "relations.ttl: missing kb:BlocksShape"
    );
}

#[test]
fn relations_blocks_targets_kb_item() {
    let content = load_shape("relations.ttl");
    let pos = content
        .find("kb:BlocksShape")
        .expect("BlocksShape not found");
    let block_end = (pos + 300).min(content.len());
    let block = &content[pos..block_end];
    assert!(
        block.contains("sh:targetClass kb:Item"),
        "relations.ttl: BlocksShape must target kb:Item (any type can block any type)"
    );
    assert!(
        block.contains("sh:class kb:Item"),
        "relations.ttl: blocks range must be kb:Item"
    );
}

#[test]
fn relations_has_typed_predicates() {
    let content = load_shape("relations.ttl");
    let typed = &[
        ("kb:ImplementsShape", "kb:Expedition", "kb:Voyage"),
        ("kb:SpawnsShape", "kb:Voyage", "kb:Expedition"),
        ("kb:TestsShape", "kb:Hypothesis", "kb:Paper"),
        ("kb:ValidatesShape", "kb:Experiment", "kb:Hypothesis"),
        ("kb:MeasuresShape", "kb:Measure", "kb:Experiment"),
    ];
    for (shape, domain, range) in typed {
        assert!(content.contains(shape), "relations.ttl: missing {shape}");
        let pos = content.find(shape).unwrap();
        let block_end = (pos + 400).min(content.len());
        let block = &content[pos..block_end];
        assert!(
            block.contains(&format!("sh:targetClass {domain}")),
            "relations.ttl: {shape} must target {domain}"
        );
        assert!(
            block.contains(&format!("sh:class {range}")),
            "relations.ttl: {shape} must have range {range}"
        );
    }
}

#[test]
fn relations_tests_and_validates_are_required() {
    let content = load_shape("relations.ttl");
    // tests: H → Paper (minCount 1)
    let pos = content.find("kb:TestsShape").unwrap();
    let block_end = (pos + 400).min(content.len());
    let block = &content[pos..block_end];
    assert!(
        block.contains("sh:minCount 1"),
        "relations.ttl: kb:tests must be sh:minCount 1 for hypotheses"
    );

    // validates: Experiment → Hypothesis (minCount 1)
    let pos = content.find("kb:ValidatesShape").unwrap();
    let block_end = (pos + 400).min(content.len());
    let block = &content[pos..block_end];
    assert!(
        block.contains("sh:minCount 1"),
        "relations.ttl: kb:validates must be sh:minCount 1 for experiments"
    );
}

// ---------------------------------------------------------------------------
// Scope: SPARQL is advisory-only, field shapes stay in dev/ and research/
// ---------------------------------------------------------------------------

#[test]
fn workflow_shapes_do_not_define_field_shapes() {
    // Field shapes (sh:in for priority, sh:pattern for ID) belong in dev/ and research/
    for file in WORKFLOW_FILES {
        let content = load_shape(file);
        if *file != "states.ttl" && *file != "terminal.ttl" && *file != "relations.ttl" {
            assert!(
                !content.contains("sh:pattern"),
                "{file}: must NOT contain sh:pattern — ID/field patterns belong in dev/ and research/ shapes"
            );
        }
    }
}
