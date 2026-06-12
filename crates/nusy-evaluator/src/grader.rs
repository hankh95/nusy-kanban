//! Phase 2 — three-tier grader.
//!
//! Five grades — `Pass / Fail / Refuse / PersonaLeak / Error`. The grader is
//! the **A7** carry-forward from S2 (CH-4318): the bash
//! `run_battery_local.sh` rule of `>20 chars + no refusal regex` scored
//! 18.5% on a being that substantively answered nothing. EX-γ's grader
//! requires:
//!
//! 1. Response is **not a refusal** (or matches the expected refusal class
//!    when `expect=uncertainty / refuse`).
//! 2. Response **contains an expected keyword** (with naive plural / `s|es`
//!    derivation tolerance, single-uppercase alphabet markers like `F` only
//!    match in alphabet context).
//! 3. Response cites a graph triple (the **graph-trace** condition;
//!    EX-α's per-triple chunk-id tagging makes this richer when it lands).
//!
//! The refusal regex is intentionally tighter than the CH-4318 Python
//! version: anchored alternations only, no `not.*area` / `outside.*domain`
//! patterns whose `.*` matches arbitrary substrings.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use thiserror::Error;

use crate::battery::{CqSpec, Expect};

/// One of the five evaluator grades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grade {
    /// Substantive answer that satisfies the CQ's expectations.
    Pass,
    /// Non-refusal, non-leak response that does not satisfy the CQ.
    Fail,
    /// Response is a refusal — counts as `pass` for `expect=uncertainty/refuse`,
    /// `fail` for `expect=answer`. Reported as `Refuse` in the per-CQ result so
    /// the score summary can distinguish refusals from substantive failures.
    Refuse,
    /// Response leaked a persona-prompt prefix instead of answering.
    /// Distinct from `Refuse` so it surfaces in summaries (A10 carry-forward).
    PersonaLeak,
    /// Pipeline error — timeout, command failure, malformed response.
    Error,
}

impl Grade {
    pub fn label(self) -> &'static str {
        match self {
            Grade::Pass => "pass",
            Grade::Fail => "fail",
            Grade::Refuse => "refuse",
            Grade::PersonaLeak => "persona_leak",
            Grade::Error => "error",
        }
    }
}

/// Configurable knobs on the grader. Defaults match the CH-4318
/// substantive-overlay scorer plus the A7 graph-trace requirement.
#[derive(Debug, Clone)]
pub struct GraderConfig {
    /// When true, `Pass` for `expect=answer` requires at least one
    /// supporting triple. When false (e.g. running against a V15 path
    /// that doesn't surface triples), the rule degrades to
    /// non-refusal + keyword. Default: `true`.
    pub require_graph_trace: bool,
}

impl Default for GraderConfig {
    fn default() -> Self {
        GraderConfig {
            require_graph_trace: true,
        }
    }
}

#[derive(Debug, Error)]
pub enum GraderError {
    #[error("grader regex compile failed: {0}")]
    Regex(#[from] regex::Error),
}

/// One grading result for a single CQ + Being response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeReport {
    pub cq_id: String,
    pub grade: Grade,
    /// Keywords that matched the response (informational; subset of the CQ's
    /// `expected_keywords`).
    pub matched_keywords: Vec<String>,
    /// Whether the response cited at least one supporting triple. This is
    /// the A7 graph-trace condition.
    pub graph_trace_present: bool,
    /// True if the response matched the refusal regex (regardless of grade).
    pub refusal_signal: bool,
    /// True if the response matched a known persona-leak prefix.
    pub persona_leak_signal: bool,
}

/// The grader. Holds compiled regexes; create once per battery run.
#[derive(Debug)]
pub struct Grader {
    config: GraderConfig,
}

impl Default for Grader {
    fn default() -> Self {
        Grader::new(GraderConfig::default())
    }
}

impl Grader {
    pub fn new(config: GraderConfig) -> Self {
        Grader { config }
    }

    /// Grade one (CQ, response, supporting-triple-count) tuple.
    pub fn grade(&self, cq: &CqSpec, response: &str, supporting_triples: usize) -> GradeReport {
        let response_trim = response.trim();

        // Empty response = error.
        if response_trim.is_empty() {
            return GradeReport {
                cq_id: cq.id.clone(),
                grade: Grade::Error,
                matched_keywords: vec![],
                graph_trace_present: false,
                refusal_signal: false,
                persona_leak_signal: false,
            };
        }

        let persona_leak_signal = is_persona_leak(response_trim);
        if persona_leak_signal {
            return GradeReport {
                cq_id: cq.id.clone(),
                grade: Grade::PersonaLeak,
                matched_keywords: vec![],
                graph_trace_present: supporting_triples > 0,
                refusal_signal: false,
                persona_leak_signal: true,
            };
        }

        let refusal_signal = is_refusal(response_trim);
        let matched_keywords = match_keywords(response_trim, &cq.expected_keywords);
        let graph_trace_present = supporting_triples > 0;

        let grade = match (cq.expect, refusal_signal) {
            // expect=refuse / uncertainty → refusal counts as pass.
            (Expect::Refuse, true) | (Expect::Uncertainty, true) => Grade::Refuse,
            // expect=refuse but the being answered substantively — that's a fail.
            (Expect::Refuse, false) | (Expect::Uncertainty, false) => Grade::Fail,
            // expect=answer + refusal → fail (we surface the refuse signal but
            // the curriculum wanted an answer).
            (Expect::Answer, true) => Grade::Refuse,
            // expect=answer + non-refusal → check substantive criteria.
            (Expect::Answer, false) => {
                let has_keyword = !matched_keywords.is_empty();
                let trace_ok = !self.config.require_graph_trace || graph_trace_present;
                if has_keyword && trace_ok {
                    Grade::Pass
                } else {
                    Grade::Fail
                }
            }
        };

        GradeReport {
            cq_id: cq.id.clone(),
            grade,
            matched_keywords,
            graph_trace_present,
            refusal_signal,
            persona_leak_signal: false,
        }
    }
}

// ── Regex catalogue ─────────────────────────────────────────────────────────

/// Refusal phrasing — anchored alternations, no greedy `.*` patterns.
/// Tightened from `scripts/ch-4318/score_substantive.py:50-62` per Mini's
/// CMT-318 review of CH-4318 (the Python regex's `not.*area` and
/// `outside.*domain` would over-match longer responses). Built from a
/// vec to keep each alternation readable.
fn refusal_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let alternations: &[&str] = &[
            // "I don't / do not / cannot / am not able to … know / answer / etc."
            r"\bi\s+(?:do(?:\s+not|\s*n['’]?t)?|am\s+not|cannot|can['’]?t|won['’]?t|will\s+not|am\s+unable\s+to)\s+(?:know|aware|sure|certain|confident|able\s+to(?:\s+answer)?|verify|confirm|provide|help|answer)\b",
            // "I … have (verified) information / knowledge"
            r"\bi\s+(?:do(?:\s+not|\s*n['’]?t)?|cannot|can['’]?t)\s+have\s+(?:verified\s+)?(?:information|knowledge|enough)\b",
            // "I do not aware …" (V15.4 broken-grammar form, observed in CH-4318)
            r"\bi\s+do\s+not\s+aware\s+of\b",
            // "don't have / don't know" without leading I
            r"\bdon['’]?t\s+(?:have|know)\s+(?:verified\s+)?(?:information|knowledge|enough)\b",
            // beyond / outside my X
            r"\b(?:beyond|outside)\s+(?:my\s+(?:training|scope|domain|knowledge)|(?:the\s+)?scope\s+of)\b",
            // not in / not able to answer / not sure about
            r"\bnot\s+(?:in\s+my\s+(?:training|scope|domain|knowledge)|able\s+to\s+answer|sure\s+about)\b",
            // explicit refusals
            r"\bi\s+refuse\b",
            r"\bnot\s+authorized\b",
            // "I'm not sure"
            r"\bi['’]?m\s+not\s+sure\b",
        ];
        let pattern = format!("(?i){}", alternations.join("|"));
        Regex::new(&pattern).expect("refusal regex compiles")
    })
}

/// V15.4 persona-prompt leak signature observed on CQ-019 in CH-4318:
/// the being returned the LoRA's persona prefix instead of answering.
/// Match the canonical opening; tolerant to UTF-8 punctuation variants.
fn persona_leak_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let alternations: &[&str] = &[
            // "I am Santiago Ramón / Miguel / …" — the V15.4 LoRA persona prefix.
            r"\bi\s+am\s+santiago\s+(?:ram[oó]n|miguel|santiago)\b",
            // "I'm a {level} being" persona claim
            r"\bi['’]?m\s+a\s+(?:toddler|grade[\s-]?school|highschool|undergraduate|graduate)\s+being\b",
            // Explicit persona declarations
            r"\bbeing\s+(?:identity|persona)[:\s]+santiago\b",
        ];
        let pattern = format!("(?i){}", alternations.join("|"));
        Regex::new(&pattern).expect("persona-leak regex compiles")
    })
}

fn is_refusal(response: &str) -> bool {
    refusal_re().is_match(response)
}

fn is_persona_leak(response: &str) -> bool {
    persona_leak_re().is_match(response)
}

/// Keyword matching with the same rules as CH-4318's
/// `score_substantive.keyword_hit` (and Mini's CMT-318 review):
/// - **Single-character alphabet markers** (`F`, `J`, `T`) match only inside
///   an alphabet-context window (within 30 chars of `letter` / `alphabet` /
///   `stand[s]? for` / `stanza`).
/// - **Multi-word keywords** match as substrings.
/// - **Other single-word keywords** match on word boundaries with naive
///   `(s|es)?` plural tolerance.
pub(crate) fn match_keywords(response: &str, keywords: &[String]) -> Vec<String> {
    let lower = response.to_lowercase();
    let mut hits: Vec<String> = Vec::new();
    for kw in keywords {
        let kw_l = kw.to_lowercase();
        if kw_l.is_empty() {
            continue;
        }
        if kw_l.chars().count() == 1 && kw.chars().all(|c| c.is_ascii_uppercase()) {
            // Alphabet-letter marker. Require alphabet context.
            if matches_in_alphabet_context(&lower, &kw_l) {
                hits.push(kw.clone());
            }
            continue;
        }
        if kw_l.contains(' ') {
            // Multi-word substring match.
            if lower.contains(&kw_l) {
                hits.push(kw.clone());
            }
            continue;
        }
        if matches_word_boundary_with_plural(&lower, &kw_l) {
            hits.push(kw.clone());
        }
    }
    hits
}

fn matches_word_boundary_with_plural(haystack: &str, kw_lower: &str) -> bool {
    let escaped = regex::escape(kw_lower);
    let pat = format!(r"\b{escaped}(?:s|es)?\b");
    Regex::new(&pat)
        .ok()
        .map(|re| re.is_match(haystack))
        .unwrap_or(false)
}

fn matches_in_alphabet_context(lower: &str, kw_lower: &str) -> bool {
    // Use the Unicode-friendly word-boundary pattern; `kw_lower` is one ASCII char.
    let escaped = regex::escape(kw_lower);
    let pat = format!(r"\b{escaped}\b");
    let Ok(re) = Regex::new(&pat) else {
        return false;
    };
    let context_re = Regex::new(r"letter|alphabet|stands?\s+for|stanza")
        .expect("alphabet-context regex compiles");
    for m in re.find_iter(lower) {
        let start = m.start().saturating_sub(30);
        let end = (m.end() + 30).min(lower.len());
        let window = &lower[start..end];
        if context_re.is_match(window) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battery::CqSpec;

    fn cq(id: &str, expect: Expect, keywords: &[&str]) -> CqSpec {
        CqSpec {
            id: id.to_string(),
            question: "test?".to_string(),
            dimension: "word_meaning".to_string(),
            expect,
            expected_keywords: keywords.iter().map(|s| s.to_string()).collect(),
            expected_resolution: Vec::new(),
            domain: "general".to_string(),
            requirement_id: None,
            scenario_id: None,
            tutor_seal_hash: None,
        }
    }

    fn grader_no_trace() -> Grader {
        Grader::new(GraderConfig {
            require_graph_trace: false,
        })
    }

    // ── Refusal regex ──────────────────────────────────────────────────────

    #[test]
    fn refusal_recognises_v15_canned_forms() {
        for r in [
            "I don't have verified information to answer this question.",
            "I do not aware of any information about Lobster.",
            "I don't know about the Sword of State.",
            "I am not able to answer.",
            "I cannot verify this fact.",
            "Beyond my training scope.",
        ] {
            assert!(is_refusal(r), "expected refusal: {r:?}");
        }
    }

    #[test]
    fn refusal_does_not_match_real_substantive_answers() {
        for r in [
            "An archer is a person who shoots arrows from a bow.",
            "Yes, all 26 letters are covered: A through Z.",
            "The queen holds a rose in her hand and sits on a throne.",
            // The CH-4318 Python regex matched "not.*area" greedily — V16
            // long-form responses must not falsely register.
            "Archers are not common in the area of modern warfare, but they appear in fantasy.",
            "Outside the realm of metallurgy, swords had ceremonial uses.",
        ] {
            assert!(!is_refusal(r), "false-positive refusal on: {r:?}");
        }
    }

    // ── Persona-leak regex (A10) ───────────────────────────────────────────

    #[test]
    fn persona_leak_detects_v154_prefix() {
        // CQ-019 in CH-4318 returned: "I am Santiago Ramón y Miguel de la rosa, a"
        let leaked = "I am Santiago Ramón y Miguel de la rosa, a toddler being";
        assert!(is_persona_leak(leaked));
    }

    #[test]
    fn persona_leak_distinct_from_refusal() {
        let leaked = "I am Santiago Ramón y Miguel de la rosa, a";
        // Persona-leak is the *first* check — must not accidentally also
        // register as a refusal (priority test).
        assert!(is_persona_leak(leaked));
    }

    #[test]
    fn persona_leak_does_not_match_normal_answers() {
        for r in [
            "Santiago is a city in Chile.",
            "An archer uses a bow.",
            "I am a toddler reading the alphabet.", // missing "being" qualifier
        ] {
            assert!(!is_persona_leak(r), "false-positive on: {r:?}");
        }
    }

    // ── Keyword matching ───────────────────────────────────────────────────

    #[test]
    fn keyword_match_with_plural_tolerance() {
        let hits = match_keywords(
            "An archer is a person who shoots arrows from a bow.",
            &["bow".into(), "arrow".into(), "shoot".into()],
        );
        assert!(hits.contains(&"bow".to_string()));
        assert!(hits.contains(&"arrow".to_string())); // plural tolerated
        assert!(hits.contains(&"shoot".to_string())); // plural tolerated (-s)
    }

    #[test]
    fn keyword_match_multi_word() {
        let hits = match_keywords(
            "The Sword of State is a ceremonial weapon.",
            &["sword of state".into(), "ceremonial".into()],
        );
        assert!(hits.contains(&"sword of state".to_string()));
        assert!(hits.contains(&"ceremonial".to_string()));
    }

    #[test]
    fn alphabet_marker_requires_context() {
        // "I do not aware..." contains pronoun "I" but no alphabet context.
        let response = "I do not aware of any information that would be relevant.";
        let hits = match_keywords(response, &["I".into()]);
        assert!(
            !hits.contains(&"I".to_string()),
            "pronoun I must not match alphabet marker keyword 'I'"
        );
    }

    #[test]
    fn alphabet_marker_matches_in_context() {
        let response = "The letter T stands for Throne and Table.";
        let hits = match_keywords(response, &["T".into()]);
        assert!(hits.contains(&"T".to_string()));
    }

    // ── Grade flow per (Expect, refusal_signal) ────────────────────────────

    #[test]
    fn pass_requires_keyword_and_non_refusal_and_graph_trace() {
        let g = Grader::default();
        let c = cq("CQ-001", Expect::Answer, &["archer", "bow"]);
        let r = g.grade(
            &c,
            "An archer uses a bow to shoot arrows.",
            3, /* triples */
        );
        assert_eq!(r.grade, Grade::Pass);
        assert!(r.graph_trace_present);
        assert!(r.matched_keywords.contains(&"archer".to_string()));
    }

    #[test]
    fn pass_fails_without_graph_trace_when_required() {
        let g = Grader::default();
        let c = cq("CQ-001", Expect::Answer, &["archer", "bow"]);
        let r = g.grade(&c, "An archer uses a bow.", 0 /* no triples */);
        assert_eq!(r.grade, Grade::Fail);
    }

    #[test]
    fn pass_without_graph_trace_when_disabled() {
        let g = grader_no_trace();
        let c = cq("CQ-001", Expect::Answer, &["archer"]);
        let r = g.grade(&c, "An archer uses a bow.", 0);
        assert_eq!(r.grade, Grade::Pass);
    }

    #[test]
    fn fail_when_keyword_missing() {
        let g = Grader::default();
        let c = cq("CQ-001", Expect::Answer, &["windmill"]);
        let r = g.grade(&c, "An archer uses a bow.", 3);
        assert_eq!(r.grade, Grade::Fail);
    }

    #[test]
    fn refusal_with_keyword_does_not_pass() {
        let g = Grader::default();
        let c = cq("CQ-008", Expect::Answer, &["sword"]);
        // Famous CH-4318 case: "I don't know about the Sword of State." has the
        // keyword 'sword' but is a refusal.
        let r = g.grade(&c, "I don't know about the Sword of State.", 0);
        assert_eq!(r.grade, Grade::Refuse);
    }

    #[test]
    fn refuse_expected_with_refusal_grades_refuse() {
        let g = Grader::default();
        let c = cq("CQ-022", Expect::Refuse, &[]);
        let r = g.grade(&c, "I don't have information about images.", 0);
        assert_eq!(r.grade, Grade::Refuse);
    }

    #[test]
    fn refuse_expected_with_substantive_answer_fails() {
        let g = Grader::default();
        let c = cq("CQ-022", Expect::Refuse, &[]);
        let r = g.grade(&c, "It looks like a man with a bow.", 0);
        assert_eq!(r.grade, Grade::Fail);
    }

    #[test]
    fn uncertainty_expected_with_refusal_grades_refuse() {
        let g = Grader::default();
        let c = cq("CQ-007", Expect::Uncertainty, &[]);
        let r = g.grade(&c, "I'm not sure about that.", 0);
        assert_eq!(r.grade, Grade::Refuse);
    }

    // ── PersonaLeak (A10) ─────────────────────────────────────────────────

    #[test]
    fn persona_leak_detected_as_distinct_grade() {
        let g = Grader::default();
        let c = cq("CQ-019", Expect::Answer, &["queen", "rose"]);
        // The exact CQ-019 leak from CH-4318.
        let r = g.grade(&c, "I am Santiago Ramón y Miguel de la rosa, a", 0);
        assert_eq!(r.grade, Grade::PersonaLeak);
        assert!(r.persona_leak_signal);
        assert!(!r.refusal_signal);
    }

    #[test]
    fn persona_leak_takes_priority_over_keyword_match() {
        let g = Grader::default();
        // Even with the 'rosa' substring matching keyword 'rose'-like, the
        // leak wins (so the leak is visible in score summaries).
        let c = cq("CQ-019", Expect::Answer, &["rose"]);
        let r = g.grade(&c, "I am Santiago Ramón y Miguel de la rosa, a", 0);
        assert_eq!(r.grade, Grade::PersonaLeak);
    }

    // ── Error grade ───────────────────────────────────────────────────────

    #[test]
    fn empty_response_grades_error() {
        let g = Grader::default();
        let c = cq("CQ-001", Expect::Answer, &["archer"]);
        let r = g.grade(&c, "", 0);
        assert_eq!(r.grade, Grade::Error);
    }

    #[test]
    fn whitespace_only_response_grades_error() {
        let g = Grader::default();
        let c = cq("CQ-001", Expect::Answer, &["archer"]);
        let r = g.grade(&c, "   \n  \t", 0);
        assert_eq!(r.grade, Grade::Error);
    }

    // ── V15.4 canonical refusals (CH-4318 lock-in) ─────────────────────────

    #[test]
    fn v154_canonical_refusals_all_score_zero_pass() {
        let g = Grader::default();
        let cases = [
            (
                "I do not aware of any information that would be relevant to the question.",
                vec!["bow", "arrow", "archer"],
            ),
            (
                "I don't have verified information to answer this question.",
                vec!["queen", "rose", "throne"],
            ),
            (
                "I do not aware of information about Lobstalker.",
                vec!["lobster", "sea", "claws"],
            ),
            (
                "I don't know about the Sword of State.",
                vec!["sword", "royal", "ceremonial"],
            ),
            ("I am not able to answer.", vec!["mary", "numbers", "wrote"]),
        ];
        for (response, keywords) in cases {
            let kws: Vec<String> = keywords.iter().map(|s| s.to_string()).collect();
            let c = CqSpec {
                id: "X".into(),
                question: "?".into(),
                dimension: "word_meaning".into(),
                expect: Expect::Answer,
                expected_keywords: kws,
                expected_resolution: Vec::new(),
                domain: "general".into(),
                requirement_id: None,
                scenario_id: None,
                tutor_seal_hash: None,
            };
            let r = g.grade(&c, response, 0);
            assert_ne!(
                r.grade,
                Grade::Pass,
                "V15.4 canned refusal incorrectly passed: {response:?}"
            );
        }
    }
}
