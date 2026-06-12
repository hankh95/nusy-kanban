//! Cognitive parameter store for the `self` namespace.
//!
//! Stores a being's tunable cognitive parameters as an Arrow RecordBatch.
//! Parameters include signal fusion weights, learning rate configs, schema
//! match thresholds, and consolidation triggers.
//!
//! # V15 Self-Evolution
//!
//! This is the foundation for HDD self-modification:
//! - Being reads its own parameters via `get()` / `list()`
//! - HDD loop modifies parameters via `set()`
//! - Changes are committed via graph-native git (`snapshot()` → Parquet)
//! - Reverts are atomic via `checkout(previous)`
//!
//! # Autonomy Tiers
//!
//! Each parameter has an autonomy tier controlling who can modify it:
//! - **Tier 1 (auto):** HDD loop can adjust freely (signal weights, learning rates)
//! - **Tier 2 (being-approved):** Being must confirm before applying (pipeline composition)
//! - **Tier 3 (captain-only):** Requires Captain approval (safety rules, core thresholds)

use std::sync::Arc;

use arrow::array::{Array, Float64Array, RecordBatch, StringArray, UInt8Array};
use arrow::datatypes::{DataType, Field, Schema};

// ── Schema ─────────────────────────────────────────────────────────────

/// Schema version for the cognitive parameters table.
pub const COGNITIVE_PARAMS_SCHEMA_VERSION: &str = "1.0.0";

/// Named column indices for the cognitive parameters table.
pub mod param_col {
    pub const PARAM_ID: usize = 0;
    pub const CATEGORY: usize = 1;
    pub const VALUE_F64: usize = 2;
    pub const VALUE_STR: usize = 3;
    pub const MIN_BOUND: usize = 4;
    pub const MAX_BOUND: usize = 5;
    pub const AUTONOMY_TIER: usize = 6;
    pub const MODIFIED_BY: usize = 7;
}

/// Parameter categories.
pub mod category {
    pub const SIGNAL_WEIGHT: &str = "signal_weight";
    pub const THRESHOLD: &str = "threshold";
    pub const LEARNING_CONFIG: &str = "learning_config";
    pub const TRIGGER: &str = "trigger";
    pub const LOSS_COEFFICIENT: &str = "loss_coefficient";
    pub const CONSOLIDATION_TRIGGER: &str = "consolidation_trigger";
}

/// Autonomy tier levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum AutonomyTier {
    /// HDD loop can adjust freely.
    Auto = 1,
    /// Being must confirm before applying.
    BeingApproved = 2,
    /// Requires Captain approval.
    CaptainOnly = 3,
}

impl AutonomyTier {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Auto),
            2 => Some(Self::BeingApproved),
            3 => Some(Self::CaptainOnly),
            _ => None,
        }
    }
}

/// Arrow schema for the cognitive parameters table.
///
/// 8 columns. Lives in the `self` namespace.
pub fn cognitive_params_schema() -> Schema {
    Schema::new(vec![
        Field::new("param_id", DataType::Utf8, false),
        Field::new("category", DataType::Utf8, false),
        Field::new("value_f64", DataType::Float64, true),
        Field::new("value_str", DataType::Utf8, true),
        Field::new("min_bound", DataType::Float64, true),
        Field::new("max_bound", DataType::Float64, true),
        Field::new("autonomy_tier", DataType::UInt8, false),
        Field::new("modified_by", DataType::Utf8, false),
    ])
}

// ── Typed parameter view ───────────────────────────────────────────────

/// A single cognitive parameter, extracted from the RecordBatch.
#[derive(Debug, Clone, PartialEq)]
pub struct CognitiveParameter {
    pub param_id: String,
    pub category: String,
    pub value_f64: Option<f64>,
    pub value_str: Option<String>,
    pub min_bound: Option<f64>,
    pub max_bound: Option<f64>,
    pub autonomy_tier: AutonomyTier,
    pub modified_by: String,
}

// ── Error type ─────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ParamStoreError {
    #[error("parameter not found: {0}")]
    NotFound(String),

    #[error("value {value} out of bounds [{min}, {max}] for parameter {param_id}")]
    OutOfBounds {
        param_id: String,
        value: f64,
        min: f64,
        max: f64,
    },

    #[error(
        "autonomy tier violation: parameter {param_id} requires tier {required:?}, caller is {caller:?}"
    )]
    TierViolation {
        param_id: String,
        required: AutonomyTier,
        caller: AutonomyTier,
    },

    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    #[error("invalid schema: {0}")]
    InvalidSchema(String),
}

// ── Store ──────────────────────────────────────────────────────────────

/// Arrow-backed store for cognitive parameters.
///
/// Wraps a RecordBatch with the `cognitive_params_schema()`. All operations
/// rebuild the batch (Arrow RecordBatches are immutable); this is cheap for
/// the ~50-100 parameters a being has.
#[derive(Debug, Clone)]
pub struct CognitiveParameterStore {
    batch: RecordBatch,
}

impl CognitiveParameterStore {
    /// Create an empty store.
    pub fn new() -> Self {
        let schema = Arc::new(cognitive_params_schema());
        let batch = RecordBatch::new_empty(schema);
        Self { batch }
    }

    /// Create from an existing Arrow RecordBatch.
    ///
    /// Validates that the batch has the correct schema.
    pub fn from_batch(batch: RecordBatch) -> Result<Self, ParamStoreError> {
        let expected = cognitive_params_schema();
        if batch.schema().fields().len() != expected.fields().len() {
            return Err(ParamStoreError::InvalidSchema(format!(
                "expected {} columns, got {}",
                expected.fields().len(),
                batch.schema().fields().len()
            )));
        }
        let actual_schema = batch.schema();
        for (i, field) in expected.fields().iter().enumerate() {
            let actual = actual_schema.field(i);
            if actual.data_type() != field.data_type() {
                return Err(ParamStoreError::InvalidSchema(format!(
                    "column {}: expected {:?}, got {:?}",
                    i,
                    field.data_type(),
                    actual.data_type()
                )));
            }
        }
        Ok(Self { batch })
    }

    /// Number of parameters in the store.
    pub fn len(&self) -> usize {
        self.batch.num_rows()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.batch.num_rows() == 0
    }

    /// Get a parameter by ID.
    pub fn get(&self, param_id: &str) -> Option<CognitiveParameter> {
        let ids = self.param_id_col();
        for i in 0..self.batch.num_rows() {
            if ids.value(i) == param_id {
                return Some(self.row_to_param(i));
            }
        }
        None
    }

    /// List all parameters in a category.
    pub fn list(&self, cat: &str) -> Vec<CognitiveParameter> {
        let categories = self.category_col();
        let mut result = Vec::new();
        for i in 0..self.batch.num_rows() {
            if categories.value(i) == cat {
                result.push(self.row_to_param(i));
            }
        }
        result
    }

    /// List all parameters.
    pub fn list_all(&self) -> Vec<CognitiveParameter> {
        (0..self.batch.num_rows())
            .map(|i| self.row_to_param(i))
            .collect()
    }

    /// Set a numeric parameter value with bounds checking and tier enforcement.
    ///
    /// `caller_tier` is the autonomy tier of the entity making the change.
    /// The caller's tier must be >= the parameter's required tier.
    pub fn set(
        &mut self,
        param_id: &str,
        value: f64,
        modified_by: &str,
        caller_tier: AutonomyTier,
    ) -> Result<(), ParamStoreError> {
        let idx = self.find_index(param_id)?;
        let param = self.row_to_param(idx);

        // Tier enforcement
        if caller_tier < param.autonomy_tier {
            return Err(ParamStoreError::TierViolation {
                param_id: param_id.to_string(),
                required: param.autonomy_tier,
                caller: caller_tier,
            });
        }

        // Bounds checking
        if let (Some(min), Some(max)) = (param.min_bound, param.max_bound)
            && (value < min || value > max)
        {
            return Err(ParamStoreError::OutOfBounds {
                param_id: param_id.to_string(),
                value,
                min,
                max,
            });
        }

        // Rebuild the batch with the updated value
        self.batch = self.rebuild_with_f64_update(idx, value, modified_by)?;
        Ok(())
    }

    /// Insert a new parameter. Replaces if param_id already exists.
    pub fn insert(&mut self, param: &CognitiveParameter) -> Result<(), ParamStoreError> {
        // Remove existing if present
        let existing_idx = {
            let ids = self.param_id_col();
            (0..self.batch.num_rows()).find(|&i| ids.value(i) == param.param_id)
        };

        let batch = if let Some(idx) = existing_idx {
            self.remove_row(idx)?
        } else {
            self.batch.clone()
        };

        // Append the new row
        self.batch = append_row(&batch, param)?;
        Ok(())
    }

    /// Get the underlying RecordBatch for persistence / graph-native git.
    pub fn snapshot(&self) -> &RecordBatch {
        &self.batch
    }

    /// Consume the store and return the RecordBatch.
    pub fn into_batch(self) -> RecordBatch {
        self.batch
    }

    // ── Internal helpers ───────────────────────────────────────────────

    fn param_id_col(&self) -> &StringArray {
        self.batch
            .column(param_col::PARAM_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("param_id column is StringArray")
    }

    fn category_col(&self) -> &StringArray {
        self.batch
            .column(param_col::CATEGORY)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("category column is StringArray")
    }

    fn find_index(&self, param_id: &str) -> Result<usize, ParamStoreError> {
        let ids = self.param_id_col();
        for i in 0..self.batch.num_rows() {
            if ids.value(i) == param_id {
                return Ok(i);
            }
        }
        Err(ParamStoreError::NotFound(param_id.to_string()))
    }

    fn row_to_param(&self, idx: usize) -> CognitiveParameter {
        let ids = self.param_id_col();
        let categories = self.category_col();
        let values_f64 = self
            .batch
            .column(param_col::VALUE_F64)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("value_f64 column");
        let values_str = self
            .batch
            .column(param_col::VALUE_STR)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("value_str column");
        let min_bounds = self
            .batch
            .column(param_col::MIN_BOUND)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("min_bound column");
        let max_bounds = self
            .batch
            .column(param_col::MAX_BOUND)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("max_bound column");
        let tiers = self
            .batch
            .column(param_col::AUTONOMY_TIER)
            .as_any()
            .downcast_ref::<UInt8Array>()
            .expect("autonomy_tier column");
        let modified = self
            .batch
            .column(param_col::MODIFIED_BY)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("modified_by column");

        CognitiveParameter {
            param_id: ids.value(idx).to_string(),
            category: categories.value(idx).to_string(),
            value_f64: if values_f64.is_null(idx) {
                None
            } else {
                Some(values_f64.value(idx))
            },
            value_str: if values_str.is_null(idx) {
                None
            } else {
                Some(values_str.value(idx).to_string())
            },
            min_bound: if min_bounds.is_null(idx) {
                None
            } else {
                Some(min_bounds.value(idx))
            },
            max_bound: if max_bounds.is_null(idx) {
                None
            } else {
                Some(max_bounds.value(idx))
            },
            autonomy_tier: AutonomyTier::from_u8(tiers.value(idx))
                .unwrap_or(AutonomyTier::CaptainOnly),
            modified_by: modified.value(idx).to_string(),
        }
    }

    fn rebuild_with_f64_update(
        &self,
        idx: usize,
        new_value: f64,
        new_modified_by: &str,
    ) -> Result<RecordBatch, ParamStoreError> {
        let n = self.batch.num_rows();

        // Copy all columns, modifying value_f64 and modified_by at `idx`
        let ids = self.param_id_col();
        let cats = self.category_col();
        let vals = self
            .batch
            .column(param_col::VALUE_F64)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("value_f64");
        let strs = self
            .batch
            .column(param_col::VALUE_STR)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("value_str");
        let mins = self
            .batch
            .column(param_col::MIN_BOUND)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("min_bound");
        let maxs = self
            .batch
            .column(param_col::MAX_BOUND)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("max_bound");
        let tiers = self
            .batch
            .column(param_col::AUTONOMY_TIER)
            .as_any()
            .downcast_ref::<UInt8Array>()
            .expect("autonomy_tier");
        let mods = self
            .batch
            .column(param_col::MODIFIED_BY)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("modified_by");

        let new_ids: Vec<&str> = (0..n).map(|i| ids.value(i)).collect();
        let new_cats: Vec<&str> = (0..n).map(|i| cats.value(i)).collect();
        let new_vals: Vec<Option<f64>> = (0..n)
            .map(|i| {
                if i == idx {
                    Some(new_value)
                } else if vals.is_null(i) {
                    None
                } else {
                    Some(vals.value(i))
                }
            })
            .collect();
        let new_strs: Vec<Option<&str>> = (0..n)
            .map(|i| {
                if strs.is_null(i) {
                    None
                } else {
                    Some(strs.value(i))
                }
            })
            .collect();
        let new_mins: Vec<Option<f64>> = (0..n)
            .map(|i| {
                if mins.is_null(i) {
                    None
                } else {
                    Some(mins.value(i))
                }
            })
            .collect();
        let new_maxs: Vec<Option<f64>> = (0..n)
            .map(|i| {
                if maxs.is_null(i) {
                    None
                } else {
                    Some(maxs.value(i))
                }
            })
            .collect();
        let new_tiers: Vec<u8> = (0..n).map(|i| tiers.value(i)).collect();
        let new_mods: Vec<&str> = (0..n)
            .map(|i| {
                if i == idx {
                    new_modified_by
                } else {
                    mods.value(i)
                }
            })
            .collect();

        Ok(RecordBatch::try_new(
            Arc::new(cognitive_params_schema()),
            vec![
                Arc::new(StringArray::from(new_ids)),
                Arc::new(StringArray::from(new_cats)),
                Arc::new(Float64Array::from(new_vals)),
                Arc::new(StringArray::from(new_strs)),
                Arc::new(Float64Array::from(new_mins)),
                Arc::new(Float64Array::from(new_maxs)),
                Arc::new(UInt8Array::from(new_tiers)),
                Arc::new(StringArray::from(new_mods)),
            ],
        )?)
    }

    fn remove_row(&self, idx: usize) -> Result<RecordBatch, ParamStoreError> {
        let n = self.batch.num_rows();
        if n == 0 {
            return Ok(self.batch.clone());
        }

        let ids = self.param_id_col();
        let cats = self.category_col();
        let vals = self
            .batch
            .column(param_col::VALUE_F64)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("value_f64");
        let strs = self
            .batch
            .column(param_col::VALUE_STR)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("value_str");
        let mins = self
            .batch
            .column(param_col::MIN_BOUND)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("min_bound");
        let maxs = self
            .batch
            .column(param_col::MAX_BOUND)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("max_bound");
        let tiers = self
            .batch
            .column(param_col::AUTONOMY_TIER)
            .as_any()
            .downcast_ref::<UInt8Array>()
            .expect("autonomy_tier");
        let mods = self
            .batch
            .column(param_col::MODIFIED_BY)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("modified_by");

        let keep: Vec<usize> = (0..n).filter(|&i| i != idx).collect();
        if keep.is_empty() {
            return Ok(RecordBatch::new_empty(Arc::new(cognitive_params_schema())));
        }

        let new_ids: Vec<&str> = keep.iter().map(|&i| ids.value(i)).collect();
        let new_cats: Vec<&str> = keep.iter().map(|&i| cats.value(i)).collect();
        let new_vals: Vec<Option<f64>> = keep
            .iter()
            .map(|&i| {
                if vals.is_null(i) {
                    None
                } else {
                    Some(vals.value(i))
                }
            })
            .collect();
        let new_strs: Vec<Option<&str>> = keep
            .iter()
            .map(|&i| {
                if strs.is_null(i) {
                    None
                } else {
                    Some(strs.value(i))
                }
            })
            .collect();
        let new_mins: Vec<Option<f64>> = keep
            .iter()
            .map(|&i| {
                if mins.is_null(i) {
                    None
                } else {
                    Some(mins.value(i))
                }
            })
            .collect();
        let new_maxs: Vec<Option<f64>> = keep
            .iter()
            .map(|&i| {
                if maxs.is_null(i) {
                    None
                } else {
                    Some(maxs.value(i))
                }
            })
            .collect();
        let new_tiers: Vec<u8> = keep.iter().map(|&i| tiers.value(i)).collect();
        let new_mods: Vec<&str> = keep.iter().map(|&i| mods.value(i)).collect();

        Ok(RecordBatch::try_new(
            Arc::new(cognitive_params_schema()),
            vec![
                Arc::new(StringArray::from(new_ids)),
                Arc::new(StringArray::from(new_cats)),
                Arc::new(Float64Array::from(new_vals)),
                Arc::new(StringArray::from(new_strs)),
                Arc::new(Float64Array::from(new_mins)),
                Arc::new(Float64Array::from(new_maxs)),
                Arc::new(UInt8Array::from(new_tiers)),
                Arc::new(StringArray::from(new_mods)),
            ],
        )?)
    }

    // ── Signal weight helpers ──────────────────────────────────────────

    /// Insert a signal weight entry.
    ///
    /// Uses convention: param_id = `sw.{dimension}.{path}`, category = `signal_weight`,
    /// bounds = [-1.5, 1.5], tier = Auto.
    /// The value is clipped to [-1.5, 1.5] before insertion.
    pub fn insert_signal_weight(
        &mut self,
        dimension: &str,
        path: &str,
        value: f64,
    ) -> Result<(), ParamStoreError> {
        let clipped = value.clamp(-1.5, 1.5);
        let param = CognitiveParameter {
            param_id: format!("sw.{dimension}.{path}"),
            category: category::SIGNAL_WEIGHT.to_string(),
            value_f64: Some(clipped),
            value_str: None,
            min_bound: Some(-1.5),
            max_bound: Some(1.5),
            autonomy_tier: AutonomyTier::Auto,
            modified_by: "init".to_string(),
        };
        self.insert(&param)
    }

    /// Get a signal weight by dimension and path.
    pub fn get_signal_weight(&self, dimension: &str, path: &str) -> Option<f64> {
        let param_id = format!("sw.{dimension}.{path}");
        self.get(&param_id).and_then(|p| p.value_f64)
    }

    /// List all signal weights as (dimension, path, value) tuples.
    pub fn signal_weights(&self) -> Vec<(String, String, f64)> {
        self.list(category::SIGNAL_WEIGHT)
            .into_iter()
            .filter_map(|p| {
                if let Some(rest) = p.param_id.strip_prefix("sw.") {
                    // Split on first '.' to get dimension, rest is path
                    if let Some(dot_pos) = rest.find('.') {
                        let dimension = rest[..dot_pos].to_string();
                        let path = rest[dot_pos + 1..].to_string();
                        return p.value_f64.map(|v| (dimension, path, v));
                    }
                }
                None
            })
            .collect()
    }

    /// Number of signal weight entries.
    pub fn signal_weight_count(&self) -> usize {
        self.list(category::SIGNAL_WEIGHT)
            .iter()
            .filter(|p| p.param_id.starts_with("sw."))
            .count()
    }

    // ── Loss coefficient helpers ────────────────────────────────────────

    /// Insert a loss coefficient parameter.
    ///
    /// Uses convention: param_id = `loss.{name}`, category = `loss_coefficient`,
    /// tier = Auto (Tier 1 — modifiable by HDD loop).
    pub fn insert_loss_coefficient(
        &mut self,
        name: &str,
        value: f64,
    ) -> Result<(), ParamStoreError> {
        let param = CognitiveParameter {
            param_id: format!("loss.{name}"),
            category: category::LOSS_COEFFICIENT.to_string(),
            value_f64: Some(value),
            value_str: None,
            min_bound: Some(0.0),
            max_bound: Some(2.0),
            autonomy_tier: AutonomyTier::Auto,
            modified_by: "init".to_string(),
        };
        self.insert(&param)
    }

    /// Get a loss coefficient by name.
    pub fn get_loss_coefficient(&self, name: &str) -> Option<f64> {
        let param_id = format!("loss.{name}");
        self.get(&param_id).and_then(|p| p.value_f64)
    }

    /// List all loss coefficients as (name, value) pairs.
    pub fn loss_coefficients(&self) -> Vec<(String, f64)> {
        self.list(category::LOSS_COEFFICIENT)
            .into_iter()
            .filter_map(|p| {
                let name = p.param_id.strip_prefix("loss.")?.to_string();
                p.value_f64.map(|v| (name, v))
            })
            .collect()
    }

    // ── Consolidation trigger helpers ───────────────────────────────────

    /// Insert a consolidation trigger parameter.
    ///
    /// Uses convention: param_id = `consol.{name}`, category = `consolidation_trigger`,
    /// tier = BeingApproved (Tier 2 — requires being confirmation before modification).
    pub fn insert_consolidation_trigger(
        &mut self,
        name: &str,
        value: f64,
    ) -> Result<(), ParamStoreError> {
        let param = CognitiveParameter {
            param_id: format!("consol.{name}"),
            category: category::CONSOLIDATION_TRIGGER.to_string(),
            value_f64: Some(value),
            value_str: None,
            min_bound: Some(0.0),
            max_bound: None,
            autonomy_tier: AutonomyTier::BeingApproved,
            modified_by: "init".to_string(),
        };
        self.insert(&param)
    }

    /// Get a consolidation trigger by name.
    pub fn get_consolidation_trigger(&self, name: &str) -> Option<f64> {
        let param_id = format!("consol.{name}");
        self.get(&param_id).and_then(|p| p.value_f64)
    }

    /// List all consolidation triggers as (name, value) pairs.
    pub fn consolidation_triggers(&self) -> Vec<(String, f64)> {
        self.list(category::CONSOLIDATION_TRIGGER)
            .into_iter()
            .filter_map(|p| {
                let name = p.param_id.strip_prefix("consol.")?.to_string();
                p.value_f64.map(|v| (name, v))
            })
            .collect()
    }
}

impl Default for CognitiveParameterStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Append a single parameter row to an existing RecordBatch.
fn append_row(
    batch: &RecordBatch,
    param: &CognitiveParameter,
) -> Result<RecordBatch, ParamStoreError> {
    let n = batch.num_rows();

    if n == 0 {
        // Build a single-row batch
        return Ok(RecordBatch::try_new(
            Arc::new(cognitive_params_schema()),
            vec![
                Arc::new(StringArray::from(vec![param.param_id.as_str()])),
                Arc::new(StringArray::from(vec![param.category.as_str()])),
                Arc::new(Float64Array::from(vec![param.value_f64])),
                Arc::new(StringArray::from(vec![param.value_str.as_deref()])),
                Arc::new(Float64Array::from(vec![param.min_bound])),
                Arc::new(Float64Array::from(vec![param.max_bound])),
                Arc::new(UInt8Array::from(vec![param.autonomy_tier as u8])),
                Arc::new(StringArray::from(vec![param.modified_by.as_str()])),
            ],
        )?);
    }

    // Extract existing columns and append
    let ids = batch
        .column(param_col::PARAM_ID)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("param_id");
    let cats = batch
        .column(param_col::CATEGORY)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("category");
    let vals = batch
        .column(param_col::VALUE_F64)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("value_f64");
    let strs = batch
        .column(param_col::VALUE_STR)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("value_str");
    let mins = batch
        .column(param_col::MIN_BOUND)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("min_bound");
    let maxs = batch
        .column(param_col::MAX_BOUND)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("max_bound");
    let tiers = batch
        .column(param_col::AUTONOMY_TIER)
        .as_any()
        .downcast_ref::<UInt8Array>()
        .expect("autonomy_tier");
    let mods = batch
        .column(param_col::MODIFIED_BY)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("modified_by");

    let mut new_ids: Vec<&str> = (0..n).map(|i| ids.value(i)).collect();
    new_ids.push(&param.param_id);

    let mut new_cats: Vec<&str> = (0..n).map(|i| cats.value(i)).collect();
    new_cats.push(&param.category);

    let mut new_vals: Vec<Option<f64>> = (0..n)
        .map(|i| {
            if vals.is_null(i) {
                None
            } else {
                Some(vals.value(i))
            }
        })
        .collect();
    new_vals.push(param.value_f64);

    let value_str_owned: Vec<Option<String>> = (0..n)
        .map(|i| {
            if strs.is_null(i) {
                None
            } else {
                Some(strs.value(i).to_string())
            }
        })
        .chain(std::iter::once(param.value_str.clone()))
        .collect();
    let new_strs: Vec<Option<&str>> = value_str_owned.iter().map(|s| s.as_deref()).collect();

    let mut new_mins: Vec<Option<f64>> = (0..n)
        .map(|i| {
            if mins.is_null(i) {
                None
            } else {
                Some(mins.value(i))
            }
        })
        .collect();
    new_mins.push(param.min_bound);

    let mut new_maxs: Vec<Option<f64>> = (0..n)
        .map(|i| {
            if maxs.is_null(i) {
                None
            } else {
                Some(maxs.value(i))
            }
        })
        .collect();
    new_maxs.push(param.max_bound);

    let mut new_tiers: Vec<u8> = (0..n).map(|i| tiers.value(i)).collect();
    new_tiers.push(param.autonomy_tier as u8);

    let mut new_mods: Vec<&str> = (0..n).map(|i| mods.value(i)).collect();
    new_mods.push(&param.modified_by);

    Ok(RecordBatch::try_new(
        Arc::new(cognitive_params_schema()),
        vec![
            Arc::new(StringArray::from(new_ids)),
            Arc::new(StringArray::from(new_cats)),
            Arc::new(Float64Array::from(new_vals)),
            Arc::new(StringArray::from(new_strs)),
            Arc::new(Float64Array::from(new_mins)),
            Arc::new(Float64Array::from(new_maxs)),
            Arc::new(UInt8Array::from(new_tiers)),
            Arc::new(StringArray::from(new_mods)),
        ],
    )?)
}

// ── Default parameters ─────────────────────────────────────────────────

/// Build a store populated with default cognitive parameters.
///
/// This captures the current hardcoded values from:
/// - `nusy-signal-fusion::weight_learner::LearningConfig::default()`
/// - Signal fusion softmax temperature
/// - Schema match thresholds
///
/// Each parameter is classified by category and autonomy tier.
pub fn default_cognitive_params() -> CognitiveParameterStore {
    let mut store = CognitiveParameterStore::new();

    // ── Learning config (from weight_learner.rs) ───────────────────
    let lc = |id: &str, val: f64, min: f64, max: f64| CognitiveParameter {
        param_id: format!("learning.{id}"),
        category: category::LEARNING_CONFIG.to_string(),
        value_f64: Some(val),
        value_str: None,
        min_bound: Some(min),
        max_bound: Some(max),
        autonomy_tier: AutonomyTier::Auto,
        modified_by: "init".to_string(),
    };

    let params = vec![
        lc("initial_learning_rate", 0.1, 0.001, 1.0),
        lc("decayed_learning_rate", 0.01, 0.0001, 0.5),
        lc("error_spike_learning_rate", 0.05, 0.001, 0.5),
        lc("decay_after_decisions", 100.0, 10.0, 10000.0),
        lc("weight_decay_lambda", 0.001, 0.0, 0.1),
        lc("max_delta_per_step", 0.1, 0.01, 1.0),
        lc("min_weight", -1.5, -10.0, 0.0),
        lc("max_weight", 1.5, 0.0, 10.0),
        lc("hebbian_lr_multiplier", 0.5, 0.0, 2.0),
        lc("error_spike_window", 20.0, 5.0, 100.0),
        lc("error_spike_threshold", 0.5, 0.1, 1.0),
    ];

    for p in &params {
        store
            .insert(p)
            .expect("default learning config insert should not fail");
    }

    // ── Signal fusion thresholds ───────────────────────────────────
    let th = |id: &str, val: f64, min: f64, max: f64| CognitiveParameter {
        param_id: format!("fusion.{id}"),
        category: category::THRESHOLD.to_string(),
        value_f64: Some(val),
        value_str: None,
        min_bound: Some(min),
        max_bound: Some(max),
        autonomy_tier: AutonomyTier::Auto,
        modified_by: "init".to_string(),
    };

    let thresholds = vec![
        th("softmax_temperature", 1.0, 0.01, 10.0),
        th("min_signal_strength", 0.01, 0.0, 1.0),
        th("refuse_low_coverage_threshold", 0.7, 0.0, 1.0),
    ];

    for p in &thresholds {
        store
            .insert(p)
            .expect("default threshold insert should not fail");
    }

    // ── Schema match thresholds ─────────────────────────────────────
    // Tier 1 (Auto) for thresholds HDD can tune; Tier 2 for structural.
    let sm_auto = |id: &str, val: f64, min: f64, max: f64| CognitiveParameter {
        param_id: format!("schema_match.{id}"),
        category: category::THRESHOLD.to_string(),
        value_f64: Some(val),
        value_str: None,
        min_bound: Some(min),
        max_bound: Some(max),
        autonomy_tier: AutonomyTier::Auto,
        modified_by: "init".to_string(),
    };
    let sm_being = |id: &str, val: f64, min: f64, max: f64| CognitiveParameter {
        param_id: format!("schema_match.{id}"),
        category: category::THRESHOLD.to_string(),
        value_f64: Some(val),
        value_str: None,
        min_bound: Some(min),
        max_bound: Some(max),
        autonomy_tier: AutonomyTier::BeingApproved,
        modified_by: "init".to_string(),
    };

    let schema_thresholds = vec![
        sm_auto("assimilate_threshold", 0.7, 0.0, 1.0),
        sm_auto("accommodate_threshold", 0.3, 0.0, 1.0),
        sm_being("novelty_high_threshold", 0.8, 0.0, 1.0),
        sm_auto("assimilation_boost", 0.05, 0.0, 0.5),
        sm_auto("coverage_saturation", 5.0, 1.0, 50.0),
    ];

    for p in &schema_thresholds {
        store
            .insert(p)
            .expect("default schema threshold insert should not fail");
    }

    // ── Consolidation triggers (Tier 2) ────────────────────────────
    let tr = |id: &str, val: f64, min: f64, max: f64| CognitiveParameter {
        param_id: format!("consolidation.{id}"),
        category: category::TRIGGER.to_string(),
        value_f64: Some(val),
        value_str: None,
        min_bound: Some(min),
        max_bound: Some(max),
        autonomy_tier: AutonomyTier::BeingApproved,
        modified_by: "init".to_string(),
    };

    let triggers = vec![
        tr("min_triples_for_training", 200.0, 10.0, 10000.0),
        tr("consolidation_cycle_interval_hours", 24.0, 1.0, 168.0),
        tr("kl_divergence_budget", 0.5, 0.01, 5.0),
    ];

    for p in &triggers {
        store
            .insert(p)
            .expect("default trigger insert should not fail");
    }

    // ── CQ thresholds (from KBDD, training, safety) ────────────────
    // Tier 1 (Auto) for HDD-tunable thresholds; Tier 3 (CaptainOnly) for
    // safety-critical provenance gate.
    let cq_auto = |id: &str, val: f64, min: f64, max: f64| CognitiveParameter {
        param_id: format!("cq.{id}"),
        category: category::THRESHOLD.to_string(),
        value_f64: Some(val),
        value_str: None,
        min_bound: Some(min),
        max_bound: Some(max),
        autonomy_tier: AutonomyTier::Auto,
        modified_by: "init".to_string(),
    };
    let cq_captain = |id: &str, val: f64, min: f64, max: f64| CognitiveParameter {
        param_id: format!("cq.{id}"),
        category: category::THRESHOLD.to_string(),
        value_f64: Some(val),
        value_str: None,
        min_bound: Some(min),
        max_bound: Some(max),
        autonomy_tier: AutonomyTier::CaptainOnly,
        modified_by: "init".to_string(),
    };

    let cq_thresholds = vec![
        // From crates/nusy-kbdd/src/depth_policy.rs:64-72
        cq_auto("high_threshold", 0.8, 0.5, 1.0),
        cq_auto("low_threshold", 0.5, 0.1, 0.8),
        // From crates/nusy-kbdd/src/loop_engine.rs:59-62
        cq_auto("convergence_threshold", 0.02, 0.001, 0.1),
        // From crates/nusy-training/src/training_loop.rs:48
        cq_auto("training_threshold", 0.65, 0.3, 1.0),
        // From crates/nusy-being/src/being.rs:96-103 (safety domains)
        cq_captain("safety_domain_threshold", 0.8, 0.5, 1.0),
        // CONCERN-6: provenance_validity >= 95% as hard gate (V15 external review)
        cq_captain("provenance_validity_threshold", 0.95, 0.8, 1.0),
    ];

    for p in &cq_thresholds {
        store
            .insert(p)
            .expect("default CQ threshold insert should not fail");
    }

    // ── Signal weights (merged from default_signal_weights) ─────────
    let sw_store = default_signal_weights();
    for p in sw_store.list_all() {
        store
            .insert(&p)
            .expect("default signal weight insert should not fail");
    }

    // ── Loss coefficients (from DualLossConfig defaults) ─────────
    // Values sourced from crates/nusy-training/src/dual_loss.rs::DualLossConfig::default()
    store
        .insert_loss_coefficient("alpha_lm", 1.0)
        .expect("default loss coefficient insert");
    store
        .insert_loss_coefficient("alpha_rel", 0.1)
        .expect("default loss coefficient insert");
    store
        .insert_loss_coefficient("alpha_causal", 0.05)
        .expect("default loss coefficient insert");
    store
        .insert_loss_coefficient("rel_warmup_fraction", 0.1)
        .expect("default loss coefficient insert");
    store
        .insert_loss_coefficient("causal_warmup_fraction", 0.2)
        .expect("default loss coefficient insert");

    // ── Consolidation triggers (from ConsolidationConfig + TrainingTriggerConfig defaults) ──
    // Values sourced from crates/nusy-dual-store/src/consolidation.rs::ConsolidationConfig::default()
    // and crates/nusy-consolidation/src/trigger.rs::TrainingTriggerConfig::default()
    store
        .insert_consolidation_trigger("dedup_threshold", 0.95)
        .expect("default consolidation trigger insert");
    store
        .insert_consolidation_trigger("max_triples_per_cycle", 500.0)
        .expect("default consolidation trigger insert");
    store
        .insert_consolidation_trigger("min_promoted_triples", 50.0)
        .expect("default consolidation trigger insert");
    store
        .insert_consolidation_trigger("training_threshold", 200.0)
        .expect("default consolidation trigger insert");
    store
        .insert_consolidation_trigger("max_pairs_per_batch", 500.0)
        .expect("default consolidation trigger insert");

    store
}

/// Build a store populated with default signal fusion weights.
///
/// Ports the 200+ weight entries from `nusy-signal-fusion::weights::default_weight_entries()`.
/// Each entry is stored as category="signal_weight" with param_id="sw.{dim}.{path}".
pub fn default_signal_weights() -> CognitiveParameterStore {
    let mut store = CognitiveParameterStore::new();

    let mut add = |dim: &str, path: &str, weight: f64| {
        store
            .insert_signal_weight(dim, path, weight)
            .expect("default signal weight insert should not fail");
    };

    // Fractal confidence/ambiguity
    add("fractal_confidence", "FAST", 0.9);
    add("fractal_confidence", "STANDARD", 0.3);
    add("fractal_confidence", "DEEP", -0.5);
    add("fractal_confidence", "REFUSE_LOW_COVERAGE", -0.5);
    add("fractal_ambiguity", "FAST", -0.5);
    add("fractal_ambiguity", "STANDARD", 0.2);
    add("fractal_ambiguity", "DEEP", 0.8);

    // Novelty
    add("novelty_surprise", "FAST", -0.3);
    add("novelty_surprise", "STANDARD", 0.1);
    add("novelty_surprise", "DEEP", 0.7);
    add("novelty_surprise", "FAST_LEARNING", 0.5);

    // FOV
    add("fov_temperature", "FAST", 0.4);
    add("fov_temperature", "STANDARD", 0.2);
    add("fov_temperature", "DEEP", -0.1);

    // Emotion
    add("emotion_confusion", "FAST", -0.2);
    add("emotion_confusion", "STANDARD", 0.3);
    add("emotion_confusion", "DEEP", 0.5);

    // State
    add("state_importance", "FAST", -0.1);
    add("state_importance", "STANDARD", 0.2);
    add("state_importance", "DEEP", 0.5);

    // Input type
    add("is_document", "TRAINING", 1.0);
    add("urgency", "FAST_LEARNING", 0.8);

    // Provenance (EXP-869)
    add("provenance_support", "FAST", 0.8);
    add("provenance_support", "STANDARD", 0.2);
    add("provenance_support", "DEEP", -0.3);
    add("provenance_coverage", "FAST", 0.5);
    add("provenance_coverage", "STANDARD", 0.3);
    add("provenance_coverage", "DEEP", -0.2);
    add("provenance_coverage", "FAST_LEARNING", -0.4);

    // Action (EXP-879)
    add("action_pending", "FAST", -0.2);
    add("action_pending", "STANDARD", 0.4);
    add("action_pending", "DEEP", 0.3);
    add("action_intent_level", "FAST", 0.6);
    add("action_intent_level", "STANDARD", 0.1);
    add("action_intent_level", "DEEP", -0.3);
    add("action_coverage", "FAST", 0.5);
    add("action_coverage", "STANDARD", 0.2);
    add("action_coverage", "DEEP", -0.2);

    // KBDD (EXP-899)
    add("kbdd_coverage", "FAST", 0.7);
    add("kbdd_coverage", "STANDARD", 0.1);
    add("kbdd_coverage", "DEEP", -0.4);
    add("kbdd_coverage", "FAST_LEARNING", -0.3);
    add("kbdd_gap_density", "FAST", -0.5);
    add("kbdd_gap_density", "STANDARD", 0.2);
    add("kbdd_gap_density", "DEEP", 0.6);
    add("kbdd_gap_density", "TRAINING", 0.4);
    add("kbdd_gap_density", "CRYSTALLIZE", 0.6);

    // Query understanding (EXP-951)
    add("query_complexity", "FAST", -0.4);
    add("query_complexity", "STANDARD", 0.2);
    add("query_complexity", "DEEP", 0.6);
    add("query_expected_depth", "FAST", -0.3);
    add("query_expected_depth", "STANDARD", 0.1);
    add("query_expected_depth", "DEEP", 0.5);
    add("query_domain_match", "FAST", 0.3);
    add("query_domain_match", "STANDARD", 0.1);
    add("query_domain_match", "REFUSE_LOW_COVERAGE", -0.3);

    // Entity grounding (EXP-932)
    add("entity_gap", "REFUSE_LOW_COVERAGE", 0.9);
    add("entity_gap", "CRYSTALLIZE", 0.3);
    add("entity_gap", "FAST", -0.4);
    add("entity_gap", "STANDARD", -0.2);
    add("coverage_gap", "REFUSE_LOW_COVERAGE", 0.7);
    add("coverage_gap", "CRYSTALLIZE", 0.4);
    add("coverage_gap", "FAST", -0.3);
    add("prose_available", "CRYSTALLIZE", 0.8);
    add("prose_available", "REFUSE_LOW_COVERAGE", -0.7);
    add("entity_grounding", "FAST", 0.3);
    add("entity_grounding", "REFUSE_LOW_COVERAGE", -0.5);

    // Competency (EXP-958)
    add("competency_match", "FAST", 0.2);
    add("competency_match", "STANDARD", 0.3);
    add("competency_match", "REFUSE_LOW_COVERAGE", -0.6);
    add("competency_expected_quality", "FAST", 0.3);
    add("competency_expected_quality", "STANDARD", 0.1);
    add("competency_expected_quality", "DEEP", -0.2);

    // Pattern (EXP-958)
    add("pattern_applicability", "FAST", -0.3);
    add("pattern_applicability", "STANDARD", 0.1);
    add("pattern_applicability", "DEEP", 0.5);
    add("pattern_structured_reasoning", "FAST", -0.2);
    add("pattern_structured_reasoning", "DEEP", 0.4);

    // Goal (EXP-958)
    add("goal_alignment", "FAST", -0.1);
    add("goal_alignment", "STANDARD", 0.4);
    add("goal_alignment", "REFUSE_LOW_COVERAGE", -0.3);
    add("goal_response_priority", "STANDARD", 0.3);
    add("goal_response_priority", "DEEP", 0.2);

    // Conversation (EXP-965)
    add("conv_turn_depth", "STANDARD", 0.2);
    add("conv_turn_depth", "DEEP", 0.3);
    add("conv_entity_continuity", "FAST", 0.3);
    add("conv_entity_continuity", "STANDARD", 0.1);
    add("conv_entity_continuity", "REFUSE_LOW_COVERAGE", -0.3);
    add("conv_topic_repetition", "DEEP", 0.4);
    add("conv_topic_repetition", "STANDARD", 0.1);
    add("conv_is_followup", "STANDARD", 0.3);
    add("conv_is_followup", "FAST", 0.1);
    add("conv_is_followup", "REFUSE_LOW_COVERAGE", -0.3);

    // Growth awareness (EXP-995)
    add("growth_depth", "FAST", -0.3);
    add("growth_depth", "STANDARD", 0.2);
    add("growth_depth", "DEEP", 0.7);
    add("growth_richness", "FAST", -0.2);
    add("growth_richness", "STANDARD", 0.4);
    add("growth_richness", "DEEP", 0.5);
    add("growth_layers", "FAST", -0.1);
    add("growth_layers", "STANDARD", 0.5);
    add("growth_layers", "DEEP", 0.3);
    add("growth_connections", "FAST", -0.2);
    add("growth_connections", "STANDARD", 0.3);
    add("growth_connections", "DEEP", 0.5);

    // LLM explore (EXP-1082)
    add("llm_explore_confidence", "STANDARD", 0.3);
    add("llm_explore_confidence", "DEEP", 0.4);
    add("llm_explore_confidence", "CRYSTALLIZE", 0.3);
    add("llm_structure_signal", "STANDARD", 0.2);
    add("llm_structure_signal", "DEEP", 0.5);
    add("llm_domain_signal", "STANDARD", 0.3);
    add("llm_domain_signal", "DEEP", 0.3);
    add("llm_relationship_richness", "STANDARD", 0.3);
    add("llm_relationship_richness", "DEEP", 0.4);
    add("llm_expansion_signal", "STANDARD", 0.2);
    add("llm_expansion_signal", "DEEP", 0.3);
    add("llm_expansion_signal", "CRYSTALLIZE", 0.2);
    add("llm_unknown_structure", "DEEP", 0.5);
    add("llm_unknown_structure", "CRYSTALLIZE", 0.4);

    // Schema match (EXP-1218)
    add("schema_match", "FAST", 0.5);
    add("schema_match", "STANDARD", 0.2);
    add("schema_match", "DEEP", -0.3);
    add("schema_match", "REFUSE_LOW_COVERAGE", -0.3);
    add("schema_novelty", "FAST", -0.4);
    add("schema_novelty", "STANDARD", 0.1);
    add("schema_novelty", "DEEP", 0.6);
    add("schema_novelty", "FAST_LEARNING", 0.4);
    add("schema_novelty", "TRAINING", 0.3);

    // Tool use (EXP-1133)
    add("tool_needed", "TOOL_USE", 0.9);
    add("tool_needed", "FAST", -0.2);
    add("tool_needed", "STANDARD", -0.1);
    add("tool_needed", "REFUSE_LOW_COVERAGE", -0.7);
    add("tool_computation", "TOOL_USE", 0.7);
    add("tool_computation", "DEEP", -0.3);
    add("tool_action", "TOOL_USE", 0.6);
    add("tool_match", "TOOL_USE", 0.8);
    add("tool_match", "REFUSE_LOW_COVERAGE", -0.5);
    add("tool_count", "TOOL_USE", 0.3);
    add("tool_auto_approve", "TOOL_USE", 0.4);
    add("tool_auto_approve", "FAST", 0.2);
    add("tool_safety_score", "TOOL_USE", 0.5);
    add("tool_sandbox_ok", "TOOL_USE", 0.3);

    // Embedding (EXP-1169, EXP-1170)
    add("embedding_computation", "TOOL_USE", 0.8);
    add("embedding_computation", "FAST", -0.2);
    add("embedding_computation", "REFUSE_LOW_COVERAGE", -0.5);
    add("embedding_mean_similarity", "TOOL_USE", 0.3);
    add("embedding_tool_match", "TOOL_USE", 0.8);
    add("embedding_tool_match", "FAST", -0.2);
    add("embedding_tool_match", "REFUSE_LOW_COVERAGE", -0.5);
    add("embedding_tool_mean", "TOOL_USE", 0.3);

    // Recency
    add("recency_weight", "FAST", 0.3);
    add("recency_weight", "STANDARD", 0.1);

    // Working Memory (EX-3239, EX-3380)
    add("wm_repetition", "FAST", 0.4);
    add("wm_repetition", "STANDARD", -0.1);
    add("wm_topic_continuity", "STANDARD", 0.3);
    add("wm_topic_continuity", "DEEP", 0.2);

    // Calibration / Competence (EX-3437)
    add("calibration_accuracy", "FAST", 0.6);
    add("calibration_accuracy", "STANDARD", 0.4);
    add("calibration_accuracy", "REFUSE_LOW_COVERAGE", -2.5);
    add("calibration_confidence", "FAST", 0.3);
    add("calibration_confidence", "DEEP", -0.2);
    add("calibration_confidence", "REFUSE_LOW_COVERAGE", -0.6);
    add("certification_gap", "FAST", -0.5);
    add("certification_gap", "STANDARD", 0.2);
    add("certification_gap", "REFUSE_LOW_COVERAGE", 0.3);
    add("calibration_maturity", "FAST", 0.2);
    add("calibration_maturity", "REFUSE_LOW_COVERAGE", -0.3);

    store
}

// ── First-run initialization ─────────────────────────────────────────────

/// Load cognitive params from Parquet, or create defaults and persist them.
///
/// This is the correct entry point for `Being::new()`: on first run, the
/// Parquet file won't exist, so defaults are created and saved. On subsequent
/// runs, the previously persisted params (potentially modified by the HDD
/// loop) are loaded.
pub fn init_cognitive_params(
    path: &std::path::Path,
) -> Result<CognitiveParameterStore, ParamStoreError> {
    match load_params_from_parquet(path)? {
        Some(store) => Ok(store),
        None => {
            let store = default_cognitive_params();
            save_params_to_parquet(&store, path)?;
            Ok(store)
        }
    }
}

// ── Parquet persistence (Phase 4: Graph-Native Git Integration) ────────

/// Save the parameter store to a Parquet file.
///
/// Uses the `nusy-arrow-git` save_named_batches pattern: the store is
/// serialized as a single Parquet file in the `self` namespace directory.
pub fn save_params_to_parquet(
    store: &CognitiveParameterStore,
    path: &std::path::Path,
) -> Result<(), ParamStoreError> {
    use crate::parquet_atomic::write_parquet_atomic;
    use parquet::arrow::ArrowWriter;

    let batch = store.snapshot();
    let schema = Arc::new(cognitive_params_schema());

    // CH-4400: atomic write so a freeze mid-snapshot can't leave a
    // 0-byte cognitive_params.parquet that fails the next being load.
    write_parquet_atomic(path, |file| {
        let mut writer = ArrowWriter::try_new(file, schema, None)
            .map_err(|e| std::io::Error::other(format!("cognitive_params writer init: {e}")))?;
        if batch.num_rows() > 0 {
            writer
                .write(batch)
                .map_err(|e| std::io::Error::other(format!("cognitive_params write: {e}")))?;
        }
        writer
            .close()
            .map_err(|e| std::io::Error::other(format!("cognitive_params close: {e}")))?;
        Ok(())
    })
    .map_err(|e| ParamStoreError::InvalidSchema(format!("atomic save: {e}")))?;

    Ok(())
}

/// Load a parameter store from a Parquet file.
///
/// Returns `None` if the file doesn't exist (first-run case).
pub fn load_params_from_parquet(
    path: &std::path::Path,
) -> Result<Option<CognitiveParameterStore>, ParamStoreError> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    if !path.exists() {
        return Ok(None);
    }

    let file = std::fs::File::open(path)
        .map_err(|e| ParamStoreError::InvalidSchema(format!("failed to open file: {e}")))?;

    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    let mut batches: Vec<RecordBatch> = Vec::new();
    for batch_result in reader {
        batches.push(batch_result?);
    }

    if batches.is_empty() {
        return Ok(Some(CognitiveParameterStore::new()));
    }

    // Concatenate all batches (typically just one)
    if batches.len() == 1 {
        return Ok(Some(CognitiveParameterStore::from_batch(
            batches.into_iter().next().unwrap(),
        )?));
    }
    let schema = batches[0].schema();
    let combined = arrow::compute::concat_batches(&schema, &batches)?;
    Ok(Some(CognitiveParameterStore::from_batch(combined)?))
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_has_correct_columns() {
        let schema = cognitive_params_schema();
        assert_eq!(schema.fields().len(), 8);
        assert_eq!(schema.field(param_col::PARAM_ID).name(), "param_id");
        assert_eq!(schema.field(param_col::CATEGORY).name(), "category");
        assert_eq!(schema.field(param_col::VALUE_F64).name(), "value_f64");
        assert_eq!(schema.field(param_col::VALUE_STR).name(), "value_str");
        assert_eq!(schema.field(param_col::MIN_BOUND).name(), "min_bound");
        assert_eq!(schema.field(param_col::MAX_BOUND).name(), "max_bound");
        assert_eq!(
            schema.field(param_col::AUTONOMY_TIER).name(),
            "autonomy_tier"
        );
        assert_eq!(schema.field(param_col::MODIFIED_BY).name(), "modified_by");
    }

    #[test]
    fn test_empty_store() {
        let store = CognitiveParameterStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        assert!(store.get("anything").is_none());
        assert!(store.list(category::SIGNAL_WEIGHT).is_empty());
    }

    #[test]
    fn test_insert_and_get() {
        let mut store = CognitiveParameterStore::new();
        let param = CognitiveParameter {
            param_id: "signal.novelty.weight".into(),
            category: category::SIGNAL_WEIGHT.into(),
            value_f64: Some(0.15),
            value_str: None,
            min_bound: Some(0.0),
            max_bound: Some(1.0),
            autonomy_tier: AutonomyTier::Auto,
            modified_by: "init".into(),
        };
        store.insert(&param).unwrap();

        assert_eq!(store.len(), 1);
        let got = store.get("signal.novelty.weight").unwrap();
        assert_eq!(got.value_f64, Some(0.15));
        assert_eq!(got.category, category::SIGNAL_WEIGHT);
        assert_eq!(got.autonomy_tier, AutonomyTier::Auto);
    }

    #[test]
    fn test_insert_replaces_existing() {
        let mut store = CognitiveParameterStore::new();
        let p1 = CognitiveParameter {
            param_id: "test.param".into(),
            category: category::THRESHOLD.into(),
            value_f64: Some(1.0),
            value_str: None,
            min_bound: None,
            max_bound: None,
            autonomy_tier: AutonomyTier::Auto,
            modified_by: "init".into(),
        };
        store.insert(&p1).unwrap();
        assert_eq!(store.len(), 1);

        let p2 = CognitiveParameter {
            value_f64: Some(2.0),
            modified_by: "hdd_loop".into(),
            ..p1.clone()
        };
        store.insert(&p2).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.get("test.param").unwrap().value_f64, Some(2.0));
        assert_eq!(store.get("test.param").unwrap().modified_by, "hdd_loop");
    }

    #[test]
    fn test_set_with_bounds_checking() {
        let mut store = CognitiveParameterStore::new();
        let param = CognitiveParameter {
            param_id: "bounded.param".into(),
            category: category::THRESHOLD.into(),
            value_f64: Some(0.5),
            value_str: None,
            min_bound: Some(0.0),
            max_bound: Some(1.0),
            autonomy_tier: AutonomyTier::Auto,
            modified_by: "init".into(),
        };
        store.insert(&param).unwrap();

        // Valid update
        store
            .set("bounded.param", 0.8, "hdd_loop", AutonomyTier::Auto)
            .unwrap();
        assert_eq!(store.get("bounded.param").unwrap().value_f64, Some(0.8));

        // Out of bounds — too high
        let err = store
            .set("bounded.param", 1.5, "hdd_loop", AutonomyTier::Auto)
            .unwrap_err();
        assert!(matches!(err, ParamStoreError::OutOfBounds { .. }));

        // Out of bounds — too low
        let err = store
            .set("bounded.param", -0.1, "hdd_loop", AutonomyTier::Auto)
            .unwrap_err();
        assert!(matches!(err, ParamStoreError::OutOfBounds { .. }));

        // Value unchanged after failed set
        assert_eq!(store.get("bounded.param").unwrap().value_f64, Some(0.8));
    }

    #[test]
    fn test_set_not_found() {
        let mut store = CognitiveParameterStore::new();
        let err = store
            .set("nonexistent", 1.0, "test", AutonomyTier::Auto)
            .unwrap_err();
        assert!(matches!(err, ParamStoreError::NotFound(_)));
    }

    #[test]
    fn test_autonomy_tier_enforcement() {
        let mut store = CognitiveParameterStore::new();
        let param = CognitiveParameter {
            param_id: "captain.safety_rule".into(),
            category: category::THRESHOLD.into(),
            value_f64: Some(0.5),
            value_str: None,
            min_bound: Some(0.0),
            max_bound: Some(1.0),
            autonomy_tier: AutonomyTier::CaptainOnly,
            modified_by: "init".into(),
        };
        store.insert(&param).unwrap();

        // Tier 1 (Auto) cannot modify Tier 3 (CaptainOnly)
        let err = store
            .set("captain.safety_rule", 0.8, "hdd_loop", AutonomyTier::Auto)
            .unwrap_err();
        assert!(matches!(err, ParamStoreError::TierViolation { .. }));

        // Tier 2 (BeingApproved) cannot modify Tier 3
        let err = store
            .set(
                "captain.safety_rule",
                0.8,
                "being",
                AutonomyTier::BeingApproved,
            )
            .unwrap_err();
        assert!(matches!(err, ParamStoreError::TierViolation { .. }));

        // Tier 3 (CaptainOnly) can modify Tier 3
        store
            .set(
                "captain.safety_rule",
                0.8,
                "captain",
                AutonomyTier::CaptainOnly,
            )
            .unwrap();
        assert_eq!(
            store.get("captain.safety_rule").unwrap().value_f64,
            Some(0.8)
        );
    }

    #[test]
    fn test_list_by_category() {
        let mut store = CognitiveParameterStore::new();

        let sw = |id: &str, val: f64| CognitiveParameter {
            param_id: id.into(),
            category: category::SIGNAL_WEIGHT.into(),
            value_f64: Some(val),
            value_str: None,
            min_bound: None,
            max_bound: None,
            autonomy_tier: AutonomyTier::Auto,
            modified_by: "init".into(),
        };

        store.insert(&sw("w1", 0.1)).unwrap();
        store.insert(&sw("w2", 0.2)).unwrap();
        store
            .insert(&CognitiveParameter {
                param_id: "t1".into(),
                category: category::THRESHOLD.into(),
                value_f64: Some(0.5),
                value_str: None,
                min_bound: None,
                max_bound: None,
                autonomy_tier: AutonomyTier::Auto,
                modified_by: "init".into(),
            })
            .unwrap();

        let weights = store.list(category::SIGNAL_WEIGHT);
        assert_eq!(weights.len(), 2);

        let thresholds = store.list(category::THRESHOLD);
        assert_eq!(thresholds.len(), 1);

        let all = store.list_all();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_default_params_populated() {
        let store = default_cognitive_params();
        assert!(!store.is_empty());

        // Verify learning config params exist
        let lr = store.get("learning.initial_learning_rate").unwrap();
        assert_eq!(lr.value_f64, Some(0.1));
        assert_eq!(lr.category, category::LEARNING_CONFIG);
        assert_eq!(lr.autonomy_tier, AutonomyTier::Auto);
        assert_eq!(lr.min_bound, Some(0.001));
        assert_eq!(lr.max_bound, Some(1.0));

        // Verify threshold params
        let softmax = store.get("fusion.softmax_temperature").unwrap();
        assert_eq!(softmax.value_f64, Some(1.0));

        // Verify schema match params (Tier 1 Auto for HDD-tunable thresholds)
        let assim = store.get("schema_match.assimilate_threshold").unwrap();
        assert_eq!(assim.autonomy_tier, AutonomyTier::Auto);
        // novelty_high_threshold stays Tier 2
        let novelty = store.get("schema_match.novelty_high_threshold").unwrap();
        assert_eq!(novelty.autonomy_tier, AutonomyTier::BeingApproved);

        // Verify trigger params
        let min_triples = store.get("consolidation.min_triples_for_training").unwrap();
        assert_eq!(min_triples.value_f64, Some(200.0));
        assert_eq!(min_triples.autonomy_tier, AutonomyTier::BeingApproved);
    }

    #[test]
    fn test_default_params_bounds_enforced() {
        let mut store = default_cognitive_params();

        // Try to set learning rate out of bounds
        let err = store
            .set(
                "learning.initial_learning_rate",
                5.0,
                "test",
                AutonomyTier::Auto,
            )
            .unwrap_err();
        assert!(matches!(err, ParamStoreError::OutOfBounds { .. }));

        // Valid update within bounds
        store
            .set(
                "learning.initial_learning_rate",
                0.5,
                "hdd_loop",
                AutonomyTier::Auto,
            )
            .unwrap();
        assert_eq!(
            store
                .get("learning.initial_learning_rate")
                .unwrap()
                .value_f64,
            Some(0.5)
        );
    }

    #[test]
    fn test_from_batch_validates_schema() {
        // Wrong number of columns
        let bad_schema = Arc::new(Schema::new(vec![
            Field::new("a", DataType::Utf8, false),
            Field::new("b", DataType::Utf8, false),
        ]));
        let bad_batch = RecordBatch::try_new(
            bad_schema,
            vec![
                Arc::new(StringArray::from(vec!["x"])),
                Arc::new(StringArray::from(vec!["y"])),
            ],
        )
        .unwrap();

        let err = CognitiveParameterStore::from_batch(bad_batch).unwrap_err();
        assert!(matches!(err, ParamStoreError::InvalidSchema(_)));
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let mut store = CognitiveParameterStore::new();
        let param = CognitiveParameter {
            param_id: "test.roundtrip".into(),
            category: category::THRESHOLD.into(),
            value_f64: Some(0.42),
            value_str: Some("test_value".into()),
            min_bound: Some(0.0),
            max_bound: Some(1.0),
            autonomy_tier: AutonomyTier::BeingApproved,
            modified_by: "test".into(),
        };
        store.insert(&param).unwrap();

        // Snapshot and recreate
        let batch = store.snapshot().clone();
        let store2 = CognitiveParameterStore::from_batch(batch).unwrap();
        let got = store2.get("test.roundtrip").unwrap();
        assert_eq!(got.value_f64, Some(0.42));
        assert_eq!(got.value_str.as_deref(), Some("test_value"));
        assert_eq!(got.autonomy_tier, AutonomyTier::BeingApproved);
    }

    #[test]
    fn test_parquet_roundtrip() {
        let store = default_cognitive_params();
        let original_count = store.len();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("self_params.parquet");

        // Save
        save_params_to_parquet(&store, &path).unwrap();
        assert!(path.exists());

        // Load
        let loaded = load_params_from_parquet(&path).unwrap().unwrap();
        assert_eq!(loaded.len(), original_count);

        // Verify a specific parameter survived
        let lr = loaded.get("learning.initial_learning_rate").unwrap();
        assert_eq!(lr.value_f64, Some(0.1));
        assert_eq!(lr.min_bound, Some(0.001));
        assert_eq!(lr.autonomy_tier, AutonomyTier::Auto);
    }

    #[test]
    fn test_parquet_load_missing_file() {
        let result =
            load_params_from_parquet(std::path::Path::new("/nonexistent/params.parquet")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_value_str_parameter() {
        let mut store = CognitiveParameterStore::new();
        let param = CognitiveParameter {
            param_id: "fusion.default_path".into(),
            category: category::THRESHOLD.into(),
            value_f64: None,
            value_str: Some("STANDARD".into()),
            min_bound: None,
            max_bound: None,
            autonomy_tier: AutonomyTier::Auto,
            modified_by: "init".into(),
        };
        store.insert(&param).unwrap();

        let got = store.get("fusion.default_path").unwrap();
        assert!(got.value_f64.is_none());
        assert_eq!(got.value_str.as_deref(), Some("STANDARD"));
    }

    #[test]
    fn test_autonomy_tier_ordering() {
        assert!(AutonomyTier::Auto < AutonomyTier::BeingApproved);
        assert!(AutonomyTier::BeingApproved < AutonomyTier::CaptainOnly);
    }

    #[test]
    fn test_set_updates_modified_by() {
        let mut store = CognitiveParameterStore::new();
        let param = CognitiveParameter {
            param_id: "track.modifier".into(),
            category: category::THRESHOLD.into(),
            value_f64: Some(0.5),
            value_str: None,
            min_bound: Some(0.0),
            max_bound: Some(1.0),
            autonomy_tier: AutonomyTier::Auto,
            modified_by: "init".into(),
        };
        store.insert(&param).unwrap();
        assert_eq!(store.get("track.modifier").unwrap().modified_by, "init");

        store
            .set("track.modifier", 0.7, "hdd_loop_v2", AutonomyTier::Auto)
            .unwrap();
        assert_eq!(
            store.get("track.modifier").unwrap().modified_by,
            "hdd_loop_v2"
        );
    }

    #[test]
    fn test_unbounded_param_accepts_any_value() {
        let mut store = CognitiveParameterStore::new();
        let param = CognitiveParameter {
            param_id: "unbounded.param".into(),
            category: category::THRESHOLD.into(),
            value_f64: Some(0.0),
            value_str: None,
            min_bound: None,
            max_bound: None,
            autonomy_tier: AutonomyTier::Auto,
            modified_by: "init".into(),
        };
        store.insert(&param).unwrap();

        // No bounds → any value accepted
        store
            .set("unbounded.param", 999.0, "test", AutonomyTier::Auto)
            .unwrap();
        assert_eq!(store.get("unbounded.param").unwrap().value_f64, Some(999.0));

        store
            .set("unbounded.param", -999.0, "test", AutonomyTier::Auto)
            .unwrap();
        assert_eq!(
            store.get("unbounded.param").unwrap().value_f64,
            Some(-999.0)
        );
    }

    // ── Signal weight tests ──────────────────────────────────────────

    #[test]
    fn test_insert_signal_weight() {
        let mut store = CognitiveParameterStore::new();
        store
            .insert_signal_weight("fractal_confidence", "FAST", 0.9)
            .unwrap();

        let val = store.get_signal_weight("fractal_confidence", "FAST");
        assert_eq!(val, Some(0.9));

        // Verify underlying param_id format
        let param = store.get("sw.fractal_confidence.FAST").unwrap();
        assert_eq!(param.category, category::SIGNAL_WEIGHT);
        assert_eq!(param.min_bound, Some(-1.5));
        assert_eq!(param.max_bound, Some(1.5));
        assert_eq!(param.autonomy_tier, AutonomyTier::Auto);
    }

    #[test]
    fn test_signal_weights_list() {
        let mut store = CognitiveParameterStore::new();
        store
            .insert_signal_weight("fractal_confidence", "FAST", 0.9)
            .unwrap();
        store
            .insert_signal_weight("fractal_confidence", "DEEP", -0.5)
            .unwrap();
        store
            .insert_signal_weight("novelty_surprise", "STANDARD", 0.1)
            .unwrap();

        let weights = store.signal_weights();
        assert_eq!(weights.len(), 3);
        assert_eq!(store.signal_weight_count(), 3);

        // Verify content
        let fractal_fast = weights
            .iter()
            .find(|(d, p, _)| d == "fractal_confidence" && p == "FAST");
        assert!(fractal_fast.is_some());
        assert!((fractal_fast.unwrap().2 - 0.9).abs() < 1e-10);
    }

    #[test]
    fn test_default_signal_weights() {
        let store = default_signal_weights();
        let count = store.signal_weight_count();
        assert!(
            count >= 140,
            "should have >= 140 signal weight entries, got {}",
            count
        );

        // Spot-check known values
        assert_eq!(
            store.get_signal_weight("fractal_confidence", "FAST"),
            Some(0.9)
        );
        assert_eq!(
            store.get_signal_weight("fractal_confidence", "DEEP"),
            Some(-0.5)
        );
        assert_eq!(
            store.get_signal_weight("tool_needed", "TOOL_USE"),
            Some(0.9)
        );
        assert_eq!(store.get_signal_weight("recency_weight", "FAST"), Some(0.3));
    }

    #[test]
    fn test_default_params_includes_signal_weights() {
        let store = default_cognitive_params();

        // Should contain both learning config AND signal weights
        let sw_count = store.signal_weight_count();
        assert!(
            sw_count >= 140,
            "default_cognitive_params should include signal weights, got {}",
            sw_count
        );

        // Learning config should still be there
        let lr = store.get("learning.initial_learning_rate");
        assert!(lr.is_some());

        // Signal weight should also be there
        let sw = store.get_signal_weight("fractal_confidence", "FAST");
        assert_eq!(sw, Some(0.9));
    }

    #[test]
    fn test_signal_weight_bounds_enforced() {
        let mut store = CognitiveParameterStore::new();

        // Insert a weight — value should be clipped to [-1.5, 1.5]
        store.insert_signal_weight("test_dim", "FAST", 2.0).unwrap();
        assert_eq!(
            store.get_signal_weight("test_dim", "FAST"),
            Some(1.5),
            "value above 1.5 should be clipped"
        );

        store
            .insert_signal_weight("test_dim2", "FAST", -3.0)
            .unwrap();
        assert_eq!(
            store.get_signal_weight("test_dim2", "FAST"),
            Some(-1.5),
            "value below -1.5 should be clipped"
        );

        // Setting via `set()` should also enforce bounds
        let err = store
            .set("sw.test_dim.FAST", 2.0, "test", AutonomyTier::Auto)
            .unwrap_err();
        assert!(matches!(err, ParamStoreError::OutOfBounds { .. }));
    }

    #[test]
    fn test_signal_weight_parquet_roundtrip() {
        let store = default_signal_weights();
        let original_count = store.signal_weight_count();
        assert!(original_count > 0);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signal_weights.parquet");

        // Save
        save_params_to_parquet(&store, &path).unwrap();
        assert!(path.exists());

        // Load
        let loaded = load_params_from_parquet(&path).unwrap().unwrap();
        assert_eq!(loaded.signal_weight_count(), original_count);

        // Spot-check a value survived
        assert_eq!(
            loaded.get_signal_weight("fractal_confidence", "FAST"),
            Some(0.9)
        );
        assert_eq!(
            loaded.get_signal_weight("tool_needed", "TOOL_USE"),
            Some(0.9)
        );
    }

    // ── Loss coefficient tests ───────────────────────────────────────

    #[test]
    fn test_insert_loss_coefficient() {
        let mut store = CognitiveParameterStore::new();
        store.insert_loss_coefficient("alpha_lm", 1.0).unwrap();

        assert_eq!(store.get_loss_coefficient("alpha_lm"), Some(1.0));
    }

    #[test]
    fn test_loss_coefficients_list() {
        let mut store = CognitiveParameterStore::new();
        store.insert_loss_coefficient("alpha_lm", 1.0).unwrap();
        store.insert_loss_coefficient("alpha_rel", 0.1).unwrap();
        store.insert_loss_coefficient("alpha_causal", 0.05).unwrap();

        let coeffs = store.loss_coefficients();
        assert_eq!(coeffs.len(), 3);
    }

    #[test]
    fn test_default_loss_coefficients_present() {
        let store = default_cognitive_params();
        assert_eq!(store.get_loss_coefficient("alpha_lm"), Some(1.0));
        assert_eq!(store.get_loss_coefficient("alpha_rel"), Some(0.1));
        assert_eq!(store.get_loss_coefficient("alpha_causal"), Some(0.05));
        assert_eq!(store.get_loss_coefficient("rel_warmup_fraction"), Some(0.1));
        assert_eq!(
            store.get_loss_coefficient("causal_warmup_fraction"),
            Some(0.2)
        );
    }

    #[test]
    fn test_loss_coefficients_are_tier_auto() {
        let store = default_cognitive_params();
        let param = store.get("loss.alpha_lm").unwrap();
        assert_eq!(param.autonomy_tier, AutonomyTier::Auto);
    }

    // ── Consolidation trigger tests ──────────────────────────────────

    #[test]
    fn test_insert_consolidation_trigger() {
        let mut store = CognitiveParameterStore::new();
        store
            .insert_consolidation_trigger("dedup_threshold", 0.95)
            .unwrap();

        assert_eq!(
            store.get_consolidation_trigger("dedup_threshold"),
            Some(0.95)
        );
    }

    #[test]
    fn test_consolidation_triggers_list() {
        let mut store = CognitiveParameterStore::new();
        store
            .insert_consolidation_trigger("dedup_threshold", 0.95)
            .unwrap();
        store
            .insert_consolidation_trigger("max_triples_per_cycle", 500.0)
            .unwrap();

        let triggers = store.consolidation_triggers();
        assert_eq!(triggers.len(), 2);
    }

    #[test]
    fn test_default_consolidation_triggers_present() {
        let store = default_cognitive_params();
        assert_eq!(
            store.get_consolidation_trigger("dedup_threshold"),
            Some(0.95)
        );
        assert_eq!(
            store.get_consolidation_trigger("max_triples_per_cycle"),
            Some(500.0)
        );
        assert_eq!(
            store.get_consolidation_trigger("min_promoted_triples"),
            Some(50.0)
        );
        assert_eq!(
            store.get_consolidation_trigger("training_threshold"),
            Some(200.0)
        );
    }

    #[test]
    fn test_consolidation_triggers_are_tier_being_approved() {
        let store = default_cognitive_params();
        let param = store.get("consol.dedup_threshold").unwrap();
        assert_eq!(param.autonomy_tier, AutonomyTier::BeingApproved);
    }

    #[test]
    fn test_loss_and_consolidation_parquet_roundtrip() {
        let store = default_cognitive_params();
        let original_loss_count = store.loss_coefficients().len();
        let original_trigger_count = store.consolidation_triggers().len();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("params.parquet");

        save_params_to_parquet(&store, &path).unwrap();
        let loaded = load_params_from_parquet(&path).unwrap().unwrap();

        assert_eq!(loaded.loss_coefficients().len(), original_loss_count);
        assert_eq!(
            loaded.consolidation_triggers().len(),
            original_trigger_count
        );
        assert_eq!(loaded.get_loss_coefficient("alpha_lm"), Some(1.0));
        assert_eq!(
            loaded.get_consolidation_trigger("dedup_threshold"),
            Some(0.95)
        );
    }

    // ── CQ threshold tests ────────────────────────────────────────────

    #[test]
    fn test_default_cq_thresholds_present() {
        let store = default_cognitive_params();

        // CQ thresholds from KBDD depth_policy and loop_engine
        assert_eq!(store.get("cq.high_threshold").unwrap().value_f64, Some(0.8));
        assert_eq!(store.get("cq.low_threshold").unwrap().value_f64, Some(0.5));
        assert_eq!(
            store.get("cq.convergence_threshold").unwrap().value_f64,
            Some(0.02)
        );
        assert_eq!(
            store.get("cq.training_threshold").unwrap().value_f64,
            Some(0.65)
        );
    }

    #[test]
    fn test_cq_auto_tiers() {
        let store = default_cognitive_params();

        // HDD-tunable CQ thresholds are Tier 1 (Auto)
        for id in &[
            "cq.high_threshold",
            "cq.low_threshold",
            "cq.convergence_threshold",
            "cq.training_threshold",
        ] {
            assert_eq!(
                store.get(id).unwrap().autonomy_tier,
                AutonomyTier::Auto,
                "{id} should be Auto tier"
            );
        }
    }

    #[test]
    fn test_cq_safety_thresholds_are_captain_only() {
        let store = default_cognitive_params();

        // Safety-critical thresholds are Tier 3 (CaptainOnly)
        let safety = store.get("cq.safety_domain_threshold").unwrap();
        assert_eq!(safety.value_f64, Some(0.8));
        assert_eq!(safety.autonomy_tier, AutonomyTier::CaptainOnly);

        // CONCERN-6: provenance_validity >= 95% hard gate
        let prov = store.get("cq.provenance_validity_threshold").unwrap();
        assert_eq!(prov.value_f64, Some(0.95));
        assert_eq!(prov.autonomy_tier, AutonomyTier::CaptainOnly);
        assert_eq!(prov.min_bound, Some(0.8));
        assert_eq!(prov.max_bound, Some(1.0));
    }

    #[test]
    fn test_cq_thresholds_cannot_be_modified_by_auto_tier() {
        let mut store = default_cognitive_params();

        // HDD loop (Auto tier) cannot modify CaptainOnly params
        let err = store
            .set(
                "cq.provenance_validity_threshold",
                0.8,
                "hdd_loop",
                AutonomyTier::Auto,
            )
            .unwrap_err();
        assert!(matches!(err, ParamStoreError::TierViolation { .. }));

        // Captain CAN modify it
        store
            .set(
                "cq.provenance_validity_threshold",
                0.97,
                "captain",
                AutonomyTier::CaptainOnly,
            )
            .unwrap();
        assert_eq!(
            store
                .get("cq.provenance_validity_threshold")
                .unwrap()
                .value_f64,
            Some(0.97)
        );
    }

    #[test]
    fn test_cq_thresholds_have_bounds() {
        let store = default_cognitive_params();

        // All CQ thresholds should have both min and max bounds
        for id in &[
            "cq.high_threshold",
            "cq.low_threshold",
            "cq.convergence_threshold",
            "cq.training_threshold",
            "cq.safety_domain_threshold",
            "cq.provenance_validity_threshold",
        ] {
            let p = store.get(id).unwrap();
            assert!(p.min_bound.is_some(), "{id} missing min_bound");
            assert!(p.max_bound.is_some(), "{id} missing max_bound");
        }
    }

    // ── First-run initialization tests ────────────────────────────────

    #[test]
    fn test_init_cognitive_params_first_run_creates_parquet() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cognitive_params.parquet");

        // File doesn't exist → creates defaults and saves
        assert!(!path.exists());
        let store = init_cognitive_params(&path).unwrap();
        assert!(path.exists(), "Parquet file should be created on first run");
        assert!(!store.is_empty());

        // Spot-check a default value
        assert_eq!(
            store
                .get("learning.initial_learning_rate")
                .unwrap()
                .value_f64,
            Some(0.1)
        );
    }

    #[test]
    fn test_init_cognitive_params_subsequent_run_loads_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cognitive_params.parquet");

        // First run: creates defaults
        let mut store = init_cognitive_params(&path).unwrap();

        // Modify a value (simulating HDD loop change)
        store
            .set(
                "learning.initial_learning_rate",
                0.5,
                "hdd_loop",
                AutonomyTier::Auto,
            )
            .unwrap();
        save_params_to_parquet(&store, &path).unwrap();

        // Second run: should load the modified value, not fresh defaults
        let loaded = init_cognitive_params(&path).unwrap();
        assert_eq!(
            loaded
                .get("learning.initial_learning_rate")
                .unwrap()
                .value_f64,
            Some(0.5),
            "should load persisted params, not fresh defaults"
        );
    }

    #[test]
    fn test_init_cognitive_params_full_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cognitive_params.parquet");

        // First run
        let store1 = init_cognitive_params(&path).unwrap();
        let count1 = store1.len();

        // Second run should yield identical store
        let store2 = init_cognitive_params(&path).unwrap();
        assert_eq!(store2.len(), count1);

        // All parameter categories should survive
        let categories = [
            category::SIGNAL_WEIGHT,
            category::THRESHOLD,
            category::LEARNING_CONFIG,
            category::TRIGGER,
            category::LOSS_COEFFICIENT,
            category::CONSOLIDATION_TRIGGER,
        ];
        for cat in &categories {
            assert!(
                !store2.list(cat).is_empty(),
                "category '{cat}' should have entries after roundtrip"
            );
        }
    }

    #[test]
    fn test_default_params_total_count() {
        let store = default_cognitive_params();
        let count = store.len();
        // 11 learning config + 3 fusion thresholds + 5 schema match thresholds
        // + 3 consolidation triggers + 6 CQ thresholds + 5 loss coefficients
        // + 5 consolidation trigger helpers + ~140+ signal weights
        assert!(
            count >= 170,
            "should have >= 170 total params (200+ with signal weights), got {}",
            count
        );
    }
}
