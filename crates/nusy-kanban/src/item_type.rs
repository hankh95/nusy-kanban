//! Item types for the dual-board kanban system.
//!
//! Development board: Expedition, Chore, Voyage, Hazard, Signal
//! Research board: Paper, Hypothesis, Experiment, Measure, Idea, Literature

/// All item types across both boards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemType {
    // Development board (nautical)
    Expedition,
    Chore,
    Voyage,
    Hazard,
    Signal,
    Feature,

    // Research board (HDD)
    Paper,
    Hypothesis,
    Experiment,
    Measure,
    Idea,
    Literature,
}

impl ItemType {
    /// All development board types.
    pub const DEV: &[ItemType] = &[
        ItemType::Expedition,
        ItemType::Chore,
        ItemType::Voyage,
        ItemType::Hazard,
        ItemType::Signal,
        ItemType::Feature,
    ];

    /// All research board types.
    pub const RESEARCH: &[ItemType] = &[
        ItemType::Paper,
        ItemType::Hypothesis,
        ItemType::Experiment,
        ItemType::Measure,
        ItemType::Idea,
        ItemType::Literature,
    ];

    /// The ID prefix for this item type.
    ///
    /// Dev items use short 2-letter prefixes (typed frequently in commits/branches).
    /// Research items keep readable prefixes (referenced in papers/SPARQL, rarely typed).
    pub fn prefix(&self) -> &'static str {
        match self {
            // Dev board — short prefixes (typed dozens of times daily)
            ItemType::Expedition => "EX",
            ItemType::Chore => "CH",
            ItemType::Voyage => "VY",
            ItemType::Hazard => "HZ",
            ItemType::Signal => "SG",
            ItemType::Feature => "FT",
            // Research board — keep readable prefixes (EXPR-131.1 > XP-131.1)
            ItemType::Paper => "PAPER",
            ItemType::Hypothesis => "H",
            ItemType::Experiment => "EXPR",
            ItemType::Measure => "M",
            ItemType::Idea => "IDEA",
            ItemType::Literature => "LIT",
        }
    }

    /// Legacy prefixes for backward compatibility (pre-v0.14.1 IDs).
    /// Used by parsers to recognize old-format IDs like EXP-1234.
    pub fn legacy_prefixes(&self) -> &'static [&'static str] {
        match self {
            ItemType::Expedition => &["EXP"],
            ItemType::Chore => &["CHORE"],
            ItemType::Voyage => &["VOY"],
            ItemType::Hazard => &["HAZ"],
            ItemType::Signal => &["SIG"],
            ItemType::Feature => &["FEAT"],
            // Research prefixes unchanged — no legacy needed
            ItemType::Paper => &[],
            ItemType::Hypothesis => &[],
            ItemType::Experiment => &[],
            ItemType::Measure => &[],
            ItemType::Idea => &[],
            ItemType::Literature => &[],
        }
    }

    /// All known prefixes for this type (current + legacy).
    pub fn all_prefixes(&self) -> Vec<&'static str> {
        let mut v = vec![self.prefix()];
        v.extend_from_slice(self.legacy_prefixes());
        v
    }

    /// Whether this type belongs to the research board.
    pub fn is_research(&self) -> bool {
        self.board() == "research"
    }

    /// The string name used in frontmatter `type:` field.
    pub fn as_str(&self) -> &'static str {
        match self {
            ItemType::Expedition => "expedition",
            ItemType::Chore => "chore",
            ItemType::Voyage => "voyage",
            ItemType::Hazard => "hazard",
            ItemType::Signal => "signal",
            ItemType::Feature => "feature",
            ItemType::Paper => "paper",
            ItemType::Hypothesis => "hypothesis",
            ItemType::Experiment => "experiment",
            ItemType::Measure => "measure",
            ItemType::Idea => "idea",
            ItemType::Literature => "literature",
        }
    }

    /// Parse from a string (case-insensitive). Accepts full names, old prefixes, and new prefixes.
    pub fn from_str_loose(s: &str) -> Option<ItemType> {
        match s.to_lowercase().as_str() {
            "expedition" | "exp" | "ex" => Some(ItemType::Expedition),
            "chore" | "task" | "status-report" | "status_report" | "bug" | "ch" => {
                Some(ItemType::Chore)
            }
            "voyage" | "voy" | "vy" => Some(ItemType::Voyage),
            "hazard" | "haz" | "hz" => Some(ItemType::Hazard),
            "signal" | "sig" | "sg" => Some(ItemType::Signal),
            "feature" | "feat" | "ft" => Some(ItemType::Feature),
            "paper" => Some(ItemType::Paper),
            "hypothesis" | "h" => Some(ItemType::Hypothesis),
            "experiment" | "expr" => Some(ItemType::Experiment),
            "measure" | "m" => Some(ItemType::Measure),
            "idea" => Some(ItemType::Idea),
            "literature" | "lit" => Some(ItemType::Literature),
            _ => None,
        }
    }

    /// Which board this type belongs to.
    pub fn board(&self) -> &'static str {
        match self {
            ItemType::Expedition
            | ItemType::Chore
            | ItemType::Voyage
            | ItemType::Hazard
            | ItemType::Signal
            | ItemType::Feature => "development",

            ItemType::Paper
            | ItemType::Hypothesis
            | ItemType::Experiment
            | ItemType::Measure
            | ItemType::Idea
            | ItemType::Literature => "research",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefix_round_trip() {
        for &ty in ItemType::DEV.iter().chain(ItemType::RESEARCH.iter()) {
            let prefix = ty.prefix();
            assert!(!prefix.is_empty(), "{:?} should have a prefix", ty);
        }
    }

    #[test]
    fn test_from_str_case_insensitive() {
        assert_eq!(
            ItemType::from_str_loose("expedition"),
            Some(ItemType::Expedition)
        );
        assert_eq!(
            ItemType::from_str_loose("Expedition"),
            Some(ItemType::Expedition)
        );
        assert_eq!(ItemType::from_str_loose("EXP"), Some(ItemType::Expedition));
        assert_eq!(ItemType::from_str_loose("voyage"), Some(ItemType::Voyage));
        assert_eq!(ItemType::from_str_loose("paper"), Some(ItemType::Paper));
        assert_eq!(ItemType::from_str_loose("nonexistent"), None);
    }

    #[test]
    fn test_board_assignment() {
        for &ty in ItemType::DEV {
            assert_eq!(ty.board(), "development");
        }
        for &ty in ItemType::RESEARCH {
            assert_eq!(ty.board(), "research");
        }
    }

    #[test]
    fn test_as_str_round_trip() {
        for &ty in ItemType::DEV.iter().chain(ItemType::RESEARCH.iter()) {
            let s = ty.as_str();
            let parsed = ItemType::from_str_loose(s).expect("should round-trip");
            assert_eq!(parsed, ty);
        }
    }
}
