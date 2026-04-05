//! `nk source` subcommands — Source code transport over NATS via git bundles.
//!
//! Replaces GitHub as the remote transport layer. Git remains the local engine;
//! NATS replaces HTTPS as the wire protocol. Bundles are stored on the server
//! (Mini) keyed by branch name.
//!
//! Commands:
//! - `nk source push` — bundle current branch, upload to NATS server
//! - `nk source pull` — download branch bundle, fetch into local repo
//! - `nk source branches` — list available branches on the server
//! - `nk source delete` — remove a branch bundle from the server

use clap::Subcommand;
use std::process::Command;

/// Source transport subcommands.
#[derive(Subcommand)]
pub enum SourceCommands {
    /// Push current branch to the NATS server (git bundle)
    Push {
        /// Branch to push (default: current branch)
        #[arg(long)]
        branch: Option<String>,
        /// Base ref for incremental bundle (default: main, sends only new commits)
        #[arg(long, default_value = "main")]
        base: String,
    },
    /// Pull a branch from the NATS server
    Pull {
        /// Branch to pull (default: main)
        #[arg(long, default_value = "main")]
        branch: String,
    },
    /// List branches available on the server
    Branches,
    /// Delete a branch bundle from the server
    Delete {
        /// Branch to delete
        branch: String,
    },
}

/// Create a git bundle for the current branch.
///
/// If `base` is provided and exists locally, creates an incremental bundle
/// (only commits not in base). Otherwise creates a full bundle.
pub fn create_bundle(branch: &str, base: &str) -> Result<Vec<u8>, String> {
    let id = std::process::id();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!(
        "nk-bundle-{}-{}-{}.bundle",
        branch.replace('/', "-"),
        id,
        ts
    ));

    // Check if base exists locally
    let base_exists = Command::new("git")
        .args(["rev-parse", "--verify", base])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let bundle_result = if base_exists {
        // Incremental: only commits in branch not in base
        Command::new("git")
            .args([
                "bundle",
                "create",
                tmp.to_str().expect("temp path"),
                &format!("{base}..{branch}"),
            ])
            .output()
    } else {
        // Full bundle of the branch
        Command::new("git")
            .args(["bundle", "create", tmp.to_str().expect("temp path"), branch])
            .output()
    };

    let output = bundle_result.map_err(|e| format!("git bundle create failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // "create" returns non-zero if nothing to bundle (branch == base)
        if stderr.contains("Refusing to create empty bundle") {
            return Err("nothing to push (branch is up-to-date with base)".to_string());
        }
        return Err(format!("git bundle create failed: {stderr}"));
    }

    let data = std::fs::read(&tmp).map_err(|e| format!("failed to read bundle file: {e}"))?;
    let _ = std::fs::remove_file(&tmp);

    Ok(data)
}

/// Apply a git bundle to the local repo.
///
/// Fetches refs from the bundle into the local repo. Does NOT merge —
/// the caller decides whether to merge, rebase, or checkout.
pub fn apply_bundle(data: &[u8], branch: &str) -> Result<String, String> {
    let id = std::process::id();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!(
        "nk-pull-{}-{}-{}.bundle",
        branch.replace('/', "-"),
        id,
        ts
    ));

    std::fs::write(&tmp, data).map_err(|e| format!("failed to write bundle: {e}"))?;

    // Verify the bundle
    let verify = Command::new("git")
        .args(["bundle", "verify", tmp.to_str().expect("temp path")])
        .output()
        .map_err(|e| format!("git bundle verify failed: {e}"))?;

    if !verify.status.success() {
        let stderr = String::from_utf8_lossy(&verify.stderr);
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("bundle verification failed: {stderr}"));
    }

    // Fetch from the bundle
    let fetch = Command::new("git")
        .args([
            "fetch",
            tmp.to_str().expect("temp path"),
            &format!("{branch}:{branch}"),
        ])
        .output()
        .map_err(|e| format!("git fetch from bundle failed: {e}"))?;

    let _ = std::fs::remove_file(&tmp);

    if !fetch.status.success() {
        let stderr = String::from_utf8_lossy(&fetch.stderr);
        // Non-fast-forward is a conflict signal, not a hard failure
        if stderr.contains("non-fast-forward") {
            return Err(format!(
                "conflict: {branch} has diverged (non-fast-forward). Rebase or merge manually."
            ));
        }
        return Err(format!("git fetch failed: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&fetch.stdout);
    Ok(format!("Pulled {branch} successfully\n{stdout}"))
}

/// Get the current branch name.
pub fn current_branch() -> Result<String, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map_err(|e| format!("git rev-parse failed: {e}"))?;

    if !output.status.success() {
        return Err("not in a git repository".to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: SourceCommands,
    }

    #[test]
    fn test_parse_push_default() {
        let cli = TestCli::parse_from(["test", "push"]);
        match cli.command {
            SourceCommands::Push { branch, base } => {
                assert!(branch.is_none());
                assert_eq!(base, "main");
            }
            _ => panic!("expected Push"),
        }
    }

    #[test]
    fn test_parse_push_with_branch() {
        let cli = TestCli::parse_from(["test", "push", "--branch", "feature/foo"]);
        match cli.command {
            SourceCommands::Push { branch, .. } => {
                assert_eq!(branch, Some("feature/foo".to_string()));
            }
            _ => panic!("expected Push"),
        }
    }

    #[test]
    fn test_parse_pull_default() {
        let cli = TestCli::parse_from(["test", "pull"]);
        match cli.command {
            SourceCommands::Pull { branch } => {
                assert_eq!(branch, "main");
            }
            _ => panic!("expected Pull"),
        }
    }

    #[test]
    fn test_parse_pull_branch() {
        let cli = TestCli::parse_from(["test", "pull", "--branch", "expedition/foo"]);
        match cli.command {
            SourceCommands::Pull { branch } => {
                assert_eq!(branch, "expedition/foo");
            }
            _ => panic!("expected Pull"),
        }
    }

    #[test]
    fn test_parse_branches() {
        let cli = TestCli::parse_from(["test", "branches"]);
        assert!(matches!(cli.command, SourceCommands::Branches));
    }

    #[test]
    fn test_parse_delete() {
        let cli = TestCli::parse_from(["test", "delete", "old-branch"]);
        match cli.command {
            SourceCommands::Delete { branch } => {
                assert_eq!(branch, "old-branch");
            }
            _ => panic!("expected Delete"),
        }
    }

    #[test]
    fn test_current_branch_in_git_repo() {
        // We're in a git repo during tests
        let branch = current_branch();
        assert!(branch.is_ok());
        assert!(!branch.unwrap().is_empty());
    }

    // ── Core function tests (Phase 4) ───────────────────────────────────
    //
    // These tests use `git -C <dir>` to avoid `set_current_dir` race conditions
    // when tests run in parallel. `create_bundle` and `apply_bundle` use CWD,
    // so we call git commands directly in tests to avoid CWD mutation.

    /// Helper: create a temp git repo with an initial commit.
    fn init_temp_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let path = dir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .expect("git config name");

        std::fs::write(path.join("README.md"), "# Test repo\n").expect("write readme");
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "initial commit"])
            .current_dir(path)
            .output()
            .expect("git commit");

        dir
    }

    /// Helper: add a commit to a temp repo.
    fn add_commit(dir: &std::path::Path, filename: &str, content: &str, message: &str) {
        std::fs::write(dir.join(filename), content).expect("write file");
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(dir)
            .output()
            .expect("git commit");
    }

    /// Create a bundle using git -C (avoids CWD mutation).
    fn create_bundle_in(
        dir: &std::path::Path,
        branch: &str,
        base: &str,
    ) -> Result<Vec<u8>, String> {
        let id = std::process::id();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = std::env::temp_dir().join(format!(
            "nk-test-{}-{}-{}.bundle",
            branch.replace('/', "-"),
            id,
            ts
        ));

        let base_exists = Command::new("git")
            .args(["-C", &dir.to_string_lossy(), "rev-parse", "--verify", base])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        let output = if base_exists {
            Command::new("git")
                .args([
                    "-C",
                    &dir.to_string_lossy(),
                    "bundle",
                    "create",
                    tmp.to_str().expect("tmp"),
                    &format!("{base}..{branch}"),
                ])
                .output()
        } else {
            Command::new("git")
                .args([
                    "-C",
                    &dir.to_string_lossy(),
                    "bundle",
                    "create",
                    tmp.to_str().expect("tmp"),
                    branch,
                ])
                .output()
        }
        .map_err(|e| format!("git bundle create: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("Refusing to create empty bundle") {
                return Err("nothing to push".to_string());
            }
            return Err(format!("git bundle create failed: {stderr}"));
        }

        let data = std::fs::read(&tmp).map_err(|e| format!("read: {e}"))?;
        let _ = std::fs::remove_file(&tmp);
        Ok(data)
    }

    /// Apply a bundle to a repo using git -C.
    fn apply_bundle_in(dir: &std::path::Path, data: &[u8], branch: &str) -> Result<String, String> {
        let id = std::process::id();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = std::env::temp_dir().join(format!("nk-test-pull-{}-{}.bundle", id, ts));
        std::fs::write(&tmp, data).map_err(|e| format!("write: {e}"))?;

        let fetch = Command::new("git")
            .args([
                "-C",
                &dir.to_string_lossy(),
                "fetch",
                tmp.to_str().expect("tmp"),
                &format!("{branch}:{branch}"),
            ])
            .output()
            .map_err(|e| format!("git fetch: {e}"))?;

        let _ = std::fs::remove_file(&tmp);

        if !fetch.status.success() {
            let stderr = String::from_utf8_lossy(&fetch.stderr);
            if stderr.contains("non-fast-forward") {
                return Err(format!("conflict: non-fast-forward on {branch}"));
            }
            return Err(format!("git fetch failed: {stderr}"));
        }

        Ok(format!("Pulled {branch}"))
    }

    #[test]
    fn test_create_bundle_produces_bytes() {
        let repo = init_temp_repo();
        let path = repo.path();

        Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(path)
            .output()
            .expect("checkout");
        add_commit(path, "feature.txt", "hello", "add feature");

        let result = create_bundle_in(path, "feature", "main");
        assert!(result.is_ok(), "create_bundle failed: {:?}", result.err());
        let data = result.unwrap();
        assert!(!data.is_empty());
        assert!(data.len() > 50, "bundle too small: {} bytes", data.len());
    }

    #[test]
    fn test_create_bundle_nothing_to_push() {
        let repo = init_temp_repo();
        let result = create_bundle_in(repo.path(), "main", "main");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("nothing to push"));
    }

    #[test]
    fn test_bundle_round_trip() {
        let src = init_temp_repo();
        Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(src.path())
            .output()
            .expect("checkout");
        add_commit(src.path(), "new.rs", "fn main() {}", "add new file");

        let bundle_data = create_bundle_in(src.path(), "feature", "main").expect("create bundle");

        let dst = init_temp_repo();
        let result = apply_bundle_in(dst.path(), &bundle_data, "feature");
        assert!(result.is_ok(), "apply_bundle failed: {:?}", result.err());

        let branches = Command::new("git")
            .args(["branch", "--list", "feature"])
            .current_dir(dst.path())
            .output()
            .expect("git branch");
        assert!(
            String::from_utf8_lossy(&branches.stdout).contains("feature"),
            "feature branch should exist in dst"
        );
    }

    #[test]
    fn test_apply_bundle_conflict_detection() {
        let src = init_temp_repo();
        Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(src.path())
            .output()
            .expect("checkout");
        add_commit(src.path(), "file.txt", "src version", "src commit");

        let bundle_data = create_bundle_in(src.path(), "feature", "main").expect("bundle");

        let dst = init_temp_repo();
        Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(dst.path())
            .output()
            .expect("checkout");
        add_commit(dst.path(), "file.txt", "dst version", "dst commit");
        // Switch back to main so we can fetch into feature
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dst.path())
            .output()
            .expect("checkout main");

        let result = apply_bundle_in(dst.path(), &bundle_data, "feature");
        assert!(result.is_err(), "should detect conflict");
        let err = result.unwrap_err();
        assert!(
            err.contains("conflict") || err.contains("non-fast-forward"),
            "error should mention conflict: {err}"
        );
    }
}
