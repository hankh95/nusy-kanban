use arrow::array::{Array, RecordBatch, StringArray, TimestampMillisecondArray};
use arrow::datatypes::Schema;
use std::sync::Arc;

use crate::schema::{proposals_col, proposals_schema};

// ── Status lifecycle ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalStatus {
    Draft,
    Open,
    Reviewing,
    Approved,
    Merged,
    Rejected,
    Revised,
    Closed,
}

impl ProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Open => "open",
            Self::Reviewing => "reviewing",
            Self::Approved => "approved",
            Self::Merged => "merged",
            Self::Rejected => "rejected",
            Self::Revised => "revised",
            Self::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Self::Draft),
            "open" => Some(Self::Open),
            "reviewing" => Some(Self::Reviewing),
            "approved" => Some(Self::Approved),
            "merged" => Some(Self::Merged),
            "rejected" => Some(Self::Rejected),
            "revised" => Some(Self::Revised),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Merged | Self::Closed)
    }

    /// Returns valid next states from this state.
    pub fn valid_transitions(self) -> &'static [ProposalStatus] {
        match self {
            Self::Draft => &[Self::Open],
            Self::Open => &[Self::Reviewing, Self::Closed],
            Self::Reviewing => &[Self::Approved, Self::Rejected],
            Self::Approved => &[Self::Merged, Self::Closed],
            Self::Rejected => &[Self::Revised, Self::Closed],
            Self::Revised => &[Self::Reviewing],
            Self::Merged => &[],
            Self::Closed => &[],
        }
    }

    pub fn can_transition_to(self, to: Self) -> bool {
        self.valid_transitions().contains(&to)
    }
}

// ── Proposal types ──────────────────────────────────────────────────────────

const VALID_PROPOSAL_TYPES: &[&str] = &[
    "knowledge_change",
    "code_change",
    "ontology_change",
    "safety_rule_change",
];

const VALID_NAMESPACES: &[&str] = &["world", "work", "research", "self"];

// ── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ProposalError {
    #[error("Proposal not found: {0}")]
    NotFound(String),

    #[error("Invalid transition from {from} to {to}")]
    InvalidTransition { from: String, to: String },

    #[error("Invalid proposal type: {0} (valid: {VALID_PROPOSAL_TYPES:?})")]
    InvalidProposalType(String),

    #[error("Invalid namespace: {0} (valid: {VALID_NAMESPACES:?})")]
    InvalidNamespace(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Unresolved comments block approval ({0} unresolved)")]
    UnresolvedComments(usize),

    #[error("Internal error: {0}")]
    InternalError(String),

    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
}

pub type Result<T> = std::result::Result<T, ProposalError>;

// ── Input struct ─────────────────────────────────────────────────────────────

pub struct CreateProposalInput<'a> {
    pub author: &'a str,
    pub title: &'a str,
    pub source_branch: &'a str,
    pub target_branch: &'a str,
    pub namespace: &'a str,
    pub proposal_type: &'a str,
    pub description: Option<&'a str>,
}

// ── ProposalStore ───────────────────────────────────────────────────────────

pub struct ProposalStore {
    proposals_batches: Vec<RecordBatch>,
    proposals_schema: Arc<Schema>,
}

impl ProposalStore {
    pub fn new() -> Self {
        Self {
            proposals_batches: Vec::new(),
            proposals_schema: proposals_schema(),
        }
    }

    pub fn proposals_batches(&self) -> &[RecordBatch] {
        &self.proposals_batches
    }

    pub fn proposals_schema(&self) -> &Arc<Schema> {
        &self.proposals_schema
    }

    pub fn load_proposals(&mut self, batches: Vec<RecordBatch>) {
        self.proposals_batches = batches;
    }

    // ── Create ──────────────────────────────────────────────────────────

    pub fn create_proposal(&mut self, input: &CreateProposalInput<'_>) -> Result<String> {
        if !VALID_PROPOSAL_TYPES.contains(&input.proposal_type) {
            return Err(ProposalError::InvalidProposalType(
                input.proposal_type.to_string(),
            ));
        }
        if !VALID_NAMESPACES.contains(&input.namespace) {
            return Err(ProposalError::InvalidNamespace(input.namespace.to_string()));
        }

        let proposal_id = format!("PROP-{}", self.next_id());
        let now_ms = chrono::Utc::now().timestamp_millis();

        let batch = RecordBatch::try_new(
            self.proposals_schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![proposal_id.as_str()])),
                Arc::new(StringArray::from(vec![input.source_branch])),
                Arc::new(StringArray::from(vec![input.target_branch])),
                Arc::new(StringArray::from(vec![input.namespace])),
                Arc::new(StringArray::from(vec![input.proposal_type])),
                Arc::new(StringArray::from(vec![ProposalStatus::Draft.as_str()])),
                Arc::new(StringArray::from(vec![input.author])),
                Arc::new(StringArray::from(vec![None::<&str>])), // reviewer
                Arc::new(StringArray::from(vec![None::<&str>])), // merged_by
                Arc::new(StringArray::from(vec![input.title])),
                Arc::new(StringArray::from(vec![input.description])),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(TimestampMillisecondArray::from(vec![now_ms]).with_timezone("UTC")),
                Arc::new(TimestampMillisecondArray::from(vec![None::<i64>]).with_timezone("UTC")),
                Arc::new(StringArray::from(vec![None::<&str>])), // resolution
                Arc::new(StringArray::from(vec![None::<&str>])), // closed_by
            ],
        )?;

        self.proposals_batches.push(batch);
        Ok(proposal_id)
    }

    // ── Lifecycle transitions ───────────────────────────────────────────

    pub fn open_proposal(&mut self, proposal_id: &str) -> Result<()> {
        self.transition(proposal_id, ProposalStatus::Open, None)
    }

    pub fn add_reviewer(&mut self, proposal_id: &str, reviewer: &str) -> Result<()> {
        self.set_field_and_transition(
            proposal_id,
            ProposalStatus::Reviewing,
            Some((proposals_col::REVIEWER, reviewer)),
            None,
        )
    }

    pub fn approve(
        &mut self,
        proposal_id: &str,
        reviewer: &str,
        unresolved_count: usize,
    ) -> Result<()> {
        // Check state transition first — invalid state is the primary error
        let current = self.get_status(proposal_id)?;
        if !current.can_transition_to(ProposalStatus::Approved) {
            return Err(ProposalError::InvalidTransition {
                from: current.as_str().to_string(),
                to: ProposalStatus::Approved.as_str().to_string(),
            });
        }
        if unresolved_count > 0 {
            return Err(ProposalError::UnresolvedComments(unresolved_count));
        }
        // Any agent can approve except the author (cross-agent review).
        let stored_author = self.get_field(proposal_id, proposals_col::AUTHOR)?;
        if stored_author.as_deref() == Some(reviewer) {
            return Err(ProposalError::Unauthorized(format!(
                "author cannot approve their own proposal (author: {reviewer})"
            )));
        }
        // Update the reviewer field to whoever is approving
        let _ = self.update_column_str(proposal_id, proposals_col::REVIEWER, reviewer);
        self.update_column_str(
            proposal_id,
            proposals_col::STATUS,
            ProposalStatus::Approved.as_str(),
        )?;
        self.touch_updated_at(proposal_id)
    }

    /// Reject a proposal. Rejection reasons are captured as review comments
    /// in the CommentStore, not stored on the proposal itself.
    pub fn reject(&mut self, proposal_id: &str, reviewer: &str) -> Result<()> {
        let current = self.get_status(proposal_id)?;
        if !current.can_transition_to(ProposalStatus::Rejected) {
            return Err(ProposalError::InvalidTransition {
                from: current.as_str().to_string(),
                to: ProposalStatus::Rejected.as_str().to_string(),
            });
        }
        // Any agent can reject except the author (cross-agent review).
        let stored_author = self.get_field(proposal_id, proposals_col::AUTHOR)?;
        if stored_author.as_deref() == Some(reviewer) {
            return Err(ProposalError::Unauthorized(format!(
                "author cannot reject their own proposal (author: {reviewer})"
            )));
        }
        // Update the reviewer field to whoever is rejecting
        let _ = self.update_column_str(proposal_id, proposals_col::REVIEWER, reviewer);
        self.update_column_str(
            proposal_id,
            proposals_col::STATUS,
            ProposalStatus::Rejected.as_str(),
        )?;
        self.touch_updated_at(proposal_id)
    }

    pub fn revise(&mut self, proposal_id: &str, _caller: &str) -> Result<()> {
        // No author restriction — anyone can re-enter review after rejection.
        // The state machine (rejected → revised → reviewing) is the gate,
        // not the caller's identity. Author checks were too strict when
        // identities change (hostname → agent name) or Captain needs to unblock.
        self.transition(proposal_id, ProposalStatus::Revised, None)?;
        self.transition(proposal_id, ProposalStatus::Reviewing, None)
    }

    pub fn mark_merged(
        &mut self,
        proposal_id: &str,
        merged_by: &str,
        resolution: Option<&str>,
        closed_by: Option<&str>,
    ) -> Result<()> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.set_field_and_transition(
            proposal_id,
            ProposalStatus::Merged,
            Some((proposals_col::MERGED_BY, merged_by)),
            Some((proposals_col::MERGED_AT, now_ms)),
        )?;
        if let Some(res) = resolution {
            let _ = self.update_column_str(proposal_id, proposals_col::RESOLUTION, res);
        }
        if let Some(cb) = closed_by {
            let _ = self.update_column_str(proposal_id, proposals_col::CLOSED_BY, cb);
        }
        Ok(())
    }

    pub fn close_proposal(
        &mut self,
        proposal_id: &str,
        _caller: &str,
        resolution: Option<&str>,
    ) -> Result<()> {
        // No author restriction — Captain or any agent can close a proposal.
        // The state machine gate (valid transitions) is sufficient protection.
        self.transition(proposal_id, ProposalStatus::Closed, None)?;
        if let Some(res) = resolution {
            let _ = self.update_column_str(proposal_id, proposals_col::RESOLUTION, res);
        }
        Ok(())
    }

    // ── Query helpers ───────────────────────────────────────────────────

    pub fn get_status(&self, proposal_id: &str) -> Result<ProposalStatus> {
        let (batch_idx, row_idx) = self.find_proposal(proposal_id)?;
        let batch = &self.proposals_batches[batch_idx];
        let statuses = batch
            .column(proposals_col::STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| ProposalError::InternalError("status column downcast".into()))?;
        ProposalStatus::parse(statuses.value(row_idx))
            .ok_or_else(|| ProposalError::NotFound(proposal_id.to_string()))
    }

    pub fn get_source_branch(&self, proposal_id: &str) -> Result<String> {
        let (batch_idx, row_idx) = self.find_proposal(proposal_id)?;
        let batch = &self.proposals_batches[batch_idx];
        let col = batch
            .column(proposals_col::SOURCE_BRANCH)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| ProposalError::InternalError("source_branch column downcast".into()))?;
        Ok(col.value(row_idx).to_string())
    }

    /// Get the reviewer who approved a proposal (HZ-3448).
    pub fn get_reviewer(&self, proposal_id: &str) -> Result<String> {
        let (batch_idx, row_idx) = self.find_proposal(proposal_id)?;
        let batch = &self.proposals_batches[batch_idx];
        let col = batch
            .column(proposals_col::REVIEWER)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| ProposalError::InternalError("reviewer column downcast".into()))?;
        if col.is_null(row_idx) {
            Ok(String::new())
        } else {
            Ok(col.value(row_idx).to_string())
        }
    }

    pub fn get_target_branch(&self, proposal_id: &str) -> Result<String> {
        let (batch_idx, row_idx) = self.find_proposal(proposal_id)?;
        let batch = &self.proposals_batches[batch_idx];
        let col = batch
            .column(proposals_col::TARGET_BRANCH)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| ProposalError::InternalError("target_branch column downcast".into()))?;
        Ok(col.value(row_idx).to_string())
    }

    pub fn get_proposal_type(&self, proposal_id: &str) -> Result<String> {
        let (batch_idx, row_idx) = self.find_proposal(proposal_id)?;
        let batch = &self.proposals_batches[batch_idx];
        let col = batch
            .column(proposals_col::PROPOSAL_TYPE)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| ProposalError::InternalError("proposal_type column downcast".into()))?;
        Ok(col.value(row_idx).to_string())
    }

    pub fn get_namespace(&self, proposal_id: &str) -> Result<String> {
        let (batch_idx, row_idx) = self.find_proposal(proposal_id)?;
        let batch = &self.proposals_batches[batch_idx];
        let col = batch
            .column(proposals_col::NAMESPACE)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| ProposalError::InternalError("namespace column downcast".into()))?;
        Ok(col.value(row_idx).to_string())
    }

    pub fn count(&self) -> usize {
        self.proposals_batches.iter().map(|b| b.num_rows()).sum()
    }

    /// List proposals, optionally filtered by status.
    ///
    /// `status_filter` values:
    /// - `None` or `Some("open")` — non-terminal proposals (open, reviewing,
    ///   approved, rejected, revised)
    /// - `Some("all")` — all proposals regardless of status
    /// - `Some("merged")`, `Some("closed")`, etc. — exact status match
    pub fn list_proposals(&self, status_filter: Option<&str>) -> Result<Vec<RecordBatch>> {
        use arrow::array::StringArray;

        if self.proposals_batches.is_empty() {
            return Ok(Vec::new());
        }

        // Determine which statuses to include
        let filter = status_filter.unwrap_or("open");
        let wanted: Vec<&str> = match filter {
            "all" => return Ok(self.proposals_batches.clone()),
            "open" => vec!["open", "reviewing", "approved", "rejected", "revised"],
            other => {
                if ProposalStatus::parse(other).is_none() {
                    return Err(ProposalError::InvalidProposalType(format!(
                        "unknown status filter: {other}"
                    )));
                }
                vec![other]
            }
        };

        let mut filtered = Vec::new();
        for batch in &self.proposals_batches {
            let statuses = batch
                .column(proposals_col::STATUS)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| ProposalError::InternalError("status column downcast".into()))?;

            // Collect indices of matching rows
            let match_indices: Vec<usize> = (0..batch.num_rows())
                .filter(|&i| wanted.contains(&statuses.value(i)))
                .collect();

            if match_indices.is_empty() {
                continue;
            }

            // Project matching rows into new batch
            let indices = arrow::array::UInt64Array::from(
                match_indices
                    .iter()
                    .map(|&i| i as u64)
                    .collect::<Vec<u64>>(),
            );
            let projected: Vec<Arc<dyn Array>> = (0..batch.num_columns())
                .map(|col_idx| {
                    arrow::compute::take(batch.column(col_idx), &indices, None)
                        .expect("take on filtered indices from same batch")
                })
                .collect();

            filtered.push(
                RecordBatch::try_new(batch.schema(), projected)
                    .map_err(|e| ProposalError::InternalError(format!("batch projection: {e}")))?,
            );
        }

        Ok(filtered)
    }

    /// Search proposals by text term across titles and descriptions.
    ///
    /// Returns projected RecordBatches with only matching rows.
    /// Searches all proposals regardless of status.
    pub fn search_proposals(&self, term: &str) -> Result<Vec<RecordBatch>> {
        let term_lower = term.to_lowercase();
        let mut result = Vec::new();

        for batch in &self.proposals_batches {
            let titles = batch
                .column(proposals_col::TITLE)
                .as_any()
                .downcast_ref::<StringArray>();
            let bodies = batch
                .column(proposals_col::DESCRIPTION)
                .as_any()
                .downcast_ref::<StringArray>();

            let match_indices: Vec<usize> = (0..batch.num_rows())
                .filter(|&i| {
                    let title_match = titles
                        .map(|t| t.value(i).to_lowercase().contains(&term_lower))
                        .unwrap_or(false);
                    let body_match = bodies
                        .and_then(|b| {
                            if b.is_null(i) {
                                None
                            } else {
                                Some(b.value(i).to_lowercase().contains(&term_lower))
                            }
                        })
                        .unwrap_or(false);
                    // Also match on proposal ID
                    let id_match = batch
                        .column(proposals_col::PROPOSAL_ID)
                        .as_any()
                        .downcast_ref::<StringArray>()
                        .map(|ids| ids.value(i).to_lowercase().contains(&term_lower))
                        .unwrap_or(false);
                    title_match || body_match || id_match
                })
                .collect();

            if match_indices.is_empty() {
                continue;
            }

            let indices = arrow::array::UInt64Array::from(
                match_indices
                    .iter()
                    .map(|&i| i as u64)
                    .collect::<Vec<u64>>(),
            );
            let projected: Vec<Arc<dyn Array>> = (0..batch.num_columns())
                .map(|col_idx| {
                    arrow::compute::take(batch.column(col_idx), &indices, None)
                        .expect("take on filtered indices from same batch")
                })
                .collect();

            result.push(
                RecordBatch::try_new(batch.schema(), projected).map_err(|e| {
                    ProposalError::InternalError(format!("projected columns retain schema: {e}"))
                })?,
            );
        }

        Ok(result)
    }

    // ── Internal helpers ────────────────────────────────────────────────

    /// ID base offset — starts at 2000 to clearly separate from GitHub PR numbers.
    const PROPOSAL_ID_BASE: usize = 2000;

    fn next_id(&self) -> String {
        format!("{}", Self::PROPOSAL_ID_BASE + self.count() + 1)
    }

    fn find_proposal(&self, proposal_id: &str) -> Result<(usize, usize)> {
        for (batch_idx, batch) in self.proposals_batches.iter().enumerate() {
            let ids = batch
                .column(proposals_col::PROPOSAL_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| {
                    ProposalError::InternalError("proposal_id column downcast".into())
                })?;
            for row_idx in 0..batch.num_rows() {
                if ids.value(row_idx) == proposal_id {
                    return Ok((batch_idx, row_idx));
                }
            }
        }
        Err(ProposalError::NotFound(proposal_id.to_string()))
    }

    fn get_field(&self, proposal_id: &str, col_idx: usize) -> Result<Option<String>> {
        let (batch_idx, row_idx) = self.find_proposal(proposal_id)?;
        let batch = &self.proposals_batches[batch_idx];
        let col = batch
            .column(col_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| ProposalError::InternalError("string column downcast".into()))?;
        if col.is_null(row_idx) {
            Ok(None)
        } else {
            Ok(Some(col.value(row_idx).to_string()))
        }
    }

    fn transition(
        &mut self,
        proposal_id: &str,
        to: ProposalStatus,
        _context: Option<&str>,
    ) -> Result<()> {
        let current = self.get_status(proposal_id)?;
        if !current.can_transition_to(to) {
            return Err(ProposalError::InvalidTransition {
                from: current.as_str().to_string(),
                to: to.as_str().to_string(),
            });
        }
        self.update_column_str(proposal_id, proposals_col::STATUS, to.as_str())?;
        self.touch_updated_at(proposal_id)
    }

    fn set_field_and_transition(
        &mut self,
        proposal_id: &str,
        to: ProposalStatus,
        str_field: Option<(usize, &str)>,
        ts_field: Option<(usize, i64)>,
    ) -> Result<()> {
        let current = self.get_status(proposal_id)?;
        if !current.can_transition_to(to) {
            return Err(ProposalError::InvalidTransition {
                from: current.as_str().to_string(),
                to: to.as_str().to_string(),
            });
        }
        // Apply string field update
        if let Some((col_idx, value)) = str_field {
            self.update_column_str(proposal_id, col_idx, value)?;
        }
        // Apply timestamp field update
        if let Some((col_idx, value)) = ts_field {
            self.update_column_ts(proposal_id, col_idx, Some(value))?;
        }
        // Apply status change
        self.update_column_str(proposal_id, proposals_col::STATUS, to.as_str())?;
        self.touch_updated_at(proposal_id)
    }

    fn touch_updated_at(&mut self, proposal_id: &str) -> Result<()> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.update_column_ts(proposal_id, proposals_col::UPDATED_AT, Some(now_ms))
    }

    fn update_column_str(&mut self, proposal_id: &str, col_idx: usize, value: &str) -> Result<()> {
        let (batch_idx, row_idx) = self.find_proposal(proposal_id)?;
        let batch = &self.proposals_batches[batch_idx];
        let mut columns: Vec<Arc<dyn Array>> = Vec::with_capacity(batch.num_columns());

        for ci in 0..batch.num_columns() {
            if ci == col_idx {
                let old = batch
                    .column(ci)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .ok_or_else(|| ProposalError::InternalError("string column downcast".into()))?;
                let vals: Vec<Option<&str>> = (0..batch.num_rows())
                    .map(|i| {
                        if i == row_idx {
                            Some(value)
                        } else if old.is_null(i) {
                            None
                        } else {
                            Some(old.value(i))
                        }
                    })
                    .collect();
                columns.push(Arc::new(StringArray::from(vals)));
            } else {
                columns.push(batch.column(ci).clone());
            }
        }

        self.proposals_batches[batch_idx] =
            RecordBatch::try_new(self.proposals_schema.clone(), columns)?;
        Ok(())
    }

    fn update_column_ts(
        &mut self,
        proposal_id: &str,
        col_idx: usize,
        value: Option<i64>,
    ) -> Result<()> {
        let (batch_idx, row_idx) = self.find_proposal(proposal_id)?;
        let batch = &self.proposals_batches[batch_idx];
        let mut columns: Vec<Arc<dyn Array>> = Vec::with_capacity(batch.num_columns());

        for ci in 0..batch.num_columns() {
            if ci == col_idx {
                let old = batch
                    .column(ci)
                    .as_any()
                    .downcast_ref::<TimestampMillisecondArray>()
                    .ok_or_else(|| {
                        ProposalError::InternalError("timestamp column downcast".into())
                    })?;
                let vals: Vec<Option<i64>> = (0..batch.num_rows())
                    .map(|i| {
                        if i == row_idx {
                            value
                        } else if old.is_null(i) {
                            None
                        } else {
                            Some(old.value(i))
                        }
                    })
                    .collect();
                columns.push(Arc::new(
                    TimestampMillisecondArray::from(vals).with_timezone("UTC"),
                ));
            } else {
                columns.push(batch.column(ci).clone());
            }
        }

        self.proposals_batches[batch_idx] =
            RecordBatch::try_new(self.proposals_schema.clone(), columns)?;
        Ok(())
    }
}

impl Default for ProposalStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store_with_proposal() -> (ProposalStore, String) {
        let mut store = ProposalStore::new();
        let id = store
            .create_proposal(&CreateProposalInput {
                author: "being-alpha",
                title: "Add reasoning rules",
                source_branch: "proposal/add-rules",
                target_branch: "main",
                namespace: "self",
                proposal_type: "knowledge_change",
                description: Some("Adding Y2 reasoning rules from experiment"),
            })
            .expect("create");
        (store, id)
    }

    #[test]
    fn test_create_proposal() {
        let (store, id) = make_store_with_proposal();
        assert_eq!(id, "PROP-2001");
        assert_eq!(store.count(), 1);
        assert_eq!(store.get_status(&id).unwrap(), ProposalStatus::Draft);
    }

    #[test]
    fn test_invalid_proposal_type() {
        let mut store = ProposalStore::new();
        let err = store
            .create_proposal(&CreateProposalInput {
                author: "author",
                title: "Title",
                source_branch: "branch",
                target_branch: "main",
                namespace: "self",
                proposal_type: "invalid_type",
                description: None,
            })
            .unwrap_err();
        assert!(matches!(err, ProposalError::InvalidProposalType(_)));
    }

    #[test]
    fn test_invalid_namespace() {
        let mut store = ProposalStore::new();
        let err = store
            .create_proposal(&CreateProposalInput {
                author: "author",
                title: "Title",
                source_branch: "branch",
                target_branch: "main",
                namespace: "invalid_ns",
                proposal_type: "knowledge_change",
                description: None,
            })
            .unwrap_err();
        assert!(matches!(err, ProposalError::InvalidNamespace(_)));
    }

    #[test]
    fn test_happy_path_lifecycle() {
        let (mut store, id) = make_store_with_proposal();

        // draft → open
        store.open_proposal(&id).unwrap();
        assert_eq!(store.get_status(&id).unwrap(), ProposalStatus::Open);

        // open → reviewing (add reviewer)
        store.add_reviewer(&id, "captain").unwrap();
        assert_eq!(store.get_status(&id).unwrap(), ProposalStatus::Reviewing);

        // reviewing → approved
        store.approve(&id, "captain", 0).unwrap();
        assert_eq!(store.get_status(&id).unwrap(), ProposalStatus::Approved);

        // approved → merged
        store.mark_merged(&id, "captain", None, None).unwrap();
        assert_eq!(store.get_status(&id).unwrap(), ProposalStatus::Merged);
    }

    #[test]
    fn test_reject_and_revise_cycle() {
        let (mut store, id) = make_store_with_proposal();
        store.open_proposal(&id).unwrap();
        store.add_reviewer(&id, "captain").unwrap();

        // reviewing → rejected
        store.reject(&id, "captain").unwrap();
        assert_eq!(store.get_status(&id).unwrap(), ProposalStatus::Rejected);

        // rejected → revised → reviewing (auto-advance)
        store.revise(&id, "being-alpha").unwrap();
        assert_eq!(store.get_status(&id).unwrap(), ProposalStatus::Reviewing);

        // reviewing → approved → merged
        store.approve(&id, "captain", 0).unwrap();
        store.mark_merged(&id, "captain", None, None).unwrap();
        assert_eq!(store.get_status(&id).unwrap(), ProposalStatus::Merged);
    }

    #[test]
    fn test_close_by_author() {
        let (mut store, id) = make_store_with_proposal();
        store.open_proposal(&id).unwrap();

        // open → closed (withdrawn)
        store.close_proposal(&id, "being-alpha", None).unwrap();
        assert_eq!(store.get_status(&id).unwrap(), ProposalStatus::Closed);
    }

    #[test]
    fn test_close_after_rejection() {
        let (mut store, id) = make_store_with_proposal();
        store.open_proposal(&id).unwrap();
        store.add_reviewer(&id, "captain").unwrap();
        store.reject(&id, "captain").unwrap();

        // rejected → closed (abandoned)
        store.close_proposal(&id, "being-alpha", None).unwrap();
        assert_eq!(store.get_status(&id).unwrap(), ProposalStatus::Closed);
    }

    #[test]
    fn test_cannot_merge_rejected() {
        let (mut store, id) = make_store_with_proposal();
        store.open_proposal(&id).unwrap();
        store.add_reviewer(&id, "captain").unwrap();
        store.reject(&id, "captain").unwrap();

        let err = store.mark_merged(&id, "captain", None, None).unwrap_err();
        assert!(matches!(err, ProposalError::InvalidTransition { .. }));
    }

    #[test]
    fn test_cannot_approve_closed() {
        let (mut store, id) = make_store_with_proposal();
        store.open_proposal(&id).unwrap();
        store.close_proposal(&id, "being-alpha", None).unwrap();

        let err = store.approve(&id, "captain", 0).unwrap_err();
        assert!(matches!(err, ProposalError::InvalidTransition { .. }));
    }

    #[test]
    fn test_cannot_review_draft() {
        let (mut store, id) = make_store_with_proposal();
        let err = store.add_reviewer(&id, "captain").unwrap_err();
        assert!(matches!(err, ProposalError::InvalidTransition { .. }));
    }

    #[test]
    fn test_cannot_reopen_merged() {
        let (mut store, id) = make_store_with_proposal();
        store.open_proposal(&id).unwrap();
        store.add_reviewer(&id, "captain").unwrap();
        store.approve(&id, "captain", 0).unwrap();
        store.mark_merged(&id, "captain", None, None).unwrap();

        let err = store.open_proposal(&id).unwrap_err();
        assert!(matches!(err, ProposalError::InvalidTransition { .. }));
    }

    #[test]
    fn test_unresolved_comments_block_approval() {
        let (mut store, id) = make_store_with_proposal();
        store.open_proposal(&id).unwrap();
        store.add_reviewer(&id, "captain").unwrap();

        let err = store.approve(&id, "captain", 3).unwrap_err();
        assert!(matches!(err, ProposalError::UnresolvedComments(3)));
    }

    #[test]
    fn test_author_cannot_approve_own_proposal() {
        let (mut store, id) = make_store_with_proposal();
        store.open_proposal(&id).unwrap();
        store.add_reviewer(&id, "captain").unwrap();

        // Author "being-alpha" cannot approve their own proposal
        let err = store.approve(&id, "being-alpha", 0).unwrap_err();
        assert!(matches!(err, ProposalError::Unauthorized(_)));
    }

    #[test]
    fn test_different_agent_can_approve() {
        let (mut store, id) = make_store_with_proposal();
        store.open_proposal(&id).unwrap();
        store.add_reviewer(&id, "captain").unwrap();

        // Any non-author agent can approve — not restricted to assigned reviewer
        store.approve(&id, "other-agent", 0).unwrap();
        assert_eq!(store.get_status(&id).unwrap(), ProposalStatus::Approved);
    }

    #[test]
    fn test_author_cannot_reject_own_proposal() {
        let (mut store, id) = make_store_with_proposal();
        store.open_proposal(&id).unwrap();
        store.add_reviewer(&id, "captain").unwrap();

        // Author "being-alpha" cannot reject their own proposal
        let err = store.reject(&id, "being-alpha").unwrap_err();
        assert!(matches!(err, ProposalError::Unauthorized(_)));
    }

    #[test]
    fn test_any_agent_can_close() {
        let (mut store, id) = make_store_with_proposal();
        store.open_proposal(&id).unwrap();

        // A different agent (not the author) can close
        store
            .close_proposal(&id, "different-agent", Some("duplicate"))
            .unwrap();
        assert_eq!(store.get_status(&id).unwrap(), ProposalStatus::Closed);
    }

    #[test]
    fn test_approved_can_be_closed() {
        let (mut store, id) = make_store_with_proposal();
        store.open_proposal(&id).unwrap();
        store.add_reviewer(&id, "captain").unwrap();
        store.approve(&id, "captain", 0).unwrap();
        assert_eq!(store.get_status(&id).unwrap(), ProposalStatus::Approved);

        // Approved proposals can now be closed (e.g., duplicate found after approval)
        store
            .close_proposal(&id, "captain", Some("duplicate"))
            .unwrap();
        assert_eq!(store.get_status(&id).unwrap(), ProposalStatus::Closed);
    }

    #[test]
    fn test_anyone_can_revise_rejected_proposal() {
        let (mut store, id) = make_store_with_proposal();
        store.open_proposal(&id).unwrap();
        store.add_reviewer(&id, "captain").unwrap();
        store.reject(&id, "captain").unwrap();

        // Any agent can revise — no author restriction
        store.revise(&id, "not-the-author").unwrap();
        let status = store.get_field(&id, proposals_col::STATUS).unwrap();
        assert_eq!(status.as_deref(), Some("reviewing"));
    }

    #[test]
    fn test_multiple_proposals() {
        let mut store = ProposalStore::new();
        let id1 = store
            .create_proposal(&CreateProposalInput {
                author: "alpha",
                title: "First",
                source_branch: "b1",
                target_branch: "main",
                namespace: "world",
                proposal_type: "knowledge_change",
                description: None,
            })
            .unwrap();
        let id2 = store
            .create_proposal(&CreateProposalInput {
                author: "beta",
                title: "Second",
                source_branch: "b2",
                target_branch: "main",
                namespace: "research",
                proposal_type: "ontology_change",
                description: None,
            })
            .unwrap();
        assert_eq!(id1, "PROP-2001");
        assert_eq!(id2, "PROP-2002");
        assert_eq!(store.count(), 2);

        // Each has independent state
        store.open_proposal(&id1).unwrap();
        assert_eq!(store.get_status(&id1).unwrap(), ProposalStatus::Open);
        assert_eq!(store.get_status(&id2).unwrap(), ProposalStatus::Draft);
    }

    // ── list_proposals status filtering ─────────────────────────────────

    /// Helper: create a store with proposals at different lifecycle stages.
    fn make_store_with_mixed_statuses() -> ProposalStore {
        let mut store = ProposalStore::new();
        // Proposal 1: draft (never opened)
        store
            .create_proposal(&CreateProposalInput {
                author: "alpha",
                title: "Draft proposal",
                source_branch: "b1",
                target_branch: "main",
                namespace: "work",
                proposal_type: "code_change",
                description: Some("still drafting"),
            })
            .unwrap();
        // Proposal 2: open
        let id2 = store
            .create_proposal(&CreateProposalInput {
                author: "beta",
                title: "Open proposal",
                source_branch: "b2",
                target_branch: "main",
                namespace: "work",
                proposal_type: "code_change",
                description: Some("ready for review"),
            })
            .unwrap();
        store.open_proposal(&id2).unwrap();
        // Proposal 3: approved
        let id3 = store
            .create_proposal(&CreateProposalInput {
                author: "gamma",
                title: "Approved proposal about tiered covenant",
                source_branch: "b3",
                target_branch: "main",
                namespace: "work",
                proposal_type: "code_change",
                description: Some("safety gate passed"),
            })
            .unwrap();
        store.open_proposal(&id3).unwrap();
        store.add_reviewer(&id3, "captain").unwrap();
        store.approve(&id3, "captain", 0).unwrap();
        // Proposal 4: merged
        let id4 = store
            .create_proposal(&CreateProposalInput {
                author: "delta",
                title: "Merged search fix",
                source_branch: "b4",
                target_branch: "main",
                namespace: "work",
                proposal_type: "code_change",
                description: Some("already merged"),
            })
            .unwrap();
        store.open_proposal(&id4).unwrap();
        store.add_reviewer(&id4, "captain").unwrap();
        store.approve(&id4, "captain", 0).unwrap();
        store.mark_merged(&id4, "captain", None, None).unwrap();
        store
    }

    #[test]
    fn test_list_proposals_open_default() {
        let store = make_store_with_mixed_statuses();
        // Default (None) = "open" = non-terminal (open, reviewing, approved, rejected, revised)
        let batches = store.list_proposals(None).unwrap();
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        // "open" filter = non-terminal post-draft: open + approved = 2
        // (draft is excluded, merged is terminal)
        assert_eq!(total, 2);
    }

    #[test]
    fn test_list_proposals_all() {
        let store = make_store_with_mixed_statuses();
        let batches = store.list_proposals(Some("all")).unwrap();
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 4);
    }

    #[test]
    fn test_list_proposals_exact_status() {
        let store = make_store_with_mixed_statuses();
        let batches = store.list_proposals(Some("merged")).unwrap();
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 1);
    }

    #[test]
    fn test_list_proposals_draft() {
        let store = make_store_with_mixed_statuses();
        let batches = store.list_proposals(Some("draft")).unwrap();
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 1);
    }

    #[test]
    fn test_list_proposals_invalid_status() {
        let store = make_store_with_mixed_statuses();
        let err = store.list_proposals(Some("bogus")).unwrap_err();
        assert!(matches!(err, ProposalError::InvalidProposalType(_)));
    }

    // ── search_proposals ────────────────────────────────────────────────

    #[test]
    fn test_search_proposals_by_title() {
        let store = make_store_with_mixed_statuses();
        let results = store.search_proposals("tiered covenant").unwrap();
        assert_eq!(results.len(), 1);
        let total: usize = results.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 1);
    }

    #[test]
    fn test_search_proposals_by_description() {
        let store = make_store_with_mixed_statuses();
        let results = store.search_proposals("safety gate").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_proposals_no_match() {
        let store = make_store_with_mixed_statuses();
        let results = store.search_proposals("nonexistent phrase").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_proposals_case_insensitive() {
        let store = make_store_with_mixed_statuses();
        let results = store.search_proposals("TIERED COVENANT").unwrap();
        assert!(!results.is_empty());
    }
}
