//! Phase 1 — battery loader and CQ-to-query translator.
//!
//! Accepts two input formats:
//!
//! 1. **TutorRecord (EX-4332 v1.0)** — the canonical Customer's Plate JSON.
//! 2. **`cq_battery.jsonl` (CH-4318 legacy)** — one JSON object per line, the
//!    CH-4318 shape Mini's review of CMT-318 confirmed reusable.
//!
//! Both formats are translated into a uniform `Vec<CqSpec>` so the rest of
//! the evaluator does not branch on input shape.

use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use nusy_tutor_record::{CqKind, OpenCq, TutorRecord};

/// What the curriculum expects the being's response to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Expect {
    /// The being should answer substantively.
    Answer,
    /// The being should signal uncertainty (gold standard marks the question open).
    Uncertainty,
    /// The being should refuse — e.g. multimodal questions before EX-η lands.
    Refuse,
}

/// One ideal `(subject, predicate, object)` triple from a TutorRecord
/// CQ's `expected_resolution` field. Used by CH-4442 strict-match
/// grading to verify the cortex extracted the intended fact, not just
/// some triple containing the right keywords.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExpectedTriple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
}

/// One CQ in the loaded battery, normalised across input formats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CqSpec {
    pub id: String,
    pub question: String,
    /// Coarse dimension label — `word_meaning`, `causal_chain`, `pattern`,
    /// `cross_stanza`, `multimodal`, `metacognitive`. Drives per-dimension
    /// roll-ups in the report.
    pub dimension: String,
    pub expect: Expect,
    /// Keywords whose presence signals a substantive (non-refusal) answer.
    /// For pattern-recognition CQs this is the closed `expected_set`. For
    /// causal-chain / metacognitive CQs derived from a TutorRecord, the
    /// translator extracts the entities mentioned in the resolution chain
    /// or the resolution triples' objects.
    ///
    /// Used by the **loose** keyword-overlap grader. Backward-compatible
    /// and the only field the legacy `cq_battery.jsonl` format populates.
    pub expected_keywords: Vec<String>,
    /// CH-4442: the original `(subject, predicate, object)` triples from a
    /// TutorRecord CQ's `expected_resolution` field. Empty for CQs loaded
    /// from `cq_battery.jsonl` (legacy format) — the strict grader treats
    /// those as keyword-only and skips strict scoring.
    ///
    /// Used by the **strict** triple-match grader (`grade_strict`) which
    /// checks that the cortex emitted a triple matching the
    /// `(subject, predicate, object)` shape, not just one containing the
    /// keywords as substrings.
    #[serde(default)]
    pub expected_resolution: Vec<ExpectedTriple>,
    pub domain: String,
    /// EX-4419 Phase 2 — pointer into the requirements layer. `None`
    /// when the CQ was loaded from a pre-Phase-5 artifact (legacy
    /// `cq_battery.jsonl`, tier 1-3 plates that haven't been
    /// re-authored). New (tier 4 / KBDD-decomposed) CQs always carry
    /// one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirement_id: Option<String>,
    /// EX-4419 Phase 2 — pointer into the KBDD scenarios layer
    /// (Tier 4 / EX-4417). `None` for non-KBDD-decomposed CQs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    /// EX-4419 Phase 2 — `TutorSeal::content_hash` for this CQ
    /// (hex-encoded blake3). Stamped at plate-authoring time;
    /// equivalent CQs across plates share a hash, refined CQs differ.
    /// `None` for legacy CQs that haven't been sealed yet — gradual
    /// upgrade rather than a flag day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tutor_seal_hash: Option<String>,
}

/// A loaded battery — ordered list of CQ specs ready for grading.
#[derive(Debug, Clone)]
pub struct Battery {
    pub source_label: String,
    pub cqs: Vec<CqSpec>,
}

#[derive(Debug, Error)]
pub enum BatteryError {
    #[error("battery file not found: {0}")]
    NotFound(std::path::PathBuf),
    #[error("IO error reading battery: {0}")]
    Io(#[from] std::io::Error),
    #[error("battery line {line} is not valid JSON: {source}")]
    BadJsonLine {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("could not parse as TutorRecord or cq_battery.jsonl: {0}")]
    UnknownFormat(String),
    #[error("battery has no CQs")]
    Empty,
}

impl Battery {
    /// Load from a path. Auto-detects between TutorRecord JSON and
    /// `cq_battery.jsonl` (line-delimited JSON). Detection: the first
    /// non-whitespace character is `{` and the file as a whole parses as a
    /// `TutorRecord` → tutor-record path; otherwise fall back to JSONL.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, BatteryError> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(BatteryError::NotFound(path.to_path_buf()));
        }
        let text = std::fs::read_to_string(path)?;
        let label = path.display().to_string();
        Self::from_text(&text, label)
    }

    /// Parse a battery from a string. Same auto-detection as [`Battery::load`].
    pub fn from_text(text: &str, source_label: String) -> Result<Self, BatteryError> {
        // Try TutorRecord first — its `deny_unknown_fields` makes it a strict probe.
        if let Ok(record) = serde_json::from_str::<TutorRecord>(text) {
            return Ok(Self::from_tutor_record(&record, source_label));
        }
        // Fall back to CH-4318 JSONL.
        Self::from_cq_battery_jsonl(text, source_label)
    }

    /// Build a battery from a `TutorRecord` Layer-3 curiosity section.
    pub fn from_tutor_record(record: &TutorRecord, source_label: String) -> Self {
        let domain = record
            .document
            .audience
            .clone()
            .unwrap_or_else(|| "general".to_string());
        let cqs = record
            .layers
            .layer_3_curiosity
            .cqs
            .iter()
            .enumerate()
            .map(|(idx, cq)| translate_open_cq(cq, idx, &domain))
            .collect();
        Battery { source_label, cqs }
    }

    /// Parse the CH-4318 legacy JSONL format (one CQ per line).
    pub fn from_cq_battery_jsonl(text: &str, source_label: String) -> Result<Self, BatteryError> {
        let mut cqs = Vec::new();
        for (i, raw_line) in text.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }
            let entry: LegacyCq =
                serde_json::from_str(line).map_err(|source| BatteryError::BadJsonLine {
                    line: i + 1,
                    source,
                })?;
            cqs.push(entry.into_spec());
        }
        if cqs.is_empty() {
            return Err(BatteryError::Empty);
        }
        Ok(Battery { source_label, cqs })
    }
}

/// Build a being-query string from a CQ spec. The current rule is the
/// identity (the question text *is* the query). EX-α may extend this to
/// inject curriculum hints; until then keep the translation explicit so the
/// extension point is named.
pub fn cq_to_query(cq: &CqSpec) -> String {
    cq.question.clone()
}

// ── Internal: TutorRecord OpenCq translator ─────────────────────────────────

fn translate_open_cq(cq: &OpenCq, idx: usize, domain: &str) -> CqSpec {
    let id = cq
        .id
        .clone()
        .unwrap_or_else(|| format!("cq_{:03}", idx + 1));
    let dimension = match cq.kind {
        CqKind::DirectWordMeaning => "word_meaning",
        CqKind::CausalChain => "causal_chain",
        CqKind::PatternRecognition => "pattern",
        CqKind::CrossStanzaRelational => "cross_stanza",
        CqKind::Multimodal => "multimodal",
        CqKind::Metacognitive => "metacognitive",
    }
    .to_string();
    let expect = if cq.open == Some(true) || cq.kind == CqKind::Multimodal {
        Expect::Refuse
    } else {
        Expect::Answer
    };
    // CH-4442: preserve the original `(s, p, o)` shape of the plate's
    // expected_resolution so the strict grader can match against the
    // cortex's emitted triples rather than substring-matching keywords.
    let expected_resolution = cq
        .expected_resolution
        .as_ref()
        .map(|triples| {
            triples
                .iter()
                .map(|t| ExpectedTriple {
                    subject: t.subject.clone(),
                    predicate: t.predicate.clone(),
                    object: t.object.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    CqSpec {
        id,
        question: cq.cq.clone(),
        dimension,
        expect,
        expected_keywords: derive_keywords(cq),
        expected_resolution,
        domain: domain.to_string(),
        requirement_id: None,
        scenario_id: None,
        tutor_seal_hash: None,
    }
}

/// Derive expected keywords from a TutorRecord OpenCq's resolution structure.
///
/// Strategy by `kind`:
/// - **PatternRecognition** → `expected_set` is the closed enumeration; tokenise.
/// - **DirectWordMeaning / CausalChain / CrossStanzaRelational / Metacognitive**
///   → extract content tokens from `expected_resolution` (objects of triples)
///   and `expected_resolution_chain` (sentence content words).
/// - **Multimodal** → resolution is deferred to Layer 5; no keywords expected
///   (the CQ is graded on refuse-discipline, not substantive content).
fn derive_keywords(cq: &OpenCq) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    if let Some(set) = &cq.expected_set {
        for token in set {
            for word in tokenize_content(token) {
                if !out.contains(&word) {
                    out.push(word);
                }
            }
        }
    }
    if let Some(triples) = &cq.expected_resolution {
        for triple in triples {
            for word in tokenize_content(&triple.object) {
                if !out.contains(&word) {
                    out.push(word);
                }
            }
        }
    }
    if let Some(chain) = &cq.expected_resolution_chain {
        for step in chain {
            for word in tokenize_content(step) {
                if !out.contains(&word) {
                    out.push(word);
                }
            }
        }
    }
    out
}

/// Lower-case content words ≥3 chars from a phrase, dropping articles and
/// connectives. Keeps single-letter alphabet markers (`F`, `T`, `J`) so
/// pattern-recognition CQs match the way CH-4318 expected.
fn tokenize_content(phrase: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "but", "for", "a", "an", "of", "to", "in", "is", "are", "was", "were", "be",
        "by", "on", "with", "as", "at", "or", "if", "it", "this", "that", "these", "those",
    ];
    let mut out = Vec::new();
    for raw in phrase.split(|c: char| !c.is_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }
        let lower = raw.to_lowercase();
        // Keep single-character alphabet markers (F, J, T from CH-4318 CQ-015).
        if lower.len() == 1 && raw.chars().all(|c| c.is_ascii_uppercase()) {
            out.push(raw.to_string());
            continue;
        }
        if lower.len() < 3 {
            continue;
        }
        if STOP.contains(&lower.as_str()) {
            continue;
        }
        out.push(lower);
    }
    out
}

// ── Internal: CH-4318 cq_battery.jsonl row ──────────────────────────────────

#[derive(Debug, Deserialize)]
struct LegacyCq {
    id: String,
    question: String,
    dimension: String,
    expect: String,
    #[serde(default)]
    expected_keywords: Vec<String>,
    #[serde(default = "default_domain")]
    domain: String,
}

fn default_domain() -> String {
    "general".to_string()
}

impl LegacyCq {
    fn into_spec(self) -> CqSpec {
        let expect = match self.expect.to_ascii_lowercase().as_str() {
            "uncertainty" => Expect::Uncertainty,
            "refuse" => Expect::Refuse,
            _ => Expect::Answer,
        };
        CqSpec {
            id: self.id,
            question: self.question,
            dimension: self.dimension,
            expect,
            expected_keywords: self.expected_keywords,
            // CH-4442: legacy JSONL format never carried (s, p, o) triples,
            // only flat keywords. Strict grader will skip these CQs.
            expected_resolution: Vec::new(),
            domain: self.domain,
            requirement_id: None,
            scenario_id: None,
            tutor_seal_hash: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jsonl_fixture() -> &'static str {
        r#"
{"id":"CQ-001","question":"What is an Archer?","dimension":"word_meaning","expect":"answer","expected_keywords":["bow","arrow"],"domain":"general_education"}
{"id":"CQ-007","question":"Why does X mean ten?","dimension":"word_meaning","expect":"uncertainty","expected_keywords":["roman"],"domain":"general_education"}
{"id":"CQ-022","question":"What does the Archer look like?","dimension":"multimodal","expect":"refuse","expected_keywords":[],"domain":"general_education"}
"#
    }

    #[test]
    fn loads_ch4318_jsonl() {
        let battery = Battery::from_text(jsonl_fixture(), "test".to_string()).unwrap();
        assert_eq!(battery.cqs.len(), 3);
        assert_eq!(battery.cqs[0].id, "CQ-001");
        assert_eq!(battery.cqs[0].expect, Expect::Answer);
        assert_eq!(battery.cqs[1].expect, Expect::Uncertainty);
        assert_eq!(battery.cqs[2].expect, Expect::Refuse);
    }

    #[test]
    fn empty_jsonl_returns_empty_error() {
        let result = Battery::from_text("\n   \n", "test".to_string());
        assert!(matches!(result, Err(BatteryError::Empty)));
    }

    #[test]
    fn malformed_jsonl_line_reports_position() {
        let bad = r#"{"id":"CQ-001","question":"x","dimension":"d","expect":"answer"}
{not even json}"#;
        match Battery::from_text(bad, "test".to_string()) {
            Err(BatteryError::BadJsonLine { line, .. }) => assert_eq!(line, 2),
            other => panic!("expected BadJsonLine{{line:2}}, got {other:?}"),
        }
    }

    #[test]
    fn cq_to_query_uses_question_text() {
        let cq = CqSpec {
            id: "CQ-001".to_string(),
            question: "What is an Archer?".to_string(),
            dimension: "word_meaning".to_string(),
            expect: Expect::Answer,
            expected_keywords: vec![],
            expected_resolution: Vec::new(),
            domain: "general".to_string(),
            requirement_id: None,
            scenario_id: None,
            tutor_seal_hash: None,
        };
        assert_eq!(cq_to_query(&cq), "What is an Archer?");
    }

    #[test]
    fn tokenize_content_drops_stop_words_and_short_tokens() {
        let words = tokenize_content("The Queen holds a Rose in her hand");
        assert!(words.contains(&"queen".to_string()));
        assert!(words.contains(&"holds".to_string()));
        assert!(words.contains(&"rose".to_string()));
        assert!(words.contains(&"hand".to_string()));
        assert!(!words.contains(&"the".to_string()));
        assert!(!words.contains(&"a".to_string()));
        assert!(!words.contains(&"in".to_string()));
    }

    #[test]
    fn tokenize_content_keeps_single_uppercase_letter_markers() {
        let words = tokenize_content("F: Flag, J: Jane, T: Throne");
        assert!(words.contains(&"F".to_string()));
        assert!(words.contains(&"J".to_string()));
        assert!(words.contains(&"T".to_string()));
        assert!(words.contains(&"flag".to_string()));
    }

    #[test]
    fn translates_tutor_record() {
        // Use the canonical example file from nusy-tutor-record.
        let json = include_str!("../../nusy-tutor-record/examples/dame_wonder.json");
        let record: TutorRecord = serde_json::from_str(json).unwrap();
        let battery = Battery::from_tutor_record(&record, "dame_wonder".to_string());

        // The Dame Wonder example has 6 CQs (per nusy-tutor-record's
        // round_trip test).
        assert_eq!(battery.cqs.len(), 6);

        // CQ-001 — DirectWordMeaning, expect=answer, keywords from
        // expected_resolution objects.
        let cq1 = &battery.cqs[0];
        assert_eq!(cq1.id, "cq_001");
        assert_eq!(cq1.dimension, "word_meaning");
        assert_eq!(cq1.expect, Expect::Answer);
        // Object of (Archer, rdf:type, Person) → "person"
        assert!(cq1.expected_keywords.iter().any(|k| k == "person"));

        // CQ-003 — open=true → expect Refuse.
        let cq3 = battery.cqs.iter().find(|c| c.id == "cq_003").unwrap();
        assert_eq!(cq3.expect, Expect::Refuse);

        // CQ-006 — Multimodal → expect Refuse regardless of open=true.
        let cq6 = battery.cqs.iter().find(|c| c.id == "cq_006").unwrap();
        assert_eq!(cq6.dimension, "multimodal");
        assert_eq!(cq6.expect, Expect::Refuse);

        // CQ-004 — pattern_recognition → keywords from expected_set.
        let cq4 = battery.cqs.iter().find(|c| c.id == "cq_004").unwrap();
        assert_eq!(cq4.dimension, "pattern");
        assert!(cq4.expected_keywords.iter().any(|k| k == "crow"));
        assert!(cq4.expected_keywords.iter().any(|k| k == "C"));
    }
}
