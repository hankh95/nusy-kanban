//! Python module resolver — maps dotted import paths to file paths.
//!
//! Builds a module index from a directory tree by walking `__init__.py`
//! locations and file paths, then resolves imports (absolute and relative)
//! to their corresponding source files.
//!
//! # Usage
//!
//! ```ignore
//! let resolver = PythonModuleResolver::from_root(Path::new("_archive/brain-v13/brain/"))?;
//!
//! // Resolve an absolute import
//! let path = resolver.resolve_import("brain.perception.signal_fusion", None);
//! // → Some("_archive/brain-v13/brain/perception/signal_fusion.py")
//!
//! // Resolve a relative import
//! let from_file = Path::new("brain/perception/assessors.py");
//! let path = resolver.resolve_import(".utils", Some(from_file));
//! // → Some("brain/perception/utils.py")
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Errors from the Python module resolver.
#[derive(Debug, thiserror::Error)]
pub enum ResolverError {
    #[error("IO error walking directory: {0}")]
    Io(#[from] std::io::Error),
}

/// Resolves Python import statements to file paths.
///
/// Builds an index of module dotted names → file paths from a root directory.
/// Handles absolute imports and relative imports (e.g., `from .utils import X`).
pub struct PythonModuleResolver {
    root: PathBuf,
    /// Dotted module name → relative file path (from root).
    module_to_file: HashMap<String, PathBuf>,
}

impl PythonModuleResolver {
    /// Build a resolver from a Python package root directory.
    ///
    /// Walks the directory tree, discovering all `.py` files and building
    /// the dotted-name → path index.
    pub fn from_root(root: &Path) -> Result<Self, ResolverError> {
        let mut module_to_file = HashMap::new();
        build_module_index(root, root, &mut module_to_file)?;
        Ok(Self {
            root: root.to_path_buf(),
            module_to_file,
        })
    }

    /// Resolve an import statement to a file path, if possible.
    ///
    /// # Arguments
    /// - `import_stmt`: the module name as it appears in source. For relative
    ///   imports (starting with `.`), the `from_file` context is required.
    ///   Examples: `"brain.perception.signal_fusion"`, `".utils"`, `"..models"`
    /// - `from_file`: the file that contains the import (needed for relative resolution).
    ///   Should be a path relative to `root`. If `None`, relative imports fail.
    ///
    /// # Returns
    /// The absolute file path if resolvable, `None` otherwise.
    pub fn resolve_import(&self, import_stmt: &str, from_file: Option<&Path>) -> Option<PathBuf> {
        if import_stmt.starts_with('.') {
            self.resolve_relative(import_stmt, from_file?)
        } else {
            self.resolve_absolute(import_stmt)
        }
    }

    /// Number of modules in the index.
    pub fn module_count(&self) -> usize {
        self.module_to_file.len()
    }

    /// Whether the given dotted module name is known.
    pub fn knows_module(&self, dotted: &str) -> bool {
        self.module_to_file.contains_key(dotted)
    }

    /// Root directory this resolver was built from.
    pub fn root(&self) -> &Path {
        &self.root
    }

    // ─── Private resolution logic ────────────────────────────────────────────

    fn resolve_absolute(&self, module: &str) -> Option<PathBuf> {
        // Direct lookup in the index
        if let Some(p) = self.module_to_file.get(module) {
            return Some(self.root.join(p));
        }

        // Fallback: strip leading package component and try again
        // (handles cases where root is the package directory, e.g., root = "brain/")
        if let Some((_pkg, rest)) = module.split_once('.')
            && let Some(p) = self.module_to_file.get(rest)
        {
            return Some(self.root.join(p));
        }

        None
    }

    fn resolve_relative(&self, import_stmt: &str, from_file: &Path) -> Option<PathBuf> {
        // Count leading dots: "." → 1 level up, ".." → 2 levels up
        let dots = import_stmt.chars().take_while(|c| *c == '.').count();
        let rest = &import_stmt[dots..];

        // `from_file` is relative to root (e.g., "perception/assessors.py")
        // Move up `dots` levels from the containing directory
        let mut base = from_file.parent()?;
        for _ in 1..dots {
            base = base.parent().unwrap_or(base);
        }

        let target_path = if rest.is_empty() {
            // `from . import X` — the package __init__.py
            base.join("__init__.py")
        } else {
            // `from .utils import X` → base/utils.py
            let rel_path = rest.replace('.', "/");
            let as_file = base.join(format!("{rel_path}.py"));
            let as_pkg = base.join(&rel_path).join("__init__.py");

            if as_file.exists() || self.module_to_file.values().any(|p| p == &as_file) {
                as_file
            } else {
                as_pkg
            }
        };

        // Convert to absolute
        let absolute = self.root.join(&target_path);
        if absolute.exists() {
            Some(absolute)
        } else {
            None
        }
    }
}

// ─── Directory walking ────────────────────────────────────────────────────────

fn build_module_index(
    root: &Path,
    dir: &Path,
    index: &mut HashMap<String, PathBuf>,
) -> Result<(), ResolverError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            // Skip non-package directories
            if name.starts_with('.')
                || name == "__pycache__"
                || name == "node_modules"
                || name == ".git"
                || name == "venv"
                || name == ".venv"
            {
                continue;
            }
            build_module_index(root, &path, index)?;
        } else if path.extension().is_some_and(|ext| ext == "py") {
            // Compute relative path and dotted module name
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let dotted = path_to_dotted(rel);
            index.insert(dotted, rel.to_path_buf());
        }
    }
    Ok(())
}

/// Convert a relative file path to a dotted Python module name.
///
/// Examples:
/// - `"brain/perception/signal_fusion.py"` → `"brain.perception.signal_fusion"`
/// - `"__init__.py"` → `"__init__"`
/// - `"brain/__init__.py"` → `"brain"`
fn path_to_dotted(rel: &Path) -> String {
    let without_ext = rel
        .with_extension("")
        .display()
        .to_string()
        .replace(['/', '\\'], ".");

    // `brain/__init__` → `brain` (the package itself)
    if without_ext.ends_with(".__init__") {
        without_ext[..without_ext.len() - ".__init__".len()].to_string()
    } else if without_ext == "__init__" {
        String::new()
    } else {
        without_ext
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_pkg(dir: &Path, files: &[(&str, &str)]) {
        for (rel, content) in files {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create dir");
            }
            fs::write(&path, content).expect("write file");
        }
    }

    #[test]
    fn test_resolver_indexes_py_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        make_pkg(
            dir.path(),
            &[
                ("brain/__init__.py", ""),
                ("brain/perception/__init__.py", ""),
                ("brain/perception/signal_fusion.py", "def fuse(): pass"),
                ("brain/utils.py", "def helper(): pass"),
            ],
        );

        let resolver = PythonModuleResolver::from_root(dir.path()).expect("build resolver");
        assert!(resolver.module_count() >= 3, "should have >= 3 modules");

        assert!(resolver.knows_module("brain.perception.signal_fusion"));
        assert!(resolver.knows_module("brain.utils"));
        assert!(resolver.knows_module("brain"));
    }

    #[test]
    fn test_resolver_absolute_import() {
        let dir = tempfile::tempdir().expect("tempdir");
        make_pkg(
            dir.path(),
            &[("brain/__init__.py", ""), ("brain/signal.py", "")],
        );

        let resolver = PythonModuleResolver::from_root(dir.path()).expect("build resolver");
        let resolved = resolver.resolve_import("brain.signal", None);
        assert!(resolved.is_some(), "should resolve brain.signal");
        assert!(
            resolved.unwrap().ends_with("brain/signal.py"),
            "should resolve to brain/signal.py"
        );
    }

    #[test]
    fn test_resolver_relative_import() {
        let dir = tempfile::tempdir().expect("tempdir");
        make_pkg(
            dir.path(),
            &[
                ("perception/__init__.py", ""),
                ("perception/signal_fusion.py", "from .utils import helper"),
                ("perception/utils.py", "def helper(): pass"),
            ],
        );

        let resolver = PythonModuleResolver::from_root(dir.path()).expect("build resolver");
        let from_file = Path::new("perception/signal_fusion.py");
        let resolved = resolver.resolve_import(".utils", Some(from_file));
        assert!(resolved.is_some(), "should resolve .utils");
        assert!(
            resolved.unwrap().ends_with("perception/utils.py"),
            "should resolve to perception/utils.py"
        );
    }

    #[test]
    fn test_resolver_unknown_import_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        make_pkg(dir.path(), &[("main.py", "")]);

        let resolver = PythonModuleResolver::from_root(dir.path()).expect("build resolver");
        let resolved = resolver.resolve_import("numpy", None);
        assert!(
            resolved.is_none(),
            "external package 'numpy' should not resolve"
        );
    }

    #[test]
    fn test_resolver_skips_pycache() {
        let dir = tempfile::tempdir().expect("tempdir");
        make_pkg(
            dir.path(),
            &[("__pycache__/module.py", ""), ("real.py", "")],
        );

        let resolver = PythonModuleResolver::from_root(dir.path()).expect("build resolver");
        assert!(
            !resolver.knows_module("__pycache__.module"),
            "should skip __pycache__"
        );
        assert!(resolver.knows_module("real"));
    }

    #[test]
    fn test_path_to_dotted() {
        assert_eq!(
            path_to_dotted(Path::new("brain/perception/signal.py")),
            "brain.perception.signal"
        );
        assert_eq!(path_to_dotted(Path::new("brain/__init__.py")), "brain");
        assert_eq!(path_to_dotted(Path::new("utils.py")), "utils");
    }
}
