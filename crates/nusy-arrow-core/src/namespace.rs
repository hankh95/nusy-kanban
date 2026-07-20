//! Namespace partitioning for the NuSy graph substrate.
//!
//! Five canonical namespaces partition all knowledge:
//! - **World**: external facts, domain ontologies
//! - **Work**: kanban items, project artifacts
//! - **Code**: source code objects, compilation artifacts, dependency edges
//! - **Research**: papers, hypotheses, experiments
//! - **Self_**: being-specific knowledge, self-model

use std::fmt;

/// The five canonical namespaces for graph partitioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Namespace {
    /// External facts, domain ontologies, shared knowledge.
    World,
    /// Kanban items, project artifacts.
    Work,
    /// Source code objects, compilation artifacts, dependency edges.
    /// Code has compilation semantics, type relationships, and test coverage
    /// that work items don't — forcing it under `work` creates impedance mismatch.
    Code,
    /// Papers, hypotheses, experiments (HDD).
    Research,
    /// Being-specific: self-model, journal, metacognition.
    Self_,
}

impl Namespace {
    /// All namespace variants in canonical order.
    pub const ALL: [Namespace; 5] = [
        Namespace::World,
        Namespace::Work,
        Namespace::Code,
        Namespace::Research,
        Namespace::Self_,
    ];

    /// Canonical string identifier used in Arrow columns and Parquet paths.
    pub fn as_str(&self) -> &'static str {
        match self {
            Namespace::World => "world",
            Namespace::Work => "work",
            Namespace::Code => "code",
            Namespace::Research => "research",
            Namespace::Self_ => "self",
        }
    }

    /// Parse from string (case-insensitive).
    pub fn from_str_loose(s: &str) -> Option<Namespace> {
        match s.to_lowercase().as_str() {
            "world" => Some(Namespace::World),
            "work" => Some(Namespace::Work),
            "code" | "codegraph" => Some(Namespace::Code),
            "research" => Some(Namespace::Research),
            "self" | "self_" => Some(Namespace::Self_),
            _ => None,
        }
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─── Patient (PHI) partition predicate ─────────────────────────────────────
//
// CH-5510 (Captain-ratified Approach 1, 2026-07-05): the patient-partition
// predicate lives here in `nusy-arrow-core` (which owns namespace/partition
// primitives) so that FOSS crates — `nusy-dual-store`, `nusy-perceive`,
// `nusy-grapher` — can check it WITHOUT depending on the PRODUCT crate
// `nusy-patient-memory` (the D17 open-core edge). `nusy-patient-memory`
// re-exports both symbols, so its public API is unchanged. This is a pure
// relocation: the D33 patient-exclusion guard behavior is preserved EXACTLY
// (identical logic — no weakening, per CH-5149). Mirrors precedent EX-5133.

/// Prefix of every per-patient named-graph handle (the PHI boundary, D4).
pub const PATIENT_GRAPH_PREFIX: &str = "patient:";

/// Is this graph handle in the PHI / patient partition? The single predicate
/// every prune/dedup/evict/COG-export surface checks (the D33 patient-exclusion
/// guard). `None` (no graph) is never patient.
pub fn is_patient_partition(graph: Option<&str>) -> bool {
    graph.is_some_and(|g| g.starts_with(PATIENT_GRAPH_PREFIX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_namespace_roundtrip() {
        for ns in Namespace::ALL {
            let s = ns.as_str();
            let parsed = Namespace::from_str_loose(s).unwrap();
            assert_eq!(ns, parsed);
        }
    }

    #[test]
    fn test_namespace_case_insensitive() {
        assert_eq!(Namespace::from_str_loose("WORLD"), Some(Namespace::World));
        assert_eq!(Namespace::from_str_loose("Self"), Some(Namespace::Self_));
        assert_eq!(Namespace::from_str_loose("CODE"), Some(Namespace::Code));
    }

    #[test]
    fn test_namespace_codegraph_alias() {
        // "codegraph" was the historical sub-partition under work; now maps to Code
        assert_eq!(
            Namespace::from_str_loose("codegraph"),
            Some(Namespace::Code)
        );
    }

    #[test]
    fn test_namespace_unknown() {
        assert_eq!(Namespace::from_str_loose("unknown"), None);
    }

    #[test]
    fn test_namespace_all_has_five() {
        assert_eq!(Namespace::ALL.len(), 5);
        // Verify canonical order
        assert_eq!(Namespace::ALL[0], Namespace::World);
        assert_eq!(Namespace::ALL[1], Namespace::Work);
        assert_eq!(Namespace::ALL[2], Namespace::Code);
        assert_eq!(Namespace::ALL[3], Namespace::Research);
        assert_eq!(Namespace::ALL[4], Namespace::Self_);
    }

    #[test]
    fn test_code_namespace_parquet_roundtrip() {
        // Verify code namespace survives string serialization (as used in Parquet columns)
        let ns = Namespace::Code;
        let serialized = ns.as_str();
        assert_eq!(serialized, "code");
        let deserialized = Namespace::from_str_loose(serialized).unwrap();
        assert_eq!(ns, deserialized);
    }

    /// CH-5510 D33-preservation guard: the relocated patient-partition predicate
    /// must behave EXACTLY as before — `patient:`-prefixed graphs are PHI, all
    /// others (and `None`) are not. A regression here would silently weaken the
    /// patient-exclusion guard the FOSS decoupling must NOT touch (CH-5149).
    #[test]
    fn test_is_patient_partition_preserves_d33_guard() {
        assert_eq!(PATIENT_GRAPH_PREFIX, "patient:");
        // PHI partition → true
        assert!(is_patient_partition(Some("patient:jane-doe")));
        assert!(is_patient_partition(Some("patient:")));
        // Non-patient graphs → false
        assert!(!is_patient_partition(Some("world")));
        assert!(!is_patient_partition(Some("work:foo")));
        assert!(!is_patient_partition(Some("not-patient:x")));
        assert!(!is_patient_partition(Some("")));
        // No graph is never patient
        assert!(!is_patient_partition(None));
    }
}
