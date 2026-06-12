//! Review sign-off sidecar for tutor records (EX-4334 / VY-4313 EX-iii).
//!
//! The plate itself ([`crate::TutorRecord`]) is `#[serde(deny_unknown_fields)]`
//! and treated as the immutable contract — review state lives next to the
//! plate in a sidecar file rather than mutating the plate. Convention:
//!
//! - `<stem>.expected.json` — the working plate (auto-generated then human-edited)
//! - `<stem>.auto.json` — immutable auto-generated baseline (snapshot)
//! - `<stem>.review.json` — this signoff, written by `nk tutor approve`
//!
//! A signoff binds a reviewer to a specific plate hash. If the plate is
//! edited after approval, [`ReviewSignoff::plate_sha256`] no longer matches
//! the file on disk and `nk tutor status` reports the signoff as diverged.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Schema version for the review sidecar. Bump on incompatible changes.
pub const REVIEW_SCHEMA_VERSION: &str = "1.0";

/// Human review of a tutor record. Stored as a sidecar `*.review.json`
/// alongside the plate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReviewSignoff {
    pub schema_version: String,
    /// Path to the plate this signoff covers. Stored as a relative path when
    /// possible so signoffs travel with the repo.
    pub plate_path: String,
    /// SHA-256 of the plate file contents at signoff time. If the plate is
    /// later edited, the recomputed hash will not match and tooling reports
    /// the signoff as diverged.
    pub plate_sha256: String,
    pub reviewer: String,
    pub approved_at: DateTime<Utc>,
    pub status: ReviewStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// SHA-256 of the auto-generated baseline (`<stem>.auto.json`) if one
    /// existed at signoff time. Lets reviewers later confirm they signed off
    /// on the same auto-baseline that was diffed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_baseline_sha256: Option<String>,
}

/// Review verdict.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    /// Reviewer accepts the plate as ground truth.
    Approved,
    /// Reviewer is still working on it (intermediate save).
    Draft,
    /// Reviewer rejects the plate; auto-tutor needs to regenerate.
    Rejected,
}

/// Hex-encoded SHA-256 of a byte slice. Used to fingerprint plate file
/// contents for [`ReviewSignoff::plate_sha256`].
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signoff_round_trips_through_serde() {
        let signoff = ReviewSignoff {
            schema_version: REVIEW_SCHEMA_VERSION.to_string(),
            plate_path: "L0_toddler/dame_wonder.expected.json".to_string(),
            plate_sha256: "deadbeef".to_string(),
            reviewer: "Mini".to_string(),
            approved_at: "2026-05-06T12:00:00Z".parse().unwrap(),
            status: ReviewStatus::Approved,
            comment: Some("Layer 3 CQs match my read-aloud notes.".into()),
            auto_baseline_sha256: Some("cafe".into()),
        };
        let json = serde_json::to_string(&signoff).unwrap();
        let back: ReviewSignoff = serde_json::from_str(&json).unwrap();
        assert_eq!(signoff, back);
    }

    #[test]
    fn rejected_signoff_serializes_with_snake_case_status() {
        let signoff = ReviewSignoff {
            schema_version: REVIEW_SCHEMA_VERSION.to_string(),
            plate_path: "x.json".to_string(),
            plate_sha256: "0".to_string(),
            reviewer: "M5".to_string(),
            approved_at: "2026-05-06T00:00:00Z".parse().unwrap(),
            status: ReviewStatus::Rejected,
            comment: None,
            auto_baseline_sha256: None,
        };
        let json = serde_json::to_string(&signoff).unwrap();
        assert!(json.contains("\"status\":\"rejected\""));
        assert!(!json.contains("comment"));
        assert!(!json.contains("auto_baseline_sha256"));
    }

    #[test]
    fn unknown_field_in_signoff_fails_to_deserialize() {
        let bad = r#"{
            "schema_version": "1.0",
            "plate_path": "p.json",
            "plate_sha256": "0",
            "reviewer": "Mini",
            "approved_at": "2026-05-06T00:00:00Z",
            "status": "approved",
            "future_field": "boom"
        }"#;
        let result: Result<ReviewSignoff, _> = serde_json::from_str(bad);
        assert!(result.is_err(), "deny_unknown_fields must reject extras");
    }

    #[test]
    fn sha256_is_deterministic_and_distinguishes_content() {
        let a = sha256_hex(b"hello");
        let a2 = sha256_hex(b"hello");
        let b = sha256_hex(b"hello!");
        assert_eq!(a, a2);
        assert_ne!(a, b);
        assert_eq!(a.len(), 64, "hex sha256 is 64 chars");
    }
}
