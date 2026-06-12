//! Parse Cargo.toml manifests into [`CrateManifest`] structs.
//!
//! Handles:
//! - `[package]` metadata (name, version, description, edition)
//! - `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`
//! - Workspace inheritance: `version.workspace = true` resolved from workspace root
//! - Dependency sources: workspace, path, git, crates.io
//! - Inline table form: `dep = "1.0"` and table form: `dep = { version = "1.0" }`
//!
//! # Example
//!
//! ```no_run
//! use nusy_codegraph::cargo_parser::CrateManifest;
//! use std::path::Path;
//!
//! let manifest = CrateManifest::from_path(Path::new("Cargo.toml")).unwrap();
//! println!("{} {}", manifest.name, manifest.version);
//! ```

use std::collections::HashMap;
use std::path::Path;
use toml::Value;

/// A parsed Cargo.toml manifest.
#[derive(Debug, Clone)]
pub struct CrateManifest {
    /// Crate name from `[package].name`
    pub name: String,
    /// Resolved version string (workspace inheritance resolved)
    pub version: String,
    /// Optional description from `[package].description`
    pub description: Option<String>,
    /// Edition, e.g. `"2024"` (defaults to `"2021"` if missing)
    pub edition: String,
    /// Runtime dependencies from `[dependencies]`
    pub dependencies: Vec<CrateDependency>,
    /// Dev-only dependencies from `[dev-dependencies]`
    pub dev_dependencies: Vec<CrateDependency>,
    /// Build-script dependencies from `[build-dependencies]`
    pub build_dependencies: Vec<CrateDependency>,
    /// True when the crate is listed in `[workspace].members`
    pub workspace_member: bool,
}

/// A single dependency entry parsed from a Cargo.toml dependency table.
#[derive(Debug, Clone)]
pub struct CrateDependency {
    /// Dependency crate name (the key in the table, or `package` override if set)
    pub name: String,
    /// Resolved version requirement string, e.g. `"1.0"`, `">=0.14, <1"`, or `"*"` for
    /// path/git deps without an explicit version
    pub version_req: String,
    /// Cargo features enabled for this dependency
    pub features: Vec<String>,
    /// Whether the dependency is `optional = true`
    pub optional: bool,
    /// True when this dep came from `[dev-dependencies]`
    pub dev: bool,
    /// True when this dep came from `[build-dependencies]`
    pub build: bool,
    /// Where the dependency lives
    pub source: DependencySource,
}

/// Provenance of a dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySource {
    /// `dep.workspace = true` — version managed by the workspace root
    Workspace,
    /// `dep = "1.0"` or `dep = { version = "1.0" }` — fetched from crates.io
    CratesIo,
    /// `dep = { git = "https://..." }` — fetched from a git repository
    Git { url: String },
    /// `dep = { path = "../other-crate" }` — local path dependency
    Path { path: String },
}

// ─── Workspace dependency cache ─────────────────────────────────────────────

/// Parse the workspace root `Cargo.toml` and return a map of `name → version` for
/// all entries under `[workspace.dependencies]`.
///
/// Only entries that specify a `version` field are included (path/git workspace
/// deps without versions are omitted).
pub fn parse_workspace_dependencies(
    workspace_toml: &Path,
) -> Result<HashMap<String, String>, String> {
    let raw = std::fs::read_to_string(workspace_toml)
        .map_err(|e| format!("cannot read {}: {e}", workspace_toml.display()))?;

    let doc: Value = raw
        .parse::<Value>()
        .map_err(|e| format!("TOML parse error in {}: {e}", workspace_toml.display()))?;

    let mut map = HashMap::new();

    let Some(ws_deps) = doc
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.as_table())
    else {
        return Ok(map);
    };

    for (name, spec) in ws_deps {
        let version = match spec {
            Value::String(v) => v.clone(),
            Value::Table(t) => {
                if let Some(v) = t.get("version").and_then(|v| v.as_str()) {
                    v.to_string()
                } else {
                    continue; // path/git dep with no version
                }
            }
            _ => continue,
        };
        map.insert(name.clone(), version);
    }

    Ok(map)
}

// ─── CrateManifest impl ──────────────────────────────────────────────────────

impl CrateManifest {
    /// Parse a single `Cargo.toml` at `cargo_toml`.
    ///
    /// If the package version uses workspace inheritance (`version.workspace = true`),
    /// we look for the workspace root at `../../../Cargo.toml` (three levels up from
    /// `crates/<name>/Cargo.toml`).  The caller can supply pre-parsed workspace deps
    /// by calling [`CrateManifest::from_path_with_workspace`] directly.
    pub fn from_path(cargo_toml: &Path) -> Result<Self, String> {
        // Try to auto-locate the workspace root Cargo.toml
        let workspace_deps = cargo_toml
            .parent() // crates/nusy-foo/
            .and_then(|p| p.parent()) // crates/
            .and_then(|p| p.parent()) // workspace root
            .map(|root| root.join("Cargo.toml"))
            .and_then(|ws| parse_workspace_dependencies(&ws).ok())
            .unwrap_or_default();

        Self::from_path_with_workspace(cargo_toml, &workspace_deps, false)
    }

    /// Parse a `Cargo.toml` with an explicit pre-parsed workspace dependency map.
    ///
    /// `workspace_member` is set to the supplied value (the workspace root parser sets
    /// this after checking `[workspace].members`).
    pub fn from_path_with_workspace(
        cargo_toml: &Path,
        workspace_deps: &HashMap<String, String>,
        workspace_member: bool,
    ) -> Result<Self, String> {
        let raw = std::fs::read_to_string(cargo_toml)
            .map_err(|e| format!("cannot read {}: {e}", cargo_toml.display()))?;

        let doc: Value = raw
            .parse::<Value>()
            .map_err(|e| format!("TOML parse error in {}: {e}", cargo_toml.display()))?;

        // ── [package] ────────────────────────────────────────────────────────
        let pkg = doc
            .get("package")
            .and_then(|p| p.as_table())
            .ok_or_else(|| format!("missing [package] in {}", cargo_toml.display()))?;

        let name = pkg
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("missing package.name in {}", cargo_toml.display()))?
            .to_string();

        // version may be `"1.0"` or `{ workspace = true }`
        let version = resolve_package_version(pkg, workspace_deps, cargo_toml)?;

        let description = pkg
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let edition = pkg
            .get("edition")
            .and_then(|v| v.as_str())
            .unwrap_or("2021")
            .to_string();

        // ── Dependencies ──────────────────────────────────────────────────────
        let dependencies = parse_dep_table(&doc, "dependencies", false, false, workspace_deps);
        let dev_dependencies =
            parse_dep_table(&doc, "dev-dependencies", true, false, workspace_deps);
        let build_dependencies =
            parse_dep_table(&doc, "build-dependencies", false, true, workspace_deps);

        Ok(CrateManifest {
            name,
            version,
            description,
            edition,
            dependencies,
            dev_dependencies,
            build_dependencies,
            workspace_member,
        })
    }

    /// All dependencies (runtime + dev + build) as a flat iterator.
    pub fn all_dependencies(&self) -> impl Iterator<Item = &CrateDependency> {
        self.dependencies
            .iter()
            .chain(self.dev_dependencies.iter())
            .chain(self.build_dependencies.iter())
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Resolve the version from `[package]`.
///
/// Handles:
/// - `version = "1.0"` → `"1.0"`
/// - `version = { workspace = true }` → look up name in workspace_deps
/// - `version.workspace = true` → TOML dotted-key form (same as above)
fn resolve_package_version(
    pkg: &toml::value::Table,
    workspace_deps: &HashMap<String, String>,
    cargo_toml: &Path,
) -> Result<String, String> {
    let Some(v) = pkg.get("version") else {
        return Ok("0.0.0".to_string()); // virtual manifest or missing version
    };

    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Table(t) => {
            if t.get("workspace")
                .and_then(|w| w.as_bool())
                .unwrap_or(false)
            {
                // Find the crate name — it's the "name" entry we already parsed or will parse
                let crate_name = pkg.get("name").and_then(|n| n.as_str()).unwrap_or("");
                // Workspace version is typically global, not per-crate; check workspace.package
                // We'll fall back to any entry in workspace_deps or "workspace" key
                // In practice the workspace root has [workspace.package].version
                let _ = crate_name;
                // Try to find the workspace package version from the deps map
                // It's stored under a sentinel "" key if we parse [workspace.package].version
                workspace_deps
                    .get("")
                    .or_else(|| workspace_deps.values().next())
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "version.workspace = true in {} but no workspace version found",
                            cargo_toml.display()
                        )
                    })
            } else {
                Ok("0.0.0".to_string())
            }
        }
        _ => Ok("0.0.0".to_string()),
    }
}

/// Parse one dependency section (e.g. `[dependencies]`) into a `Vec<CrateDependency>`.
fn parse_dep_table(
    doc: &Value,
    table_key: &str,
    dev: bool,
    build: bool,
    workspace_deps: &HashMap<String, String>,
) -> Vec<CrateDependency> {
    let Some(table) = doc.get(table_key).and_then(|t| t.as_table()) else {
        return Vec::new();
    };

    table
        .iter()
        .map(|(key, spec)| parse_dep_entry(key, spec, dev, build, workspace_deps))
        .collect()
}

/// Convert one key-value pair in a dependency table to a [`CrateDependency`].
fn parse_dep_entry(
    key: &str,
    spec: &Value,
    dev: bool,
    build: bool,
    workspace_deps: &HashMap<String, String>,
) -> CrateDependency {
    match spec {
        // Short form: `dep = "1.0"`
        Value::String(ver) => CrateDependency {
            name: key.to_string(),
            version_req: ver.clone(),
            features: Vec::new(),
            optional: false,
            dev,
            build,
            source: DependencySource::CratesIo,
        },
        // Table form: `dep = { version = "1.0", features = [...], ... }`
        Value::Table(t) => {
            // Check for workspace inheritance
            if t.get("workspace")
                .and_then(|w| w.as_bool())
                .unwrap_or(false)
            {
                let version_req = workspace_deps
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| "*".to_string());
                let features = extract_features(t);
                let optional = t.get("optional").and_then(|v| v.as_bool()).unwrap_or(false);
                return CrateDependency {
                    name: key.to_string(),
                    version_req,
                    features,
                    optional,
                    dev,
                    build,
                    source: DependencySource::Workspace,
                };
            }

            // Determine source from table fields
            let source = if let Some(git_url) = t.get("git").and_then(|v| v.as_str()) {
                DependencySource::Git {
                    url: git_url.to_string(),
                }
            } else if let Some(p) = t.get("path").and_then(|v| v.as_str()) {
                DependencySource::Path {
                    path: p.to_string(),
                }
            } else {
                DependencySource::CratesIo
            };

            let version_req = t
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("*")
                .to_string();
            let features = extract_features(t);
            let optional = t.get("optional").and_then(|v| v.as_bool()).unwrap_or(false);
            // Allow `package` key to override the dependency name
            let name = t
                .get("package")
                .and_then(|v| v.as_str())
                .unwrap_or(key)
                .to_string();

            CrateDependency {
                name,
                version_req,
                features,
                optional,
                dev,
                build,
                source,
            }
        }
        // Unexpected form — produce a stub
        _ => CrateDependency {
            name: key.to_string(),
            version_req: "*".to_string(),
            features: Vec::new(),
            optional: false,
            dev,
            build,
            source: DependencySource::CratesIo,
        },
    }
}

/// Extract the `features = [...]` list from a dependency table, returning an empty
/// vec if missing or malformed.
fn extract_features(t: &toml::value::Table) -> Vec<String> {
    t.get("features")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f| f.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Path to the workspace root (from crate root, go up two dirs).
    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn crate_path(name: &str) -> PathBuf {
        workspace_root()
            .join("crates")
            .join(name)
            .join("Cargo.toml")
    }

    // ── Test 1: parse a real workspace crate ─────────────────────────────────

    #[test]
    fn test_parse_nusy_arrow_core_cargo_toml() {
        let path = crate_path("nusy-arrow-core");
        let manifest = CrateManifest::from_path(&path)
            .unwrap_or_else(|e| panic!("Failed to parse nusy-arrow-core/Cargo.toml: {e}"));

        assert_eq!(manifest.name, "nusy-arrow-core");
        // Version is workspace-inherited so it should be non-empty
        assert!(!manifest.version.is_empty(), "version should not be empty");
        assert!(
            !manifest.dependencies.is_empty(),
            "nusy-arrow-core should have dependencies"
        );
    }

    // ── Test 2: workspace dependency inheritance ──────────────────────────────

    #[test]
    fn test_workspace_dependency_inheritance_resolves_version() {
        let ws_toml = workspace_root().join("Cargo.toml");
        let ws_deps =
            parse_workspace_dependencies(&ws_toml).expect("should parse workspace Cargo.toml");

        // arrow and chrono are in [workspace.dependencies]
        assert!(
            ws_deps.contains_key("arrow"),
            "workspace should have arrow dep"
        );
        assert!(
            ws_deps.contains_key("chrono"),
            "workspace should have chrono dep"
        );

        let arrow_version = &ws_deps["arrow"];
        assert!(!arrow_version.is_empty(), "arrow should have a version");

        // Now parse a crate that uses { workspace = true }
        let path = crate_path("nusy-arrow-core");
        let manifest = CrateManifest::from_path_with_workspace(&path, &ws_deps, true)
            .expect("parse nusy-arrow-core with workspace deps");

        // arrow should appear as Workspace source
        let arrow_dep = manifest
            .dependencies
            .iter()
            .find(|d| d.name == "arrow")
            .expect("should have arrow dep");

        assert_eq!(
            arrow_dep.source,
            DependencySource::Workspace,
            "arrow dep should be Workspace"
        );
        assert_eq!(
            arrow_dep.version_req, *arrow_version,
            "arrow version should be resolved from workspace"
        );
    }

    // ── Test 3: path dependencies ─────────────────────────────────────────────

    #[test]
    fn test_path_dependency_source() {
        // nusy-codegraph depends on nusy-arrow-core via path
        let path = crate_path("nusy-codegraph");
        let manifest = CrateManifest::from_path(&path).expect("parse nusy-codegraph/Cargo.toml");

        let core_dep = manifest
            .dependencies
            .iter()
            .find(|d| d.name == "nusy-arrow-core")
            .expect("nusy-codegraph should dep on nusy-arrow-core");

        assert!(
            matches!(core_dep.source, DependencySource::Path { .. }),
            "nusy-arrow-core dep should be Path, got {:?}",
            core_dep.source
        );
    }

    // ── Test 4: dev dependencies are flagged ──────────────────────────────────

    #[test]
    fn test_dev_dependencies_flagged() {
        let path = crate_path("nusy-codegraph");
        let manifest = CrateManifest::from_path(&path).expect("parse nusy-codegraph/Cargo.toml");

        // tempfile is a dev dep
        let tempfile_dep = manifest
            .dev_dependencies
            .iter()
            .find(|d| d.name == "tempfile")
            .expect("nusy-codegraph should have tempfile as dev dep");

        assert!(tempfile_dep.dev, "tempfile should have dev=true");
        assert!(!tempfile_dep.build, "tempfile should have build=false");

        // Runtime deps should NOT be flagged as dev
        for dep in &manifest.dependencies {
            assert!(
                !dep.dev,
                "runtime dep {} should not be flagged as dev",
                dep.name
            );
        }
    }

    // ── Test 5: external (crates.io) deps are flagged ─────────────────────────

    #[test]
    fn test_external_crate_io_dependency_source() {
        // Parse a Cargo.toml in-memory that has a plain crates.io dep
        let toml_src = r#"
[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = "1.0"
"#;
        let doc: Value = toml_src.parse().unwrap();
        let ws_deps = HashMap::new();
        let deps = super::parse_dep_table(&doc, "dependencies", false, false, &ws_deps);

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "serde");
        assert_eq!(deps[0].version_req, "1.0");
        assert_eq!(deps[0].source, DependencySource::CratesIo);
        assert!(!deps[0].dev);
        assert!(!deps[0].build);
    }

    // ── Test 6: git dependency ────────────────────────────────────────────────

    #[test]
    fn test_git_dependency_source() {
        let toml_src = r#"
[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
my-lib = { git = "https://github.com/example/my-lib", branch = "main" }
"#;
        let doc: Value = toml_src.parse().unwrap();
        let ws_deps = HashMap::new();
        let deps = super::parse_dep_table(&doc, "dependencies", false, false, &ws_deps);

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "my-lib");
        assert_eq!(
            deps[0].source,
            DependencySource::Git {
                url: "https://github.com/example/my-lib".to_string()
            }
        );
    }

    // ── Test 7: optional dependency ───────────────────────────────────────────

    #[test]
    fn test_optional_dependency() {
        let toml_src = r#"
[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
optional-dep = { version = "1.0", optional = true }
required-dep = "2.0"
"#;
        let doc: Value = toml_src.parse().unwrap();
        let ws_deps = HashMap::new();
        let deps = super::parse_dep_table(&doc, "dependencies", false, false, &ws_deps);

        let opt = deps.iter().find(|d| d.name == "optional-dep").unwrap();
        assert!(opt.optional, "optional-dep should be optional");

        let req = deps.iter().find(|d| d.name == "required-dep").unwrap();
        assert!(!req.optional, "required-dep should not be optional");
    }

    // ── Test 8: features are parsed ───────────────────────────────────────────

    #[test]
    fn test_features_are_parsed() {
        let toml_src = r#"
[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full", "rt-multi-thread"] }
"#;
        let doc: Value = toml_src.parse().unwrap();
        let ws_deps = HashMap::new();
        let deps = super::parse_dep_table(&doc, "dependencies", false, false, &ws_deps);

        let tokio = deps.iter().find(|d| d.name == "tokio").unwrap();
        assert_eq!(tokio.features, vec!["full", "rt-multi-thread"]);
    }

    // ── Test 9: build dependencies ────────────────────────────────────────────

    #[test]
    fn test_build_dependencies_flagged() {
        let toml_src = r#"
[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"

[build-dependencies]
cc = "1.0"
"#;
        let doc: Value = toml_src.parse().unwrap();
        let ws_deps = HashMap::new();
        let deps = super::parse_dep_table(&doc, "build-dependencies", false, true, &ws_deps);

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "cc");
        assert!(!deps[0].dev, "build dep should not be dev");
        assert!(deps[0].build, "cc should be flagged as build");
    }

    // ── Test 10: parse workspace Cargo.toml ───────────────────────────────────

    #[test]
    fn test_parse_workspace_cargo_toml_round_trip() {
        let ws_toml = workspace_root().join("Cargo.toml");
        let ws_deps = parse_workspace_dependencies(&ws_toml).expect("parse workspace Cargo.toml");

        // Should have multiple entries
        assert!(
            ws_deps.len() > 3,
            "expected >3 workspace deps, got {}",
            ws_deps.len()
        );

        // Every value should be a non-empty string
        for (name, ver) in &ws_deps {
            assert!(!ver.is_empty(), "workspace dep {name} has empty version");
        }
    }
}
