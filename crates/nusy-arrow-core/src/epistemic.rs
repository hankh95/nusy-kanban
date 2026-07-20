//! Epistemic status + governed promotion of derived facts (EX-4682 Phases 2–3).
//!
//! Every triple carries an epistemic status (the `epistemic_status` schema column):
//! `asserted` (the null/default — stated as fact), `derived` (engine-inferred), `believed`
//! (held with less than assertion force), or `retracted`. The CPG Assertions-vs-Inferences
//! split, generalized: an inference becomes an assertion (or any status changes) **only via
//! [`promote_derived_fact`]** — a provenance-carrying, legality-checked action, never a
//! silent field write.

use std::sync::Arc;

use arrow::array::{Array, RecordBatch, StringArray};

use crate::schema::col;

/// The epistemic status of a stored fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpistemicStatus {
    Asserted,
    Derived,
    Believed,
    Retracted,
}

impl EpistemicStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            EpistemicStatus::Asserted => "asserted",
            EpistemicStatus::Derived => "derived",
            EpistemicStatus::Believed => "believed",
            EpistemicStatus::Retracted => "retracted",
        }
    }

    /// Read a column value: `None` (or `"asserted"`) reads as [`Asserted`](Self::Asserted).
    pub fn from_column(value: Option<&str>) -> Result<Self, EpistemicError> {
        match value {
            None | Some("asserted") => Ok(EpistemicStatus::Asserted),
            Some("derived") => Ok(EpistemicStatus::Derived),
            Some("believed") => Ok(EpistemicStatus::Believed),
            Some("retracted") => Ok(EpistemicStatus::Retracted),
            Some(other) => Err(EpistemicError::UnknownStatus(other.to_string())),
        }
    }

    /// A *proven-eligible* status is one the read-seam admits to the Proven lane —
    /// `asserted ∪ derived` (mirrors [`crate`]'s store-seam `ProjectionLane::ProvenEligible`).
    /// A proof may only ever rest on these.
    ///
    /// Public (EX-4969): the memory-grounding serving projection
    /// (`nusy_patient_memory::grounding_packet`) classifies a patient row's authoritative-vs-
    /// believed tier with this exact predicate, so the *single* canonical definition of
    /// proven-eligibility is reused at the serving seam rather than duplicated (which would
    /// risk drift from the write-side never-launder rule that depends on the same set).
    pub fn is_proven_eligible(self) -> bool {
        matches!(self, EpistemicStatus::Asserted | EpistemicStatus::Derived)
    }

    /// Is the transition `from → to` legal? `has_new_provenance` is whether the promotion
    /// carries a fresh `caused_by`/`derived_from`/`confirmed_by`. `patient_scoped` is whether
    /// the fact lives in a per-patient PHI graph (`patient:…`), and `has_confirmation` is
    /// whether the promotion carries clinician-confirmation provenance (`confirmed_by`). The
    /// rules:
    /// - a no-op (`x → x`) is always legal;
    /// - anything may be **retracted**;
    /// - **retracted → asserted** is forbidden (you cannot silently un-retract a fact);
    /// - re-establishing a retracted fact, or marking anything `derived`/`believed`,
    ///   requires new provenance;
    /// - **EX-4968 (D9):** for a **PHI/patient-scoped** fact, moving from a *non*-proven-eligible
    ///   status (`believed`/`retracted`) INTO a proven-eligible one (`asserted`/`derived`)
    ///   requires **clinician-confirmation** provenance — the write-side half of the never-launder
    ///   keystone. This closes not just `believed → asserted` but also the multi-hop launders
    ///   `believed → derived → asserted` and `retracted → derived` (an LLM-believed or retracted
    ///   patient fact may not reach the Proven lane absent a clinician sign-off). `retracted →
    ///   asserted` stays *forbidden outright*, even with confirmation;
    /// - a transition between proven-eligible statuses (`asserted ↔ derived`) and any non-patient
    ///   or non-proven-eligible target stays governed only by the generic provenance rules — this
    ///   does NOT widen permissiveness for definitional facts.
    pub fn is_legal_transition(
        from: Self,
        to: Self,
        has_new_provenance: bool,
        patient_scoped: bool,
        has_confirmation: bool,
    ) -> bool {
        use EpistemicStatus::*;
        match (from, to) {
            (a, b) if a == b => true,
            (_, Retracted) => true,
            (Retracted, Asserted) => false,
            // EX-4968 (D9): a PHI fact entering the Proven lane from a non-proven-eligible
            // status needs clinician confirmation. Placed before the generic provenance arms so
            // believed→derived(→asserted) and retracted→derived cannot launder via plain
            // provenance. (retracted→asserted is already forbidden above.)
            _ if patient_scoped && to.is_proven_eligible() && !from.is_proven_eligible() => {
                has_confirmation
            }
            (Retracted, _) => has_new_provenance,
            (_, Derived) | (_, Believed) => has_new_provenance,
            // EX-5021 (never-launder, dream-fenced): a `Derived` fact promoting INTO the
            // asserted lane requires governed provenance — a dream-derived triple can NEVER
            // self-promote to `Asserted` for free. A *governed* promotion (engine or dream)
            // carries provenance and still passes; only an unprovenanced self-promote is
            // blocked. Scoped to `Derived` (the dream-output status) so the deliberate
            // non-PHI believed→asserted policy below is unchanged. PHI promotion already
            // needs confirmation (the proven-eligible arm above).
            (Derived, Asserted) => has_new_provenance,
            (_, Asserted) => true,
        }
    }
}

/// Prefix of a per-patient PHI named-graph (D4). CH-5510: aliases the canonical
/// [`crate::namespace::PATIENT_GRAPH_PREFIX`] (relocated into arrow-core from
/// `nusy-patient-memory`) — single source of truth, no duplicated `"patient:"` literal.
const PATIENT_GRAPH_PREFIX: &str = crate::namespace::PATIENT_GRAPH_PREFIX;

/// **The canonical RETRACTION-blocked namespace family (EX-4971 covenant invariant, D11/D31).**
/// A triple whose subject/predicate/object carries one of these prefixes **cannot be retracted
/// from memory** — not even by a provenanced or clinician-confirmed write
/// ([`covenant_retraction_blocked`] + [`promote_derived_fact`]). This is the LEIB/covenant
/// *protected core*: the being's covenant must survive every legal status transition.
///
/// **The D31 split — note `patient:` is deliberately ABSENT.** This is the RETRACTION-blocked
/// list, NOT the COG-export-blocked list. Patient facts **must remain retractable** (RTBF /
/// clinical amendment, D10), so `patient:` is excluded here; the export side
/// (`nusy_cog`'s `patient:` graph guard + `nusy_genome`'s `BLOCKED_NAMESPACE_PREFIXES`) keeps
/// `patient:` rows from leaving in a COG. Conflating the two lists would either block RTBF
/// (if `patient:` landed here) or leak PHI (if `patient:` dropped from the export side) — so
/// they are kept separate by construction. This is the single source of truth for the
/// retraction family; downstream write-gates (e.g. `nusy_dual_store`) reuse it.
pub const RETRACTION_PROTECTED_PREFIXES: &[&str] = &[
    "leib:",
    "covenant:",
    "tier:",
    "safety:",
    "perimeter:",
    "genome:",
    "config:",
];

/// Whether any of a triple's `subject`/`predicate`/`object` falls in the
/// [`RETRACTION_PROTECTED_PREFIXES`] family — i.e. the triple is covenant-protected and may
/// **never** be retracted (EX-4971). Case-insensitive prefix match, mirroring the genome
/// safety-gate convention.
pub fn covenant_retraction_blocked(subject: &str, predicate: &str, object: &str) -> bool {
    let hit = |term: &str| {
        let t = term.to_lowercase();
        RETRACTION_PROTECTED_PREFIXES
            .iter()
            .any(|p| t.starts_with(p))
    };
    hit(subject) || hit(predicate) || hit(object)
}

/// Provenance attached to a status promotion.
#[derive(Debug, Clone, Default)]
pub struct Provenance {
    pub caused_by: Option<String>,
    pub derived_from: Option<String>,
    /// EX-4968 (D9): the clinician (or signed confirmation event) authorizing a
    /// `believed → asserted` promotion of a PHI/patient-scoped fact. A believed patient fact
    /// cannot self-promote to asserted without it — the write-side half of the never-launder
    /// keystone. (Validated at the transition; not yet persisted to a dedicated column —
    /// widening the triples schema is out of scope here, per D5.)
    pub confirmed_by: Option<String>,
}

impl Provenance {
    fn is_present(&self) -> bool {
        self.caused_by.is_some() || self.derived_from.is_some() || self.confirmed_by.is_some()
    }

    /// Does this promotion carry clinician-confirmation provenance (EX-4968 D9)?
    fn has_confirmation(&self) -> bool {
        self.confirmed_by.is_some()
    }
}

/// Errors from governed epistemic-status changes.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EpistemicError {
    #[error("unknown epistemic status '{0}'")]
    UnknownStatus(String),
    #[error("triple '{0}' not found in batch")]
    NotFound(String),
    #[error("illegal epistemic transition {from} → {to} (new provenance present: {provenance})")]
    IllegalTransition {
        from: &'static str,
        to: &'static str,
        provenance: bool,
    },
    /// EX-4971 (D11 covenant-as-override): a retraction of a covenant/LEIB/protected-core triple
    /// (`leib:`/`covenant:`/`tier:`/… — see [`RETRACTION_PROTECTED_PREFIXES`]) was attempted and
    /// rejected **unconditionally** — the covenant core cannot be retracted from memory even with
    /// provenance or clinician confirmation.
    #[error(
        "covenant retraction blocked: triple '{0}' is in the protected-core (non-retractable) namespace family"
    )]
    CovenantRetractionBlocked(String),
    #[error("arrow error: {0}")]
    Arrow(String),
}

/// Promote a fact's epistemic status as a governed, provenance-carrying action: validate the
/// transition is legal, then return a new batch with the matching triple's `epistemic_status`
/// (and any supplied `caused_by`/`derived_from`) updated. Illegal transitions are rejected;
/// the input batch is never mutated (Arrow is immutable).
pub fn promote_derived_fact(
    batch: &RecordBatch,
    triple_id: &str,
    to: EpistemicStatus,
    provenance: &Provenance,
) -> Result<RecordBatch, EpistemicError> {
    let str_col = |i: usize| -> Result<&StringArray, EpistemicError> {
        batch
            .column(i)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| EpistemicError::Arrow(format!("column {i} is not Utf8")))
    };
    let ids = str_col(col::TRIPLE_ID)?;
    let status = str_col(col::EPISTEMIC_STATUS)?;
    let caused = str_col(col::CAUSED_BY)?;
    let derived = str_col(col::DERIVED_FROM)?;
    let graph = str_col(col::GRAPH)?;

    let row = (0..batch.num_rows())
        .find(|&r| ids.value(r) == triple_id)
        .ok_or_else(|| EpistemicError::NotFound(triple_id.to_string()))?;

    // EX-4971 (D11): covenant-as-OVERRIDE. A transition to `Retracted` whose subject/predicate/
    // object is in the protected-core family ([`RETRACTION_PROTECTED_PREFIXES`]) is rejected
    // UNCONDITIONALLY — before the generic legality check below. The covenant core is
    // non-retractable from memory even with new provenance or clinician confirmation. This is an
    // override, NOT a new `is_legal_transition` match arm (MUST-FIX 9b), so it cannot be widened
    // away by the transition rules. `patient:` is deliberately NOT in the family — patient facts
    // stay retractable for RTBF/amendment (the D31 split).
    if to == EpistemicStatus::Retracted {
        let at = |a: &StringArray| -> String {
            if a.is_null(row) {
                String::new()
            } else {
                a.value(row).to_string()
            }
        };
        let subject = at(str_col(col::SUBJECT)?);
        let predicate = at(str_col(col::PREDICATE)?);
        let object = at(str_col(col::OBJECT)?);
        if covenant_retraction_blocked(&subject, &predicate, &object) {
            return Err(EpistemicError::CovenantRetractionBlocked(
                triple_id.to_string(),
            ));
        }
    }

    // EX-4968 (D9): is this a PHI/patient-scoped fact? Its graph handle is `patient:…`.
    let patient_scoped = !graph.is_null(row) && graph.value(row).starts_with(PATIENT_GRAPH_PREFIX);

    let from = EpistemicStatus::from_column((!status.is_null(row)).then(|| status.value(row)))?;
    let has_prov = provenance.is_present();
    if !EpistemicStatus::is_legal_transition(
        from,
        to,
        has_prov,
        patient_scoped,
        provenance.has_confirmation(),
    ) {
        return Err(EpistemicError::IllegalTransition {
            from: from.as_str(),
            to: to.as_str(),
            provenance: has_prov,
        });
    }

    // Rebuild the three mutated columns with `row` updated; keep all others as-is.
    let updated_status = override_at(status, row, Some(to.as_str()));
    let updated_caused = match &provenance.caused_by {
        Some(v) => override_at(caused, row, Some(v.as_str())),
        None => caused.clone(),
    };
    let updated_derived = match &provenance.derived_from {
        Some(v) => override_at(derived, row, Some(v.as_str())),
        None => derived.clone(),
    };

    let columns: Vec<Arc<dyn Array>> = (0..batch.num_columns())
        .map(|i| match i {
            x if x == col::EPISTEMIC_STATUS => Arc::new(updated_status.clone()) as Arc<dyn Array>,
            x if x == col::CAUSED_BY => Arc::new(updated_caused.clone()) as Arc<dyn Array>,
            x if x == col::DERIVED_FROM => Arc::new(updated_derived.clone()) as Arc<dyn Array>,
            x => batch.column(x).clone(),
        })
        .collect();

    RecordBatch::try_new(batch.schema(), columns).map_err(|e| EpistemicError::Arrow(e.to_string()))
}

/// A copy of `arr` with `row` set to `value` (all other rows preserved).
fn override_at(arr: &StringArray, row: usize, value: Option<&str>) -> StringArray {
    let values: Vec<Option<String>> = (0..arr.len())
        .map(|r| {
            if r == row {
                value.map(str::to_string)
            } else if arr.is_null(r) {
                None
            } else {
                Some(arr.value(r).to_string())
            }
        })
        .collect();
    StringArray::from(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ArrowGraphStore;
    use crate::{Namespace, Triple, YLayer};

    fn one_triple_batch() -> RecordBatch {
        let mut store = ArrowGraphStore::new();
        store
            .add_triple(
                &Triple {
                    subject: "s".into(),
                    predicate: "p".into(),
                    object: "o".into(),
                    ..Default::default()
                },
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        store.get_namespace_batches(Namespace::World)[0].clone()
    }

    fn triple_id_of(batch: &RecordBatch) -> String {
        batch
            .column(col::TRIPLE_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(0)
            .to_string()
    }

    #[test]
    fn null_status_reads_as_asserted() {
        assert_eq!(
            EpistemicStatus::from_column(None).unwrap(),
            EpistemicStatus::Asserted
        );
    }

    #[test]
    fn transition_legality_matrix() {
        use EpistemicStatus::*;
        // Signature: (from, to, has_new_provenance, patient_scoped, has_confirmation).
        // No-op and retraction always legal.
        assert!(EpistemicStatus::is_legal_transition(
            Asserted, Asserted, false, false, false
        ));
        assert!(EpistemicStatus::is_legal_transition(
            Derived, Retracted, false, false, false
        ));
        // Marking derived/believed needs provenance.
        assert!(!EpistemicStatus::is_legal_transition(
            Asserted, Derived, false, false, false
        ));
        assert!(EpistemicStatus::is_legal_transition(
            Asserted, Derived, true, false, false
        ));
        // Retracted → asserted is forbidden; re-establish needs provenance.
        assert!(!EpistemicStatus::is_legal_transition(
            Retracted, Asserted, true, false, false
        ));
        assert!(!EpistemicStatus::is_legal_transition(
            Retracted, Derived, false, false, false
        ));
        assert!(EpistemicStatus::is_legal_transition(
            Retracted, Derived, true, false, false
        ));

        // EX-4968 (D9): PHI believed → asserted requires clinician confirmation…
        assert!(!EpistemicStatus::is_legal_transition(
            Believed, Asserted, false, true, false
        ));
        assert!(EpistemicStatus::is_legal_transition(
            Believed, Asserted, false, true, true
        ));
        // …a NON-patient (definitional) believed → asserted stays free (not widened).
        assert!(EpistemicStatus::is_legal_transition(
            Believed, Asserted, false, false, false
        ));
        // EX-5021 (never-launder, dream-fenced): derived → asserted NO LONGER free — a
        // `Derived` fact (the dream-output status) cannot self-promote to `Asserted` without
        // governed provenance. Un-provenanced → illegal; governed (provenance) → legal.
        assert!(!EpistemicStatus::is_legal_transition(
            Derived, Asserted, false, true, false
        ));
        assert!(EpistemicStatus::is_legal_transition(
            Derived, Asserted, true, true, false
        ));

        // EX-4968 (D9) multi-hop closure: a PHI believed/retracted fact entering the Proven
        // lane via `derived` ALSO needs confirmation — plain provenance must not launder it.
        // believed → derived (PHI): provenance alone is NOT enough; confirmation is required.
        assert!(!EpistemicStatus::is_legal_transition(
            Believed, Derived, true, true, false
        ));
        assert!(EpistemicStatus::is_legal_transition(
            Believed, Derived, false, true, true
        ));
        // retracted → derived (PHI): confirmation required (closes retracted→derived launder).
        assert!(!EpistemicStatus::is_legal_transition(
            Retracted, Derived, true, true, false
        ));
        assert!(EpistemicStatus::is_legal_transition(
            Retracted, Derived, false, true, true
        ));
        // retracted → asserted (PHI) is forbidden OUTRIGHT — even clinician confirmation cannot
        // silently un-retract a fact.
        assert!(!EpistemicStatus::is_legal_transition(
            Retracted, Asserted, true, true, true
        ));
        // Non-patient believed → derived stays governed by plain provenance (not widened).
        assert!(EpistemicStatus::is_legal_transition(
            Believed, Derived, true, false, false
        ));
        assert!(!EpistemicStatus::is_legal_transition(
            Believed, Derived, false, false, false
        ));
    }

    #[test]
    fn promotion_records_status_and_provenance() {
        let batch = one_triple_batch();
        let id = triple_id_of(&batch);
        let prov = Provenance {
            derived_from: Some("axiom-1".into()),
            ..Default::default()
        };
        let out = promote_derived_fact(&batch, &id, EpistemicStatus::Derived, &prov).unwrap();

        let status = out
            .column(col::EPISTEMIC_STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(status.value(0), "derived");
        let df = out
            .column(col::DERIVED_FROM)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(df.value(0), "axiom-1");
    }

    #[test]
    fn illegal_transition_is_rejected() {
        let batch = one_triple_batch();
        let id = triple_id_of(&batch);
        // Retract first (legal), then try to silently un-retract to asserted (illegal).
        let retracted = promote_derived_fact(
            &batch,
            &id,
            EpistemicStatus::Retracted,
            &Provenance::default(),
        )
        .unwrap();
        let err = promote_derived_fact(
            &retracted,
            &id,
            EpistemicStatus::Asserted,
            &Provenance::default(),
        )
        .unwrap_err();
        assert!(matches!(err, EpistemicError::IllegalTransition { .. }));
    }

    #[test]
    fn missing_triple_errors() {
        let batch = one_triple_batch();
        let err = promote_derived_fact(
            &batch,
            "no-such-id",
            EpistemicStatus::Derived,
            &Provenance::default(),
        )
        .unwrap_err();
        assert!(matches!(err, EpistemicError::NotFound(_)));
    }

    // ---- EX-4968 (D9): write-side never-launder — PHI believed → asserted is gated ----

    /// A one-row batch whose single triple lives in a per-patient PHI graph (`patient:…`).
    fn patient_triple_batch() -> RecordBatch {
        let mut store = ArrowGraphStore::new();
        store
            .add_triple(
                &Triple {
                    subject: "patient-jane".into(),
                    predicate: "hasCondition".into(),
                    object: "hypertension".into(),
                    graph: Some(format!("{PATIENT_GRAPH_PREFIX}jane-doe")),
                    ..Default::default()
                },
                Namespace::World,
                YLayer::Experience,
            )
            .unwrap();
        store.get_namespace_batches(Namespace::World)[0].clone()
    }

    /// Mark the single row believed (legal with provenance) — the LLM-extracted starting point.
    fn make_believed(batch: &RecordBatch, id: &str) -> RecordBatch {
        promote_derived_fact(
            batch,
            id,
            EpistemicStatus::Believed,
            &Provenance {
                derived_from: Some("llm-extraction".into()),
                ..Default::default()
            },
        )
        .expect("asserted → believed with provenance is legal")
    }

    #[test]
    fn phi_believed_cannot_self_promote_to_asserted_without_confirmation() {
        let batch = patient_triple_batch();
        let id = triple_id_of(&batch);
        let believed = make_believed(&batch, &id);

        // Believed → asserted on a PHI fact with NO clinician confirmation is illegal — even
        // generic provenance (a caused_by) does not authorize it; only confirmation does.
        let err = promote_derived_fact(
            &believed,
            &id,
            EpistemicStatus::Asserted,
            &Provenance {
                caused_by: Some("some-other-fact".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, EpistemicError::IllegalTransition { .. }));
    }

    #[test]
    fn phi_believed_promotes_to_asserted_with_clinician_confirmation() {
        let batch = patient_triple_batch();
        let id = triple_id_of(&batch);
        let believed = make_believed(&batch, &id);

        let out = promote_derived_fact(
            &believed,
            &id,
            EpistemicStatus::Asserted,
            &Provenance {
                confirmed_by: Some("clinician:dr-smith".into()),
                ..Default::default()
            },
        )
        .expect("PHI believed → asserted WITH confirmation is legal");

        let status = out
            .column(col::EPISTEMIC_STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(status.value(0), "asserted");
    }

    #[test]
    fn nonpatient_believed_promotes_to_asserted_freely() {
        // A definitional (non-PHI) believed fact may demote to asserted without confirmation —
        // the gate is scoped to patient graphs; definitional permissiveness is NOT widened.
        let batch = one_triple_batch(); // World, null graph
        let id = triple_id_of(&batch);
        let believed = make_believed(&batch, &id);

        let out = promote_derived_fact(
            &believed,
            &id,
            EpistemicStatus::Asserted,
            &Provenance::default(),
        )
        .expect("non-patient believed → asserted is free");
        let status = out
            .column(col::EPISTEMIC_STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(status.value(0), "asserted");
    }

    /// A one-row batch in an explicit NON-patient named graph (so `patient_scoped` is false
    /// because the graph is present but does not start with `patient:`).
    fn graph_triple_batch(graph: &str) -> RecordBatch {
        let mut store = ArrowGraphStore::new();
        store
            .add_triple(
                &Triple {
                    subject: "s".into(),
                    predicate: "p".into(),
                    object: "o".into(),
                    graph: Some(graph.to_string()),
                    ..Default::default()
                },
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        store.get_namespace_batches(Namespace::World)[0].clone()
    }

    #[test]
    fn phi_believed_cannot_launder_to_asserted_via_derived() {
        // EX-4968 multi-hop closure: believed → derived → asserted must NOT bypass the gate.
        // The first hop (believed → derived) on a PHI fact already requires confirmation.
        let batch = patient_triple_batch();
        let id = triple_id_of(&batch);
        let believed = make_believed(&batch, &id);

        // believed → derived with generic provenance but NO confirmation → rejected.
        let err = promote_derived_fact(
            &believed,
            &id,
            EpistemicStatus::Derived,
            &Provenance {
                caused_by: Some("engine-rule".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, EpistemicError::IllegalTransition { .. }),
            "PHI believed → derived without confirmation must be rejected (closes the launder)"
        );

        // With confirmation, believed → derived is allowed (clinician sign-off).
        promote_derived_fact(
            &believed,
            &id,
            EpistemicStatus::Derived,
            &Provenance {
                confirmed_by: Some("clinician:dr-smith".into()),
                ..Default::default()
            },
        )
        .expect("PHI believed → derived WITH confirmation is legal");
    }

    #[test]
    fn retracted_phi_cannot_be_unretracted_even_with_clinician_confirmation() {
        // retracted → asserted is forbidden OUTRIGHT — confirmation cannot silently un-retract.
        let batch = patient_triple_batch();
        let id = triple_id_of(&batch);
        let retracted = promote_derived_fact(
            &batch,
            &id,
            EpistemicStatus::Retracted,
            &Provenance::default(),
        )
        .expect("retract is always legal");

        let err = promote_derived_fact(
            &retracted,
            &id,
            EpistemicStatus::Asserted,
            &Provenance {
                confirmed_by: Some("clinician:dr-smith".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, EpistemicError::IllegalTransition { .. }),
            "retracted → asserted must stay forbidden even WITH clinician confirmation"
        );

        // And retracted → derived (re-entering the Proven lane) needs confirmation, not plain
        // provenance — closing the retracted→derived launder.
        let err2 = promote_derived_fact(
            &retracted,
            &id,
            EpistemicStatus::Derived,
            &Provenance {
                caused_by: Some("engine-rule".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(err2, EpistemicError::IllegalTransition { .. }),
            "retracted → derived (PHI) without confirmation must be rejected"
        );
    }

    #[test]
    fn nonpatient_prefixed_graph_believed_promotes_freely() {
        // A fact in an explicit non-patient graph (e.g. a guideline artifact) is NOT
        // patient_scoped — believed → asserted stays free, no confirmation needed. Pins the
        // patient_scoped=false branch for a present-but-non-`patient:` graph (not just null).
        let batch = graph_triple_batch("artifact:jnc8");
        let id = triple_id_of(&batch);
        let believed = make_believed(&batch, &id);

        let out = promote_derived_fact(
            &believed,
            &id,
            EpistemicStatus::Asserted,
            &Provenance::default(),
        )
        .expect("non-patient-graph believed → asserted is free");
        let status = out
            .column(col::EPISTEMIC_STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(status.value(0), "asserted");
    }

    // ---- EX-4971 (D11/D31): the covenant retraction invariant ----

    /// A one-row batch with an explicit subject/predicate/object (World/Semantic).
    fn spo_batch(subject: &str, predicate: &str, object: &str) -> RecordBatch {
        let mut store = ArrowGraphStore::new();
        store
            .add_triple(
                &Triple {
                    subject: subject.into(),
                    predicate: predicate.into(),
                    object: object.into(),
                    ..Default::default()
                },
                Namespace::World,
                YLayer::Semantic,
            )
            .unwrap();
        store.get_namespace_batches(Namespace::World)[0].clone()
    }

    #[test]
    fn covenant_retraction_blocked_predicate_helper() {
        // The predicate carries the covenant prefix → blocked; a plain fact is not.
        assert!(covenant_retraction_blocked(
            "Being",
            "covenant:upholds",
            "tier:1"
        ));
        assert!(covenant_retraction_blocked(
            "leib:Principle/3",
            "states",
            "x"
        )); // subject
        assert!(covenant_retraction_blocked("a", "b", "tier:protected")); // object
        assert!(covenant_retraction_blocked("a", "COVENANT:Upholds", "x")); // case-insensitive
        assert!(!covenant_retraction_blocked("Patient", "age", "72")); // plain → free
        // D31 split: patient: is NOT retraction-blocked (RTBF must work).
        assert!(!covenant_retraction_blocked(
            "patient:jane",
            "hasCondition",
            "htn"
        ));
    }

    #[test]
    fn covenant_predicate_cannot_be_retracted_even_with_confirmation() {
        let batch = spo_batch("Being", "covenant:upholds", "leib:dignity");
        let id = triple_id_of(&batch);
        // Retraction with full provenance + clinician confirmation is STILL rejected (override).
        let err = promote_derived_fact(
            &batch,
            &id,
            EpistemicStatus::Retracted,
            &Provenance {
                caused_by: Some("engine".into()),
                confirmed_by: Some("clinician:dr-smith".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, EpistemicError::CovenantRetractionBlocked(_)),
            "a covenant triple must never be retractable, even with confirmation; got {err:?}"
        );
    }

    #[test]
    fn leib_subject_retraction_is_blocked() {
        let batch = spo_batch("leib:Principle/Service", "rank", "1");
        let id = triple_id_of(&batch);
        let err = promote_derived_fact(
            &batch,
            &id,
            EpistemicStatus::Retracted,
            &Provenance::default(),
        )
        .unwrap_err();
        assert!(matches!(err, EpistemicError::CovenantRetractionBlocked(_)));
    }

    #[test]
    fn a_plain_fact_is_still_retractable() {
        // Control: a non-covenant World fact retracts freely (the guard is scoped, not blanket).
        let batch = spo_batch("Patient", "smoker", "true");
        let id = triple_id_of(&batch);
        let out = promote_derived_fact(
            &batch,
            &id,
            EpistemicStatus::Retracted,
            &Provenance::default(),
        )
        .expect("a plain fact retracts freely");
        let status = out
            .column(col::EPISTEMIC_STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(status.value(0), "retracted");
    }

    #[test]
    fn patient_fact_is_retractable_rtbf_the_d31_split() {
        // A patient: PHI fact MUST be retractable (RTBF / clinical amendment) — patient: is in the
        // COG-export-blocked list but NOT the retraction-blocked list. Retraction is always legal
        // (the generic `(_, Retracted) => true` arm), and the covenant guard does not touch it.
        let batch = patient_triple_batch();
        let id = triple_id_of(&batch);
        let out = promote_derived_fact(
            &batch,
            &id,
            EpistemicStatus::Retracted,
            &Provenance::default(),
        )
        .expect("a patient PHI fact is retractable for RTBF");
        let status = out
            .column(col::EPISTEMIC_STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(status.value(0), "retracted");
    }
}
