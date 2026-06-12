//! CH-4442 — Strict triple-match grader.
//!
//! The default grader (`Grader::grade`) credits a CQ when the responder's
//! response string contains any of the CQ's `expected_keywords` as a
//! substring. That's a loose check — `(stanza_5, mentions, ocean_lobster)`
//! and `(Lobster, lives_in, Ocean)` both score the same against the
//! keyword "ocean," but only the second actually answers "Where do
//! Lobsters live?". Captain feedback (2026-05-08): "we need to know if
//! we extracted the actual fact, not just if a keyword leaked through
//! somewhere in the store."
//!
//! This module provides the **strict** alternative: for each CQ with
//! a non-empty `expected_resolution`, scan the dual store for triples
//! whose `(subject, predicate, object)` shape matches the expected
//! triple. A CQ passes strict iff at least 50% of its
//! `expected_resolution` entries have a match in the store.
//!
//! Match rules:
//! * **subject:** case-insensitive substring (store-subject contains
//!   expected-subject OR expected-subject contains store-subject).
//! * **predicate:** bidirectional case-insensitive substring. This is
//!   intentionally lenient — the V13 predicate vocabulary uses snake-
//!   case (`lives_in`, `pictured_with`); cortex output may use varied
//!   forms (`Lives in`, `lives_in`, `livesIn`). Stricter `rdfs:subPropertyOf`
//!   descent is left for a follow-up chore.
//! * **object:** same as subject — case-insensitive bidirectional substring.
//!
//! Designed to coexist with the loose grader, not replace it. Run both;
//! the gap between loose and strict tells us how much keyword-leak is
//! in the loose number.

use serde::{Deserialize, Serialize};

use crate::battery::{CqSpec, ExpectedTriple};

/// Per-CQ strict-grade result. Mirrors `GradeReport` for the loose
/// grader but reports per-expected-triple match status so debug walks
/// can see which `expected_resolution` triples were extracted and
/// which weren't.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StrictGradeReport {
    pub cq_id: String,
    /// `expected_resolution` count.
    pub expected_total: usize,
    /// How many of the expected triples have a matching triple in the store.
    pub matched_count: usize,
    /// Per-expected-triple result: which expected entries matched.
    /// Same length as the CQ's `expected_resolution`.
    pub matches: Vec<TripleMatch>,
    /// `true` iff `matched_count * 2 >= expected_total` (≥ 50%) AND
    /// `expected_total > 0`. Empty-resolution CQs always fail strict
    /// (they have no triples to match against).
    pub pass: bool,
    /// `true` if this CQ's `expected_resolution` is empty — strict
    /// grading isn't applicable. Distinguishes "no expected triples"
    /// from "expected triples but none matched."
    pub not_applicable: bool,
}

/// Whether a single expected triple was matched against the store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TripleMatch {
    pub expected: ExpectedTriple,
    pub matched: bool,
    /// If matched, which store triple satisfied it. `None` otherwise.
    pub matched_triple: Option<(String, String, String)>,
}

/// Strict-mode grading threshold: fraction of `expected_resolution`
/// triples that must match in the store for the CQ to pass strict.
/// Set to 0.5 — half-credit on partial extraction is the right shape
/// for "completeness" grading without demanding perfection.
pub const STRICT_PASS_THRESHOLD: f64 = 0.5;

/// Grade a single CQ in strict mode against the supplied store triples.
///
/// `store_triples` is a slice of `(subject, predicate, object)` tuples
/// — the caller is responsible for loading them from the dual store.
/// (Use `nusy_being::fast_chat::load_triples` or equivalent.)
pub fn grade_strict_cq(
    cq: &CqSpec,
    store_triples: &[(String, String, String)],
) -> StrictGradeReport {
    let expected_total = cq.expected_resolution.len();

    if expected_total == 0 {
        return StrictGradeReport {
            cq_id: cq.id.clone(),
            expected_total: 0,
            matched_count: 0,
            matches: Vec::new(),
            pass: false,
            not_applicable: true,
        };
    }

    let mut matches = Vec::with_capacity(expected_total);
    let mut matched_count = 0;

    for expected in &cq.expected_resolution {
        let s_exp = expected.subject.to_lowercase();
        let p_exp = expected.predicate.to_lowercase();
        let o_exp = expected.object.to_lowercase();

        let mut found: Option<(String, String, String)> = None;
        for triple in store_triples {
            let s_store = triple.0.to_lowercase();
            let p_store = triple.1.to_lowercase();
            let o_store = triple.2.to_lowercase();

            // Bidirectional substring on subject — store may carry
            // qualifiers ("Lobster_3" contains "Lobster") or the
            // expected may be a substring of a longer canonical name.
            let s_match = s_store.contains(&s_exp) || s_exp.contains(&s_store);
            // Same for predicate (V13 used snake_case; cortex may use
            // camelCase or spaces).
            let p_match = p_store.contains(&p_exp) || p_exp.contains(&p_store);
            // And object.
            let o_match = o_store.contains(&o_exp) || o_exp.contains(&o_store);

            if s_match && p_match && o_match {
                found = Some(triple.clone());
                break;
            }
        }

        if found.is_some() {
            matched_count += 1;
        }
        matches.push(TripleMatch {
            expected: expected.clone(),
            matched: found.is_some(),
            matched_triple: found,
        });
    }

    let frac = matched_count as f64 / expected_total as f64;
    let pass = frac >= STRICT_PASS_THRESHOLD;

    StrictGradeReport {
        cq_id: cq.id.clone(),
        expected_total,
        matched_count,
        matches,
        pass,
        not_applicable: false,
    }
}

/// Aggregate strict-grade result for an entire battery.
///
/// `eligible` = CQs with non-empty `expected_resolution` (strict applicable).
/// `pass_rate` = passes / eligible. Mirrors `substantive_pass_rate` from
/// the loose grader's `ScenariosPassResult` but normalized to the strict-
/// applicable subset (which is typically tighter than `substantive_eligible`
/// because legacy JSONL plates have no `expected_resolution`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrictBatteryReport {
    pub passes: usize,
    pub eligible: usize,
    pub pass_rate: f64,
    pub per_cq: Vec<StrictGradeReport>,
}

/// Grade an entire battery in strict mode.
pub fn grade_strict_battery(
    cqs: &[CqSpec],
    store_triples: &[(String, String, String)],
) -> StrictBatteryReport {
    let mut per_cq = Vec::with_capacity(cqs.len());
    let mut passes = 0;
    let mut eligible = 0;
    for cq in cqs {
        let report = grade_strict_cq(cq, store_triples);
        if !report.not_applicable {
            eligible += 1;
            if report.pass {
                passes += 1;
            }
        }
        per_cq.push(report);
    }
    let pass_rate = if eligible == 0 {
        0.0
    } else {
        passes as f64 / eligible as f64
    };
    StrictBatteryReport {
        passes,
        eligible,
        pass_rate,
        per_cq,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battery::Expect;

    fn cq_with_resolution(id: &str, expected: Vec<ExpectedTriple>) -> CqSpec {
        CqSpec {
            id: id.to_string(),
            question: "test?".to_string(),
            dimension: "test".to_string(),
            expect: Expect::Answer,
            expected_keywords: Vec::new(),
            expected_resolution: expected,
            domain: "test".to_string(),
            requirement_id: None,
            scenario_id: None,
            tutor_seal_hash: None,
        }
    }

    fn t(s: &str, p: &str, o: &str) -> ExpectedTriple {
        ExpectedTriple {
            subject: s.to_string(),
            predicate: p.to_string(),
            object: o.to_string(),
        }
    }

    fn store_t(s: &str, p: &str, o: &str) -> (String, String, String) {
        (s.to_string(), p.to_string(), o.to_string())
    }

    #[test]
    fn exact_match_passes_strict() {
        let cq = cq_with_resolution("cq_1", vec![t("Lobster", "lives_in", "Ocean")]);
        let store = vec![store_t("Lobster", "lives_in", "Ocean")];
        let r = grade_strict_cq(&cq, &store);
        assert!(r.pass, "exact match should pass strict, got: {r:?}");
        assert_eq!(r.matched_count, 1);
        assert_eq!(r.expected_total, 1);
        assert!(!r.not_applicable);
    }

    #[test]
    fn keyword_only_overlap_fails_strict() {
        // Reproduces the "keyword leak" failure mode the loose grader
        // would credit. Triple mentions "Lobster" but the predicate +
        // object don't match the expected `lives_in / Ocean`.
        let cq = cq_with_resolution("cq_2", vec![t("Lobster", "lives_in", "Ocean")]);
        let store = vec![
            // Loose grader would credit this on keyword "ocean"; strict
            // sees no predicate match, no object match.
            store_t("stanza_5", "mentions", "Ocean_Lobster"),
            // And this on keyword "lobster".
            store_t("Lobster", "rdf:type", "Animal"),
        ];
        let r = grade_strict_cq(&cq, &store);
        assert!(
            !r.pass,
            "keyword-only overlap must NOT pass strict, got: {r:?}"
        );
        assert_eq!(r.matched_count, 0);
    }

    #[test]
    fn empty_resolution_is_not_applicable() {
        let cq = cq_with_resolution("cq_3", Vec::new());
        let r = grade_strict_cq(&cq, &[]);
        assert!(r.not_applicable);
        assert!(!r.pass);
        assert_eq!(r.expected_total, 0);
    }

    #[test]
    fn partial_match_above_threshold_passes() {
        // CQ has 2 expected; store satisfies 1 → 50% → passes (threshold = 0.5).
        let cq = cq_with_resolution(
            "cq_4",
            vec![t("Archer", "uses", "Bow"), t("Bow", "shoots", "Arrow")],
        );
        let store = vec![store_t("Archer", "uses", "Bow")];
        let r = grade_strict_cq(&cq, &store);
        assert!(r.pass);
        assert_eq!(r.matched_count, 1);
        assert_eq!(r.expected_total, 2);
    }

    #[test]
    fn partial_match_below_threshold_fails() {
        // CQ has 3 expected; store satisfies 1 → 33% → fails (< 0.5).
        let cq = cq_with_resolution(
            "cq_5",
            vec![t("A", "p1", "X"), t("B", "p2", "Y"), t("C", "p3", "Z")],
        );
        let store = vec![store_t("A", "p1", "X")];
        let r = grade_strict_cq(&cq, &store);
        assert!(!r.pass);
        assert_eq!(r.matched_count, 1);
        assert_eq!(r.expected_total, 3);
    }

    #[test]
    fn case_insensitive_match() {
        let cq = cq_with_resolution("cq_6", vec![t("LOBSTER", "Lives_In", "OCEAN")]);
        let store = vec![store_t("lobster", "lives_in", "ocean")];
        let r = grade_strict_cq(&cq, &store);
        assert!(r.pass);
    }

    #[test]
    fn substring_subject_match() {
        // Store has a longer canonical subject; expected is a substring.
        let cq = cq_with_resolution("cq_7", vec![t("Lobster", "lives_in", "Ocean")]);
        let store = vec![store_t("Lobster_canonical_3", "lives_in", "Atlantic_Ocean")];
        let r = grade_strict_cq(&cq, &store);
        assert!(r.pass);
    }

    #[test]
    fn battery_aggregates_correctly() {
        let cqs = vec![
            cq_with_resolution("cq_a", vec![t("X", "p", "Y")]), // pass
            cq_with_resolution("cq_b", vec![t("M", "q", "N")]), // fail (not in store)
            cq_with_resolution("cq_c", Vec::new()),             // not applicable
        ];
        let store = vec![store_t("X", "p", "Y")];
        let r = grade_strict_battery(&cqs, &store);
        assert_eq!(r.passes, 1);
        assert_eq!(r.eligible, 2); // cq_c is N/A and excluded
        assert!((r.pass_rate - 0.5).abs() < 1e-9);
        assert_eq!(r.per_cq.len(), 3);
    }
}
