//! nusy-graph-review — Graph-native proposal review workflow with safety gates.
//!
//! Provides:
//! - **Safety Gates** — Y-layer × domain approval requirements (EXP-1287)
//! - **Proposals** — external review workflow (EXP-1285)
//! - **Comments** — threaded review comments on proposals
//! - **Diff View** — triple-oriented diff display

pub mod ci_store;
pub mod comments;
pub mod diff_view;
pub mod experiment_bridge;
pub mod gated_proposals;
pub mod hdd_loop;
pub mod kanban_bridge;
pub mod proposals;
pub mod safety_gates;
pub mod schema;

pub use ci_store::{CiResultInput, CiResultStore, CiResultView, CiStatus, CiStoreError};
pub use comments::{CommentError, CommentStore};
pub use diff_view::{DiffStats, proposal_diff, proposal_stats};
pub use experiment_bridge::{
    ExperimentOutcome, ExperimentRecord, MetricComparison, generate_experiment_record,
    render_experiment_markdown,
};
pub use gated_proposals::{
    GatedProposalError, ProposalSafetyMetadata, ShadowEvalResult, check_approval_gate,
    check_merge_gate, classify_and_gate_proposal, classify_proposal,
};
pub use hdd_loop::{
    CrossBoardTriple, HypothesisEvidence, accumulate_evidence, generate_cross_board_links,
};
pub use kanban_bridge::{
    KanbanAction, on_proposal_created, on_proposal_transition, proposal_to_kanban_id,
    safety_to_priority,
};
pub use proposals::{CreateProposalInput, ProposalError, ProposalStatus, ProposalStore};
pub use safety_gates::{
    ApprovalRequirement, SafetyGateError, SafetyGatesTable, YLayer, classify_change,
    classify_proposal_changes, default_gates,
};
pub use schema::{ci_results_schema, comments_schema, diff_view_schema, proposals_schema};
