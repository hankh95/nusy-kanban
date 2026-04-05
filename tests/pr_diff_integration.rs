//! Integration tests for `nk pr diff` — semantic diff via git worktree.
//!
//! EXP-3057 Phase 3: Tests that exercise the full pipeline from
//! git branches through codegraph ingestion to formatted output.

use std::path::Path;
use std::process::Command;

/// Create a minimal git repo in a temp directory with base and feature branches.
///
/// Returns (repo_path, base_branch, feature_branch).
fn setup_git_repo_with_branches(
    base_files: &[(&str, &str)],
    feature_files: &[(&str, &str)],
) -> (tempfile::TempDir, String, String) {
    let repo_dir = tempfile::tempdir().expect("create temp repo dir");
    let repo = repo_dir.path();

    // Init repo
    run_git(repo, &["init", "-b", "main"]);
    run_git(repo, &["config", "user.email", "test@test.com"]);
    run_git(repo, &["config", "user.name", "Test"]);

    // Write base files and commit
    for (name, content) in base_files {
        std::fs::write(repo.join(name), content).expect("write base file");
    }
    run_git(repo, &["add", "."]);
    run_git(repo, &["commit", "-m", "base commit"]);

    // Create feature branch with changes
    run_git(repo, &["checkout", "-b", "feature/test"]);
    for (name, content) in feature_files {
        std::fs::write(repo.join(name), content).expect("write feature file");
    }
    run_git(repo, &["add", "."]);
    run_git(repo, &["commit", "-m", "feature changes"]);

    (repo_dir, "main".to_string(), "feature/test".to_string())
}

fn run_git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_pr_diff_with_git_branches() {
    let base_py = "def hello():\n    return 'hello'\n";
    let feature_py = "def hello():\n    return 'hi there'\n\ndef goodbye():\n    return 'bye'\n";

    let (repo_dir, _base, _feature) =
        setup_git_repo_with_branches(&[("app.py", base_py)], &[("app.py", feature_py)]);
    let repo = repo_dir.path();

    // We're on the feature branch — ingest head from working tree
    let head_result = nusy_codegraph::ingest_directory(repo).expect("head ingest");
    let head_nodes = head_result.nodes_batch().expect("head nodes");
    let head_edges = head_result.edges_batch().expect("head edges");

    // Create a worktree for the base branch
    let base_worktree = repo.join("_base_worktree");
    run_git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            &base_worktree.display().to_string(),
            "main",
        ],
    );

    let base_result = nusy_codegraph::ingest_directory(&base_worktree).expect("base ingest");
    let base_nodes = base_result.nodes_batch().expect("base nodes");

    // Cleanup worktree before assertions (in case assertions panic)
    let _ = Command::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            &base_worktree.display().to_string(),
        ])
        .current_dir(repo)
        .output();

    // Run the diff pipeline
    let diff = nusy_codegraph::codegraph_diff(&base_nodes, &head_nodes).expect("diff");
    assert!(!diff.entries.is_empty(), "should have changes");

    let semantic = nusy_codegraph::semantic_diff(&diff, &head_nodes, &head_edges);
    let output = nusy_codegraph::format_semantic_diff(&semantic);

    assert!(output.contains("hello"), "should show modified function");
    assert!(output.contains("goodbye"), "should show added function");
    assert!(output.contains("app.py"), "should show file path");
}

#[test]
fn test_pr_diff_no_python_files() {
    // Repo with only non-Python files
    let (repo_dir, _base, _feature) = setup_git_repo_with_branches(
        &[("README.md", "# Hello")],
        &[("README.md", "# Hello World")],
    );
    let repo = repo_dir.path();

    let head_result = nusy_codegraph::ingest_directory(repo).expect("head ingest");
    let head_nodes = head_result.nodes_batch().expect("head nodes");

    let base_worktree = repo.join("_base_worktree");
    run_git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            &base_worktree.display().to_string(),
            "main",
        ],
    );

    let base_result = nusy_codegraph::ingest_directory(&base_worktree).expect("base ingest");
    let base_nodes = base_result.nodes_batch().expect("base nodes");

    let _ = Command::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            &base_worktree.display().to_string(),
        ])
        .current_dir(repo)
        .output();

    let diff = nusy_codegraph::codegraph_diff(&base_nodes, &head_nodes).expect("diff");
    assert!(
        diff.entries.is_empty(),
        "no Python files means no codegraph changes"
    );
}

#[test]
fn test_pr_diff_multiple_files() {
    let (repo_dir, _base, _feature) = setup_git_repo_with_branches(
        &[
            ("models.py", "class User:\n    pass\n"),
            ("utils.py", "def helper():\n    pass\n"),
        ],
        &[
            (
                "models.py",
                "class User:\n    def name(self):\n        return 'User'\n",
            ),
            ("utils.py", "def helper():\n    pass\n"),
            ("new_module.py", "def fresh():\n    return True\n"),
        ],
    );
    let repo = repo_dir.path();

    let head_result = nusy_codegraph::ingest_directory(repo).expect("head ingest");
    let head_nodes = head_result.nodes_batch().expect("head nodes");
    let head_edges = head_result.edges_batch().expect("head edges");

    let base_worktree = repo.join("_base_worktree");
    run_git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            &base_worktree.display().to_string(),
            "main",
        ],
    );
    let base_result = nusy_codegraph::ingest_directory(&base_worktree).expect("base ingest");
    let base_nodes = base_result.nodes_batch().expect("base nodes");

    let _ = Command::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            &base_worktree.display().to_string(),
        ])
        .current_dir(repo)
        .output();

    let diff = nusy_codegraph::codegraph_diff(&base_nodes, &head_nodes).expect("diff");
    assert!(!diff.entries.is_empty(), "should have changes");

    let semantic = nusy_codegraph::semantic_diff(&diff, &head_nodes, &head_edges);
    let output = nusy_codegraph::format_semantic_diff(&semantic);

    // Should cover changes across multiple files
    assert!(
        output.contains("models.py") || output.contains("new_module.py"),
        "should reference changed files"
    );
}
