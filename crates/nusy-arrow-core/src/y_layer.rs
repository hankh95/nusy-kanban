//! Y-layer partitioning within namespaces.
//!
//! The Y-layer hierarchy organizes knowledge by type:
//! - **Definitional (Y0-Y2):** transferable via COGs
//! - **Experiential (Y3-Y6):** being-specific

use std::fmt;

/// The seven Y-layers of being knowledge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum YLayer {
    /// Y0: Raw text chunks with provenance.
    Prose = 0,
    /// Y1: Entities, relationships, concepts.
    Semantic = 1,
    /// Y2: Rules, ontology, constraints.
    Reasoning = 2,
    /// Y3: Conversations, actions, learning events.
    Experience = 3,
    /// Y4: Opinions, reflections, mental notes.
    Journal = 4,
    /// Y5: Workflows, skills.
    Procedural = 5,
    /// Y6: Calibration, error tracking.
    Metacognitive = 6,
}

impl YLayer {
    /// All layers in order.
    pub const ALL: [YLayer; 7] = [
        YLayer::Prose,
        YLayer::Semantic,
        YLayer::Reasoning,
        YLayer::Experience,
        YLayer::Journal,
        YLayer::Procedural,
        YLayer::Metacognitive,
    ];

    /// Definitional layers (Y0-Y2) — transferable via COGs.
    pub const DEFINITIONAL: [YLayer; 3] = [YLayer::Prose, YLayer::Semantic, YLayer::Reasoning];

    /// Experiential layers (Y3-Y6) — being-specific.
    pub const EXPERIENTIAL: [YLayer; 4] = [
        YLayer::Experience,
        YLayer::Journal,
        YLayer::Procedural,
        YLayer::Metacognitive,
    ];

    /// Numeric value (0-6).
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// Parse from u8.
    pub fn from_u8(v: u8) -> Option<YLayer> {
        match v {
            0 => Some(YLayer::Prose),
            1 => Some(YLayer::Semantic),
            2 => Some(YLayer::Reasoning),
            3 => Some(YLayer::Experience),
            4 => Some(YLayer::Journal),
            5 => Some(YLayer::Procedural),
            6 => Some(YLayer::Metacognitive),
            _ => None,
        }
    }

    /// Whether this layer is definitional (transferable).
    pub fn is_definitional(self) -> bool {
        (self as u8) <= 2
    }

    /// Whether this layer is experiential (being-specific).
    pub fn is_experiential(self) -> bool {
        (self as u8) >= 3
    }

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            YLayer::Prose => "Prose",
            YLayer::Semantic => "Semantic",
            YLayer::Reasoning => "Reasoning",
            YLayer::Experience => "Experience",
            YLayer::Journal => "Journal",
            YLayer::Procedural => "Procedural",
            YLayer::Metacognitive => "Metacognitive",
        }
    }
}

impl fmt::Display for YLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Y{}: {}", self.as_u8(), self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layer_roundtrip() {
        for layer in YLayer::ALL {
            let v = layer.as_u8();
            let parsed = YLayer::from_u8(v).unwrap();
            assert_eq!(layer, parsed);
        }
    }

    #[test]
    fn test_definitional_experiential_split() {
        for layer in YLayer::DEFINITIONAL {
            assert!(layer.is_definitional());
            assert!(!layer.is_experiential());
        }
        for layer in YLayer::EXPERIENTIAL {
            assert!(layer.is_experiential());
            assert!(!layer.is_definitional());
        }
    }

    #[test]
    fn test_invalid_layer() {
        assert_eq!(YLayer::from_u8(7), None);
        assert_eq!(YLayer::from_u8(255), None);
    }
}
