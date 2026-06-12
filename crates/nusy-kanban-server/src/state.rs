//! Server state — all stores bundled into one struct.

use nusy_kanban::crud::KanbanStore;
use nusy_kanban::relations::RelationsStore;
use std::path::PathBuf;

/// All server state in one place.
///
/// Feature-gated fields are only present when the corresponding feature is enabled.
pub struct ServerState {
    pub store: KanbanStore,
    pub relations: RelationsStore,
    #[cfg(feature = "pr")]
    pub proposals: nusy_graph_review::ProposalStore,
    #[cfg(feature = "pr")]
    pub comments: nusy_graph_review::CommentStore,
    #[cfg(feature = "pr")]
    pub ci_results: nusy_graph_review::CiResultStore,
    pub data_dir: PathBuf,
}
