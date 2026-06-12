//! Shared test utilities for nusy-arrow-git integration tests.

use nusy_arrow_core::Triple;

/// Create a simple test triple with the given subject and object.
///
/// Uses `rdf:type` as predicate and 0.9 confidence. Suitable for
/// integration tests that need representative but not domain-specific data.
pub fn sample_triple(subj: &str, obj: &str) -> Triple {
    Triple {
        subject: subj.to_string(),
        predicate: "rdf:type".to_string(),
        object: obj.to_string(),
        graph: None,
        confidence: Some(0.9),
        source_document: None,
        source_chunk_id: None,
        extracted_by: None,
        caused_by: None,
        derived_from: None,
        consolidated_at: None,
        certifiability_class: None,
        object_datatype: None,
    }
}
