//! Content-addressed safety verification.
//!
//! EX-3353: Compute SHA-256 hashes of safety-critical source files at build
//! time, store in a signed manifest, verify at startup. Fail-closed: any
//! mismatch or missing manifest prevents awakening.
//!
//! ## What to do when verification fails
//!
//! 1. **Signature invalid** → manifest was tampered with. Re-generate from clean checkout.
//! 2. **File hash mismatch** → safety code was modified. Investigate, restore from git.
//! 3. **File missing** → safety code was deleted. Restore from git.
//! 4. **Manifest missing** → never generated or deleted. Run manifest generator.
//!
//! In all cases: the being MUST NOT awaken until verification passes.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// A manifest of SHA-256 hashes for safety-critical files.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SafetyManifest {
    /// File path → SHA-256 hex hash.
    pub hashes: BTreeMap<String, String>,
    /// HMAC-SHA256 signature of the sorted hash entries.
    pub signature: String,
    /// When this manifest was generated.
    pub generated_at: String,
}

/// Result of startup verification.
#[derive(Debug)]
pub struct VerificationResult {
    pub passed: bool,
    pub files_checked: usize,
    pub mismatches: Vec<String>,
}

/// Files that are part of the safety perimeter.
const SAFETY_CRATES: &[&str] = &["crates/nusy-safety", "crates/nusy-reasoning-causal"];
const SAFETY_MODULES: &[&str] = &[
    "crates/nusy-being/src/tier_enforcer.rs",
    "crates/nusy-being/src/safety_perimeter.rs",
    "crates/nusy-being/src/code_modifier.rs",
];

/// Generate a safety manifest for all files in the perimeter.
pub fn generate_manifest(workspace_root: &Path, secret: &str) -> SafetyManifest {
    let mut hashes = BTreeMap::new();

    // Hash all .rs files in safety crates.
    for crate_dir in SAFETY_CRATES {
        let src = workspace_root.join(crate_dir).join("src");
        if src.is_dir() {
            walk_and_hash(&src, workspace_root, &mut hashes);
        }
    }

    // Hash specific safety modules.
    for module in SAFETY_MODULES {
        let path = workspace_root.join(module);
        if path.is_file()
            && let Ok(content) = std::fs::read(&path)
        {
            let hash = sha256_hex(&content);
            hashes.insert(module.to_string(), hash);
        }
    }

    let signature = sign_hashes(&hashes, secret);

    SafetyManifest {
        hashes,
        signature,
        generated_at: chrono::Utc::now().to_rfc3339(),
    }
}

/// Verify a manifest against the current filesystem.
///
/// Fail-closed: returns failure if manifest is missing, signature is invalid,
/// or any file hash doesn't match.
pub fn verify_manifest(
    workspace_root: &Path,
    manifest: &SafetyManifest,
    secret: &str,
) -> VerificationResult {
    eprintln!(
        "[safety] Verifying {} files in safety perimeter...",
        manifest.hashes.len()
    );

    // Verify signature first.
    let expected_sig = sign_hashes(&manifest.hashes, secret);
    if expected_sig != manifest.signature {
        eprintln!("[safety] FAIL: manifest signature invalid — possible tampering");
        return VerificationResult {
            passed: false,
            files_checked: 0,
            mismatches: vec!["Manifest signature invalid (possible tampering)".into()],
        };
    }

    let mut mismatches = Vec::new();
    let mut files_checked = 0;

    for (path, expected_hash) in &manifest.hashes {
        let full_path = workspace_root.join(path);
        files_checked += 1;

        match std::fs::read(&full_path) {
            Ok(content) => {
                let actual_hash = sha256_hex(&content);
                if actual_hash != *expected_hash {
                    mismatches.push(format!(
                        "{path}: expected {}, got {}",
                        &expected_hash[..12],
                        &actual_hash[..12]
                    ));
                }
            }
            Err(_) => {
                mismatches.push(format!("{path}: file missing"));
            }
        }
    }

    if mismatches.is_empty() {
        eprintln!("[safety] PASS: all {files_checked} safety files verified");
    } else {
        eprintln!(
            "[safety] FAIL: {} mismatches in safety perimeter",
            mismatches.len()
        );
        for m in &mismatches {
            eprintln!("[safety]   {m}");
        }
    }

    VerificationResult {
        passed: mismatches.is_empty(),
        files_checked,
        mismatches,
    }
}

/// Load a manifest from a JSON file.
pub fn load_manifest(path: &Path) -> Result<SafetyManifest, String> {
    let data = std::fs::read_to_string(path).map_err(|e| format!("manifest missing: {e}"))?;
    serde_json::from_str(&data).map_err(|e| format!("manifest corrupt: {e}"))
}

/// Save a manifest to a JSON file.
pub fn save_manifest(manifest: &SafetyManifest, path: &Path) -> Result<(), String> {
    let json = serde_json::to_string_pretty(manifest).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("write: {e}"))
}

// ── Internal helpers ────────────────────────────────────────────────────────

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn sign_hashes(hashes: &BTreeMap<String, String>, secret: &str) -> String {
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<Sha256>;

    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    // Sign the sorted entries deterministically.
    for (path, hash) in hashes {
        mac.update(path.as_bytes());
        mac.update(b":");
        mac.update(hash.as_bytes());
        mac.update(b"\n");
    }
    format!("{:x}", mac.finalize().into_bytes())
}

/// Walk directory and hash all .rs files. Panics on unreadable files (fail-closed).
fn walk_and_hash(dir: &Path, root: &Path, hashes: &mut BTreeMap<String, String>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| {
        panic!(
            "Safety perimeter directory unreadable {}: {e}",
            dir.display()
        )
    });
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| {
            panic!(
                "Safety perimeter entry unreadable in {}: {e}",
                dir.display()
            )
        });
        let path = entry.path();
        if path.is_dir() {
            walk_and_hash(&path, root, hashes);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let content = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("Safety file unreadable {}: {e}", path.display()));
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            hashes.insert(rel, sha256_hex(&content));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_value() {
        let hash = sha256_hex(b"hello world");
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn sign_deterministic() {
        let mut hashes = BTreeMap::new();
        hashes.insert("a.rs".into(), "hash_a".into());
        hashes.insert("b.rs".into(), "hash_b".into());

        let sig1 = sign_hashes(&hashes, "secret");
        let sig2 = sign_hashes(&hashes, "secret");
        assert_eq!(sig1, sig2);

        let sig3 = sign_hashes(&hashes, "different");
        assert_ne!(sig1, sig3);
    }

    #[test]
    fn manifest_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("crates/nusy-safety/src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("lib.rs"), "fn safe() {}").unwrap();

        let manifest = generate_manifest(dir.path(), "test-secret");
        assert!(!manifest.hashes.is_empty());
        assert!(!manifest.signature.is_empty());

        let result = verify_manifest(dir.path(), &manifest, "test-secret");
        assert!(result.passed);
        assert!(result.mismatches.is_empty());
    }

    #[test]
    fn tampered_file_detected() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("crates/nusy-safety/src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("lib.rs"), "fn safe() {}").unwrap();

        let manifest = generate_manifest(dir.path(), "secret");

        // Tamper with the file.
        std::fs::write(src_dir.join("lib.rs"), "fn HACKED() {}").unwrap();

        let result = verify_manifest(dir.path(), &manifest, "secret");
        assert!(!result.passed);
        assert_eq!(result.mismatches.len(), 1);
        assert!(result.mismatches[0].contains("nusy-safety"));
    }

    #[test]
    fn tampered_signature_detected() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("crates/nusy-safety/src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("lib.rs"), "fn safe() {}").unwrap();

        let mut manifest = generate_manifest(dir.path(), "secret");
        manifest.signature = "forged_signature".into();

        let result = verify_manifest(dir.path(), &manifest, "secret");
        assert!(!result.passed);
        assert!(result.mismatches[0].contains("signature invalid"));
    }

    #[test]
    fn missing_file_detected() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("crates/nusy-safety/src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("lib.rs"), "fn safe() {}").unwrap();

        let manifest = generate_manifest(dir.path(), "secret");

        // Delete the file.
        std::fs::remove_file(src_dir.join("lib.rs")).unwrap();

        let result = verify_manifest(dir.path(), &manifest, "secret");
        assert!(!result.passed);
        assert!(result.mismatches[0].contains("missing"));
    }

    #[test]
    fn manifest_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = SafetyManifest {
            hashes: BTreeMap::from([("test.rs".into(), "abc123".into())]),
            signature: "sig".into(),
            generated_at: "2026-03-20".into(),
        };

        let path = dir.path().join("manifest.json");
        save_manifest(&manifest, &path).unwrap();
        let loaded = load_manifest(&path).unwrap();
        assert_eq!(loaded.hashes, manifest.hashes);
        assert_eq!(loaded.signature, manifest.signature);
    }

    #[test]
    fn missing_manifest_is_error() {
        let result = load_manifest(Path::new("/nonexistent/manifest.json"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing"));
    }
}
