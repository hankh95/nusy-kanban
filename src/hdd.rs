//! HDD (Research board) commands — create research items with auto-linking.
//!
//! Supports 6 research item types with complex ID formats:
//! - Paper: PAPER-{N}
//! - Hypothesis: H{paper}.{seq} (paper-scoped) or H-{N} (standalone)
//! - Experiment: EXPR-{paper}.{seq} (paper-scoped) or EXPR-{N} (standalone)
//! - Measure: M-{N}
//! - Idea: IDEA-{N}
//! - Literature: LIT-{N}
//!
//! Auto-linking: hypothesis→paper, experiment→hypothesis, measure→experiment.

use crate::crud::{CreateItemInput, CrudError, KanbanStore};
use crate::item_type::ItemType;
use crate::relations::RelationsStore;
use crate::schema::items_col;
use arrow::array::{Array, RecordBatch, StringArray};

/// Errors specific to HDD operations.
#[derive(Debug, thiserror::Error)]
pub enum HddError {
    #[error("CRUD error: {0}")]
    Crud(#[from] CrudError),

    #[error("Relation error: {0}")]
    Relation(#[from] crate::relations::RelationError),

    #[error("Paper not found: {0}")]
    PaperNotFound(u32),

    #[error("Hypothesis not found: {0}")]
    HypothesisNotFound(String),

    #[error("Experiment not found: {0}")]
    ExperimentNotFound(String),
}

pub type Result<T> = std::result::Result<T, HddError>;

/// Result of creating a research item — includes ID and any auto-created relations.
#[derive(Debug)]
pub struct HddCreateResult {
    /// The allocated ID (e.g., "H1300.1", "EXPR-1300.1", "PAPER-1300").
    pub id: String,
    /// IDs of auto-created relations.
    pub auto_links: Vec<String>,
}

/// Allocate a paper-scoped hypothesis ID: H{paper_num}.{seq}.
///
/// Scans existing items for IDs matching `H{paper_num}.*` and returns the next sequence.
fn allocate_hypothesis_id(batches: &[RecordBatch], paper_num: u32) -> String {
    let prefix = format!("H{}", paper_num);
    let mut max_seq = 0u32;

    for batch in batches {
        let ids = batch
            .column(items_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("id column should be StringArray");

        for i in 0..ids.len() {
            if ids.is_null(i) {
                continue;
            }
            let id_str = ids.value(i);
            // Match "H{paper_num}.{seq}" pattern
            if let Some(rest) = id_str.strip_prefix(&prefix)
                && let Some(seq_str) = rest.strip_prefix('.')
                && let Ok(seq) = seq_str.parse::<u32>()
                && seq > max_seq
            {
                max_seq = seq;
            }
        }
    }

    format!("H{}.{}", paper_num, max_seq + 1)
}

/// Allocate a paper-scoped experiment ID: EXPR-{paper_num}.{seq}.
fn allocate_experiment_id(batches: &[RecordBatch], paper_num: u32) -> String {
    let prefix = format!("EXPR-{}", paper_num);
    let mut max_seq = 0u32;

    for batch in batches {
        let ids = batch
            .column(items_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("id column should be StringArray");

        for i in 0..ids.len() {
            if ids.is_null(i) {
                continue;
            }
            let id_str = ids.value(i);
            // Match "EXPR-{paper_num}.{seq}" pattern
            if let Some(rest) = id_str.strip_prefix(&prefix)
                && let Some(seq_str) = rest.strip_prefix('.')
                && let Ok(seq) = seq_str.parse::<u32>()
                && seq > max_seq
            {
                max_seq = seq;
            }
        }
    }

    format!("EXPR-{}.{}", paper_num, max_seq + 1)
}

/// Extract the paper number from a paper ID like "PAPER-1300".
#[cfg(test)]
fn parse_paper_num(paper_id: &str) -> Option<u32> {
    paper_id.strip_prefix("PAPER-")?.parse::<u32>().ok()
}

/// Create a paper on the research board.
pub fn create_paper(
    store: &mut KanbanStore,
    title: &str,
    tags: Vec<String>,
) -> Result<HddCreateResult> {
    let input = CreateItemInput {
        title: title.to_string(),
        item_type: ItemType::Paper,
        priority: Some("medium".to_string()),
        assignee: None,
        tags,
        related: vec![],
        depends_on: vec![],
        body: None,
    };
    let id = store.create_item(&input)?;
    Ok(HddCreateResult {
        id,
        auto_links: vec![],
    })
}

/// Create a hypothesis linked to a paper.
///
/// ID format: H{paper_num}.{seq} (e.g., H130.1, H130.2).
/// Auto-links: hypothesis → paper via "tests" predicate.
pub fn create_hypothesis(
    store: &mut KanbanStore,
    relations: &mut RelationsStore,
    title: &str,
    paper_num: u32,
    tags: Vec<String>,
) -> Result<HddCreateResult> {
    // Verify paper exists
    let paper_id = format!("PAPER-{}", paper_num);
    store
        .get_item(&paper_id)
        .map_err(|_| HddError::PaperNotFound(paper_num))?;

    // Allocate paper-scoped ID
    let hyp_id = allocate_hypothesis_id(store.items_batches(), paper_num);

    // Create the item with the custom ID
    let input = CreateItemInput {
        title: title.to_string(),
        item_type: ItemType::Hypothesis,
        priority: Some("medium".to_string()),
        assignee: None,
        tags,
        related: vec![paper_id.clone()],
        depends_on: vec![],
        body: None,
    };

    // Use create_item_with_id for paper-scoped IDs (H130.1 format)
    let id = store.create_item_with_id(&hyp_id, &input)?;

    // Auto-link hypothesis → paper
    let rel_id = relations.add_relation(&id, &paper_id, "tests")?;

    Ok(HddCreateResult {
        id,
        auto_links: vec![rel_id],
    })
}

/// Create an experiment linked to a hypothesis.
///
/// ID format: EXPR-{paper_num}.{seq} (e.g., EXPR-131.1, EXPR-131.2).
/// The paper_num is derived from the hypothesis ID (e.g., H131.1 → paper 131).
/// Auto-links: experiment → hypothesis via "validates" predicate.
pub fn create_experiment(
    store: &mut KanbanStore,
    relations: &mut RelationsStore,
    title: &str,
    hypothesis_id: &str,
    tags: Vec<String>,
) -> Result<HddCreateResult> {
    // Verify hypothesis exists
    store
        .get_item(hypothesis_id)
        .map_err(|_| HddError::HypothesisNotFound(hypothesis_id.to_string()))?;

    // Extract paper number from hypothesis ID (H{paper_num}.{seq})
    let paper_num = hypothesis_id
        .strip_prefix('H')
        .and_then(|rest| rest.split('.').next())
        .and_then(|num_str| num_str.parse::<u32>().ok())
        .ok_or_else(|| HddError::HypothesisNotFound(hypothesis_id.to_string()))?;

    // Allocate paper-scoped experiment ID
    let expr_id = allocate_experiment_id(store.items_batches(), paper_num);

    let input = CreateItemInput {
        title: title.to_string(),
        item_type: ItemType::Experiment,
        priority: Some("medium".to_string()),
        assignee: None,
        tags,
        related: vec![hypothesis_id.to_string()],
        depends_on: vec![],
        body: None,
    };

    // Use create_item_with_id for paper-scoped IDs (EXPR-131.1 format)
    let id = store.create_item_with_id(&expr_id, &input)?;

    // Auto-link experiment → hypothesis
    let rel_id = relations.add_relation(&id, hypothesis_id, "validates")?;

    Ok(HddCreateResult {
        id,
        auto_links: vec![rel_id],
    })
}

/// Create a measure linked to an experiment.
///
/// Auto-links: measure → experiment via "measures" predicate.
pub fn create_measure(
    store: &mut KanbanStore,
    relations: &mut RelationsStore,
    title: &str,
    experiment_id: Option<&str>,
    tags: Vec<String>,
) -> Result<HddCreateResult> {
    if let Some(expr_id) = experiment_id {
        store
            .get_item(expr_id)
            .map_err(|_| HddError::ExperimentNotFound(expr_id.to_string()))?;
    }

    let input = CreateItemInput {
        title: title.to_string(),
        item_type: ItemType::Measure,
        priority: Some("medium".to_string()),
        assignee: None,
        tags,
        related: experiment_id
            .map(|e| vec![e.to_string()])
            .unwrap_or_default(),
        depends_on: vec![],
        body: None,
    };

    let id = store.create_item(&input)?;

    let mut auto_links = vec![];
    if let Some(expr_id) = experiment_id {
        let rel_id = relations.add_relation(&id, expr_id, "measures")?;
        auto_links.push(rel_id);
    }

    Ok(HddCreateResult { id, auto_links })
}

/// Create an idea (standalone, no auto-linking).
pub fn create_idea(
    store: &mut KanbanStore,
    title: &str,
    tags: Vec<String>,
) -> Result<HddCreateResult> {
    let input = CreateItemInput {
        title: title.to_string(),
        item_type: ItemType::Idea,
        priority: Some("low".to_string()),
        assignee: None,
        tags,
        related: vec![],
        depends_on: vec![],
        body: None,
    };
    let id = store.create_item(&input)?;
    Ok(HddCreateResult {
        id,
        auto_links: vec![],
    })
}

/// Create a literature reference (standalone, no auto-linking).
pub fn create_literature(
    store: &mut KanbanStore,
    title: &str,
    tags: Vec<String>,
) -> Result<HddCreateResult> {
    let input = CreateItemInput {
        title: title.to_string(),
        item_type: ItemType::Literature,
        priority: Some("medium".to_string()),
        assignee: None,
        tags,
        related: vec![],
        depends_on: vec![],
        body: None,
    };
    let id = store.create_item(&input)?;
    Ok(HddCreateResult {
        id,
        auto_links: vec![],
    })
}

/// Validate the HDD research board for integrity issues.
///
/// Returns a list of validation errors found.
pub fn validate_hdd(store: &KanbanStore, relations: &RelationsStore) -> Vec<String> {
    let mut errors = Vec::new();

    // Check for orphaned hypotheses (no "tests" relation to a paper)
    let hypotheses = store.query_items(None, Some("hypothesis"), Some("research"), None);
    for batch in &hypotheses {
        let ids = batch
            .column(items_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("id column");
        for i in 0..batch.num_rows() {
            let id = ids.value(i);
            let rels = relations.query_relations(id);
            let has_paper_link = rels.iter().any(|r| {
                let preds = r
                    .column(crate::schema::rel_col::PREDICATE)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("predicate");
                preds.value(0) == "tests"
            });
            if !has_paper_link {
                errors.push(format!("Orphaned hypothesis: {} has no paper link", id));
            }
        }
    }

    // Check for orphaned experiments (no "validates" relation to a hypothesis)
    let experiments = store.query_items(None, Some("experiment"), Some("research"), None);
    for batch in &experiments {
        let ids = batch
            .column(items_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("id column");
        for i in 0..batch.num_rows() {
            let id = ids.value(i);
            let rels = relations.query_relations(id);
            let has_hyp_link = rels.iter().any(|r| {
                let preds = r
                    .column(crate::schema::rel_col::PREDICATE)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("predicate");
                preds.value(0) == "validates"
            });
            if !has_hyp_link {
                errors.push(format!(
                    "Orphaned experiment: {} has no hypothesis link",
                    id
                ));
            }
        }
    }

    // Check for broken depends_on references
    let all_items = store.query_items(None, None, None, None);
    let all_ids: Vec<String> = all_items
        .iter()
        .flat_map(|batch| {
            let ids = batch
                .column(items_col::ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("id column");
            (0..batch.num_rows()).map(move |i| ids.value(i).to_string())
        })
        .collect();

    for batch in &all_items {
        let ids = batch
            .column(items_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("id");
        let depends = batch
            .column(items_col::DEPENDS_ON)
            .as_any()
            .downcast_ref::<arrow::array::ListArray>()
            .expect("depends_on");

        for i in 0..batch.num_rows() {
            let item_id = ids.value(i);
            if !depends.is_null(i) {
                let deps = depends.value(i);
                let dep_strings = deps
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("dep strings");
                for j in 0..dep_strings.len() {
                    if !dep_strings.is_null(j) {
                        let dep_id = dep_strings.value(j);
                        if !dep_id.is_empty() && !all_ids.contains(&dep_id.to_string()) {
                            errors.push(format!(
                                "Broken dependency: {} depends on non-existent {}",
                                item_id, dep_id
                            ));
                        }
                    }
                }
            }
        }
    }

    errors
}

/// Build a registry view: paper → hypothesis → experiment → measure chains.
///
/// Returns a list of chain descriptions.
pub fn build_registry(store: &KanbanStore, relations: &RelationsStore) -> Vec<RegistryChain> {
    let mut chains = Vec::new();

    let papers = store.query_items(None, Some("paper"), Some("research"), None);
    for batch in &papers {
        let ids = batch
            .column(items_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("id");
        let titles = batch
            .column(items_col::TITLE)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("title");

        for i in 0..batch.num_rows() {
            let paper_id = ids.value(i).to_string();
            let paper_title = titles.value(i).to_string();

            // Find hypotheses testing this paper
            let hyp_ids = find_related_by_predicate(relations, &paper_id, "tests");

            let mut hypotheses = Vec::new();
            for hyp_id in &hyp_ids {
                let hyp_title = get_title(store, hyp_id).unwrap_or_default();

                // Find experiments validating this hypothesis
                let expr_ids = find_related_by_predicate(relations, hyp_id, "validates");
                let mut experiments = Vec::new();
                for expr_id in &expr_ids {
                    let expr_title = get_title(store, expr_id).unwrap_or_default();

                    // Find measures for this experiment
                    let measure_ids = find_related_by_predicate(relations, expr_id, "measures");
                    let measures: Vec<RegistryItem> = measure_ids
                        .iter()
                        .map(|m_id| RegistryItem {
                            id: m_id.clone(),
                            title: get_title(store, m_id).unwrap_or_default(),
                        })
                        .collect();

                    experiments.push(RegistryExperiment {
                        id: expr_id.clone(),
                        title: expr_title,
                        measures,
                    });
                }

                hypotheses.push(RegistryHypothesis {
                    id: hyp_id.clone(),
                    title: hyp_title,
                    experiments,
                });
            }

            chains.push(RegistryChain {
                paper_id,
                paper_title,
                hypotheses,
            });
        }
    }

    chains
}

/// A paper → hypothesis → experiment → measure chain.
#[derive(Debug)]
pub struct RegistryChain {
    pub paper_id: String,
    pub paper_title: String,
    pub hypotheses: Vec<RegistryHypothesis>,
}

#[derive(Debug)]
pub struct RegistryHypothesis {
    pub id: String,
    pub title: String,
    pub experiments: Vec<RegistryExperiment>,
}

#[derive(Debug)]
pub struct RegistryExperiment {
    pub id: String,
    pub title: String,
    pub measures: Vec<RegistryItem>,
}

#[derive(Debug)]
pub struct RegistryItem {
    pub id: String,
    pub title: String,
}

/// Query experiment queue: items with experiment type filtered by run status.
///
/// Returns experiments that match the given run_status filter.
/// If `ready_only` is true, returns only experiments that have no unresolved blockers.
pub fn query_experiment_queue(
    store: &KanbanStore,
    relations: &RelationsStore,
    status_filter: Option<&str>,
    ready_only: bool,
) -> Vec<RecordBatch> {
    let experiments = store.query_items(None, Some("experiment"), Some("research"), None);

    if !ready_only && status_filter.is_none() {
        return experiments;
    }

    let mut results = Vec::new();
    for batch in &experiments {
        let ids = batch
            .column(items_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("id");
        let statuses = batch
            .column(items_col::STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("status");

        for i in 0..batch.num_rows() {
            let id = ids.value(i);
            let status = statuses.value(i);

            // Filter by status if requested
            if let Some(filter) = status_filter
                && status != filter
            {
                continue;
            }

            // If ready_only, check that no blockers exist
            if ready_only {
                let rels = relations.query_relations(id);
                let has_unresolved_blocker = rels.iter().any(|r| {
                    let preds = r
                        .column(crate::schema::rel_col::PREDICATE)
                        .as_any()
                        .downcast_ref::<StringArray>()
                        .expect("predicate");
                    preds.value(0) == "blocked_by"
                });
                if has_unresolved_blocker {
                    continue;
                }
            }

            results.push(batch.slice(i, 1));
        }
    }

    results
}

/// Traverse relations N levels deep from a starting item.
///
/// E.g., given PAPER-130 and predicate "tests", returns all hypotheses.
/// With depth=2 and predicates ["tests", "validates"], follows paper→hypothesis→experiment.
pub fn traverse_relations(
    relations: &RelationsStore,
    start_id: &str,
    predicates: &[&str],
    max_depth: usize,
) -> Vec<String> {
    let mut results = Vec::new();
    let mut current_ids = vec![start_id.to_string()];

    for depth in 0..max_depth {
        let predicate = if depth < predicates.len() {
            predicates[depth]
        } else {
            return results;
        };

        let mut next_ids = Vec::new();
        for id in &current_ids {
            let rels = relations.query_relations(id);
            for rel_batch in &rels {
                let sources = rel_batch
                    .column(crate::schema::rel_col::SOURCE_ID)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("source_id");
                let targets = rel_batch
                    .column(crate::schema::rel_col::TARGET_ID)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("target_id");
                let preds = rel_batch
                    .column(crate::schema::rel_col::PREDICATE)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("predicate");

                for i in 0..rel_batch.num_rows() {
                    if preds.value(i) == predicate {
                        // The related item is whichever end isn't our current ID
                        let other = if sources.value(i) == id.as_str() {
                            targets.value(i)
                        } else {
                            sources.value(i)
                        };
                        let other_str = other.to_string();
                        if !results.contains(&other_str) {
                            results.push(other_str.clone());
                            next_ids.push(other_str);
                        }
                    }
                }
            }
        }

        current_ids = next_ids;
    }

    results
}

// --- Internal helpers ---

/// Find items related to `item_id` via a specific predicate.
fn find_related_by_predicate(
    relations: &RelationsStore,
    item_id: &str,
    predicate: &str,
) -> Vec<String> {
    let rels = relations.query_relations(item_id);
    let mut related = Vec::new();

    for batch in &rels {
        let sources = batch
            .column(crate::schema::rel_col::SOURCE_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("source_id");
        let targets = batch
            .column(crate::schema::rel_col::TARGET_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("target_id");
        let preds = batch
            .column(crate::schema::rel_col::PREDICATE)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("predicate");

        for i in 0..batch.num_rows() {
            if preds.value(i) == predicate {
                let other = if sources.value(i) == item_id {
                    targets.value(i)
                } else {
                    sources.value(i)
                };
                related.push(other.to_string());
            }
        }
    }

    related
}

/// Get the title of an item by ID.
fn get_title(store: &KanbanStore, id: &str) -> Option<String> {
    store.get_item(id).ok().map(|batch| {
        batch
            .column(items_col::TITLE)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("title")
            .value(0)
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (KanbanStore, RelationsStore) {
        (KanbanStore::new(), RelationsStore::new())
    }

    // --- Phase 1: Create commands ---

    #[test]
    fn test_create_paper() {
        let (mut store, _rels) = setup();
        let result =
            create_paper(&mut store, "Cognitive Signal Fusion", vec!["v12".into()]).unwrap();
        assert!(result.id.starts_with("PAPER-"));
        assert!(result.auto_links.is_empty());
        assert_eq!(store.active_item_count(), 1);
    }

    #[test]
    fn test_create_hypothesis_with_paper_scoped_id() {
        let (mut store, mut rels) = setup();
        let paper = create_paper(&mut store, "Test Paper", vec![]).unwrap();
        let paper_num = parse_paper_num(&paper.id).unwrap();

        let hyp = create_hypothesis(
            &mut store,
            &mut rels,
            "Unified perceive() maintains quality",
            paper_num,
            vec![],
        )
        .unwrap();

        // ID should be H{paper_num}.1
        assert_eq!(hyp.id, format!("H{}.1", paper_num));
        assert_eq!(hyp.auto_links.len(), 1); // tests relation

        // Verify relation exists
        let paper_rels = rels.query_relations(&paper.id);
        assert_eq!(paper_rels.len(), 1);
    }

    #[test]
    fn test_hypothesis_sequential_scoped_ids() {
        let (mut store, mut rels) = setup();
        let paper = create_paper(&mut store, "Paper 130", vec![]).unwrap();
        let paper_num = parse_paper_num(&paper.id).unwrap();

        let h1 = create_hypothesis(&mut store, &mut rels, "H1", paper_num, vec![]).unwrap();
        let h2 = create_hypothesis(&mut store, &mut rels, "H2", paper_num, vec![]).unwrap();

        assert_eq!(h1.id, format!("H{}.1", paper_num));
        assert_eq!(h2.id, format!("H{}.2", paper_num));
    }

    #[test]
    fn test_create_hypothesis_paper_not_found() {
        let (mut store, mut rels) = setup();
        let result = create_hypothesis(&mut store, &mut rels, "Orphan", 999, vec![]);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_experiment_with_paper_scoped_id() {
        let (mut store, mut rels) = setup();
        let paper = create_paper(&mut store, "Paper", vec![]).unwrap();
        let paper_num = parse_paper_num(&paper.id).unwrap();
        let hyp = create_hypothesis(&mut store, &mut rels, "Hyp", paper_num, vec![]).unwrap();

        let expr =
            create_experiment(&mut store, &mut rels, "Test Experiment", &hyp.id, vec![]).unwrap();

        assert!(expr.id.starts_with(&format!("EXPR-{}.", paper_num)));
        assert_eq!(expr.auto_links.len(), 1); // validates relation

        // Verify relation
        let hyp_rels = rels.query_relations(&hyp.id);
        assert!(hyp_rels.len() >= 1); // at least the validates relation
    }

    #[test]
    fn test_create_experiment_hypothesis_not_found() {
        let (mut store, mut rels) = setup();
        let result = create_experiment(&mut store, &mut rels, "Test", "H999.1", vec![]);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_measure_with_experiment_link() {
        let (mut store, mut rels) = setup();
        let paper = create_paper(&mut store, "Paper", vec![]).unwrap();
        let paper_num = parse_paper_num(&paper.id).unwrap();
        let hyp = create_hypothesis(&mut store, &mut rels, "Hyp", paper_num, vec![]).unwrap();
        let expr = create_experiment(&mut store, &mut rels, "Expr", &hyp.id, vec![]).unwrap();

        let measure =
            create_measure(&mut store, &mut rels, "Latency", Some(&expr.id), vec![]).unwrap();

        assert!(measure.id.starts_with("M-"));
        assert_eq!(measure.auto_links.len(), 1);
    }

    #[test]
    fn test_create_measure_standalone() {
        let (mut store, mut rels) = setup();
        let measure =
            create_measure(&mut store, &mut rels, "Standalone Metric", None, vec![]).unwrap();
        assert!(measure.id.starts_with("M-"));
        assert!(measure.auto_links.is_empty());
    }

    #[test]
    fn test_create_idea() {
        let (mut store, _rels) = setup();
        let result = create_idea(&mut store, "Three modes of knowledge", vec![]).unwrap();
        assert!(result.id.starts_with("IDEA-"));
    }

    #[test]
    fn test_create_literature() {
        let (mut store, _rels) = setup();
        let result = create_literature(&mut store, "KEPLER Survey", vec![]).unwrap();
        assert!(result.id.starts_with("LIT-"));
    }

    #[test]
    fn test_all_auto_links_in_relations_table() {
        let (mut store, mut rels) = setup();
        let paper = create_paper(&mut store, "Paper", vec![]).unwrap();
        let paper_num = parse_paper_num(&paper.id).unwrap();
        let hyp = create_hypothesis(&mut store, &mut rels, "Hyp", paper_num, vec![]).unwrap();
        let expr = create_experiment(&mut store, &mut rels, "Expr", &hyp.id, vec![]).unwrap();
        let _measure =
            create_measure(&mut store, &mut rels, "Measure", Some(&expr.id), vec![]).unwrap();

        // 3 relations: tests, validates, measures
        assert_eq!(rels.active_count(), 3);
    }

    // --- Phase 2: Validation ---

    #[test]
    fn test_validate_clean_board() {
        let (mut store, mut rels) = setup();
        let paper = create_paper(&mut store, "Paper", vec![]).unwrap();
        let paper_num = parse_paper_num(&paper.id).unwrap();
        let hyp = create_hypothesis(&mut store, &mut rels, "Hyp", paper_num, vec![]).unwrap();
        let _expr = create_experiment(&mut store, &mut rels, "Expr", &hyp.id, vec![]).unwrap();

        let errors = validate_hdd(&store, &rels);
        assert!(
            errors.is_empty(),
            "Clean board should have no errors: {:?}",
            errors
        );
    }

    #[test]
    fn test_validate_catches_orphan_hypothesis() {
        let (mut store, rels) = setup();
        // Create hypothesis without paper link (directly via CRUD)
        let input = CreateItemInput {
            title: "Orphan Hyp".into(),
            item_type: ItemType::Hypothesis,
            priority: None,
            assignee: None,
            tags: vec![],
            related: vec![],
            depends_on: vec![],
            body: None,
        };
        store.create_item(&input).unwrap();

        let errors = validate_hdd(&store, &rels);
        assert!(errors.iter().any(|e| e.contains("Orphaned hypothesis")));
    }

    #[test]
    fn test_validate_catches_orphan_experiment() {
        let (mut store, rels) = setup();
        let input = CreateItemInput {
            title: "Orphan Expr".into(),
            item_type: ItemType::Experiment,
            priority: None,
            assignee: None,
            tags: vec![],
            related: vec![],
            depends_on: vec![],
            body: None,
        };
        store.create_item(&input).unwrap();

        let errors = validate_hdd(&store, &rels);
        assert!(errors.iter().any(|e| e.contains("Orphaned experiment")));
    }

    // --- Phase 2: Registry ---

    #[test]
    fn test_registry_shows_full_chain() {
        let (mut store, mut rels) = setup();
        let paper = create_paper(&mut store, "Signal Fusion Paper", vec![]).unwrap();
        let paper_num = parse_paper_num(&paper.id).unwrap();
        let hyp = create_hypothesis(
            &mut store,
            &mut rels,
            "Fusion Hypothesis",
            paper_num,
            vec![],
        )
        .unwrap();
        let expr =
            create_experiment(&mut store, &mut rels, "Fusion Experiment", &hyp.id, vec![]).unwrap();
        let _measure = create_measure(
            &mut store,
            &mut rels,
            "Latency Metric",
            Some(&expr.id),
            vec![],
        )
        .unwrap();

        let chains = build_registry(&store, &rels);
        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0].paper_title, "Signal Fusion Paper");
        assert_eq!(chains[0].hypotheses.len(), 1);
        assert_eq!(chains[0].hypotheses[0].experiments.len(), 1);
        assert_eq!(chains[0].hypotheses[0].experiments[0].measures.len(), 1);
    }

    // --- Phase 3: Queries ---

    #[test]
    fn test_traverse_paper_to_experiments() {
        let (mut store, mut rels) = setup();
        let paper = create_paper(&mut store, "Paper", vec![]).unwrap();
        let paper_num = parse_paper_num(&paper.id).unwrap();
        let hyp = create_hypothesis(&mut store, &mut rels, "Hyp", paper_num, vec![]).unwrap();
        let _expr = create_experiment(&mut store, &mut rels, "Expr", &hyp.id, vec![]).unwrap();

        // Traverse paper → hypothesis (1 level)
        let hypotheses = traverse_relations(&rels, &paper.id, &["tests"], 1);
        assert_eq!(hypotheses.len(), 1);

        // Traverse paper → hypothesis → experiment (2 levels)
        let experiments = traverse_relations(&rels, &paper.id, &["tests", "validates"], 2);
        assert_eq!(experiments.len(), 2); // hyp + expr
    }

    #[test]
    fn test_experiment_queue_all() {
        let (mut store, mut rels) = setup();
        let paper = create_paper(&mut store, "Paper", vec![]).unwrap();
        let paper_num = parse_paper_num(&paper.id).unwrap();
        let hyp = create_hypothesis(&mut store, &mut rels, "Hyp", paper_num, vec![]).unwrap();
        let _e1 = create_experiment(&mut store, &mut rels, "Expr 1", &hyp.id, vec![]).unwrap();
        let _e2 = create_experiment(&mut store, &mut rels, "Expr 2", &hyp.id, vec![]).unwrap();

        let queue = query_experiment_queue(&store, &rels, None, false);
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn test_experiment_queue_ready_only() {
        let (mut store, mut rels) = setup();
        let paper = create_paper(&mut store, "Paper", vec![]).unwrap();
        let paper_num = parse_paper_num(&paper.id).unwrap();
        let hyp = create_hypothesis(&mut store, &mut rels, "Hyp", paper_num, vec![]).unwrap();
        let e1 = create_experiment(&mut store, &mut rels, "Ready", &hyp.id, vec![]).unwrap();
        let e2 = create_experiment(&mut store, &mut rels, "Blocked", &hyp.id, vec![]).unwrap();

        // Add a blocker to e2
        rels.add_relation(&e2.id, "EXP-999", "blocked_by").unwrap();

        let ready = query_experiment_queue(&store, &rels, None, true);
        assert_eq!(ready.len(), 1);

        // Verify it's the non-blocked one
        let id = ready[0]
            .column(items_col::ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(0);
        assert_eq!(id, e1.id);
    }
}
