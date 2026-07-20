//! The typed relationship vocabulary (CH-6109).
//!
//! # Why this exists
//!
//! `nk` had a typed relation store (`RelationsStore`, `relations_schema`) whose doc
//! comment already named `implements` / `spawns` / `blocks` — the graph was designed. But
//! nothing reached it: `relations.parquet` never existed on disk, no CLI command called
//! `add_relation`, and all real relationship data lived in two flat, untyped list columns
//! (`items.related`, `items.depends_on`) that `nk create` could not even set.
//!
//! So an H→M→EXPR research trio was wired with `--related`, which discards **direction**
//! and **kind**: you could not ask *"which experiment validates this hypothesis?"* without
//! reading prose. Five of the seven relationships the predecessor (yurtle-kanban) modelled
//! had no representation at all — and they were exactly the research-side ones.
//!
//! This module is the vocabulary: a superset of `.yurtle-kanban/ontology/kanban.ttl` plus
//! the predicates its SHACL shapes reference.
//!
//! # One source of truth
//!
//! `related` and `dependsOn` are ALSO stored as typed edges. The flat `items.related` /
//! `items.depends_on` columns are a **projection** of those edges, written in the same
//! operation — never independently. Everything downstream (`roadmap`, `critical-path`,
//! `worklist`) keeps reading the flat columns unchanged; they simply stop being the place
//! relationships are *authored*. Two independently-written stores would drift, which is the
//! failure this fleet keeps paying for.

use crate::item_type::ItemType;

/// A relationship predicate: the kind of a typed edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Predicate {
    /// Canonical name as stored in `relations.predicate` and accepted on the CLI.
    pub name: &'static str,
    /// Item types allowed as the SOURCE, or empty for "any item".
    pub domain: &'static [&'static str],
    /// Item types allowed as the TARGET, or empty for "any item".
    pub range: &'static [&'static str],
    /// The inverse predicate's name, if this edge has a named inverse.
    ///
    /// Only ONE direction is ever stored. Storing both invites them to disagree; the
    /// inverse is derived at query time.
    pub inverse: Option<&'static str>,
    /// One-line meaning, surfaced in `--help` and in validation errors.
    pub doc: &'static str,
}

/// Type prefixes, matching the ID scheme (`EX-`, `VY-`, `H-`, `EXPR-`, `M-`, `PAPER-`).
const DEV_ANY: &[&str] = &[];
const EXPEDITION: &[&str] = &["expedition"];
const VOYAGE: &[&str] = &["voyage"];
const HYPOTHESIS: &[&str] = &["hypothesis"];
const PAPER: &[&str] = &["paper"];
const EXPERIMENT: &[&str] = &["experiment"];
const MEASURE: &[&str] = &["measure"];

/// The vocabulary. A superset of yurtle-kanban's seven declared `owl:ObjectProperty`
/// terms plus `blocks` / `parent`, which its SHACL shapes reference.
///
/// Deliberately NOT included: `kb:acfDimension` is a value constraint
/// (`sh:in ("AC1" … "AC5")`) — a typed attribute on a hypothesis, not an item→item edge.
/// Modelling it here would misrepresent it.
pub const PREDICATES: &[Predicate] = &[
    // ── Item ↔ Item (the two that already existed, now typed) ──
    Predicate {
        name: "related",
        domain: DEV_ANY,
        range: DEV_ANY,
        inverse: None, // symmetric in practice
        doc: "informational link between any two items (the existing --related)",
    },
    Predicate {
        name: "dependsOn",
        domain: DEV_ANY,
        range: DEV_ANY,
        inverse: Some("blocks"),
        doc: "this item is BLOCKED BY the target (the existing --depends-on)",
    },
    Predicate {
        name: "blocks",
        domain: DEV_ANY,
        range: DEV_ANY,
        inverse: Some("dependsOn"),
        doc: "this item BLOCKS the target (inverse of dependsOn; stored one way only)",
    },
    Predicate {
        name: "parent",
        domain: DEV_ANY,
        range: DEV_ANY,
        inverse: None,
        doc: "hierarchical containment — this item is contained by the target",
    },
    // ── Dev-board structure ──
    Predicate {
        name: "implements",
        domain: EXPEDITION,
        range: VOYAGE,
        inverse: Some("spawns"),
        doc: "an expedition delivers work toward a voyage",
    },
    Predicate {
        name: "spawns",
        domain: VOYAGE,
        range: EXPEDITION,
        inverse: Some("implements"),
        doc: "a voyage creates and coordinates an expedition",
    },
    // ── Research board: the H → M → EXPR trio, finally expressible ──
    Predicate {
        name: "tests",
        domain: HYPOTHESIS,
        range: PAPER,
        inverse: None,
        doc: "a hypothesis is tested in the context of a paper",
    },
    Predicate {
        name: "validates",
        domain: EXPERIMENT,
        range: HYPOTHESIS,
        inverse: None,
        doc: "an experiment produces evidence for or against a hypothesis",
    },
    Predicate {
        name: "measures",
        domain: MEASURE,
        // The ontology declares Measure → Experiment, but `/hypothesize` links measures to
        // the HYPOTHESIS. Rather than canonise one and silently invalidate existing
        // practice, both targets are accepted — the mismatch is real and documented in
        // CH-6109 for a follow-up ruling, not resolved by fiat here.
        range: &["experiment", "hypothesis"],
        inverse: None,
        doc: "a measure tracks the outcome of an experiment (or, per current practice, a hypothesis)",
    },
];

/// Look up a predicate by name (case-insensitive; `depends_on`/`depends-on` accepted for
/// `dependsOn`, so the CLI spelling of the existing flag keeps working).
pub fn lookup(name: &str) -> Option<&'static Predicate> {
    let n = name.trim().to_ascii_lowercase().replace(['_', '-'], "");
    PREDICATES.iter().find(|p| p.name.to_ascii_lowercase() == n)
}

/// All predicate names, for `--help` and error messages.
pub fn names() -> Vec<&'static str> {
    PREDICATES.iter().map(|p| p.name).collect()
}

/// Why a proposed edge was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeError {
    /// The predicate is not in the vocabulary.
    UnknownPredicate {
        given: String,
        known: Vec<&'static str>,
    },
    /// The source item's type is not in the predicate's domain.
    BadDomain {
        predicate: &'static str,
        got: String,
        allowed: Vec<&'static str>,
    },
    /// The target item's type is not in the predicate's range.
    BadRange {
        predicate: &'static str,
        got: String,
        allowed: Vec<&'static str>,
    },
    /// An item cannot relate to itself.
    SelfEdge { id: String },
}

impl std::fmt::Display for EdgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EdgeError::UnknownPredicate { given, known } => write!(
                f,
                "unknown relationship '{given}' — known: {}",
                known.join(", ")
            ),
            EdgeError::BadDomain {
                predicate,
                got,
                allowed,
            } => write!(
                f,
                "'{predicate}' cannot start from a {got} (allowed: {})",
                allowed.join(", ")
            ),
            EdgeError::BadRange {
                predicate,
                got,
                allowed,
            } => write!(
                f,
                "'{predicate}' cannot point at a {got} (allowed: {})",
                allowed.join(", ")
            ),
            EdgeError::SelfEdge { id } => write!(f, "{id} cannot relate to itself"),
        }
    }
}

/// Validate a proposed edge against the vocabulary.
///
/// `source_type` / `target_type` are the item types (e.g. `"expedition"`). A `None` target
/// type means the target could not be resolved — validation of RANGE is then skipped
/// rather than guessed, so a cross-board edge to an item this store cannot see is still
/// allowed. Refusing an edge because we could not look up its target would make the
/// research trio unusable across boards, which is the point of adding it.
pub fn validate_edge(
    predicate: &str,
    source_id: &str,
    source_type: &str,
    target_id: &str,
    target_type: Option<&str>,
) -> std::result::Result<&'static Predicate, EdgeError> {
    let p = lookup(predicate).ok_or_else(|| EdgeError::UnknownPredicate {
        given: predicate.to_string(),
        known: names(),
    })?;

    if source_id.eq_ignore_ascii_case(target_id) {
        return Err(EdgeError::SelfEdge {
            id: source_id.to_string(),
        });
    }

    if !p.domain.is_empty() && !p.domain.iter().any(|d| d.eq_ignore_ascii_case(source_type)) {
        return Err(EdgeError::BadDomain {
            predicate: p.name,
            got: source_type.to_string(),
            allowed: p.domain.to_vec(),
        });
    }
    if let Some(tt) = target_type
        && !p.range.is_empty()
        && !p.range.iter().any(|r| r.eq_ignore_ascii_case(tt))
    {
        return Err(EdgeError::BadRange {
            predicate: p.name,
            got: tt.to_string(),
            allowed: p.range.to_vec(),
        });
    }
    Ok(p)
}

/// Parse a `predicate:TARGET-ID` CLI argument.
///
/// Splits on the FIRST colon only, so an ID containing a colon survives.
pub fn parse_spec(spec: &str) -> Option<(&str, &str)> {
    let (pred, target) = spec.split_once(':')?;
    let (pred, target) = (pred.trim(), target.trim());
    (!pred.is_empty() && !target.is_empty()).then_some((pred, target))
}

/// Whether a predicate's edges are ALSO projected into the flat `items.related` /
/// `items.depends_on` columns, so existing readers (`roadmap`, `critical-path`,
/// `worklist`, `nk show`) keep working unchanged.
pub fn flat_column_for(predicate: &str) -> Option<&'static str> {
    match lookup(predicate)?.name {
        "related" => Some("related"),
        "dependsOn" => Some("depends_on"),
        _ => None,
    }
}

/// The item type of an ID, inferred from its prefix — for validating an edge whose target
/// this store cannot resolve (e.g. a cross-board research item).
pub fn type_from_id(id: &str) -> Option<&'static str> {
    let prefix = id.split('-').next()?.to_ascii_uppercase();
    Some(match prefix.as_str() {
        "EX" | "EXP" => "expedition",
        "VY" | "VOY" => "voyage",
        "CH" | "CHORE" => "chore",
        "HZ" => "hazard",
        "SG" | "SIG" => "signal",
        "H" => "hypothesis",
        "EXPR" => "experiment",
        "M" => "measure",
        "PAPER" => "paper",
        "LIT" => "literature",
        "IDEA" => "idea",
        _ => return None,
    })
}

/// Best-effort item type for an ID: the store's answer if it has one, else the prefix.
pub fn resolve_type(id: &str, from_store: Option<&str>) -> Option<String> {
    from_store
        .map(str::to_string)
        .or_else(|| type_from_id(id).map(str::to_string))
}

/// Convenience: does this ItemType satisfy the named predicate's domain?
pub fn domain_allows(predicate: &str, t: ItemType) -> bool {
    match lookup(predicate) {
        Some(p) => {
            p.domain.is_empty() || p.domain.iter().any(|d| d.eq_ignore_ascii_case(t.as_str()))
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vocabulary_supersets_yurtle_kanban() {
        // The seven declared owl:ObjectProperty terms from .yurtle-kanban/ontology/kanban.ttl…
        for p in [
            "related",
            "dependsOn",
            "implements",
            "spawns",
            "tests",
            "validates",
            "measures",
        ] {
            assert!(
                lookup(p).is_some(),
                "missing declared yurtle predicate: {p}"
            );
        }
        // …plus the two its SHACL shapes reference.
        for p in ["blocks", "parent"] {
            assert!(
                lookup(p).is_some(),
                "missing shape-referenced predicate: {p}"
            );
        }
    }

    /// acfDimension is an sh:in value constraint, not an edge. Modelling it as a
    /// relationship would misrepresent the ontology.
    #[test]
    fn acf_dimension_is_not_a_relationship() {
        assert!(lookup("acfDimension").is_none());
    }

    #[test]
    fn lookup_is_forgiving_about_cli_spelling() {
        assert_eq!(lookup("dependsOn").map(|p| p.name), Some("dependsOn"));
        assert_eq!(lookup("depends_on").map(|p| p.name), Some("dependsOn"));
        assert_eq!(lookup("depends-on").map(|p| p.name), Some("dependsOn"));
        assert_eq!(lookup("DEPENDSON").map(|p| p.name), Some("dependsOn"));
        assert!(lookup("nonsense").is_none());
    }

    #[test]
    fn the_research_trio_is_expressible() {
        // The whole point: H → M → EXPR, which --related could not express with direction.
        assert!(
            validate_edge(
                "validates",
                "EXPR-1",
                "experiment",
                "H-2",
                Some("hypothesis")
            )
            .is_ok()
        );
        assert!(validate_edge("measures", "M-3", "measure", "EXPR-1", Some("experiment")).is_ok());
        assert!(validate_edge("tests", "H-2", "hypothesis", "PAPER-4", Some("paper")).is_ok());
    }

    /// The ontology says Measure → Experiment; /hypothesize links measures to the
    /// HYPOTHESIS. Both are accepted rather than silently invalidating live practice.
    #[test]
    fn measures_accepts_both_the_ontology_and_current_practice() {
        assert!(validate_edge("measures", "M-1", "measure", "EXPR-2", Some("experiment")).is_ok());
        assert!(validate_edge("measures", "M-1", "measure", "H-2", Some("hypothesis")).is_ok());
        // But not something unrelated.
        assert!(validate_edge("measures", "M-1", "measure", "CH-9", Some("chore")).is_err());
    }

    #[test]
    fn domain_and_range_are_enforced() {
        // A chore cannot implement a voyage — implements is Expedition → Voyage.
        let e = validate_edge("implements", "CH-1", "chore", "VY-2", Some("voyage")).unwrap_err();
        assert!(matches!(e, EdgeError::BadDomain { .. }), "got {e:?}");
        // An expedition cannot implement a chore.
        let e =
            validate_edge("implements", "EX-1", "expedition", "CH-2", Some("chore")).unwrap_err();
        assert!(matches!(e, EdgeError::BadRange { .. }), "got {e:?}");
    }

    /// A target this store cannot resolve must NOT be rejected — the research trio is
    /// cross-board, and refusing unresolvable targets would make it unusable.
    #[test]
    fn an_unresolvable_target_skips_range_validation_rather_than_failing() {
        assert!(validate_edge("validates", "EXPR-1", "experiment", "H-999", None).is_ok());
        // Domain is still enforced — we DO know the source's type.
        assert!(validate_edge("implements", "CH-1", "chore", "VY-999", None).is_err());
    }

    #[test]
    fn self_edges_are_refused() {
        let e =
            validate_edge("related", "EX-1", "expedition", "EX-1", Some("expedition")).unwrap_err();
        assert!(matches!(e, EdgeError::SelfEdge { .. }));
        // …case-insensitively.
        assert!(
            validate_edge("related", "EX-1", "expedition", "ex-1", Some("expedition")).is_err()
        );
    }

    #[test]
    fn unknown_predicates_name_the_alternatives() {
        let e =
            validate_edge("frobnicates", "EX-1", "expedition", "VY-2", Some("voyage")).unwrap_err();
        match &e {
            EdgeError::UnknownPredicate { known, given } => {
                assert_eq!(given, "frobnicates");
                assert!(
                    known.contains(&"implements"),
                    "the error should list real options"
                );
            }
            other => panic!("expected UnknownPredicate, got {other:?}"),
        }
        // The message a user actually sees must name both the bad input and the way out.
        let msg = e.to_string();
        assert!(
            msg.contains("frobnicates"),
            "message should name the bad predicate: {msg}"
        );
        assert!(
            msg.contains("implements"),
            "message should list known predicates: {msg}"
        );
    }

    #[test]
    fn spec_parsing_splits_on_the_first_colon_only() {
        assert_eq!(
            parse_spec("implements:VY-1234"),
            Some(("implements", "VY-1234"))
        );
        assert_eq!(parse_spec(" validates : H-5 "), Some(("validates", "H-5")));
        // An ID containing a colon survives.
        assert_eq!(parse_spec("related:NS:ID-7"), Some(("related", "NS:ID-7")));
        assert_eq!(parse_spec("noseparator"), None);
        assert_eq!(parse_spec("implements:"), None);
        assert_eq!(parse_spec(":VY-1"), None);
    }

    /// Only related/dependsOn project into the flat columns — that projection is what
    /// keeps roadmap/critical-path/worklist working unchanged.
    #[test]
    fn only_the_two_legacy_predicates_project_to_flat_columns() {
        assert_eq!(flat_column_for("related"), Some("related"));
        assert_eq!(flat_column_for("dependsOn"), Some("depends_on"));
        assert_eq!(flat_column_for("depends_on"), Some("depends_on"));
        assert_eq!(flat_column_for("implements"), None);
        assert_eq!(flat_column_for("validates"), None);
    }

    #[test]
    fn ids_resolve_to_types_by_prefix() {
        assert_eq!(type_from_id("EX-6109"), Some("expedition"));
        assert_eq!(type_from_id("VY-5855"), Some("voyage"));
        assert_eq!(type_from_id("CH-1"), Some("chore"));
        assert_eq!(type_from_id("H-5924"), Some("hypothesis"));
        assert_eq!(type_from_id("EXPR-6073"), Some("experiment"));
        assert_eq!(type_from_id("M-6072"), Some("measure"));
        assert_eq!(type_from_id("PAPER-122"), Some("paper"));
        assert_eq!(type_from_id("WAT-1"), None);
    }

    /// Inverses are declared but only ONE direction is ever stored — storing both would
    /// let them disagree.
    #[test]
    fn inverses_are_declared_and_mutually_consistent() {
        let d = lookup("dependsOn").unwrap();
        let b = lookup("blocks").unwrap();
        assert_eq!(d.inverse, Some("blocks"));
        assert_eq!(b.inverse, Some("dependsOn"));
        let i = lookup("implements").unwrap();
        let s = lookup("spawns").unwrap();
        assert_eq!(i.inverse, Some("spawns"));
        assert_eq!(s.inverse, Some("implements"));
        // Every declared inverse must itself be a real predicate pointing back.
        for p in PREDICATES {
            if let Some(inv) = p.inverse {
                let q = lookup(inv)
                    .unwrap_or_else(|| panic!("{} names a missing inverse {inv}", p.name));
                assert_eq!(q.inverse, Some(p.name), "{} and {inv} disagree", p.name);
            }
        }
    }
}
