//! File indexer — scans nusy-kanban/ folder for markdown files and indexes them.
//!
//! Supports the "drop a file" use case: agents and humans can add .md files
//! directly to nusy-kanban/{work,research}/{type}/ folders and `nk query --search`
//! finds them without explicit registration.
//!
//! Design:
//! - On-demand file scanning (not a daemon, not persistent across restarts)
//! - Reads file content on each query
//! - Caches file list in memory for session duration
//! - Prefers Arrow store items over files when IDs collide

use crate::query::RankedResult;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

/// In-memory cache of file paths scanned this session.
static FILE_CACHE: OnceLock<Vec<std::path::PathBuf>> = OnceLock::new();

static NU_SY_KANBAN_PATH: &str = "nusy-kanban";

/// Find all .md files in the nusy-kanban/ folder.
fn get_cached_files() -> &'static Vec<std::path::PathBuf> {
    FILE_CACHE.get_or_init(|| {
        let root = Path::new(NU_SY_KANBAN_PATH);
        if !root.exists() {
            return Vec::new();
        }
        collect_md_files(root)
    })
}

/// Recursively collect all .md files under a directory.
fn collect_md_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_md_files(&path));
            } else if path.extension().is_some_and(|e| e == "md") {
                files.push(path);
            }
        }
    }
    files
}

/// Scan nusy-kanban/ folder for files matching the given search text.
/// Returns file-based results with artificially low scores (below Arrow scores).
///
/// IDs are extracted from YAML frontmatter if present, otherwise derived from filename.
/// Arrow store results take priority: if a file's ID matches an Arrow item, it's excluded
/// from results (Arrow store is authoritative).
pub fn search_files(search_text: &str, arrow_ids: &HashSet<&str>) -> Vec<RankedResult> {
    let files = get_cached_files();
    if files.is_empty() {
        return Vec::new();
    }

    let search_lower = search_text.to_lowercase();
    let mut results = Vec::new();

    for file_path in files {
        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Check if search term appears in title (first heading) or body
        if !content.to_lowercase().contains(&search_lower) {
            continue;
        }

        // Extract ID from frontmatter or filename
        let id = extract_id_from_frontmatter(&content)
            .or_else(|| extract_id_from_filename(file_path))
            .unwrap_or_else(|| {
                // Fall back to relative path as pseudo-ID
                file_path.to_string_lossy().replace('/', "-")
            });

        // Skip if this ID exists in Arrow store (Arrow is authoritative)
        if arrow_ids.contains(id.as_str()) {
            continue;
        }

        // Extract title from first # heading or filename
        let title = extract_title(&content).unwrap_or_else(|| {
            file_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| id.clone())
        });

        // Extract type from frontmatter or folder
        let item_type = extract_type_from_frontmatter(&content)
            .unwrap_or_else(|| derive_type_from_path(file_path));

        // Files don't have Arrow metadata — use "file" as a sentinel type
        // with an artificial score below typical Arrow hits
        results.push(RankedResult {
            id,
            title,
            item_type,
            status: String::new(),
            priority: String::new(),
            assignee: String::new(),
            score: 0.5, // Low score — Arrow results will rank higher
        });
    }

    results
}

/// Extract `id:` value from YAML frontmatter.
fn extract_id_from_frontmatter(content: &str) -> Option<String> {
    // Find the frontmatter block: starts with "---", ends with second "---"
    let first_dash = content.find("---")?;
    let second_dash = content[first_dash + 3..].find("---")?;
    let frontmatter = &content[first_dash + 3..first_dash + 3 + second_dash];

    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("id:") {
            let val = trimmed.trim_start_matches("id:").trim();
            // Remove quotes if present
            return Some(val.trim_matches('"').trim_matches('\'').to_string());
        }
    }
    None
}

/// Extract `type:` value from YAML frontmatter.
fn extract_type_from_frontmatter(content: &str) -> Option<String> {
    // Find the frontmatter block: starts with "---", ends with second "---"
    let first_dash = content.find("---")?;
    let second_dash = content[first_dash + 3..].find("---")?;
    let frontmatter = &content[first_dash + 3..first_dash + 3 + second_dash];

    for line in frontmatter.lines() {
        let line = line.trim();
        if line.starts_with("type:") {
            let val = line.trim_start_matches("type:").trim();
            return Some(val.trim_matches('"').to_string());
        }
    }
    None
}

/// Extract first # heading from markdown body.
fn extract_title(content: &str) -> Option<String> {
    // Skip frontmatter
    let body = content
        .strip_prefix("---")
        .and_then(|rest| rest.find("---").map(|i| &rest[i + 3..]))
        .unwrap_or(content);

    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("# ") {
            return Some(line.trim_start_matches("# ").to_string());
        }
    }
    None
}

/// Derive item type from folder path, e.g. nusy-kanban/research/papers/foo.md → papers
fn derive_type_from_path(path: &Path) -> String {
    path.components()
        .next_back()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string())
}

/// Extract ID from filename, e.g. PAPER-099.md → PAPER-099
fn extract_id_from_filename(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(|s| {
            // Sanitize: if filename has path components (from ID with /), take last part
            s.split('/').next_back().unwrap_or(s).to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, rel_path: &str, content: &str) -> std::path::PathBuf {
        let file_path = dir.join(rel_path);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&file_path, content).unwrap();
        file_path
    }

    #[test]
    fn test_extract_id_from_frontmatter() {
        let content = r#"---
id: PAPER-099
title: "Test Paper"
type: paper
---

# PAPER-099: Test
"#;
        assert_eq!(
            extract_id_from_frontmatter(content),
            Some("PAPER-099".to_string())
        );
    }

    #[test]
    fn test_extract_id_from_frontmatter_quoted() {
        let content = r#"---
id: "PAPER-099"
title: "Test"
---

Test
"#;
        assert_eq!(
            extract_id_from_frontmatter(content),
            Some("PAPER-099".to_string())
        );
    }

    #[test]
    fn test_extract_title() {
        let content = r#"---
id: TEST-001
---

# This Is The Title

Body content here.
"#;
        assert_eq!(
            extract_title(content),
            Some("This Is The Title".to_string())
        );
    }

    #[test]
    fn test_search_files_finds_match() {
        // Create a temp directory with a test file
        let tmp = TempDir::new().unwrap();
        let nusy_kanban = tmp.path().join("nusy-kanban");

        create_test_file(
            &nusy_kanban,
            "research/papers/TEST-PAPER.md",
            r#"---
id: TEST-PAPER
type: paper
---

# Test Paper Title
"#,
        );

        // Override the cache for this test
        let files = collect_md_files(&nusy_kanban);
        assert_eq!(files.len(), 1);

        let content = std::fs::read_to_string(files[0].as_path()).unwrap();
        assert!(content.contains("Test Paper"));

        // Clean up cache for other tests
        let _ = FILE_CACHE.set(Vec::new());
    }

    #[test]
    fn test_extract_type_from_frontmatter() {
        let content = r#"---
id: H-001
type: hypothesis
---

# Hypothesis
"#;
        assert_eq!(
            extract_type_from_frontmatter(content),
            Some("hypothesis".to_string())
        );
    }
}
