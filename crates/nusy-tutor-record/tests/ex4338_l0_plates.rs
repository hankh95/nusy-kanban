//! EX-4338 / VY-4313 EX-vii — L0 toddler plates validation.
//!
//! Verifies that all 25 auto-generated L0 plates are structurally valid
//! (schema + semantics), have the correct curriculum level, and meet
//! minimum layer thresholds for L0 depth.
//!
//! L0 (toddler) plates are expected to be simpler than L3: shorter CQ lists
//! and smaller triple counts are normal and correct. Thresholds are set
//! accordingly.

use nusy_tutor_record::{Level, TutorRecord, validate_against_schema};

/// Minimum layer-3 CQs for a valid L0 plate.
const MIN_L0_CQS: usize = 15;

/// Minimum layer-1 triples for a valid L0 plate.
const MIN_L0_TRIPLES: usize = 20;

/// Minimum layer-2 entities for a valid L0 plate.
const MIN_L0_ENTITIES: usize = 5;

/// Expected total count of L0 plates.
const EXPECTED_PLATE_COUNT: usize = 25;

const PLATES: &[&str] = &[
    "00_alphabet_dame_wonder",
    "01_alphabet_little_people",
    "02_arithmetic_rays_primary",
    "03_first_reader_mcguffey",
    "adventures_of_tom_sawyer",
    "aesops_fables",
    "alice_wonderland",
    "black_beauty",
    "childs_garden_verses",
    "emily_post_etiquette",
    "florence_hartley_etiquette",
    "grimms_fairy_tales",
    "jungle_book",
    "just_so_stories",
    "little_women",
    "peter_pan",
    "pinocchio",
    "pollyanna",
    "the_rose_and_the_ring",
    "the_secret_garden",
    "the_water_babies",
    "wind_in_willows",
    "wizard_of_oz",
    "wonder_book_for_girls_and_boys",
    "treasure_island",
];

fn workspace_root() -> std::path::PathBuf {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn plate_path(name: &str) -> std::path::PathBuf {
    workspace_root()
        .join("research/shared/eval-data/curriculum-plates/L0_toddler")
        .join(format!("{name}.expected.json"))
}

fn load_plate(name: &str) -> TutorRecord {
    let path = plate_path(name);
    let json =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read plate {name}: {e}"));
    serde_json::from_str(&json).unwrap_or_else(|e| panic!("{name} failed to deserialize: {e}"))
}

#[test]
fn complete_set_of_25_l0_plates() {
    let plates_dir =
        workspace_root().join("research/shared/eval-data/curriculum-plates/L0_toddler");
    let count = std::fs::read_dir(&plates_dir)
        .expect("L0_toddler plates dir must exist")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".expected.json"))
        .count();
    assert_eq!(
        count, EXPECTED_PLATE_COUNT,
        "expected {EXPECTED_PLATE_COUNT} L0 plates, found {count}"
    );
}

#[test]
fn all_l0_plates_have_correct_level() {
    for name in PLATES {
        let record = load_plate(name);
        assert_eq!(
            record.document.level,
            Level::L0_toddler,
            "{name}: level must be L0_toddler, got {:?}",
            record.document.level
        );
    }
}

#[test]
fn all_l0_plates_pass_schema_validation() {
    for name in PLATES {
        let path = plate_path(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {name}: {e}"));
        let json: serde_json::Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("{name} is not valid JSON: {e}"));
        validate_against_schema(&json)
            .unwrap_or_else(|e| panic!("{name} failed schema validation: {e}"));
    }
}

#[test]
fn all_l0_plates_pass_semantic_validation() {
    for name in PLATES {
        let record = load_plate(name);
        record
            .validate_semantics()
            .unwrap_or_else(|e| panic!("{name} failed semantic validation: {e}"));
    }
}

#[test]
fn all_l0_plates_meet_cq_minimum() {
    for name in PLATES {
        let record = load_plate(name);
        let cq_count = record.layers.layer_3_curiosity.cqs.len();
        assert!(
            cq_count >= MIN_L0_CQS,
            "{name}: expected ≥{MIN_L0_CQS} CQs, got {cq_count}"
        );
    }
}

#[test]
fn all_l0_plates_meet_triple_minimum() {
    for name in PLATES {
        let record = load_plate(name);
        let triple_count = record.layers.layer_1_literal.triples.len();
        assert!(
            triple_count >= MIN_L0_TRIPLES,
            "{name}: expected ≥{MIN_L0_TRIPLES} L1 triples, got {triple_count}"
        );
    }
}

#[test]
fn all_l0_plates_meet_entity_minimum() {
    for name in PLATES {
        let record = load_plate(name);
        let entity_count = record.layers.layer_2_ontology.entities.len();
        assert!(
            entity_count >= MIN_L0_ENTITIES,
            "{name}: expected ≥{MIN_L0_ENTITIES} L2 entities, got {entity_count}"
        );
    }
}

#[test]
fn alphabet_plates_reference_letters() {
    // The two alphabet primers must mention letters (a, b, c) in their CQs or L1 content.
    for name in ["00_alphabet_dame_wonder", "01_alphabet_little_people"] {
        let record = load_plate(name);
        let all_text = serde_json::to_string(&record).unwrap().to_lowercase();
        assert!(
            all_text.contains("letter")
                || all_text.contains("alphabet")
                || all_text.contains(" a "),
            "{name}: alphabet primer must reference letters or the alphabet"
        );
    }
}

#[test]
fn aesops_plate_has_fable_content() {
    let record = load_plate("aesops_fables");
    let all_text = serde_json::to_string(&record).unwrap().to_lowercase();
    assert!(
        all_text.contains("fable") || all_text.contains("moral") || all_text.contains("aesop"),
        "aesops_fables plate must contain fable-related content"
    );
}

#[test]
fn alice_plate_has_wonderland_content() {
    let record = load_plate("alice_wonderland");
    let all_text = serde_json::to_string(&record).unwrap().to_lowercase();
    assert!(
        all_text.contains("alice")
            || all_text.contains("wonderland")
            || all_text.contains("rabbit"),
        "alice_wonderland plate must contain Wonderland content"
    );
}
