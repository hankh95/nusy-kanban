//! `nk pr` subcommands — GitHub-compatible graph-native proposal workflow.
//!
//! Mirrors `gh pr` conventions for zero learning curve:
//! - `nk pr create` → create proposal
//! - `nk pr list` → list proposals
//! - `nk pr view` → show proposal details
//! - `nk pr diff` → show graph-native diff
//! - `nk pr review` → approve/request-changes with safety gates
//! - `nk pr merge` → merge + cleanup
//! - `nk pr close` → close without merging
//! - `nk pr comment` → add review comment
//! - `nk pr checks` → show safety gate status

use clap::Subcommand;
use nusy_graph_review::{
    CiResultInput, CiResultStore, CiStatus, CommentStore, CreateProposalInput, ProposalStore,
    check_approval_gate, classify_proposal, default_gates, safety_gates::ChangeEntry,
};

/// PR subcommands — mirrors `gh pr`.
#[derive(Subcommand)]
pub enum PrCommands {
    /// Create a new proposal from the current branch
    Create {
        /// Proposal title
        #[arg(long)]
        title: String,
        /// Target branch (default: main)
        #[arg(long, default_value = "main")]
        base: String,
        /// Proposal body/description
        #[arg(long)]
        body: Option<String>,
    },
    /// List open proposals
    List,
    /// View proposal details
    View {
        /// Proposal ID (e.g., PROP-001)
        id: String,
    },
    /// Show graph-native diff for a proposal
    Diff {
        /// Proposal ID
        id: String,
    },
    /// Review a proposal (approve or request changes)
    Review {
        /// Proposal ID
        id: String,
        /// Approve the proposal
        #[arg(long)]
        approve: bool,
        /// Request changes
        #[arg(long)]
        request_changes: bool,
        /// Review body (required for request-changes)
        #[arg(long)]
        body: Option<String>,
        /// Reviewer name
        #[arg(long)]
        reviewer: Option<String>,
    },
    /// Merge a proposal
    Merge {
        /// Proposal ID
        id: String,
        /// Delete the source branch after merge
        #[arg(long)]
        delete_branch: bool,
        /// Resolution (default: completed)
        #[arg(long)]
        resolution: Option<String>,
        /// Work item this proposal closes (e.g., EX-3218)
        #[arg(long)]
        closed_by: Option<String>,
    },
    /// Close a proposal without merging
    Close {
        /// Proposal ID
        id: String,
        /// Resolution (e.g., wont_do, superseded, duplicate)
        #[arg(long)]
        resolution: Option<String>,
    },
    /// Add a comment to a proposal
    Comment {
        /// Proposal ID
        id: String,
        /// Comment body
        #[arg(long)]
        body: String,
    },
    /// Show safety gate status for a proposal
    Checks {
        /// Proposal ID
        id: String,
    },
    /// Mark a rejected proposal as revised (re-enters review)
    Revise {
        /// Proposal ID
        id: String,
    },
    /// Resolve a review comment
    Resolve {
        /// Proposal ID
        id: String,
        /// Comment ID to resolve
        #[arg(long)]
        comment_id: String,
    },
    /// Re-run CI checks for a proposal
    Recheck {
        /// Proposal ID
        id: String,
    },
}

/// Execute a PR subcommand against the proposal, comment, and CI stores.
pub fn run_pr_command(
    cmd: &PrCommands,
    proposals: &mut ProposalStore,
    comments: &mut CommentStore,
    ci_results: &mut CiResultStore,
    agent_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        PrCommands::Create { title, base, body } => {
            let current_branch = get_current_branch();
            let input = CreateProposalInput {
                title,
                description: body.as_deref(),
                author: agent_name,
                source_branch: &current_branch,
                target_branch: base,
                proposal_type: "code_change",
                namespace: "work",
            };
            let id = proposals.create_proposal(&input)?;
            proposals.open_proposal(&id)?;
            println!("Created proposal {id}: {title}");
            println!("  {current_branch} → {base}");

            // Classify with safety gates
            let gates = default_gates()?;
            let changes = changes_for_proposal(proposals, &id);
            let req = classify_proposal(&gates, &changes);
            if req.requires_human {
                println!("  Safety: HUMAN GATE REQUIRED ({})", req.gate_id);
            } else if req.requires_shadow {
                println!(
                    "  Safety: Shadow eval required (threshold: {:.2})",
                    req.auto_approve_threshold
                );
            } else {
                println!("  Safety: Auto-approvable");
            }
        }

        PrCommands::List => {
            let batches = proposals.proposals_batches();
            if batches.is_empty() {
                println!("No proposals.");
                return Ok(());
            }

            println!("Proposals:\n");
            for batch in batches {
                print_proposal_row(batch);
            }
        }

        PrCommands::View { id } => {
            // Verify it exists
            let status = proposals.get_status(id)?;
            let source = proposals.get_source_branch(id)?;
            let target = proposals.get_target_branch(id)?;

            println!("Proposal {id}");
            println!("  Status:  {}", status.as_str());
            println!("  Branch:  {source} → {target}");
            // HZ-3448: Show reviewer for approved/merged proposals.
            if let Ok(reviewer) = proposals.get_reviewer(id) {
                if !reviewer.is_empty() {
                    println!("  Reviewer: {reviewer}");
                }
            }

            // Show comments
            let comment_list = comments.list_comments(id)?;
            if !comment_list.is_empty() {
                println!("\nComments ({}):", comment_list.len());
                for c in &comment_list {
                    print_comment(c);
                }
            }

            // Show safety classification
            let gates = default_gates()?;
            let changes = changes_for_proposal(proposals, id);
            let req = classify_proposal(&gates, &changes);
            println!("\nSafety Classification:");
            println!("  Gate: {}", req.gate_id);
            println!("  Human required: {}", req.requires_human);
            println!("  Shadow required: {}", req.requires_shadow);
            println!("  Threshold: {:.2}", req.auto_approve_threshold);
        }

        PrCommands::Diff { id } => {
            let source = proposals.get_source_branch(id)?;
            let target = proposals.get_target_branch(id)?;
            println!("Diff: {source} → {target}\n");

            match semantic_diff_for_branches(&target, &source) {
                Ok(output) => print!("{output}"),
                Err(e) => {
                    eprintln!("Semantic diff unavailable: {e}");
                    println!("(Falling back to branch info only)");
                }
            }
        }

        PrCommands::Review {
            id,
            approve,
            request_changes,
            body,
            reviewer,
        } => {
            let reviewer_name = reviewer.as_deref().unwrap_or(agent_name);

            if *approve {
                // Check safety gates
                let gates = default_gates()?;
                let changes = changes_for_proposal(proposals, id);
                let req = classify_proposal(&gates, &changes);
                let unresolved = comments.unresolved_count(id)?;

                // Add reviewer if not already assigned
                let _ = proposals.add_reviewer(id, reviewer_name);

                check_approval_gate(&req, true, None)?;
                proposals.approve(id, reviewer_name, unresolved)?;
                println!("Approved {id} by {reviewer_name}");
            } else if *request_changes {
                let review_body = body.as_deref().unwrap_or("Changes requested");
                let _ = proposals.add_reviewer(id, reviewer_name);
                proposals.reject(id, reviewer_name)?;
                comments.add_comment(id, reviewer_name, review_body, None, None)?;
                println!("Changes requested on {id} by {reviewer_name}");
                println!("  {review_body}");
            } else {
                return Err("Specify --approve or --request-changes".into());
            }
        }

        PrCommands::Merge {
            id,
            delete_branch,
            resolution,
            closed_by,
        } => {
            let source = proposals.get_source_branch(id)?;
            proposals.mark_merged(
                id,
                agent_name,
                resolution.as_deref().or(Some("completed")),
                closed_by.as_deref(),
            )?;
            println!("Merged {id}");

            if *delete_branch {
                println!("  Branch {source} marked for deletion");
                // TODO: Wire to nusy-arrow-git delete_branch + shell git push --delete
            }

            // Shell git push (Option A — parallel with GitHub)
            let push_result = std::process::Command::new("git")
                .args(["push", "origin", "HEAD"])
                .status();
            match push_result {
                Ok(s) if s.success() => println!("  Pushed to remote"),
                _ => println!("  Warning: git push failed — push manually"),
            }
        }

        PrCommands::Close { id, resolution } => {
            // Need author for authorization — use agent_name
            proposals.close_proposal(id, agent_name, resolution.as_deref())?;
            println!("Closed {id}");
        }

        PrCommands::Comment { id, body } => {
            comments.add_comment(id, agent_name, body, None, None)?;
            println!("Comment added to {id}");
        }

        PrCommands::Checks { id } => {
            let gates = default_gates()?;
            let changes = changes_for_proposal(proposals, id);
            let req = classify_proposal(&gates, &changes);

            println!("Checks for {id}:\n");

            // CI results section
            match ci_results.get_result(id) {
                Ok(Some(view)) => {
                    print!("{}", view.format_checks());
                }
                Ok(None) => {
                    println!("CI Status: not run");
                    println!("  (use `nk pr recheck {id}` to run CI checks)\n");
                }
                Err(e) => {
                    println!("CI Status: error reading results ({e})\n");
                }
            }

            // Safety gates section
            println!("Safety Gates:");
            println!("  Gate ID:          {}", req.gate_id);
            println!(
                "  Human required:   {}",
                if req.requires_human { "YES" } else { "no" }
            );
            println!(
                "  Shadow required:  {}",
                if req.requires_shadow { "YES" } else { "no" }
            );
            println!("  Threshold:        {:.2}", req.auto_approve_threshold);
            println!("  Description:      {}", req.description);

            let unresolved = comments.unresolved_count(id).unwrap_or(0);
            if unresolved > 0 {
                println!("\n  Unresolved comments: {unresolved} (must resolve before merge)");
            } else {
                println!("\n  All comments resolved");
            }

            // Semantic diff summary (best-effort, with timeout)
            let source = proposals.get_source_branch(id);
            let target = proposals.get_target_branch(id);
            if let (Ok(src), Ok(tgt)) = (source, target) {
                println!("\nSemantic Diff:");
                match semantic_diff_stats_with_timeout(&tgt, &src) {
                    Ok(Some(stats)) => {
                        print_diff_stats(&stats);
                    }
                    Ok(None) => {
                        println!("  No codegraph changes detected");
                    }
                    Err(e) => {
                        println!("  Unavailable: {e}");
                    }
                }
            }
        }

        PrCommands::Revise { id } => {
            proposals.revise(id, agent_name)?;
            println!("Revised {id} — re-entered review");
        }

        PrCommands::Resolve { id, comment_id } => {
            // Verify proposal exists
            let _ = proposals.get_status(id)?;
            comments.resolve_comment(comment_id)?;
            println!("Resolved comment {comment_id} on {id}");
        }

        PrCommands::Recheck { id } => {
            // Verify proposal exists
            let _ = proposals.get_status(id)?;

            let repo_root =
                get_repo_root().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

            println!("Running CI checks for {id}...\n");
            let suite = nusy_conductor::ci_runner::run_ci_checks(&repo_root);

            // Convert CiCheckSuite → CiResultInput for storage
            let (test_passed, test_failed, clippy_warnings, fmt_clean) =
                extract_check_counts(&suite);

            let status = if suite.passed {
                CiStatus::Passed
            } else if suite.error.is_some() {
                CiStatus::Error
            } else {
                CiStatus::Failed
            };

            let summary_text = suite.summary();
            let error_msg = suite.error.as_deref();

            let input = CiResultInput {
                proposal_id: id,
                status,
                test_passed,
                test_failed,
                clippy_warnings,
                fmt_clean,
                duration_secs: suite.total_duration.as_secs_f64(),
                error_message: error_msg,
                summary: &summary_text,
            };

            let run_id = ci_results.record_result(&input)?;
            println!("{summary_text}");
            println!("\nStored as {run_id}");
        }
    }

    Ok(())
}

/// Extract test/clippy/fmt counts from a CiCheckSuite.
fn extract_check_counts(suite: &nusy_conductor::ci_runner::CiCheckSuite) -> (u32, u32, u32, bool) {
    use nusy_conductor::ci_runner::CheckType;

    let mut test_passed = 0u32;
    let mut test_failed = 0u32;
    let mut clippy_warnings = 0u32;
    let mut fmt_clean = true;

    for check in &suite.checks {
        match check.check_type {
            CheckType::Test => {
                // Parse "X passed, Y failed" from summary
                let (p, f) = parse_test_counts(&check.summary);
                test_passed = p;
                test_failed = f;
            }
            CheckType::Clippy => {
                if !check.passed {
                    // Parse "N warning(s)" from summary
                    clippy_warnings = parse_warning_count(&check.summary);
                }
            }
            CheckType::Fmt => {
                fmt_clean = check.passed;
            }
        }
    }

    (test_passed, test_failed, clippy_warnings, fmt_clean)
}

/// Parse "42 passed, 3 failed" → (42, 3) from test summary.
fn parse_test_counts(summary: &str) -> (u32, u32) {
    let passed = summary
        .split_whitespace()
        .zip(summary.split_whitespace().skip(1))
        .find(|(_, label)| *label == "passed" || label.starts_with("passed"))
        .and_then(|(num, _)| num.parse().ok())
        .unwrap_or(0);
    let failed = summary
        .split_whitespace()
        .zip(summary.split_whitespace().skip(1))
        .find(|(_, label)| *label == "failed" || label.starts_with("failed"))
        .and_then(|(num, _)| num.parse().ok())
        .unwrap_or(0);
    (passed, failed)
}

/// Parse "N warning(s)" → N from clippy summary.
fn parse_warning_count(summary: &str) -> u32 {
    summary
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1) // Default to 1 warning if we can't parse
}

/// Derive safety gate change entries from a proposal's metadata.
///
/// Maps proposal_type → Y-layer and namespace → domain so that `classify_proposal`
/// returns meaningful gate data instead of always using the Y0/general default.
fn changes_for_proposal(proposals: &ProposalStore, proposal_id: &str) -> Vec<ChangeEntry> {
    let proposal_type = proposals
        .get_proposal_type(proposal_id)
        .unwrap_or_else(|_| "code_change".to_string());
    let namespace = proposals
        .get_namespace(proposal_id)
        .unwrap_or_else(|_| "work".to_string());

    let y_layer = match proposal_type.as_str() {
        "knowledge_change" => 1,   // Y1: Semantic
        "ontology_change" => 2,    // Y2: Reasoning
        "safety_rule_change" => 6, // Y6: Metacognition
        "code_change" => 0,        // Y0: Prose (default)
        _ => 0,
    };

    let domain = match namespace.as_str() {
        "self" => "general".to_string(),
        other => other.to_string(),
    };

    vec![ChangeEntry { y_layer, domain }]
}

/// Get current git branch name.
fn get_current_branch() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Get the git repo root directory.
fn get_repo_root() -> std::result::Result<std::path::PathBuf, String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !output.status.success() {
        return Err("not inside a git repository".to_string());
    }
    let root =
        String::from_utf8(output.stdout).map_err(|e| format!("invalid UTF-8 in repo path: {e}"))?;
    Ok(std::path::PathBuf::from(root.trim()))
}

/// Create a temporary git worktree for `base_branch`, call `f(base_dir, repo_root)`,
/// then clean up the worktree regardless of success or failure.
///
/// Head is always the current working tree (repo_root). The `_head_branch` param on
/// callers exists for API symmetry with proposal source/target but is unused — head
/// is always the live checkout.
fn with_base_worktree<F, T>(base_branch: &str, f: F) -> std::result::Result<T, String>
where
    F: FnOnce(&std::path::Path, &std::path::Path) -> std::result::Result<T, String>,
{
    let repo_root = get_repo_root()?;

    // Verify the base branch ref exists before creating a worktree
    let ref_check = std::process::Command::new("git")
        .args(["rev-parse", "--verify", base_branch])
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !ref_check.status.success() {
        return Err(format!("branch '{base_branch}' not found"));
    }

    // Unique temp dir (pid + timestamp) to avoid collisions in parallel tests
    let unique = format!(
        "nk-diff-base-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let temp_dir = std::env::temp_dir().join(unique);

    let worktree_added = std::process::Command::new("git")
        .args([
            "worktree",
            "add",
            "--detach",
            &temp_dir.display().to_string(),
            base_branch,
        ])
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("failed to create worktree: {e}"))?;

    if !worktree_added.status.success() {
        let stderr = String::from_utf8_lossy(&worktree_added.stderr);
        return Err(format!("git worktree add failed: {stderr}"));
    }

    let result = f(&temp_dir, &repo_root);

    // Cleanup worktree on all exit paths
    let _ = std::process::Command::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            &temp_dir.display().to_string(),
        ])
        .current_dir(&repo_root)
        .output();

    result
}

/// Run semantic diff between two branches using codegraph ingestion.
///
/// `_head_branch` is unused — head is always the current working tree.
fn semantic_diff_for_branches(
    base_branch: &str,
    _head_branch: &str,
) -> std::result::Result<String, String> {
    with_base_worktree(base_branch, |base_dir, head_dir| {
        run_semantic_diff_pipeline(base_dir, head_dir)
    })
}

/// Inner pipeline: ingest base + head, compute diff, return semantic diff.
fn run_semantic_diff_analysis(
    base_dir: &std::path::Path,
    head_dir: &std::path::Path,
) -> std::result::Result<Option<nusy_codegraph::SemanticDiff>, String> {
    // Ingest base branch
    let base_result = nusy_codegraph::ingest_directory(base_dir)
        .map_err(|e| format!("base ingestion failed: {e}"))?;
    let base_nodes = base_result
        .nodes_batch()
        .map_err(|e| format!("base nodes batch: {e}"))?;

    // Ingest head (current working tree)
    let head_result = nusy_codegraph::ingest_directory(head_dir)
        .map_err(|e| format!("head ingestion failed: {e}"))?;
    let head_nodes = head_result
        .nodes_batch()
        .map_err(|e| format!("head nodes batch: {e}"))?;
    let head_edges = head_result
        .edges_batch()
        .map_err(|e| format!("head edges batch: {e}"))?;

    // Object-level diff
    let diff = nusy_codegraph::codegraph_diff(&base_nodes, &head_nodes)
        .map_err(|e| format!("codegraph diff: {e}"))?;

    if diff.entries.is_empty() {
        return Ok(None);
    }

    // Semantic enrichment
    Ok(Some(nusy_codegraph::semantic_diff(
        &diff,
        &head_nodes,
        &head_edges,
    )))
}

/// Inner pipeline: ingest base + head, compute diff, format output.
fn run_semantic_diff_pipeline(
    base_dir: &std::path::Path,
    head_dir: &std::path::Path,
) -> std::result::Result<String, String> {
    match run_semantic_diff_analysis(base_dir, head_dir)? {
        Some(semantic) => Ok(nusy_codegraph::format_semantic_diff(&semantic)),
        None => Ok("No codegraph changes detected.\n".to_string()),
    }
}

/// Run semantic diff and return only the stats summary for a proposal's branches.
///
/// `_head_branch` is unused — head is always the current working tree.
fn semantic_diff_stats_for_branches(
    base_branch: &str,
    _head_branch: &str,
) -> std::result::Result<Option<nusy_codegraph::DiffStats>, String> {
    with_base_worktree(base_branch, |base_dir, head_dir| {
        run_semantic_diff_analysis(base_dir, head_dir).map(|opt| opt.map(|s| s.stats))
    })
}

/// Run semantic diff stats with a 30-second timeout to avoid blocking on large repos.
///
/// Note: on timeout the spawned thread continues running in the background until the
/// worktree cleanup completes. This is acceptable for a CLI tool where the process
/// exits shortly after, but would need cancellation support in a long-lived server.
fn semantic_diff_stats_with_timeout(
    base_branch: &str,
    head_branch: &str,
) -> std::result::Result<Option<nusy_codegraph::DiffStats>, String> {
    let base = base_branch.to_string();
    let head = head_branch.to_string();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = semantic_diff_stats_for_branches(&base, &head);
        let _ = tx.send(result);
    });

    match rx.recv_timeout(std::time::Duration::from_secs(30)) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err("timed out (repo too large for inline analysis)".to_string())
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("semantic diff thread panicked".to_string())
        }
    }
}

/// Format and print DiffStats to stdout.
fn print_diff_stats(stats: &nusy_codegraph::DiffStats) {
    let total = stats.added + stats.modified + stats.removed;
    println!(
        "  {total} functions changed ({} API-breaking), {} affected edges",
        stats.api_breaking, stats.affected_edges,
    );
    println!(
        "  {} added, {} modified, {} removed across {} files",
        stats.added, stats.modified, stats.removed, stats.files_touched,
    );
    if stats.test_changes > 0 {
        println!("  {} test changes", stats.test_changes);
    }
}

/// Print a single proposal row for list output.
fn print_proposal_row(batch: &arrow::array::RecordBatch) {
    use arrow::array::{Array, StringArray};
    use nusy_graph_review::schema::proposals_col;

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
    // HZ-3448: Show reviewer in list output for audit trail.
    let reviewers = batch
        .column(proposals_col::REVIEWER)
        .as_any()
        .downcast_ref::<StringArray>();

    for i in 0..batch.num_rows() {
        let title = truncate_str(titles.value(i), 50);
        let reviewer = reviewers
            .and_then(|r| {
                if r.is_null(i) {
                    None
                } else {
                    let v = r.value(i);
                    if v.is_empty() { None } else { Some(v) }
                }
            })
            .unwrap_or("-");
        let status = statuses.value(i);
        // Show reviewer for approved/merged proposals.
        if (status == "approved" || status == "merged") && reviewer != "-" {
            println!(
                "  {}  {}  {}  {} (reviewed: {})",
                ids.value(i),
                title,
                status,
                authors.value(i),
                reviewer,
            );
        } else {
            println!(
                "  {}  {}  {}  {}",
                ids.value(i),
                title,
                status,
                authors.value(i),
            );
        }
    }
}

/// Truncate a string to `max` characters, appending "..." if truncated.
/// Safe for multi-byte UTF-8 (operates on char boundaries, not byte indices).
fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

/// Print a comment.
fn print_comment(batch: &arrow::array::RecordBatch) {
    use arrow::array::{Array, StringArray};
    use nusy_graph_review::schema::comments_col;

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

    for i in 0..batch.num_rows() {
        println!("    @{}: {}", reviewers.value(i), bodies.value(i));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusy_graph_review::ProposalStatus;

    fn get_first_proposal_id(proposals: &ProposalStore) -> String {
        use arrow::array::{Array, StringArray};
        use nusy_graph_review::schema::proposals_col;
        let batch = &proposals.proposals_batches()[0];
        batch
            .column(proposals_col::PROPOSAL_ID)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("id")
            .value(0)
            .to_string()
    }

    #[test]
    fn test_pr_create() {
        let mut proposals = ProposalStore::new();
        let mut comments = CommentStore::new();
        let mut ci = CiResultStore::new();
        run_pr_command(
            &PrCommands::Create {
                title: "Test PR".to_string(),
                base: "main".to_string(),
                body: Some("Test body".to_string()),
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "Mini",
        )
        .expect("create");
        assert!(!proposals.proposals_batches().is_empty());
    }

    #[test]
    fn test_pr_approve() {
        let mut proposals = ProposalStore::new();
        let mut comments = CommentStore::new();
        let mut ci = CiResultStore::new();
        run_pr_command(
            &PrCommands::Create {
                title: "Approvable".to_string(),
                base: "main".to_string(),
                body: None,
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "Mini",
        )
        .expect("create");
        let id = get_first_proposal_id(&proposals);
        run_pr_command(
            &PrCommands::Review {
                id: id.clone(),
                approve: true,
                request_changes: false,
                body: None,
                reviewer: Some("M5".to_string()),
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "M5",
        )
        .expect("approve");
        assert_eq!(proposals.get_status(&id).unwrap(), ProposalStatus::Approved);
    }

    #[test]
    fn test_pr_comment() {
        let mut proposals = ProposalStore::new();
        let mut comments = CommentStore::new();
        let mut ci = CiResultStore::new();
        run_pr_command(
            &PrCommands::Create {
                title: "Commentable".to_string(),
                base: "main".to_string(),
                body: None,
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "Mini",
        )
        .expect("create");
        let id = get_first_proposal_id(&proposals);
        run_pr_command(
            &PrCommands::Comment {
                id: id.clone(),
                body: "Looks good!".to_string(),
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "DGX",
        )
        .expect("comment");
        assert_eq!(comments.list_comments(&id).unwrap().len(), 1);
    }

    #[test]
    fn test_pr_checks() {
        let mut proposals = ProposalStore::new();
        let mut comments = CommentStore::new();
        let mut ci = CiResultStore::new();
        run_pr_command(
            &PrCommands::Create {
                title: "Checkable".to_string(),
                base: "main".to_string(),
                body: None,
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "Mini",
        )
        .expect("create");
        let id = get_first_proposal_id(&proposals);
        run_pr_command(
            &PrCommands::Checks { id },
            &mut proposals,
            &mut comments,
            &mut ci,
            "Mini",
        )
        .expect("checks");
    }

    // ── Integration tests ──

    #[test]
    fn test_full_lifecycle_create_review_approve_merge() {
        let mut proposals = ProposalStore::new();
        let mut comments = CommentStore::new();
        let mut ci = CiResultStore::new();

        // 1. Create
        run_pr_command(
            &PrCommands::Create {
                title: "Full lifecycle PR".to_string(),
                base: "main".to_string(),
                body: Some("Integration test".to_string()),
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "Mini",
        )
        .expect("create");
        let id = get_first_proposal_id(&proposals);
        assert_eq!(proposals.get_status(&id).unwrap(), ProposalStatus::Open);

        // 2. Comment (then resolve so it doesn't block approval)
        run_pr_command(
            &PrCommands::Comment {
                id: id.clone(),
                body: "LGTM".to_string(),
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "DGX",
        )
        .expect("comment");
        let comment_batches = comments.list_comments(&id).unwrap();
        assert_eq!(comment_batches.len(), 1);
        // Resolve the comment so it doesn't block approval
        let comment_id = {
            use arrow::array::{Array, StringArray};
            use nusy_graph_review::schema::comments_col;
            comment_batches[0]
                .column(comments_col::COMMENT_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("comment_id")
                .value(0)
                .to_string()
        };
        comments.resolve_comment(&comment_id).expect("resolve");
        assert_eq!(comments.unresolved_count(&id).unwrap(), 0);

        // 3. Approve (cross-agent)
        run_pr_command(
            &PrCommands::Review {
                id: id.clone(),
                approve: true,
                request_changes: false,
                body: None,
                reviewer: Some("DGX".to_string()),
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "DGX",
        )
        .expect("approve");
        assert_eq!(proposals.get_status(&id).unwrap(), ProposalStatus::Approved);

        // 4. Merge (git push will fail in test — that's fine, we check graph state)
        run_pr_command(
            &PrCommands::Merge {
                id: id.clone(),
                delete_branch: false,
                resolution: None,
                closed_by: None,
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "Mini",
        )
        .expect("merge");
        assert_eq!(proposals.get_status(&id).unwrap(), ProposalStatus::Merged);
    }

    #[test]
    fn test_request_changes_rejects_and_records_comment() {
        let mut proposals = ProposalStore::new();
        let mut comments = CommentStore::new();
        let mut ci = CiResultStore::new();

        // 1. Create
        run_pr_command(
            &PrCommands::Create {
                title: "Needs revision".to_string(),
                base: "main".to_string(),
                body: None,
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "Mini",
        )
        .expect("create");
        let id = get_first_proposal_id(&proposals);
        assert_eq!(proposals.get_status(&id).unwrap(), ProposalStatus::Open);

        // 2. Request changes — should reject + add review comment
        run_pr_command(
            &PrCommands::Review {
                id: id.clone(),
                approve: false,
                request_changes: true,
                body: Some("Fix the UTF-8 bug".to_string()),
                reviewer: Some("M5".to_string()),
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "M5",
        )
        .expect("request-changes");
        assert_eq!(proposals.get_status(&id).unwrap(), ProposalStatus::Rejected);
        // Review comment was recorded
        assert_eq!(comments.list_comments(&id).unwrap().len(), 1);
    }

    #[test]
    fn test_reject_revise_approve_merge_lifecycle() {
        // Full reject→revise→re-review→approve→merge lifecycle
        // Uses direct ProposalStore calls (no CLI "revise" command yet)
        use nusy_graph_review::CreateProposalInput;

        let mut proposals = ProposalStore::new();

        // 1. Create + open
        let input = CreateProposalInput {
            title: "Lifecycle test",
            description: None,
            author: "Mini",
            source_branch: "feat-branch",
            target_branch: "main",
            proposal_type: "code_change",
            namespace: "work",
        };
        let id = proposals.create_proposal(&input).expect("create");
        proposals.open_proposal(&id).expect("open");
        assert_eq!(proposals.get_status(&id).unwrap(), ProposalStatus::Open);

        // 2. Add reviewer + reject
        proposals.add_reviewer(&id, "M5").expect("add reviewer");
        assert_eq!(
            proposals.get_status(&id).unwrap(),
            ProposalStatus::Reviewing
        );
        proposals.reject(&id, "M5").expect("reject");
        assert_eq!(proposals.get_status(&id).unwrap(), ProposalStatus::Rejected);

        // 3. Revise — auto-advances: Rejected → Revised → Reviewing
        proposals.revise(&id, "Mini").expect("revise");
        assert_eq!(
            proposals.get_status(&id).unwrap(),
            ProposalStatus::Reviewing
        );

        // 5. Approve (0 unresolved comments)
        proposals.approve(&id, "M5", 0).expect("approve");
        assert_eq!(proposals.get_status(&id).unwrap(), ProposalStatus::Approved);

        // 6. Merge
        proposals
            .mark_merged(&id, "Mini", None, None)
            .expect("merge");
        assert_eq!(proposals.get_status(&id).unwrap(), ProposalStatus::Merged);
    }

    #[test]
    fn test_y6_safety_gate_blocks_agent_approval() {
        use nusy_graph_review::{check_approval_gate, classify_proposal, default_gates};

        let gates = default_gates().expect("gates");

        // Y6 metacognition changes — should require human gate
        let changes = vec![ChangeEntry {
            y_layer: 6,
            domain: "general".to_string(),
        }];
        let req = classify_proposal(&gates, &changes);
        assert!(req.requires_human, "Y6 must require human gate");
        assert!(req.requires_shadow, "Y6 must require shadow eval");

        // Agent (non-human) tries to approve — should be blocked
        let result = check_approval_gate(&req, false, None);
        assert!(result.is_err(), "Y6 approval by non-human must be rejected");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("human") || err_msg.contains("Human"),
            "error should mention human gate: {err_msg}"
        );

        // Human reviewer can pass the human gate (but still needs shadow eval)
        let result = check_approval_gate(&req, true, None);
        assert!(
            result.is_err(),
            "Y6 still needs shadow eval even with human"
        );
    }

    #[test]
    fn test_changes_for_proposal_derives_from_metadata() {
        use nusy_graph_review::CreateProposalInput;

        let mut proposals = ProposalStore::new();

        // Create a knowledge_change proposal in "self" namespace
        let input = CreateProposalInput {
            title: "Knowledge update",
            description: None,
            author: "M5",
            source_branch: "feature",
            target_branch: "main",
            proposal_type: "knowledge_change",
            namespace: "self",
        };
        let id = proposals.create_proposal(&input).expect("create");
        proposals.open_proposal(&id).expect("open");

        let changes = changes_for_proposal(&proposals, &id);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].y_layer, 1); // Y1: Semantic
        assert_eq!(changes[0].domain, "general"); // "self" maps to "general"
    }

    #[test]
    fn test_changes_for_proposal_safety_rule_triggers_y6() {
        use nusy_graph_review::CreateProposalInput;

        let mut proposals = ProposalStore::new();

        let input = CreateProposalInput {
            title: "Safety rule update",
            description: None,
            author: "Mini",
            source_branch: "feature",
            target_branch: "main",
            proposal_type: "safety_rule_change",
            namespace: "world",
        };
        let id = proposals.create_proposal(&input).expect("create");
        proposals.open_proposal(&id).expect("open");

        let changes = changes_for_proposal(&proposals, &id);
        assert_eq!(changes[0].y_layer, 6); // Y6: Metacognition
        assert_eq!(changes[0].domain, "world");

        // Verify this triggers human gate
        let gates = default_gates().expect("gates");
        let req = classify_proposal(&gates, &changes);
        assert!(
            req.requires_human,
            "safety_rule_change should require human gate"
        );
    }

    #[test]
    fn test_changes_for_proposal_code_change_default() {
        let mut proposals = ProposalStore::new();
        let mut comments = CommentStore::new();
        let mut ci = CiResultStore::new();

        // Create via CLI (defaults to code_change/work)
        run_pr_command(
            &PrCommands::Create {
                title: "Code fix".to_string(),
                base: "main".to_string(),
                body: None,
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "DGX",
        )
        .expect("create");
        let id = get_first_proposal_id(&proposals);

        let changes = changes_for_proposal(&proposals, &id);
        assert_eq!(changes[0].y_layer, 0); // Y0: default for code_change
        assert_eq!(changes[0].domain, "work");
    }

    #[test]
    fn test_pr_resolve_comment() {
        let mut proposals = ProposalStore::new();
        let mut comments = CommentStore::new();
        let mut ci = CiResultStore::new();

        // Create proposal
        run_pr_command(
            &PrCommands::Create {
                title: "Resolvable".to_string(),
                base: "main".to_string(),
                body: None,
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "Mini",
        )
        .expect("create");
        let id = get_first_proposal_id(&proposals);

        // Add a comment
        run_pr_command(
            &PrCommands::Comment {
                id: id.clone(),
                body: "Fix this".to_string(),
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "M5",
        )
        .expect("comment");
        assert_eq!(comments.unresolved_count(&id).unwrap(), 1);

        // Extract comment ID
        let comment_id = {
            use arrow::array::{Array, StringArray};
            use nusy_graph_review::schema::comments_col;
            let batches = comments.list_comments(&id).unwrap();
            batches[0]
                .column(comments_col::COMMENT_ID)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("comment_id")
                .value(0)
                .to_string()
        };

        // Resolve via CLI
        run_pr_command(
            &PrCommands::Resolve {
                id: id.clone(),
                comment_id,
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "Mini",
        )
        .expect("resolve");
        assert_eq!(comments.unresolved_count(&id).unwrap(), 0);
    }

    #[test]
    fn test_semantic_diff_pipeline_detects_changes() {
        let base_dir = tempfile::tempdir().expect("base tempdir");
        let head_dir = tempfile::tempdir().expect("head tempdir");

        // Base: one function
        std::fs::write(
            base_dir.path().join("example.py"),
            "def greet(name):\n    return f'Hello {name}'\n",
        )
        .expect("write base");

        // Head: function modified + new function added
        std::fs::write(
            head_dir.path().join("example.py"),
            "def greet(name, greeting='Hi'):\n    return f'{greeting} {name}'\n\ndef farewell(name):\n    return f'Goodbye {name}'\n",
        )
        .expect("write head");

        let output =
            run_semantic_diff_pipeline(base_dir.path(), head_dir.path()).expect("diff pipeline");

        assert!(output.contains("greet"), "should mention modified function");
        assert!(output.contains("farewell"), "should mention added function");
    }

    #[test]
    fn test_semantic_diff_pipeline_no_changes() {
        let base_dir = tempfile::tempdir().expect("base tempdir");
        let head_dir = tempfile::tempdir().expect("head tempdir");

        let source = "def unchanged():\n    pass\n";
        std::fs::write(base_dir.path().join("example.py"), source).expect("write base");
        std::fs::write(head_dir.path().join("example.py"), source).expect("write head");

        let output =
            run_semantic_diff_pipeline(base_dir.path(), head_dir.path()).expect("diff pipeline");

        assert!(
            output.contains("No codegraph changes"),
            "should report no changes"
        );
    }

    #[test]
    fn test_semantic_diff_pipeline_empty_dirs() {
        let base_dir = tempfile::tempdir().expect("base tempdir");
        let head_dir = tempfile::tempdir().expect("head tempdir");

        // No Python files in either directory
        let output =
            run_semantic_diff_pipeline(base_dir.path(), head_dir.path()).expect("diff pipeline");

        assert!(
            output.contains("No codegraph changes"),
            "empty dirs should report no changes"
        );
    }

    #[test]
    fn test_semantic_diff_pipeline_deleted_function() {
        let base_dir = tempfile::tempdir().expect("base tempdir");
        let head_dir = tempfile::tempdir().expect("head tempdir");

        // Base: two functions
        std::fs::write(
            base_dir.path().join("mod.py"),
            "def keep():\n    pass\n\ndef remove_me():\n    pass\n",
        )
        .expect("write base");

        // Head: one function removed
        std::fs::write(head_dir.path().join("mod.py"), "def keep():\n    pass\n")
            .expect("write head");

        let output =
            run_semantic_diff_pipeline(base_dir.path(), head_dir.path()).expect("diff pipeline");

        assert!(
            output.contains("remove_me"),
            "should mention removed function"
        );
    }

    #[test]
    fn test_semantic_diff_pipeline_class_changes() {
        let base_dir = tempfile::tempdir().expect("base tempdir");
        let head_dir = tempfile::tempdir().expect("head tempdir");

        std::fs::write(
            base_dir.path().join("models.py"),
            "class Dog:\n    def bark(self):\n        return 'woof'\n",
        )
        .expect("write base");

        std::fs::write(
            head_dir.path().join("models.py"),
            "class Dog:\n    def bark(self):\n        return 'WOOF'\n\n    def sit(self):\n        pass\n",
        )
        .expect("write head");

        let output =
            run_semantic_diff_pipeline(base_dir.path(), head_dir.path()).expect("diff pipeline");

        // Should detect the change in bark and the new sit method
        assert!(
            output.contains("bark") || output.contains("sit") || output.contains("Dog"),
            "should detect class/method changes"
        );
    }

    // ── CI integration tests ──

    #[test]
    fn test_checks_shows_ci_results_when_present() {
        let mut proposals = ProposalStore::new();
        let mut comments = CommentStore::new();
        let mut ci = CiResultStore::new();

        // Create proposal
        run_pr_command(
            &PrCommands::Create {
                title: "CI test".to_string(),
                base: "main".to_string(),
                body: None,
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "Mini",
        )
        .expect("create");
        let id = get_first_proposal_id(&proposals);

        // Manually record a CI result
        ci.record_result(&CiResultInput {
            proposal_id: &id,
            status: CiStatus::Passed,
            test_passed: 74,
            test_failed: 0,
            clippy_warnings: 0,
            fmt_clean: true,
            duration_secs: 12.5,
            error_message: None,
            summary: "74 passed, 0 failed",
        })
        .expect("record ci");

        // Checks should now show CI results
        let result = ci.get_result(&id).expect("get").expect("found");
        assert_eq!(result.status, CiStatus::Passed);
        assert_eq!(result.test_passed, 74);
        assert!(result.fmt_clean);

        let formatted = result.format_checks();
        assert!(formatted.contains("PASSED"));
        assert!(formatted.contains("74 passed"));
    }

    #[test]
    fn test_checks_shows_not_run_without_ci() {
        let mut proposals = ProposalStore::new();
        let mut comments = CommentStore::new();
        let mut ci = CiResultStore::new();

        run_pr_command(
            &PrCommands::Create {
                title: "No CI yet".to_string(),
                base: "main".to_string(),
                body: None,
            },
            &mut proposals,
            &mut comments,
            &mut ci,
            "Mini",
        )
        .expect("create");
        let id = get_first_proposal_id(&proposals);

        // No CI result recorded — get_result should return None
        let result = ci.get_result(&id).expect("get");
        assert!(result.is_none());
    }

    #[test]
    fn test_ci_result_replacement_on_recheck() {
        let mut ci = CiResultStore::new();

        // First run — failed
        ci.record_result(&CiResultInput {
            proposal_id: "PROP-TEST",
            status: CiStatus::Failed,
            test_passed: 10,
            test_failed: 3,
            clippy_warnings: 2,
            fmt_clean: false,
            duration_secs: 5.0,
            error_message: None,
            summary: "10 passed, 3 failed",
        })
        .expect("first run");

        let v1 = ci.get_result("PROP-TEST").expect("get").expect("found");
        assert_eq!(v1.status, CiStatus::Failed);
        assert_eq!(v1.test_failed, 3);

        // Second run — passed (simulating recheck after fixes)
        ci.record_result(&CiResultInput {
            proposal_id: "PROP-TEST",
            status: CiStatus::Passed,
            test_passed: 13,
            test_failed: 0,
            clippy_warnings: 0,
            fmt_clean: true,
            duration_secs: 6.0,
            error_message: None,
            summary: "13 passed, 0 failed",
        })
        .expect("second run");

        let v2 = ci.get_result("PROP-TEST").expect("get").expect("found");
        assert_eq!(v2.status, CiStatus::Passed);
        assert_eq!(v2.test_passed, 13);
        assert_eq!(v2.test_failed, 0);
    }

    #[test]
    fn test_parse_test_counts() {
        assert_eq!(parse_test_counts("42 passed, 3 failed"), (42, 3));
        assert_eq!(parse_test_counts("all tests passed"), (0, 0));
        assert_eq!(parse_test_counts("18 passed"), (18, 0));
        assert_eq!(parse_test_counts("0 passed, 5 failed"), (0, 5));
    }

    #[test]
    fn test_parse_warning_count() {
        assert_eq!(parse_warning_count("5 warning(s)"), 5);
        assert_eq!(parse_warning_count("no warnings"), 1); // unparseable → default 1
    }

    #[test]
    fn test_extract_check_counts_from_suite() {
        use nusy_conductor::ci_runner::{CheckResult, CheckType, CiCheckSuite};
        use std::time::Duration;

        let suite = CiCheckSuite {
            checks: vec![
                CheckResult {
                    check_type: CheckType::Test,
                    passed: true,
                    summary: "42 passed".to_string(),
                    output: String::new(),
                    duration: Duration::from_secs(5),
                },
                CheckResult {
                    check_type: CheckType::Clippy,
                    passed: false,
                    summary: "3 warning(s)".to_string(),
                    output: String::new(),
                    duration: Duration::from_secs(2),
                },
                CheckResult {
                    check_type: CheckType::Fmt,
                    passed: false,
                    summary: "2 file(s) need formatting".to_string(),
                    output: String::new(),
                    duration: Duration::from_secs(1),
                },
            ],
            passed: false,
            total_duration: Duration::from_secs(8),
            error: None,
        };

        let (tp, tf, cw, fc) = extract_check_counts(&suite);
        assert_eq!(tp, 42);
        assert_eq!(tf, 0);
        assert_eq!(cw, 3);
        assert!(!fc);
    }

    // ── Semantic diff analysis tests (Phase 3: checks integration) ──

    #[test]
    fn test_semantic_diff_analysis_returns_stats() {
        let base_dir = tempfile::tempdir().expect("base tempdir");
        let head_dir = tempfile::tempdir().expect("head tempdir");

        // Base: one function
        std::fs::write(
            base_dir.path().join("example.py"),
            "def greet(name):\n    return f'Hello {name}'\n",
        )
        .expect("write base");

        // Head: function modified + new function added
        std::fs::write(
            head_dir.path().join("example.py"),
            "def greet(name, greeting='Hi'):\n    return f'{greeting} {name}'\n\ndef farewell(name):\n    return f'Goodbye {name}'\n",
        )
        .expect("write head");

        let result = run_semantic_diff_analysis(base_dir.path(), head_dir.path())
            .expect("analysis should succeed");
        let semantic = result.expect("should detect changes");

        assert!(
            semantic.stats.added > 0 || semantic.stats.modified > 0,
            "should report added or modified functions"
        );
        assert!(
            semantic.stats.files_touched > 0,
            "should touch at least one file"
        );
    }

    #[test]
    fn test_semantic_diff_analysis_none_when_identical() {
        let base_dir = tempfile::tempdir().expect("base tempdir");
        let head_dir = tempfile::tempdir().expect("head tempdir");

        let source = "def unchanged():\n    pass\n";
        std::fs::write(base_dir.path().join("example.py"), source).expect("write base");
        std::fs::write(head_dir.path().join("example.py"), source).expect("write head");

        let result = run_semantic_diff_analysis(base_dir.path(), head_dir.path())
            .expect("analysis should succeed");
        assert!(result.is_none(), "identical code should return None");
    }

    #[test]
    fn test_semantic_diff_analysis_api_breaking_detection() {
        let base_dir = tempfile::tempdir().expect("base tempdir");
        let head_dir = tempfile::tempdir().expect("head tempdir");

        // Base: public function
        std::fs::write(
            base_dir.path().join("api.py"),
            "def process(data):\n    return data\n",
        )
        .expect("write base");

        // Head: function removed (API-breaking)
        std::fs::write(
            head_dir.path().join("api.py"),
            "def new_process(data, mode):\n    return data\n",
        )
        .expect("write head");

        let result = run_semantic_diff_analysis(base_dir.path(), head_dir.path())
            .expect("analysis should succeed");
        let semantic = result.expect("should detect changes");

        // Should detect both addition and removal
        assert!(
            semantic.stats.added > 0 || semantic.stats.removed > 0,
            "should detect added/removed functions"
        );
    }
}
