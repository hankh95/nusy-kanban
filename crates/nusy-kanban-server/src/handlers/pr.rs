//! PR / Proposal lifecycle handlers.

use super::{error_response, parse_payload, serialize_response};
use nusy_graph_review::{
    CiResultInput, CiResultStore, CiStatus, CommentStore, CreateProposalInput, ProposalStore,
};
use serde::Deserialize;

/// Normalize a proposal ID: accepts "2208" or "PROP-2208", always returns "PROP-2208".
fn normalize_proposal_id(id: &str) -> String {
    if id.starts_with("PROP-") || id.starts_with("prop-") {
        id.to_uppercase()
    } else {
        format!("PROP-{}", id)
    }
}

#[derive(Deserialize)]
struct PrCreateRequest {
    title: String,
    base: Option<String>,
    body: Option<String>,
    source_branch: Option<String>,
    agent_name: Option<String>,
}

pub(crate) fn handle_pr_create(
    payload: &[u8],
    proposals: &mut ProposalStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let req: PrCreateRequest = parse_payload(payload)?;
    let source = req.source_branch.as_deref().unwrap_or("unknown");
    let target = req.base.as_deref().unwrap_or("main");

    let author = req.agent_name.as_deref().unwrap_or("agent");
    let input = CreateProposalInput {
        title: &req.title,
        description: req.body.as_deref(),
        author,
        source_branch: source,
        target_branch: target,
        proposal_type: "code_change",
        namespace: "work",
    };

    let id = proposals
        .create_proposal(&input)
        .map_err(|e| error_response(&format!("{e}"), "PR_CREATE_FAILED"))?;
    proposals
        .open_proposal(&id)
        .map_err(|e| error_response(&format!("{e}"), "PR_OPEN_FAILED"))?;

    serialize_response(&serde_json::json!({
        "id": id,
        "title": req.title,
        "source_branch": source,
        "target_branch": target,
        "status": "open",
    }))
}

#[derive(Deserialize)]
struct PrListRequest {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    search: Option<String>,
}

pub(crate) fn handle_pr_list(
    payload: &[u8],
    proposals: &ProposalStore,
) -> Result<Vec<u8>, Vec<u8>> {
    use arrow::array::StringArray;
    use nusy_graph_review::schema::proposals_col;

    let req: PrListRequest = parse_payload(payload)?;

    let filtered = if let Some(ref term) = req.search {
        proposals
            .search_proposals(term)
            .map_err(|e| error_response(&format!("{e}"), "PR_LIST_FAILED"))?
    } else {
        proposals
            .list_proposals(req.status.as_deref())
            .map_err(|e| error_response(&format!("{e}"), "PR_LIST_FAILED"))?
    };

    if filtered.is_empty() {
        return serialize_response(&serde_json::json!({
            "count": 0,
            "table": "No proposals.\n",
        }));
    }

    let mut lines = Vec::new();
    for batch in &filtered {
        let ids = batch
            .column(proposals_col::PROPOSAL_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("id");
        let titles = batch
            .column(proposals_col::TITLE)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("title");
        let statuses = batch
            .column(proposals_col::STATUS)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("status");
        let authors = batch
            .column(proposals_col::AUTHOR)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("author");
        for i in 0..batch.num_rows() {
            lines.push(format!(
                "  {}  {}  {}  {}\n",
                ids.value(i),
                titles.value(i),
                statuses.value(i),
                authors.value(i),
            ));
        }
    }

    let table = if lines.is_empty() {
        "No proposals.\n".to_string()
    } else {
        format!("Proposals:\n\n{}", lines.join(""))
    };

    serialize_response(&serde_json::json!({
        "count": lines.len(),
        "table": table,
    }))
}

#[derive(Deserialize)]
struct PrIdRequest {
    id: String,
}

pub(crate) fn handle_pr_view(
    payload: &[u8],
    proposals: &ProposalStore,
    comments: &CommentStore,
) -> Result<Vec<u8>, Vec<u8>> {
    use arrow::array::{Array, BooleanArray, StringArray};
    use nusy_graph_review::schema::comments_col;

    let req: PrIdRequest = parse_payload(payload)?;
    let pid = normalize_proposal_id(&req.id);

    let status = proposals
        .get_status(&pid)
        .map_err(|e| error_response(&format!("{e}"), "PR_NOT_FOUND"))?;
    let source = proposals
        .get_source_branch(&pid)
        .map_err(|e| error_response(&format!("{e}"), "PR_NOT_FOUND"))?;
    let target = proposals
        .get_target_branch(&pid)
        .map_err(|e| error_response(&format!("{e}"), "PR_NOT_FOUND"))?;

    let comment_batches = comments.list_comments(&pid).unwrap_or_default();

    // Build detail string with comment bodies (matches local-mode pr_cli output)
    let mut detail = format!(
        "Proposal {}\n  Status:  {}\n  Branch:  {} → {}\n  Comments: {}",
        pid,
        status.as_str(),
        source,
        target,
        comment_batches.len(),
    );

    if !comment_batches.is_empty() {
        detail.push_str("\n\nComments:\n");
        for batch in &comment_batches {
            let reviewers = batch
                .column(comments_col::REVIEWER)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("reviewer");
            let bodies = batch
                .column(comments_col::BODY)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("body");
            let comment_ids = batch
                .column(comments_col::COMMENT_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("comment_id");
            let resolved = batch
                .column(comments_col::RESOLVED)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .expect("resolved");

            for i in 0..batch.num_rows() {
                let status_marker = if resolved.value(i) {
                    "[resolved]"
                } else {
                    "[open]"
                };
                detail.push_str(&format!(
                    "  {} @{} ({}): {}\n",
                    status_marker,
                    reviewers.value(i),
                    comment_ids.value(i),
                    bodies.value(i),
                ));
            }
        }
    }

    detail.push('\n');

    serialize_response(&serde_json::json!({
        "id": pid,
        "detail": detail,
    }))
}

pub(crate) fn handle_pr_diff(
    payload: &[u8],
    proposals: &ProposalStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let req: PrIdRequest = parse_payload(payload)?;
    let pid = normalize_proposal_id(&req.id);

    let source = proposals
        .get_source_branch(&pid)
        .map_err(|e| error_response(&format!("{e}"), "PR_NOT_FOUND"))?;
    let target = proposals
        .get_target_branch(&pid)
        .map_err(|e| error_response(&format!("{e}"), "PR_NOT_FOUND"))?;

    let detail = format!(
        "Diff: {} → {}\n\n(Graph-native diff available when proposals track Arrow state)\n",
        source, target,
    );

    serialize_response(&serde_json::json!({
        "id": pid,
        "detail": detail,
    }))
}

#[derive(Deserialize)]
struct PrReviewRequest {
    id: String,
    #[serde(default)]
    approve: bool,
    #[serde(default)]
    request_changes: bool,
    body: Option<String>,
    reviewer: Option<String>,
}

pub(crate) fn handle_pr_review(
    payload: &[u8],
    proposals: &mut ProposalStore,
    comments: &mut CommentStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let req: PrReviewRequest = parse_payload(payload)?;
    let pid = normalize_proposal_id(&req.id);
    let reviewer = req.reviewer.as_deref().unwrap_or("agent");

    if req.approve {
        let _ = proposals.add_reviewer(&pid, reviewer);
        let unresolved = comments.unresolved_count(&pid).unwrap_or(0);
        proposals
            .approve(&pid, reviewer, unresolved)
            .map_err(|e| error_response(&format!("{e}"), "PR_APPROVE_FAILED"))?;
        serialize_response(&serde_json::json!({
            "id": pid,
            "message": format!("Approved {} by {}", req.id, reviewer),
        }))
    } else if req.request_changes {
        let review_body = req.body.as_deref().unwrap_or("Changes requested");
        let _ = proposals.add_reviewer(&pid, reviewer);
        proposals
            .reject(&pid, reviewer)
            .map_err(|e| error_response(&format!("{e}"), "PR_REJECT_FAILED"))?;
        let _ = comments.add_comment(&pid, reviewer, review_body, None, None);
        serialize_response(&serde_json::json!({
            "id": pid,
            "message": format!("Changes requested on {} by {}", pid, reviewer),
        }))
    } else {
        Err(error_response(
            "Specify approve or request_changes",
            "INVALID_REVIEW",
        ))
    }
}

#[derive(Deserialize)]
struct PrMergeRequest {
    id: String,
    #[serde(default)]
    delete_branch: bool,
    agent_name: Option<String>,
    resolution: Option<String>,
    closed_by: Option<String>,
}

pub(crate) fn handle_pr_merge(
    payload: &[u8],
    proposals: &mut ProposalStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let req: PrMergeRequest = parse_payload(payload)?;
    let pid = normalize_proposal_id(&req.id);
    let agent = req.agent_name.as_deref().unwrap_or("agent");

    proposals
        .mark_merged(
            &pid,
            agent,
            req.resolution.as_deref().or(Some("completed")),
            req.closed_by.as_deref(),
        )
        .map_err(|e| error_response(&format!("{e}"), "PR_MERGE_FAILED"))?;

    serialize_response(&serde_json::json!({
        "id": pid,
        "merged": true,
        "delete_branch": req.delete_branch,
    }))
}

#[derive(Deserialize)]
struct PrCloseRequest {
    id: String,
    agent_name: Option<String>,
    resolution: Option<String>,
}

pub(crate) fn handle_pr_close(
    payload: &[u8],
    proposals: &mut ProposalStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let req: PrCloseRequest = parse_payload(payload)?;
    let pid = normalize_proposal_id(&req.id);
    let agent = req.agent_name.as_deref().unwrap_or("agent");

    proposals
        .close_proposal(&pid, agent, req.resolution.as_deref())
        .map_err(|e| error_response(&format!("{e}"), "PR_CLOSE_FAILED"))?;

    serialize_response(&serde_json::json!({
        "id": pid,
        "closed": true,
    }))
}

#[derive(Deserialize)]
struct PrCommentRequest {
    id: String,
    body: String,
    agent_name: Option<String>,
}

pub(crate) fn handle_pr_comment(
    payload: &[u8],
    proposals: &ProposalStore,
    comments: &mut CommentStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let req: PrCommentRequest = parse_payload(payload)?;
    let pid = normalize_proposal_id(&req.id);
    let agent = req.agent_name.as_deref().unwrap_or("agent");

    // Verify proposal exists
    proposals
        .get_status(&pid)
        .map_err(|e| error_response(&format!("{e}"), "PR_NOT_FOUND"))?;

    comments
        .add_comment(&pid, agent, &req.body, None, None)
        .map_err(|e| error_response(&format!("{e}"), "PR_COMMENT_FAILED"))?;

    serialize_response(&serde_json::json!({
        "id": pid,
        "comment": req.body,
    }))
}

pub(crate) fn handle_pr_checks(
    payload: &[u8],
    proposals: &ProposalStore,
    ci_results: &CiResultStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let req: PrIdRequest = parse_payload(payload)?;
    let pid = normalize_proposal_id(&req.id);

    // Read proposal's namespace to determine safety classification
    use nusy_graph_review::{classify_proposal, default_gates, safety_gates::ChangeEntry};
    let namespace = proposals
        .get_namespace(&pid)
        .unwrap_or_else(|_| "general".to_string());

    // Map namespace to Y-layer: self=6 (metacognition), world=1, work=0, research=0
    let y_layer = match namespace.as_str() {
        "self" => 6,
        "world" => 1,
        _ => 0,
    };

    let gates = default_gates().map_err(|e| error_response(&format!("{e}"), "GATES_FAILED"))?;
    let changes = vec![ChangeEntry {
        y_layer,
        domain: namespace.clone(),
    }];
    let classification = classify_proposal(&gates, &changes);

    // Build CI result section
    let ci_section = match ci_results.get_result(&pid) {
        Ok(Some(view)) => view.format_checks(),
        Ok(None) => format!(
            "CI Status: not run\n  (use `nk pr recheck {}` to run CI checks)\n",
            pid
        ),
        Err(e) => format!("CI Status: error ({e})\n"),
    };

    // Build CI JSON for structured access
    let ci_json = match ci_results.get_result(&pid) {
        Ok(Some(view)) => serde_json::json!({
            "status": view.status.to_string(),
            "run_id": view.run_id,
            "test_passed": view.test_passed,
            "test_failed": view.test_failed,
            "clippy_warnings": view.clippy_warnings,
            "fmt_clean": view.fmt_clean,
            "duration_secs": view.duration_secs,
            "summary": view.summary,
        }),
        _ => serde_json::json!(null),
    };

    let detail = format!(
        "{ci_section}\nSafety Checks for {}:\n\n  Gate ID:          {}\n  Human required:   {}\n  Shadow required:  {}\n  Threshold:        {:.2}\n  Description:      {}\n",
        pid,
        classification.gate_id,
        if classification.requires_human {
            "YES"
        } else {
            "no"
        },
        if classification.requires_shadow {
            "YES"
        } else {
            "no"
        },
        classification.auto_approve_threshold,
        classification.description,
    );

    serialize_response(&serde_json::json!({
        "id": pid,
        "detail": detail,
        "ci": ci_json,
    }))
}

/// Store a CI result for a proposal (called after CI service completes a run).
pub(crate) fn handle_pr_ci_store(
    payload: &[u8],
    ci_results: &mut CiResultStore,
) -> Result<Vec<u8>, Vec<u8>> {
    #[derive(Deserialize)]
    struct CiStoreRequest {
        proposal_id: String,
        status: String,
        test_passed: u32,
        test_failed: u32,
        clippy_warnings: u32,
        fmt_clean: bool,
        duration_secs: f64,
        error_message: Option<String>,
        summary: String,
    }

    let req: CiStoreRequest = parse_payload(payload)?;

    let status = match req.status.as_str() {
        "passed" => CiStatus::Passed,
        "failed" => CiStatus::Failed,
        "error" => CiStatus::Error,
        _ => CiStatus::Pending,
    };

    let input = CiResultInput {
        proposal_id: &req.proposal_id,
        status,
        test_passed: req.test_passed,
        test_failed: req.test_failed,
        clippy_warnings: req.clippy_warnings,
        fmt_clean: req.fmt_clean,
        duration_secs: req.duration_secs,
        error_message: req.error_message.as_deref(),
        summary: &req.summary,
    };

    let run_id = ci_results
        .record_result(&input)
        .map_err(|e| error_response(&format!("{e}"), "CI_STORE_FAILED"))?;

    serialize_response(&serde_json::json!({
        "run_id": run_id,
        "proposal_id": req.proposal_id,
        "status": req.status,
    }))
}

#[derive(Deserialize)]
struct PrReviseRequest {
    id: String,
    agent_name: Option<String>,
}

pub(crate) fn handle_pr_revise(
    payload: &[u8],
    proposals: &mut ProposalStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let req: PrReviseRequest = parse_payload(payload)?;
    let pid = normalize_proposal_id(&req.id);
    let agent = req.agent_name.as_deref().unwrap_or("agent");

    proposals
        .revise(&pid, agent)
        .map_err(|e| error_response(&format!("{e}"), "PR_REVISE_FAILED"))?;

    serialize_response(&serde_json::json!({
        "id": pid,
        "message": format!("Revised {} — re-entered review", pid),
    }))
}

#[derive(Deserialize)]
struct PrResolveRequest {
    id: String,
    comment_id: String,
}

pub(crate) fn handle_pr_resolve(
    payload: &[u8],
    proposals: &mut ProposalStore,
    comments: &mut CommentStore,
) -> Result<Vec<u8>, Vec<u8>> {
    let req: PrResolveRequest = parse_payload(payload)?;
    let pid = normalize_proposal_id(&req.id);

    // Verify proposal exists
    proposals
        .get_status(&pid)
        .map_err(|e| error_response(&format!("{e}"), "PR_NOT_FOUND"))?;

    comments
        .resolve_comment(&req.comment_id)
        .map_err(|e| error_response(&format!("{e}"), "PR_RESOLVE_FAILED"))?;

    serialize_response(&serde_json::json!({
        "id": pid,
        "comment_id": req.comment_id,
        "message": format!("Resolved comment {} on {}", req.comment_id, pid),
    }))
}
