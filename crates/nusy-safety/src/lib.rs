//! Safety gates for NuSy — hallucination detection, justification, and adapter validation.
//!
//! EX-3129: Basic Zorblaxia test harness with 10 generic probes.
//! EX-3154: Full battery system with domain-specific probes, risk tiers,
//! fact-count classification, KBDD integration, and ExamRunner framework.
//! EX-3241: Justification engine — evidence trails with source triples.

pub mod battery;
pub mod domain_rules;
pub mod exam_runner;
pub mod hallucination_gate;
pub mod integrity;
pub mod justification;
pub mod query_intent;
pub mod zorblaxia;

pub use battery::{ProbeBattery, RiskTier};
pub use domain_rules::{DomainRule, DomainRuleStore, DomainVerdict, domain_rules_schema};
pub use exam_runner::{ActualOutcome, ExamQuestion, ExamResult, ExamRunner, ExpectedOutcome};
pub use hallucination_gate::{GateResult, HallucinationGate};
pub use justification::{
    ChunkRef, EvidenceTrail, JustificationBuilder, JustificationError, JustificationPolicy,
    JustificationVerdict, check_justification, justification_schema, trail_to_arrow,
};
pub use query_intent::QueryIntent;
pub use zorblaxia::{
    ProbeQuestion, ProbeResult, ResponseClassification, ZorblaxiaError, ZorblaxiaReport,
    ZorblaxiaTest, pre_merge_safety_check,
};
