//! # `nk git` subcommands — Graph-Native Git Operations
//!
//! ## GRAPH MINDSET: NO FILES, NO SERIALIZATION TO DISK
//!
//! These commands operate on in-memory Arrow RecordBatches. Push/pull
//! transfers graph state as Parquet bytes over NATS — no files touched.
//! This works for ANY Arrow graph: being knowledge, code objects, research.
//!
//! Commands:
//! - `nk git push` — send local graph state to remote
//! - `nk git pull` — receive remote graph state
//! - `nk git clone` — first-time download of remote store
//! - `nk git log` — show commit history
//! - `nk git blame` — per-triple provenance
//! - `nk git rebase` — replay commits onto new base

use clap::Subcommand;

/// Git subcommands — graph-native versioning.
#[derive(Subcommand)]
pub enum GitCommands {
    /// Push local graph state to the remote NATS server
    Push {
        /// Store path (default: .nusy-arrow)
        #[arg(long, default_value = ".nusy-arrow")]
        store: String,
    },
    /// Pull remote graph state from the NATS server
    Pull {
        /// Store path (default: .nusy-arrow)
        #[arg(long, default_value = ".nusy-arrow")]
        store: String,
    },
    /// Clone a remote graph store (first-time download)
    Clone {
        /// Store path to write to (default: .nusy-arrow)
        #[arg(long, default_value = ".nusy-arrow")]
        store: String,
    },
    /// Show commit history
    Log {
        /// Maximum number of commits to show (0 = all)
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Store path
        #[arg(long, default_value = ".nusy-arrow")]
        store: String,
    },
    /// Show per-triple provenance (who added what and when)
    Blame {
        /// Maximum commits to walk (0 = all)
        #[arg(long, default_value = "0")]
        limit: usize,
        /// Store path
        #[arg(long, default_value = ".nusy-arrow")]
        store: String,
    },
    /// Rebase commits onto a new base
    Rebase {
        /// Old base commit ID (exclusive)
        start: String,
        /// Tip to rebase (inclusive)
        end: String,
        /// New base to replay onto
        onto: String,
        /// Store path
        #[arg(long, default_value = ".nusy-arrow")]
        store: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: GitCommands,
    }

    #[test]
    fn test_parse_push() {
        let cli = TestCli::parse_from(["test", "push"]);
        assert!(matches!(cli.command, GitCommands::Push { .. }));
    }

    #[test]
    fn test_parse_pull() {
        let cli = TestCli::parse_from(["test", "pull"]);
        assert!(matches!(cli.command, GitCommands::Pull { .. }));
    }

    #[test]
    fn test_parse_clone() {
        let cli = TestCli::parse_from(["test", "clone"]);
        assert!(matches!(cli.command, GitCommands::Clone { .. }));
    }

    #[test]
    fn test_parse_log_with_limit() {
        let cli = TestCli::parse_from(["test", "log", "--limit", "5"]);
        match cli.command {
            GitCommands::Log { limit, .. } => assert_eq!(limit, 5),
            _ => panic!("expected Log"),
        }
    }

    #[test]
    fn test_parse_blame() {
        let cli = TestCli::parse_from(["test", "blame"]);
        assert!(matches!(cli.command, GitCommands::Blame { .. }));
    }

    #[test]
    fn test_parse_rebase() {
        let cli = TestCli::parse_from(["test", "rebase", "abc", "def", "ghi"]);
        match cli.command {
            GitCommands::Rebase {
                start, end, onto, ..
            } => {
                assert_eq!(start, "abc");
                assert_eq!(end, "def");
                assert_eq!(onto, "ghi");
            }
            _ => panic!("expected Rebase"),
        }
    }

    #[test]
    fn test_default_store_path() {
        let cli = TestCli::parse_from(["test", "push"]);
        match cli.command {
            GitCommands::Push { store } => assert_eq!(store, ".nusy-arrow"),
            _ => panic!("expected Push"),
        }
    }

    #[test]
    fn test_custom_store_path() {
        let cli = TestCli::parse_from(["test", "push", "--store", "/custom/path"]);
        match cli.command {
            GitCommands::Push { store } => assert_eq!(store, "/custom/path"),
            _ => panic!("expected Push"),
        }
    }
}
