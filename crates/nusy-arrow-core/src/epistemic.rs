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

    /// Is the transition `from → to` legal? `has_new_provenance` is whether the promotion
    /// carries a fresh `caused_by`/`derived_from`. The rules:
    /// - a no-op (`x → x`) is always legal;
    /// - anything may be **retracted**;
    /// - **retracted → asserted** is forbidden (you cannot silently un-retract a fact);
    /// - re-establishing a retracted fact, or marking anything `derived`/`believed`,
    ///   requires new provenance;
    /// - demoting to `asserted` (from derived/believed) is allowed.
    pub fn is_legal_transition(from: Self, to: Self, has_new_provenance: bool) -> bool {
        use EpistemicStatus::*;
        match (from, to) {
            (a, b) if a == b => true,
            (_, Retracted) => true,
            (Retracted, Asserted) => false,
            (Retracted, _) => has_new_provenance,
            (_, Derived) | (_, Believed) => has_new_provenance,
            (_, Asserted) => true,
        }
    }
}

/// Provenance attached to a status promotion.
#[derive(Debug, Clone, Default)]
pub struct Provenance {
    pub caused_by: Option<String>,
    pub derived_from: Option<String>,
}

impl Provenance {
    fn is_present(&self) -> bool {
        self.caused_by.is_some() || self.derived_from.is_some()
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

    let row = (0..batch.num_rows())
        .find(|&r| ids.value(r) == triple_id)
        .ok_or_else(|| EpistemicError::NotFound(triple_id.to_string()))?;

    let from = EpistemicStatus::from_column((!status.is_null(row)).then(|| status.value(row)))?;
    let has_prov = provenance.is_present();
    if !EpistemicStatus::is_legal_transition(from, to, has_prov) {
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
        // No-op and retraction always legal.
        assert!(EpistemicStatus::is_legal_transition(
            Asserted, Asserted, false
        ));
        assert!(EpistemicStatus::is_legal_transition(
            Derived, Retracted, false
        ));
        // Marking derived/believed needs provenance.
        assert!(!EpistemicStatus::is_legal_transition(
            Asserted, Derived, false
        ));
        assert!(EpistemicStatus::is_legal_transition(
            Asserted, Derived, true
        ));
        // Retracted → asserted is forbidden; re-establish needs provenance.
        assert!(!EpistemicStatus::is_legal_transition(
            Retracted, Asserted, true
        ));
        assert!(!EpistemicStatus::is_legal_transition(
            Retracted, Derived, false
        ));
        assert!(EpistemicStatus::is_legal_transition(
            Retracted, Derived, true
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
}
