//! Evidence/certainty ratings over knowledge artifacts (EX-4682 Phase 1).
//!
//! FHIR R5 `Evidence.certainty` generalized: a [`Certainty`] is a rating of a kind
//! (`certainty_type`) under a named `rating_system` (GRADE is *one* system — data, not an
//! enum), optionally decomposed into sub-component ratings via `parent_certainty_id`
//! (e.g. an `Overall` rating built from `RiskOfBias` + `Imprecision`). Persisted to the
//! [`certainty_schema`](crate::schema::certainty_schema) Parquet table.

use std::io;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{Array, RecordBatch, StringArray};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::parquet_atomic::write_parquet_atomic;
use crate::schema::{certainty_col as col, certainty_schema};

/// One certainty rating. `rating_system` and `certainty_type` are open vocabularies
/// (strings), never enums — a second rating system sits beside GRADE with no code change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Certainty {
    pub certainty_id: String,
    pub artifact_id: String,
    /// e.g. `Overall` | `RiskOfBias` | `Inconsistency` | `Imprecision` | …
    pub certainty_type: String,
    /// The rating value in the rating system's scale (e.g. `high` | `moderate` | `low`).
    pub rating: String,
    pub rater: Option<String>,
    /// e.g. `GRADE` — a value, not an enum.
    pub rating_system: String,
    /// COG context-match axis: `low` | `moderate` | `high` | `exact`.
    pub directness: Option<String>,
    /// Parent (overall) rating this is a sub-component of — `None` for a top-level rating.
    pub parent_certainty_id: Option<String>,
}

/// Render a set of certainty ratings as a [`certainty_schema`] [`RecordBatch`].
pub fn to_record_batch(ratings: &[Certainty]) -> RecordBatch {
    let s = |get: &dyn Fn(&Certainty) -> String| -> Arc<dyn Array> {
        Arc::new(StringArray::from(
            ratings.iter().map(get).collect::<Vec<_>>(),
        ))
    };
    let so = |get: &dyn Fn(&Certainty) -> Option<String>| -> Arc<dyn Array> {
        Arc::new(StringArray::from(
            ratings.iter().map(get).collect::<Vec<Option<String>>>(),
        ))
    };
    RecordBatch::try_new(
        Arc::new(certainty_schema()),
        vec![
            s(&|c| c.certainty_id.clone()),
            s(&|c| c.artifact_id.clone()),
            s(&|c| c.certainty_type.clone()),
            s(&|c| c.rating.clone()),
            so(&|c| c.rater.clone()),
            s(&|c| c.rating_system.clone()),
            so(&|c| c.directness.clone()),
            so(&|c| c.parent_certainty_id.clone()),
        ],
    )
    .expect("columns match certainty_schema")
}

/// Reconstruct certainty ratings from a [`certainty_schema`] [`RecordBatch`].
pub fn from_record_batch(batch: &RecordBatch) -> io::Result<Vec<Certainty>> {
    let c = |i: usize| -> io::Result<&StringArray> {
        batch
            .column(i)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| io::Error::other(format!("certainty column {i} is not Utf8")))
    };
    let (id, art, ty, rating, rater, sys, direct, parent) = (
        c(col::CERTAINTY_ID)?,
        c(col::ARTIFACT_ID)?,
        c(col::CERTAINTY_TYPE)?,
        c(col::RATING)?,
        c(col::RATER)?,
        c(col::RATING_SYSTEM)?,
        c(col::DIRECTNESS)?,
        c(col::PARENT_CERTAINTY_ID)?,
    );
    let opt = |a: &StringArray, row: usize| {
        if a.is_null(row) {
            None
        } else {
            Some(a.value(row).to_string())
        }
    };
    Ok((0..batch.num_rows())
        .map(|r| Certainty {
            certainty_id: id.value(r).to_string(),
            artifact_id: art.value(r).to_string(),
            certainty_type: ty.value(r).to_string(),
            rating: rating.value(r).to_string(),
            rater: opt(rater, r),
            rating_system: sys.value(r).to_string(),
            directness: opt(direct, r),
            parent_certainty_id: opt(parent, r),
        })
        .collect())
}

/// Atomically persist certainty ratings to a Parquet file.
pub fn write_parquet(ratings: &[Certainty], path: &Path) -> io::Result<()> {
    let batch = to_record_batch(ratings);
    write_parquet_atomic(path, |file| {
        let mut w = ArrowWriter::try_new(file, batch.schema(), None)
            .map_err(|e| io::Error::other(e.to_string()))?;
        w.write(&batch)
            .map_err(|e| io::Error::other(e.to_string()))?;
        w.close().map_err(|e| io::Error::other(e.to_string()))?;
        Ok(())
    })
}

/// Load certainty ratings from a Parquet file.
pub fn read_parquet(path: &Path) -> io::Result<Vec<Certainty>> {
    let file = std::fs::File::open(path)?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| io::Error::other(e.to_string()))?
        .build()
        .map_err(|e| io::Error::other(e.to_string()))?;
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.map_err(|e| io::Error::other(e.to_string()))?;
        out.extend(from_record_batch(&batch)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overall() -> Certainty {
        Certainty {
            certainty_id: "c-overall".into(),
            artifact_id: "art-1".into(),
            certainty_type: "Overall".into(),
            rating: "high".into(),
            rater: Some("guideline-panel".into()),
            rating_system: "GRADE".into(),
            directness: Some("exact".into()),
            parent_certainty_id: None,
        }
    }
    fn sub(id: &str, ty: &str, system: &str) -> Certainty {
        Certainty {
            certainty_id: id.into(),
            artifact_id: "art-1".into(),
            certainty_type: ty.into(),
            rating: "moderate".into(),
            rater: None,
            rating_system: system.into(),
            directness: None,
            parent_certainty_id: Some("c-overall".into()),
        }
    }

    #[test]
    fn recursive_certainty_round_trips_through_parquet() {
        let ratings = vec![
            overall(),
            sub("c-rob", "RiskOfBias", "GRADE"),
            sub("c-imp", "Imprecision", "GRADE"),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("certainty.parquet");
        write_parquet(&ratings, &path).unwrap();
        let loaded = read_parquet(&path).unwrap();
        assert_eq!(loaded, ratings);
        // The two sub-components point at the overall rating.
        assert_eq!(
            loaded
                .iter()
                .filter(|c| c.parent_certainty_id.as_deref() == Some("c-overall"))
                .count(),
            2
        );
    }

    #[test]
    fn rating_systems_are_data_not_enums() {
        // GRADE and a synthetic second system coexist with no code change.
        let ratings = vec![
            sub("c-grade", "RiskOfBias", "GRADE"),
            sub("c-oxford", "RiskOfBias", "OxfordCEBM"),
        ];
        let batch = to_record_batch(&ratings);
        let back = from_record_batch(&batch).unwrap();
        let systems: Vec<&str> = back.iter().map(|c| c.rating_system.as_str()).collect();
        assert!(systems.contains(&"GRADE"));
        assert!(systems.contains(&"OxfordCEBM"));
    }
}
