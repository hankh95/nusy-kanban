//! EX-4340 / VY-4313 EX-ix — L3 high school plates validation.
//!
//! Verifies that all 10 auto-generated L3 plates are structurally valid
//! (schema + semantics) and have sufficient CQ coverage for the 35Q
//! general battery (`research/shared/eval-data/v14-live-battery/battery.jsonl`).
//!
//! Two test tiers:
//! 1. Schema + semantics: each plate must deserialize, validate against JSON
//!    schema, and pass `validate_semantics()`.
//! 2. Battery coverage: each plate must contain at least `MIN_CQS` layer-3
//!    CQs and `MIN_L3_TRIPLES` layer-1 triples — the thresholds are set low
//!    enough to accept the auto-generated plates while catching degenerate
//!    outputs.

use nusy_tutor_record::{Level, TutorRecord, validate_against_schema};

/// Minimum layer-3 CQs a valid L3 plate must have.
const MIN_CQS: usize = 25;

/// Minimum layer-1 triples a valid L3 plate must have.
const MIN_L3_TRIPLES: usize = 40;

/// Minimum layer-2 entities a valid L3 plate must have.
const MIN_L2_ENTITIES: usize = 10;

/// Plate paths relative to the workspace root.
const PLATES: &[(&str, &str)] = &[
    (
        "pg1513_romeo_and_juliet",
        "research/shared/eval-data/curriculum-plates/L3_high_school/pg1513_romeo_and_juliet.expected.json",
    ),
    (
        "pg1787_hamlet",
        "research/shared/eval-data/curriculum-plates/L3_high_school/pg1787_hamlet.expected.json",
    ),
    (
        "pg28233_principia",
        "research/shared/eval-data/curriculum-plates/L3_high_school/pg28233_principia.expected.json",
    ),
    (
        "dna_molecular_biology",
        "research/shared/eval-data/curriculum-plates/L3_high_school/dna_molecular_biology.expected.json",
    ),
    (
        "pg55_wizard_of_oz",
        "research/shared/eval-data/curriculum-plates/L3_high_school/pg55_wizard_of_oz.expected.json",
    ),
    (
        "pg500_pinocchio",
        "research/shared/eval-data/curriculum-plates/L3_high_school/pg500_pinocchio.expected.json",
    ),
    (
        "pg11339_aesops_fables",
        "research/shared/eval-data/curriculum-plates/L3_high_school/pg11339_aesops_fables.expected.json",
    ),
    (
        "pg2591_grimms_fairy_tales",
        "research/shared/eval-data/curriculum-plates/L3_high_school/pg2591_grimms_fairy_tales.expected.json",
    ),
    (
        "pg2680_meditations",
        "research/shared/eval-data/curriculum-plates/L3_high_school/pg2680_meditations.expected.json",
    ),
    (
        "pg84_frankenstein",
        "research/shared/eval-data/curriculum-plates/L3_high_school/pg84_frankenstein.expected.json",
    ),
];

fn workspace_root() -> std::path::PathBuf {
    // Cargo sets CARGO_MANIFEST_DIR to the crate root; workspace root is two levels up.
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn load_plate(relative_path: &str) -> TutorRecord {
    let path = workspace_root().join(relative_path);
    let json = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read plate {relative_path}: {e}"));
    serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("plate {relative_path} failed to deserialize: {e}"))
}

#[test]
fn all_l3_plates_have_correct_level() {
    for (name, path) in PLATES {
        let record = load_plate(path);
        assert_eq!(
            record.document.level,
            Level::L3_high_school,
            "plate {name} must be level L3_high_school, got {:?}",
            record.document.level
        );
    }
}

#[test]
fn all_l3_plates_pass_schema_validation() {
    for (name, path) in PLATES {
        let full_path = workspace_root().join(path);
        let bytes = std::fs::read(&full_path).unwrap_or_else(|e| panic!("cannot read {name}: {e}"));
        let json: serde_json::Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("{name} is not valid JSON: {e}"));
        validate_against_schema(&json)
            .unwrap_or_else(|e| panic!("{name} failed JSON schema validation: {e}"));
    }
}

#[test]
fn all_l3_plates_pass_semantic_validation() {
    for (name, path) in PLATES {
        let record = load_plate(path);
        record.validate_semantics().unwrap_or_else(|e| {
            panic!("{name} failed semantic validation: {e}");
        });
    }
}

#[test]
fn all_l3_plates_meet_cq_minimum() {
    for (name, path) in PLATES {
        let record = load_plate(path);
        let cq_count = record.layers.layer_3_curiosity.cqs.len();
        assert!(
            cq_count >= MIN_CQS,
            "{name}: expected ≥{MIN_CQS} CQs, got {cq_count}"
        );
    }
}

#[test]
fn all_l3_plates_meet_triple_minimum() {
    for (name, path) in PLATES {
        let record = load_plate(path);
        let triple_count = record.layers.layer_1_literal.triples.len();
        assert!(
            triple_count >= MIN_L3_TRIPLES,
            "{name}: expected ≥{MIN_L3_TRIPLES} L1 triples, got {triple_count}"
        );
    }
}

#[test]
fn all_l3_plates_meet_entity_minimum() {
    for (name, path) in PLATES {
        let record = load_plate(path);
        let entity_count = record.layers.layer_2_ontology.entities.len();
        assert!(
            entity_count >= MIN_L2_ENTITIES,
            "{name}: expected ≥{MIN_L2_ENTITIES} L2 entities, got {entity_count}"
        );
    }
}

#[test]
fn shakespeare_plates_reference_their_plays() {
    // Romeo and Juliet plate must mention "romeo" or "juliet" in its CQs.
    let r_and_j = load_plate(
        "research/shared/eval-data/curriculum-plates/L3_high_school/pg1513_romeo_and_juliet.expected.json",
    );
    let cq_text: String = r_and_j
        .layers
        .layer_3_curiosity
        .cqs
        .iter()
        .map(|cq| cq.cq.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        cq_text.contains("romeo") || cq_text.contains("juliet"),
        "Romeo and Juliet plate CQs must reference the play's characters"
    );

    // Hamlet plate must mention "hamlet" in its CQs.
    let hamlet = load_plate(
        "research/shared/eval-data/curriculum-plates/L3_high_school/pg1787_hamlet.expected.json",
    );
    let hamlet_cq_text: String = hamlet
        .layers
        .layer_3_curiosity
        .cqs
        .iter()
        .map(|cq| cq.cq.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        hamlet_cq_text.contains("hamlet"),
        "Hamlet plate CQs must reference Hamlet"
    );
}

#[test]
fn dna_plate_references_base_pairs() {
    let dna = load_plate(
        "research/shared/eval-data/curriculum-plates/L3_high_school/dna_molecular_biology.expected.json",
    );
    let all_text = serde_json::to_string(&dna).unwrap().to_lowercase();
    // The four DNA bases must all appear somewhere in the plate.
    for base in ["adenine", "guanine", "thymine", "cytosine"] {
        assert!(
            all_text.contains(base),
            "DNA plate must mention {base} (a DNA base)"
        );
    }
}

#[test]
fn complete_set_of_10_plates() {
    let plates_dir =
        workspace_root().join("research/shared/eval-data/curriculum-plates/L3_high_school");
    let count = std::fs::read_dir(&plates_dir)
        .expect("L3_high_school plates dir must exist")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".expected.json"))
        .count();
    assert_eq!(count, 10, "must have exactly 10 L3 plates, found {count}");
}
