//! EX-4339 / VY-4313 — L1 grade-school plates validation.
//!
//! Verifies the 30 auto-generated L1 plates are structurally valid (schema +
//! semantics) and have sufficient depth for the 35Q general battery.
//! Source corpus: `beings/m5-highschool-v10/corpus/grade_school/` (extracted
//! Project Gutenberg books + math/science/social studies primers).
//!
//! Three test tiers:
//! 1. Schema + semantics: each plate deserializes, validates against the
//!    embedded JSON schema, and passes `validate_semantics()`.
//! 2. Coverage minimums: each plate has at least `MIN_CQS` layer-3 CQs and
//!    `MIN_L1_TRIPLES` layer-1 triples — set conservatively to catch
//!    degenerate auto-tutor outputs without overspecifying.
//! 3. Set integrity: exactly 30 plates exist in the L1 directory.

use nusy_tutor_record::{Level, TutorRecord, validate_against_schema};

/// Minimum layer-3 CQs a valid L1 plate must have.
const MIN_CQS: usize = 15;

/// Minimum layer-1 triples a valid L1 plate must have.
const MIN_L1_TRIPLES: usize = 20;

/// Minimum layer-2 entities a valid L1 plate must have.
const MIN_L2_ENTITIES: usize = 8;

/// Plates exempt from the full-document coverage minimums. `pg68662_..._ch1` is
/// EX-4458's deliberate single-chapter, CQ-first *demo* plate (3 entities / 14
/// triples / 10 CQs by design); it is still schema- and level-validated, just not
/// held to the full-book minimums. (CH-4428 review #3.)
const MINIMUM_EXEMPT: &[&str] = &["pg68662_elements_of_arithmetic_ch1"];

/// Plate paths relative to the workspace root.
const PLATES: &[(&str, &str)] = &[
    // Extracted Project Gutenberg books (full text)
    (
        "pg120_treasure_island",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/pg120_treasure_island.expected.json",
    ),
    (
        "pg12292_handbook_of_nature_study",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/pg12292_handbook_of_nature_study.expected.json",
    ),
    (
        "pg16713_amusements_in_mathematics",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/pg16713_amusements_in_mathematics.expected.json",
    ),
    (
        "pg201_flatland_a_romance_of_many_dimensions",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/pg201_flatland_a_romance_of_many_dimensions.expected.json",
    ),
    (
        "pg22461_a_wonder_book_for_girls_and_boys",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/pg22461_a_wonder_book_for_girls_and_boys.expected.json",
    ),
    (
        "pg36368_a_child_s_geography_of_the_world",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/pg36368_a_child_s_geography_of_the_world.expected.json",
    ),
    (
        "pg37785_stories_of_starland",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/pg37785_stories_of_starland.expected.json",
    ),
    (
        "pg40383_the_new_arithmetic",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/pg40383_the_new_arithmetic.expected.json",
    ),
    (
        "pg49095_first_book_in_botany",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/pg49095_first_book_in_botany.expected.json",
    ),
    // Mathematics primers (part 1 of multi-part books)
    (
        "arithmetic_advanced_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/arithmetic_advanced_part1.expected.json",
    ),
    (
        "elementary_geometry_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/elementary_geometry_part1.expected.json",
    ),
    (
        "mathematical_recreations_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/mathematical_recreations_part1.expected.json",
    ),
    (
        "measurement_for_beginners_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/measurement_for_beginners_part1.expected.json",
    ),
    (
        "mental_arithmetic_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/mental_arithmetic_part1.expected.json",
    ),
    (
        "new_elementary_arithmetic_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/new_elementary_arithmetic_part1.expected.json",
    ),
    (
        "psychology_of_arithmetic_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/psychology_of_arithmetic_part1.expected.json",
    ),
    // Reading / Social studies primers
    (
        "aesops_fables",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/aesops_fables.expected.json",
    ),
    (
        "childs_history_england_dickens_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/childs_history_england_dickens_part1.expected.json",
    ),
    (
        "first_book_american_history_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/first_book_american_history_part1.expected.json",
    ),
    (
        "our_little_korean_cousin_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/our_little_korean_cousin_part1.expected.json",
    ),
    (
        "story_of_mankind_van_loon_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/story_of_mankind_van_loon_part1.expected.json",
    ),
    // Science primers
    (
        "astronomy_young_folks_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/astronomy_young_folks_part1.expected.json",
    ),
    (
        "chemical_history_candle_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/chemical_history_candle_part1.expected.json",
    ),
    (
        "first_principles_physics",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/first_principles_physics.expected.json",
    ),
    (
        "human_physiology_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/human_physiology_part1.expected.json",
    ),
    (
        "introduction_botany_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/introduction_botany_part1.expected.json",
    ),
    (
        "nature_study_lessons_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/nature_study_lessons_part1.expected.json",
    ),
    (
        "popular_scientific_lectures_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/popular_scientific_lectures_part1.expected.json",
    ),
    (
        "science_for_beginners_part1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/science_for_beginners_part1.expected.json",
    ),
    (
        "young_folks_history_animals",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/young_folks_history_animals.expected.json",
    ),
    // CH-4428: full-source De Morgan plate (this PR)
    (
        "pg68662_elements_of_arithmetic",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/pg68662_elements_of_arithmetic.expected.json",
    ),
    // EX-4458: ch1 CQ-first plate (shipped without being registered here)
    (
        "pg68662_elements_of_arithmetic_ch1",
        "research/shared/eval-data/curriculum-plates/L1_grade_school/pg68662_elements_of_arithmetic_ch1.expected.json",
    ),
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

fn load_plate(relative_path: &str) -> TutorRecord {
    let path = workspace_root().join(relative_path);
    let json = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read plate {relative_path}: {e}"));
    serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("plate {relative_path} failed to deserialize: {e}"))
}

#[test]
fn all_l1_plates_have_correct_level() {
    for (name, path) in PLATES {
        let record = load_plate(path);
        assert_eq!(
            record.document.level,
            Level::L1_grade_school,
            "plate {name} must be level L1_grade_school, got {:?}",
            record.document.level
        );
    }
}

#[test]
fn all_l1_plates_pass_schema_validation() {
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
fn all_l1_plates_pass_semantic_validation() {
    for (name, path) in PLATES {
        let record = load_plate(path);
        record.validate_semantics().unwrap_or_else(|e| {
            panic!("{name} failed semantic validation: {e}");
        });
    }
}

#[test]
fn all_l1_plates_meet_cq_minimum() {
    for (name, path) in PLATES {
        if MINIMUM_EXEMPT.contains(name) {
            continue;
        }
        let record = load_plate(path);
        let cq_count = record.layers.layer_3_curiosity.cqs.len();
        assert!(
            cq_count >= MIN_CQS,
            "{name}: expected ≥{MIN_CQS} CQs, got {cq_count}"
        );
    }
}

#[test]
fn all_l1_plates_meet_triple_minimum() {
    for (name, path) in PLATES {
        if MINIMUM_EXEMPT.contains(name) {
            continue;
        }
        let record = load_plate(path);
        let triple_count = record.layers.layer_1_literal.triples.len();
        assert!(
            triple_count >= MIN_L1_TRIPLES,
            "{name}: expected ≥{MIN_L1_TRIPLES} L1 triples, got {triple_count}"
        );
    }
}

#[test]
fn all_l1_plates_meet_entity_minimum() {
    for (name, path) in PLATES {
        if MINIMUM_EXEMPT.contains(name) {
            continue;
        }
        let record = load_plate(path);
        let entity_count = record.layers.layer_2_ontology.entities.len();
        assert!(
            entity_count >= MIN_L2_ENTITIES,
            "{name}: expected ≥{MIN_L2_ENTITIES} L2 entities, got {entity_count}"
        );
    }
}

#[test]
fn treasure_island_plate_references_pirates() {
    let record = load_plate(
        "research/shared/eval-data/curriculum-plates/L1_grade_school/pg120_treasure_island.expected.json",
    );
    let all_text = serde_json::to_string(&record).unwrap().to_lowercase();
    assert!(
        all_text.contains("pirate")
            || all_text.contains("treasure")
            || all_text.contains("jim hawkins")
            || all_text.contains("silver"),
        "Treasure Island plate must reference pirate / treasure / Jim Hawkins / Long John Silver"
    );
}

#[test]
fn flatland_plate_references_dimensions() {
    let record = load_plate(
        "research/shared/eval-data/curriculum-plates/L1_grade_school/pg201_flatland_a_romance_of_many_dimensions.expected.json",
    );
    let all_text = serde_json::to_string(&record).unwrap().to_lowercase();
    assert!(
        all_text.contains("dimension")
            || all_text.contains("square")
            || all_text.contains("flatland"),
        "Flatland plate must reference dimension / square / Flatland"
    );
}

#[test]
fn aesops_fables_plate_references_fables() {
    let record = load_plate(
        "research/shared/eval-data/curriculum-plates/L1_grade_school/aesops_fables.expected.json",
    );
    let all_text = serde_json::to_string(&record).unwrap().to_lowercase();
    assert!(
        all_text.contains("fable") || all_text.contains("moral") || all_text.contains("aesop"),
        "Aesop's Fables plate must reference fable / moral / Aesop"
    );
}

#[test]
fn plates_dir_matches_registry() {
    // Derive from PLATES so the registry and the on-disk set can't silently drift
    // (CH-4428 review: the hard-coded "30" went stale when EX-4458 + this PR added plates).
    let plates_dir =
        workspace_root().join("research/shared/eval-data/curriculum-plates/L1_grade_school");
    let count = std::fs::read_dir(&plates_dir)
        .expect("L1_grade_school plates dir must exist")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".expected.json"))
        .count();
    assert_eq!(
        count,
        PLATES.len(),
        "on-disk L1 plate count ({count}) must equal the PLATES registry ({})",
        PLATES.len()
    );
}
