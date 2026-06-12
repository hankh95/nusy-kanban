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
}
