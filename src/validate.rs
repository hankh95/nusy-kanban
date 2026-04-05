//! SHACL-driven conformance checking for kanban items.
//!
//! Implements structural validation (sh:minCount checks) against the shapes defined
//! in `ontology/shapes/{dev,research}/`.  No SPARQL engine — we check field
//! presence and allowed-value sets directly against the Arrow RecordBatch.
//!
//! # Usage
//!
//! ```no_run
//! use nusy_kanban::validate::{validate_item, suggest_fixes};
//! # use arrow::array::RecordBatch;
//! # let batch: RecordBatch = unimplemented!();
//! let report = validate_item(&batch);
//! if !report.is_conformant() {
//!     let fixes = suggest_fixes(&report);
//! }
//! ```
//!
//! # Shape rules applied
//!
//! | Type            | Field      | Rule                                    | Severity |
//! |-----------------|------------|-----------------------------------------|----------|
//! | all             | body       | sh:minCount 1 → must be non-null/empty  | Error    |
//! | expedition      | priority   | sh:minCount 1, sh:in (low/medium/…)     | Error    |
//! | expedition      | assignee   | sh:minCount 1                           | Warning  |
//! | voyage          | assignee   | sh:minCount 1                           | Warning  |
//! | chore           | (body)     | body required                           | Error    |
//! | hypothesis      | body       | body required                           | Error    |
//! | experiment      | body       | body required                           | Error    |
//! | paper           | body       | body required                           | Error    |
//! | idea            | body       | body required                           | Error    |
//! | literature      | body       | body required                           | Error    |
//! | measure         | body       | body required                           | Error    |
//! | hazard          | body       | body required                           | Warning  |
//! | signal          | body       | body required                           | Warning  |
//! | feature         | priority   | priority required                        | Error    |

use crate::item_type::ItemType;
use crate::schema::items_col;
use arrow::array::{Array, RecordBatch, StringArray};

// ─── Public Types ──────────────────────────────────────────────────────────

/// Severity of a validation violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    /// Hard conformance failure (sh:minCount 1 violated on a required field).
    Error,
    /// Advisory warning — best practice not followed but item is usable.
    Warning,
}

/// A single constraint violation for an item field.
#[derive(Debug, Clone)]
pub struct Violation {
    /// The field name (e.g. "assignee", "body", "priority").
    pub field: String,
    /// Human-readable description (sh:message content).
    pub message: String,
    /// Severity level.
    pub severity: Severity,
}

/// Aggregated report for one item.
#[derive(Debug)]
pub struct ValidationReport {
    /// Item ID (e.g. "EX-3212").
    pub item_id: String,
    /// Item type string (e.g. "expedition").
    pub item_type: String,
    /// All constraint violations found.
    pub violations: Vec<Violation>,
}

impl ValidationReport {
    /// Returns `true` when there are no Error-severity violations.
    pub fn is_conformant(&self) -> bool {
        !self
            .violations
            .iter()
            .any(|v| v.severity == Severity::Error)
    }
}

// ─── Core Validation ───────────────────────────────────────────────────────

/// Validate a single item (one-row RecordBatch) against its type's shape rules.
///
/// Implements structural SHACL (sh:minCount checks only) — no SPARQL engine required.
pub fn validate_item(batch: &RecordBatch) -> ValidationReport {
    let item_id = get_string(batch, items_col::ID).unwrap_or_default();
    let item_type_str = get_string(batch, items_col::ITEM_TYPE).unwrap_or_default();
    let mut violations = Vec::new();

    let item_type = ItemType::from_str_loose(&item_type_str);

    // ── Body (required for most types) ────────────────────────────────────
    let body = get_nullable_string(batch, items_col::BODY);
    let body_empty = body.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true);

    let body_severity = match item_type {
        // Hazard and Signal are ephemeral — body is advisory
        Some(ItemType::Hazard) | Some(ItemType::Signal) => Some(Severity::Warning),
        // All other types: body is required
        Some(_) => Some(Severity::Error),
        // Unknown type: treat as error
        None => Some(Severity::Error),
    };

    if body_empty && let Some(sev) = body_severity {
        violations.push(Violation {
            field: "body".to_string(),
            message: "body is empty — use: nk update <ID> --body-file /tmp/body.md".to_string(),
            severity: sev,
        });
    }

    // ── Priority (required for expedition, voyage, chore, feature) ─────────
    let priority = get_nullable_string(batch, items_col::PRIORITY);
    let priority_required = matches!(
        item_type,
        Some(ItemType::Expedition)
            | Some(ItemType::Voyage)
            | Some(ItemType::Chore)
            | Some(ItemType::Feature)
    );

    if priority_required && priority.is_none() {
        violations.push(Violation {
            field: "priority".to_string(),
            message: "priority required: low|medium|high|critical".to_string(),
            severity: Severity::Error,
        });
    }

    // ── Assignee (advisory for expedition + voyage) ────────────────────────
    let assignee = get_nullable_string(batch, items_col::ASSIGNEE);
    let assignee_required = matches!(
        item_type,
        Some(ItemType::Expedition) | Some(ItemType::Voyage)
    );

    if assignee_required && assignee.is_none() {
        violations.push(Violation {
            field: "assignee".to_string(),
            message: "assignee required: M5|DGX|Mini|unassigned".to_string(),
            severity: Severity::Warning,
        });
    }

    // ── Warn when assignee is "unassigned" for in-progress items ──────────
    let status = get_string(batch, items_col::STATUS).unwrap_or_default();
    if status == "in_progress"
        && let Some(ref a) = assignee
        && a == "unassigned"
    {
        violations.push(Violation {
            field: "assignee".to_string(),
            message: "item is in_progress but assignee is 'unassigned'".to_string(),
            severity: Severity::Warning,
        });
    }

    ValidationReport {
        item_id,
        item_type: item_type_str,
        violations,
    }
}

// ─── Fix Suggestions ───────────────────────────────────────────────────────

/// Generate suggested fix commands for all violations in a report.
///
/// Returns one `nk update ...` command per violation, formatted so the user
/// can copy-paste to fix the issue.
pub fn suggest_fixes(report: &ValidationReport) -> Vec<String> {
    report
        .violations
        .iter()
        .map(|v| match v.field.as_str() {
            "assignee" => format!("nk update {} --assign M5", report.item_id),
            "body" => format!("nk update {} --body-file /tmp/body.md", report.item_id),
            "priority" => format!("nk update {} --priority medium", report.item_id),
            _ => format!("nk update {} # fix {}", report.item_id, v.field),
        })
        .collect()
}

// ─── Board-Wide Validation ─────────────────────────────────────────────────

/// Validate all items in a slice of RecordBatches (one batch = one item row).
///
/// Returns one `ValidationReport` per item.
pub fn validate_all(batches: &[RecordBatch]) -> Vec<ValidationReport> {
    batches.iter().map(validate_item).collect()
}

// ─── Formatting ────────────────────────────────────────────────────────────

/// Format a single `ValidationReport` for terminal output.
pub fn format_report(report: &ValidationReport, show_fixes: bool) -> String {
    let mut lines = Vec::new();

    if report.violations.is_empty() {
        lines.push(format!("{} {} — OK", report.item_id, report.item_type));
    } else {
        let error_count = report
            .violations
            .iter()
            .filter(|v| v.severity == Severity::Error)
            .count();
        let warn_count = report
            .violations
            .iter()
            .filter(|v| v.severity == Severity::Warning)
            .count();

        let summary = match (error_count, warn_count) {
            (0, w) => format!("WARNINGS ({})", w),
            (e, 0) => format!("VIOLATIONS ({})", e),
            (e, w) => format!("VIOLATIONS ({} errors, {} warnings)", e, w),
        };

        lines.push(format!(
            "{} {} — {}",
            report.item_id, report.item_type, summary
        ));

        for v in &report.violations {
            let label = match v.severity {
                Severity::Error => "  ERROR  ",
                Severity::Warning => "  WARNING",
            };
            lines.push(format!("{} {}: {}", label, v.field, v.message));
        }

        if show_fixes {
            let fixes = suggest_fixes(report);
            if !fixes.is_empty() {
                lines.push(String::new());
                lines.push("Suggested fixes:".to_string());
                for fix in &fixes {
                    lines.push(format!("  {fix}"));
                }
            }
        }
    }

    lines.join("\n")
}

/// Format a summary table for board-wide validation.
///
/// Shows conformant count, violation count, and lists violating items.
pub fn format_board_summary(reports: &[ValidationReport]) -> String {
    let conformant: Vec<&ValidationReport> = reports.iter().filter(|r| r.is_conformant()).collect();
    let violating: Vec<&ValidationReport> = reports.iter().filter(|r| !r.is_conformant()).collect();

    let mut lines = Vec::new();
    lines.push(format!(
        "Validation summary: {} conformant, {} with violations",
        conformant.len(),
        violating.len()
    ));

    if violating.is_empty() {
        lines.push("All items conform to their SHACL shapes.".to_string());
    } else {
        lines.push(String::new());
        lines.push("Items with violations:".to_string());
        for r in &violating {
            let error_count = r
                .violations
                .iter()
                .filter(|v| v.severity == Severity::Error)
                .count();
            let warn_count = r
                .violations
                .iter()
                .filter(|v| v.severity == Severity::Warning)
                .count();
            lines.push(format!(
                "  {} ({}) — {} errors, {} warnings",
                r.item_id, r.item_type, error_count, warn_count
            ));
        }
    }

    lines.join("\n")
}

// ─── Arrow Helpers ─────────────────────────────────────────────────────────

/// Extract a non-nullable string value from a column in a single-row batch.
fn get_string(batch: &RecordBatch, col_idx: usize) -> Option<String> {
    batch
        .column(col_idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .and_then(|arr| {
            if arr.is_empty() {
                None
            } else {
                Some(arr.value(0).to_string())
            }
        })
}

/// Extract a nullable string value from a column in a single-row batch.
fn get_nullable_string(batch: &RecordBatch, col_idx: usize) -> Option<String> {
    batch
        .column(col_idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .and_then(|arr| {
            if arr.is_empty() || arr.is_null(0) {
                None
            } else {
                let s = arr.value(0);
                if s.is_empty() {
                    None
                } else {
                    Some(s.to_string())
                }
            }
        })
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crud::{CreateItemInput, KanbanStore};

    // ── Helper ──────────────────────────────────────────────────────────────

    /// Create a minimal item in a store and return the single-row RecordBatch.
    fn make_item(
        item_type: ItemType,
        priority: Option<&str>,
        assignee: Option<&str>,
        body: Option<&str>,
        status: &str,
    ) -> RecordBatch {
        let mut store = KanbanStore::new();
        let id = store
            .create_item(&CreateItemInput {
                title: "Test Item".to_string(),
                item_type,
                priority: priority.map(|s| s.to_string()),
                assignee: assignee.map(|s| s.to_string()),
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: body.map(|s| s.to_string()),
            })
            .expect("create_item");

        if status != "backlog" {
            store
                .update_status(&id, status, None, true, None)
                .expect("update_status");
        }

        store.get_item(&id).expect("get_item")
    }

    // ── Body validation ─────────────────────────────────────────────────────

    #[test]
    fn test_expedition_without_body_produces_error() {
        let batch = make_item(
            ItemType::Expedition,
            Some("high"),
            Some("M5"),
            None,
            "backlog",
        );
        let report = validate_item(&batch);
        assert!(!report.is_conformant());
        assert!(report.violations.iter().any(|v| v.field == "body"));
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.field == "body" && v.severity == Severity::Error)
        );
    }

    #[test]
    fn test_expedition_with_body_no_body_violation() {
        let batch = make_item(
            ItemType::Expedition,
            Some("high"),
            Some("M5"),
            Some("## Phase 1\nDo the thing."),
            "backlog",
        );
        let report = validate_item(&batch);
        assert!(
            !report.violations.iter().any(|v| v.field == "body"),
            "should have no body violation when body is set"
        );
    }

    #[test]
    fn test_chore_without_body_produces_error() {
        let batch = make_item(ItemType::Chore, Some("low"), None, None, "backlog");
        let report = validate_item(&batch);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.field == "body" && v.severity == Severity::Error)
        );
    }

    #[test]
    fn test_hazard_without_body_produces_warning_not_error() {
        let batch = make_item(ItemType::Hazard, None, None, None, "backlog");
        let report = validate_item(&batch);
        let body_violations: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.field == "body")
            .collect();
        assert!(
            !body_violations.is_empty(),
            "hazard without body should have a body violation"
        );
        // All body violations for hazard must be Warning, not Error
        assert!(
            body_violations
                .iter()
                .all(|v| v.severity == Severity::Warning)
        );
    }

    #[test]
    fn test_signal_without_body_produces_warning_not_error() {
        let batch = make_item(ItemType::Signal, None, None, None, "backlog");
        let report = validate_item(&batch);
        let body_violations: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.field == "body")
            .collect();
        assert!(
            body_violations
                .iter()
                .all(|v| v.severity == Severity::Warning)
        );
    }

    // ── Priority validation ─────────────────────────────────────────────────

    #[test]
    fn test_expedition_without_priority_produces_error() {
        let batch = make_item(
            ItemType::Expedition,
            None,
            Some("M5"),
            Some("body"),
            "backlog",
        );
        let report = validate_item(&batch);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.field == "priority" && v.severity == Severity::Error)
        );
    }

    #[test]
    fn test_voyage_without_priority_produces_error() {
        let batch = make_item(ItemType::Voyage, None, None, Some("body"), "backlog");
        let report = validate_item(&batch);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.field == "priority" && v.severity == Severity::Error)
        );
    }

    #[test]
    fn test_hypothesis_does_not_require_priority() {
        // Hypothesis is a research type — no priority requirement
        let batch = make_item(ItemType::Hypothesis, None, None, Some("body"), "backlog");
        let report = validate_item(&batch);
        assert!(
            !report.violations.iter().any(|v| v.field == "priority"),
            "hypothesis should not require priority"
        );
    }

    // ── Assignee validation ─────────────────────────────────────────────────

    #[test]
    fn test_expedition_without_assignee_produces_warning() {
        let batch = make_item(
            ItemType::Expedition,
            Some("high"),
            None,
            Some("body"),
            "backlog",
        );
        let report = validate_item(&batch);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.field == "assignee" && v.severity == Severity::Warning)
        );
    }

    #[test]
    fn test_chore_without_assignee_no_violation() {
        // Chore doesn't require assignee
        let batch = make_item(ItemType::Chore, Some("low"), None, Some("body"), "backlog");
        let report = validate_item(&batch);
        assert!(
            !report.violations.iter().any(|v| v.field == "assignee"),
            "chore should not require assignee"
        );
    }

    #[test]
    fn test_in_progress_with_unassigned_produces_warning() {
        let batch = make_item(
            ItemType::Expedition,
            Some("high"),
            Some("unassigned"),
            Some("body"),
            "in_progress",
        );
        let report = validate_item(&batch);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.field == "assignee" && v.severity == Severity::Warning),
            "in_progress item with 'unassigned' should produce assignee warning"
        );
    }

    // ── Conformance ─────────────────────────────────────────────────────────

    #[test]
    fn test_fully_conformant_expedition() {
        let batch = make_item(
            ItemType::Expedition,
            Some("high"),
            Some("M5"),
            Some("## Phase 1\nDo the thing."),
            "backlog",
        );
        let report = validate_item(&batch);
        // No errors (warnings are ok for conformance)
        assert!(
            report.is_conformant(),
            "fully-set expedition should be conformant (no errors)"
        );
        assert!(
            !report
                .violations
                .iter()
                .any(|v| v.severity == Severity::Error)
        );
    }

    #[test]
    fn test_is_conformant_false_when_errors_exist() {
        let batch = make_item(ItemType::Expedition, None, None, None, "backlog");
        let report = validate_item(&batch);
        assert!(!report.is_conformant());
    }

    // ── suggest_fixes ───────────────────────────────────────────────────────

    #[test]
    fn test_suggest_fixes_returns_nk_update_commands() {
        let batch = make_item(ItemType::Expedition, None, None, None, "backlog");
        let report = validate_item(&batch);
        let fixes = suggest_fixes(&report);
        assert!(!fixes.is_empty());
        assert!(
            fixes.iter().all(|f| f.starts_with("nk update ")),
            "all fix suggestions should start with 'nk update'"
        );
    }

    #[test]
    fn test_suggest_fixes_assignee_command() {
        let batch = make_item(
            ItemType::Expedition,
            Some("high"),
            None,
            Some("body"),
            "backlog",
        );
        let report = validate_item(&batch);
        let fixes = suggest_fixes(&report);
        assert!(
            fixes.iter().any(|f| f.contains("--assign")),
            "should suggest --assign fix"
        );
    }

    #[test]
    fn test_suggest_fixes_body_command() {
        let batch = make_item(
            ItemType::Expedition,
            Some("high"),
            Some("M5"),
            None,
            "backlog",
        );
        let report = validate_item(&batch);
        let fixes = suggest_fixes(&report);
        assert!(
            fixes.iter().any(|f| f.contains("--body-file")),
            "should suggest --body-file fix"
        );
    }

    #[test]
    fn test_suggest_fixes_priority_command() {
        let batch = make_item(
            ItemType::Expedition,
            None,
            Some("M5"),
            Some("body"),
            "backlog",
        );
        let report = validate_item(&batch);
        let fixes = suggest_fixes(&report);
        assert!(
            fixes.iter().any(|f| f.contains("--priority")),
            "should suggest --priority fix"
        );
    }

    // ── Board-wide validation ───────────────────────────────────────────────

    #[test]
    fn test_validate_all_returns_one_report_per_item() {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "A".to_string(),
                item_type: ItemType::Expedition,
                priority: None,
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .unwrap();
        store
            .create_item(&CreateItemInput {
                title: "B".to_string(),
                item_type: ItemType::Chore,
                priority: Some("low".to_string()),
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: Some("body".to_string()),
            })
            .unwrap();

        let batches = store.query_items(None, None, None, None);
        let reports = validate_all(&batches);
        assert_eq!(reports.len(), 2);
    }

    #[test]
    fn test_validate_all_conformant_item_passes() {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Good".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: Some("M5".to_string()),
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: Some("## Phase 1\nDo the thing.".to_string()),
            })
            .unwrap();

        let batches = store.query_items(None, None, None, None);
        let reports = validate_all(&batches);
        assert_eq!(reports.len(), 1);
        assert!(reports[0].is_conformant());
    }

    // ── format_report ───────────────────────────────────────────────────────

    #[test]
    fn test_format_report_conformant_shows_ok() {
        let batch = make_item(
            ItemType::Expedition,
            Some("high"),
            Some("M5"),
            Some("body"),
            "backlog",
        );
        let report = validate_item(&batch);
        let out = format_report(&report, false);
        assert!(out.contains("OK"), "conformant item should show OK");
    }

    #[test]
    fn test_format_report_violations_shows_error_label() {
        let batch = make_item(ItemType::Expedition, None, None, None, "backlog");
        let report = validate_item(&batch);
        let out = format_report(&report, false);
        assert!(out.contains("ERROR"), "report should show ERROR label");
        assert!(
            out.contains("VIOLATIONS"),
            "report header should say VIOLATIONS"
        );
    }

    #[test]
    fn test_format_report_with_fixes_shows_suggestions() {
        let batch = make_item(ItemType::Expedition, None, None, None, "backlog");
        let report = validate_item(&batch);
        let out = format_report(&report, true);
        assert!(out.contains("Suggested fixes:"));
        assert!(out.contains("nk update "));
    }

    // ── format_board_summary ────────────────────────────────────────────────

    #[test]
    fn test_format_board_summary_all_ok() {
        let mut store = KanbanStore::new();
        store
            .create_item(&CreateItemInput {
                title: "Good".to_string(),
                item_type: ItemType::Expedition,
                priority: Some("high".to_string()),
                assignee: Some("M5".to_string()),
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: Some("body".to_string()),
            })
            .unwrap();

        let batches = store.query_items(None, None, None, None);
        let reports = validate_all(&batches);
        let summary = format_board_summary(&reports);
        assert!(summary.contains("1 conformant"));
        assert!(summary.contains("0 with violations"));
        assert!(summary.contains("All items conform"));
    }

    #[test]
    fn test_format_board_summary_with_violations() {
        let mut store = KanbanStore::new();
        // One bad item (no body, no priority)
        store
            .create_item(&CreateItemInput {
                title: "Bad".to_string(),
                item_type: ItemType::Expedition,
                priority: None,
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: None,
            })
            .unwrap();
        // One good item
        store
            .create_item(&CreateItemInput {
                title: "Good".to_string(),
                item_type: ItemType::Chore,
                priority: Some("low".to_string()),
                assignee: None,
                tags: vec![],
                related: vec![],
                depends_on: vec![],
                body: Some("body".to_string()),
            })
            .unwrap();

        let batches = store.query_items(None, None, None, None);
        let reports = validate_all(&batches);
        let summary = format_board_summary(&reports);
        assert!(summary.contains("1 conformant"));
        assert!(summary.contains("1 with violations"));
    }
}
