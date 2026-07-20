//! Pure-data knowledge-artifact types — arrow-free (EX-4920).
//!
//! Extracted from `nusy-arrow-core::artifacts` so the defeasible reasoner (and any
//! future FOSS crate that reasons over artifact lifecycle / supersession) can compile
//! without pulling in Apache Arrow as a transitive dependency. `nusy-arrow-core`
//! re-exports these from its own `artifacts` module unchanged.

use std::fmt;

/// Lifecycle state of a knowledge artifact. FHIR's `unknown` is never stored — it is
/// rejected or mapped at the import boundary, so this enum has exactly three states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactStatus {
    /// Editable; not yet published. Content may still change in place.
    Draft,
    /// Published and frozen (CRMI immutability). A change requires a new version.
    Active,
    /// Withdrawn. Terminal.
    Retired,
}

impl ArtifactStatus {
    /// The stored string form.
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactStatus::Draft => "draft",
            ArtifactStatus::Active => "active",
            ArtifactStatus::Retired => "retired",
        }
    }

    /// Parse a stored status. `unknown` (and anything else) is rejected — only the three
    /// canonical states are valid in the store.
    pub fn parse(s: &str) -> Result<Self, ArtifactError> {
        match s {
            "draft" => Ok(ArtifactStatus::Draft),
            "active" => Ok(ArtifactStatus::Active),
            "retired" => Ok(ArtifactStatus::Retired),
            other => Err(ArtifactError::InvalidStatus(other.to_string())),
        }
    }
}

/// A `Major.Minor.Revision` business version (CRMI semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Breaking change.
    pub major: u32,
    /// Backwards-compatible addition.
    pub minor: u32,
    /// Fix / editorial revision.
    pub revision: u32,
}

impl Version {
    /// Construct a version.
    pub fn new(major: u32, minor: u32, revision: u32) -> Self {
        Self {
            major,
            minor,
            revision,
        }
    }

    /// Parse `"M.m.r"`.
    pub fn parse(s: &str) -> Result<Self, ArtifactError> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 3 {
            return Err(ArtifactError::InvalidVersion(s.to_string()));
        }
        let p = |x: &str| {
            x.parse::<u32>()
                .map_err(|_| ArtifactError::InvalidVersion(s.to_string()))
        };
        Ok(Version::new(p(parts[0])?, p(parts[1])?, p(parts[2])?))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.revision)
    }
}

/// The kind of a dependency edge between artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepType {
    /// `from` needs `to` to function (the manifest closure follows these).
    DependsOn,
    /// `from` is assembled out of `to` (a bundle/part-of relation).
    ComposedOf,
    /// `from` was produced from `to` (lineage).
    DerivedFrom,
}

impl DepType {
    /// The stored string form.
    pub fn as_str(self) -> &'static str {
        match self {
            DepType::DependsOn => "depends-on",
            DepType::ComposedOf => "composed-of",
            DepType::DerivedFrom => "derived-from",
        }
    }

    /// Parse a stored dependency type.
    pub fn parse(s: &str) -> Result<Self, ArtifactError> {
        match s {
            "depends-on" => Ok(DepType::DependsOn),
            "composed-of" => Ok(DepType::ComposedOf),
            "derived-from" => Ok(DepType::DerivedFrom),
            other => Err(ArtifactError::InvalidDepType(other.to_string())),
        }
    }
}

/// One versioned knowledge artifact (a row of the KnowledgeArtifacts table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeArtifact {
    /// Stable business identity across versions.
    pub artifact_id: String,
    /// Generic kind: `rule-set` | `decision-graph` | `ontology` | … (never clinical).
    pub artifact_type: String,
    /// Business version.
    pub version: Version,
    /// Lifecycle state.
    pub status: ArtifactStatus,
    /// Stable URL identity; with `version` forms the named-graph handle.
    pub canonical_url: String,
    /// Owning agent / org.
    pub steward: String,
    /// Last-changed timestamp (epoch millis, UTC).
    pub date: i64,
    /// Applicability window start (epoch millis), if any.
    pub effective_start: Option<i64>,
    /// Applicability window end (epoch millis), if any.
    pub effective_end: Option<i64>,
    /// The `artifact_id` this version replaces, if any (the supersession edge).
    pub supersedes: Option<String>,
}

impl KnowledgeArtifact {
    /// The named-graph handle that ties this artifact to its triples in the graph store's
    /// `graph` column: `canonical_url|version`.
    pub fn named_graph(&self) -> String {
        format!("{}|{}", self.canonical_url, self.version)
    }
}

/// Errors from artifact lifecycle / manifest operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactError {
    /// A `(artifact_id, version)` pair already exists.
    Duplicate(String, Version),
    /// No artifact for the given `(artifact_id, version)`.
    NotFound(String, Version),
    /// An illegal lifecycle transition was attempted (e.g. retired → active).
    IllegalTransition {
        from: ArtifactStatus,
        to: ArtifactStatus,
    },
    /// An attempt to mutate an `active` (frozen) artifact in place.
    ImmutableActive(String, Version),
    /// A dependency cycle was detected during manifest construction.
    DependencyCycle(String),
    /// A status string outside {draft, active, retired}.
    InvalidStatus(String),
    /// A malformed `Major.Minor.Revision` version string.
    InvalidVersion(String),
    /// A dependency type outside {depends-on, composed-of, derived-from}.
    InvalidDepType(String),
    /// A schema/decoding error reading a RecordBatch.
    Decode(String),
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactError::Duplicate(id, v) => write!(f, "artifact {id}@{v} already exists"),
            ArtifactError::NotFound(id, v) => write!(f, "artifact {id}@{v} not found"),
            ArtifactError::IllegalTransition { from, to } => {
                write!(
                    f,
                    "illegal lifecycle transition {} → {}",
                    from.as_str(),
                    to.as_str()
                )
            }
            ArtifactError::ImmutableActive(id, v) => {
                write!(
                    f,
                    "artifact {id}@{v} is active (frozen); a change needs a new version + supersedes edge"
                )
            }
            ArtifactError::DependencyCycle(at) => write!(f, "dependency cycle through {at}"),
            ArtifactError::InvalidStatus(s) => write!(
                f,
                "invalid artifact status `{s}` (expected draft|active|retired)"
            ),
            ArtifactError::InvalidVersion(s) => {
                write!(f, "invalid version `{s}` (expected Major.Minor.Revision)")
            }
            ArtifactError::InvalidDepType(s) => write!(f, "invalid dependency type `{s}`"),
            ArtifactError::Decode(s) => write!(f, "artifact table decode error: {s}"),
        }
    }
}

impl std::error::Error for ArtifactError {}
