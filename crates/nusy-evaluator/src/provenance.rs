//! Phase 3 — provenance chain types (D4 carry-forward).
//!
//! Each `PassResult` carries a `Vec<TripleRef>` — the supporting triples
//! that backed the answer, tagged by Y-layer so a reviewer can explain
//! *why* the being knows X. The chain is harvested from
//! `nusy_safety::justification::EvidenceTrail` (the existing being chat
//! trail), which already exposes `supporting_triples` and per-chunk
//! `y_layer` strings.
//!
//! When EX-α (the cortex API) lands with per-triple `source_chunk_id`
//! tagging, populating this struct gets richer without an API change.

use serde::{Deserialize, Serialize};

use nusy_safety::justification::{ChunkRef, EvidenceTrail};

use crate::grader::{Grade, GradeReport};

/// One supporting triple in the provenance chain. The Y-layer tag is the
/// load-bearing field for D4: every pass cites at least one triple, and
/// the reviewer can see at a glance whether the answer leaned on Y0
/// (raw prose) vs Y1 (semantic) vs Y2 (reasoning).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TripleRef {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    /// `"y0" | "y1" | "y2" | …` — string form to round-trip cleanly through
    /// `nusy-safety`'s `ChunkRef::y_layer`.
    pub y_layer: String,
    /// FK to ChunkTable. None until EX-α (and the existing chat trail when
    /// not configured to surface it) populates it.
    pub source_chunk_id: Option<String>,
    /// Optional document path the chunk came from (Y0 prose origin).
    pub source_document: Option<String>,
}

/// One graded CQ result, with the answer the being gave and the provenance
/// chain that backed it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PassResult {
    pub cq_id: String,
    pub question: String,
    pub dimension: String,
    pub grade: Grade,
    pub response: String,
    /// Keywords from the CQ's expected set that matched the response.
    pub matched_keywords: Vec<String>,
    /// Provenance chain. Empty vec = no graph trace was available
    /// (degraded mode pre-EX-α, or graceful "I don't know" path).
    pub provenance: Vec<TripleRef>,
    /// Raw refusal-regex match — informational, useful for debugging
    /// over-/under-matching.
    pub refusal_signal: bool,
    /// Persona-leak signature matched (A10).
    pub persona_leak_signal: bool,
}

impl PassResult {
    /// Compose a `PassResult` from the grader's `GradeReport`, the CQ, the
    /// being's response text, and the being's evidence trail.
    pub fn build(
        cq_id: String,
        question: String,
        dimension: String,
        response: String,
        report: GradeReport,
        provenance: Vec<TripleRef>,
    ) -> Self {
        PassResult {
            cq_id,
            question,
            dimension,
            grade: report.grade,
            response,
            matched_keywords: report.matched_keywords,
            provenance,
            refusal_signal: report.refusal_signal,
            persona_leak_signal: report.persona_leak_signal,
        }
    }
}

/// Convert an `EvidenceTrail` from `nusy-safety` into the evaluator's
/// `Vec<TripleRef>`. The trail's `supporting_triples` are `(s,p,o)` tuples;
/// the trail's `source_chunks` carry per-chunk `y_layer` and `document`.
/// We pair them up best-effort: for each supporting triple, we attach the
/// Y-layer of the *first* source chunk (since the trail does not currently
/// link triples to specific chunks). EX-α will tighten this once it ships
/// per-triple chunk-id tagging.
pub fn provenance_from_trail(trail: &EvidenceTrail) -> Vec<TripleRef> {
    if trail.supporting_triples.is_empty() {
        return Vec::new();
    }
    let primary_chunk = trail.source_chunks.first();
    trail
        .supporting_triples
        .iter()
        .map(|(s, p, o)| TripleRef {
            subject: s.clone(),
            predicate: p.clone(),
            object: o.clone(),
            y_layer: primary_chunk
                .map(|c: &ChunkRef| c.y_layer.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            source_chunk_id: primary_chunk.map(|c: &ChunkRef| c.chunk_id.clone()),
            source_document: primary_chunk.map(|c: &ChunkRef| c.document.clone()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusy_safety::justification::ChunkRef;

    fn trail_with_triples() -> EvidenceTrail {
        EvidenceTrail {
            query: "What is an Archer?".to_string(),
            conclusion: "An archer is a person who uses a bow.".to_string(),
            supporting_triples: vec![
                (
                    "Archer".to_string(),
                    "rdf:type".to_string(),
                    "Person".to_string(),
                ),
                (
                    "Archer".to_string(),
                    "uses_tool".to_string(),
                    "Bow".to_string(),
                ),
            ],
            source_chunks: vec![ChunkRef {
                chunk_id: "chunk_002".to_string(),
                document: "dame_wonder.md".to_string(),
                paragraph: "A-D stanza".to_string(),
                y_layer: "y1".to_string(),
            }],
            confidence: 0.9,
            reasoning_path: vec!["lookup Archer".to_string(), "compose answer".to_string()],
        }
    }

    #[test]
    fn provenance_inherits_y_layer_from_first_chunk() {
        let prov = provenance_from_trail(&trail_with_triples());
        assert_eq!(prov.len(), 2);
        for r in &prov {
            assert_eq!(r.y_layer, "y1");
            assert_eq!(r.source_chunk_id.as_deref(), Some("chunk_002"));
            assert_eq!(r.source_document.as_deref(), Some("dame_wonder.md"));
        }
    }

    #[test]
    fn empty_trail_produces_empty_chain() {
        let trail = EvidenceTrail {
            query: "?".to_string(),
            conclusion: String::new(),
            supporting_triples: vec![],
            source_chunks: vec![],
            confidence: 0.0,
            reasoning_path: vec![],
        };
        assert!(provenance_from_trail(&trail).is_empty());
    }

    #[test]
    fn triples_without_chunks_get_unknown_y_layer() {
        let trail = EvidenceTrail {
            query: "?".to_string(),
            conclusion: "x".to_string(),
            supporting_triples: vec![("A".to_string(), "p".to_string(), "B".to_string())],
            source_chunks: vec![],
            confidence: 0.5,
            reasoning_path: vec![],
        };
        let prov = provenance_from_trail(&trail);
        assert_eq!(prov.len(), 1);
        assert_eq!(prov[0].y_layer, "unknown");
        assert!(prov[0].source_chunk_id.is_none());
    }
}
