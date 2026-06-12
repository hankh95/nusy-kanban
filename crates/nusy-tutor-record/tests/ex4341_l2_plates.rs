//! L2 (middle-school) Customer's Plate validation.
//!
//! CH-4428 review (PROP-2578 #4): the L2 Boyden algebra plate shipped with zero
//! automated coverage. This mirrors the L1/L3 suites: every registered L2 plate
//! deserializes, validates against the embedded JSON schema, and is declared at
//! the correct level. New L2 plates must be appended to `PLATES`.

use nusy_tutor_record::{Level, TutorRecord, validate_against_schema};

const PLATES: &[(&str, &str)] = &[
    // CH-4428: full First Book in Algebra (Wallace C. Boyden) plate
    (
        "pg13309_first_book_algebra",
        "research/shared/eval-data/curriculum-plates/L2_middle_school/pg13309_first_book_algebra.expected.json",
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
fn all_l2_plates_have_correct_level() {
    for (name, path) in PLATES {
        let record = load_plate(path);
        assert_eq!(
            record.document.level,
            Level::L2_middle_school,
            "plate {name} must be level L2_middle_school, got {:?}",
            record.document.level
        );
    }
}

#[test]
fn all_l2_plates_pass_schema_validation() {
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
fn plates_dir_matches_registry() {
    let plates_dir =
        workspace_root().join("research/shared/eval-data/curriculum-plates/L2_middle_school");
    let count = std::fs::read_dir(&plates_dir)
        .expect("L2_middle_school plates dir must exist")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".expected.json"))
        .count();
    assert_eq!(
        count,
        PLATES.len(),
        "on-disk L2 plate count ({count}) must equal the PLATES registry ({})",
        PLATES.len()
    );
}
