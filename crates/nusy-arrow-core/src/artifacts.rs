//! Knowledge-artifact lifecycle, versioning, dependencies, and manifest (EX-4680, VY-4679 E1).
//!
//! The generic form of FHIR-CPG/CRMI knowledge-artifact discipline (V19-VISION §4): a
//! [`KnowledgeArtifact`] is a versioned, lifecycle-managed unit of transferable knowledge
//! — a rule set, a decision graph, an ontology fragment — never a clinical structure. The
//! [`ArtifactStore`] enforces:
//!
//! - **Lifecycle** — `draft → active → retired`, a one-way state machine.
//! - **CRMI immutability** — once an artifact version is `active`, its content is frozen;
//!   a change is a **new business version** plus a `supersedes` edge, never an in-place
//!   edit. (The supersession edge is what defeasible reasoning reads.)
//! - **Named-graph handle** — `canonical_url|version`, the key that ties an artifact to its
//!   triples in the [`ArrowGraphStore`](crate::ArrowGraphStore)'s `graph` column.
//! - **Manifest** — the version-pinned transitive dependency closure for an
//!   `(artifact_id, version)`: the `$package` / `Cargo.lock` analog and the COG-transfer
//!   packaging format. Dependency cycles are an error.
//!
//! Both tables round-trip to Parquet via [`crate::write_parquet_atomic`], each carrying its
//! own schema version for future read-path migration.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{Array, RecordBatch, StringArray, TimestampMillisecondArray};
use arrow::datatypes::Schema;

use crate::schema::{
    ARTIFACT_DEPENDENCIES_SCHEMA_VERSION, KNOWLEDGE_ARTIFACTS_SCHEMA_VERSION, artifact_col,
    artifact_dep_col, artifact_dependencies_schema, knowledge_artifacts_schema,
};

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

    /// `(artifact_id, version)` — the manifest pin key.
    fn key(&self) -> (String, Version) {
        (self.artifact_id.clone(), self.version)
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

/// A typed dependency edge, version-pinned on the `from` side.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Dependency {
    from: (String, Version),
    to: (String, Version),
    dep_type: DepType,
}

/// In-memory store of knowledge artifacts and their dependency edges, with lifecycle
/// enforcement and manifest construction. Serializes to the Arrow `knowledge_artifacts` /
/// `artifact_dependencies` tables and round-trips through Parquet.
#[derive(Debug, Default)]
pub struct ArtifactStore {
    artifacts: HashMap<(String, Version), KnowledgeArtifact>,
    deps: Vec<Dependency>,
}

impl ArtifactStore {
    /// A new, empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of artifact versions held.
    pub fn len(&self) -> usize {
        self.artifacts.len()
    }

    /// Whether the store holds no artifacts.
    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
    }

    /// Create a new artifact version in `draft`. The artifact's own `status` is forced to
    /// `Draft` regardless of the value passed in. Errors if `(artifact_id, version)` exists.
    pub fn create(&mut self, mut artifact: KnowledgeArtifact) -> Result<(), ArtifactError> {
        artifact.status = ArtifactStatus::Draft;
        let key = artifact.key();
        if self.artifacts.contains_key(&key) {
            return Err(ArtifactError::Duplicate(key.0, key.1));
        }
        self.artifacts.insert(key, artifact);
        Ok(())
    }

    /// Look up an artifact version.
    pub fn get(&self, artifact_id: &str, version: Version) -> Option<&KnowledgeArtifact> {
        self.artifacts.get(&(artifact_id.to_string(), version))
    }

    fn transition(
        &mut self,
        artifact_id: &str,
        version: Version,
        to: ArtifactStatus,
        legal_from: ArtifactStatus,
    ) -> Result<(), ArtifactError> {
        let key = (artifact_id.to_string(), version);
        let a = self
            .artifacts
            .get_mut(&key)
            .ok_or_else(|| ArtifactError::NotFound(key.0.clone(), key.1))?;
        if a.status != legal_from {
            return Err(ArtifactError::IllegalTransition { from: a.status, to });
        }
        a.status = to;
        Ok(())
    }

    /// `draft → active`. Errors if the artifact is not currently `draft`.
    pub fn activate(&mut self, artifact_id: &str, version: Version) -> Result<(), ArtifactError> {
        self.transition(
            artifact_id,
            version,
            ArtifactStatus::Active,
            ArtifactStatus::Draft,
        )
    }

    /// `active → retired`. Errors if the artifact is not currently `active`.
    pub fn retire(&mut self, artifact_id: &str, version: Version) -> Result<(), ArtifactError> {
        self.transition(
            artifact_id,
            version,
            ArtifactStatus::Retired,
            ArtifactStatus::Active,
        )
    }

    /// Edit a **draft** artifact's mutable content in place (here: type/url/steward/window).
    /// Mutating an `active` (frozen) artifact is an error — CRMI requires a new version +
    /// `supersedes` edge instead (see [`supersede`](Self::supersede)).
    pub fn edit_draft(
        &mut self,
        artifact_id: &str,
        version: Version,
        f: impl FnOnce(&mut KnowledgeArtifact),
    ) -> Result<(), ArtifactError> {
        let key = (artifact_id.to_string(), version);
        let a = self
            .artifacts
            .get_mut(&key)
            .ok_or_else(|| ArtifactError::NotFound(key.0.clone(), key.1))?;
        if a.status != ArtifactStatus::Draft {
            return Err(ArtifactError::ImmutableActive(key.0, key.1));
        }
        f(a);
        Ok(())
    }

    /// Create a new draft version that supersedes an existing one — the CRMI "change" path.
    /// The new artifact's `supersedes` is set to the prior `artifact_id`.
    pub fn supersede(
        &mut self,
        prior_id: &str,
        prior_version: Version,
        mut new_version: KnowledgeArtifact,
    ) -> Result<(), ArtifactError> {
        if !self
            .artifacts
            .contains_key(&(prior_id.to_string(), prior_version))
        {
            return Err(ArtifactError::NotFound(prior_id.to_string(), prior_version));
        }
        new_version.supersedes = Some(prior_id.to_string());
        self.create(new_version)
    }

    /// Record a version-pinned dependency edge.
    pub fn add_dependency(
        &mut self,
        from: (&str, Version),
        to: (&str, Version),
        dep_type: DepType,
    ) -> Result<(), ArtifactError> {
        let f = (from.0.to_string(), from.1);
        let t = (to.0.to_string(), to.1);
        if !self.artifacts.contains_key(&f) {
            return Err(ArtifactError::NotFound(f.0, f.1));
        }
        if !self.artifacts.contains_key(&t) {
            return Err(ArtifactError::NotFound(t.0, t.1));
        }
        self.deps.push(Dependency {
            from: f,
            to: t,
            dep_type,
        });
        Ok(())
    }

    /// The version-pinned **transitive dependency closure** of `(artifact_id, version)` —
    /// the manifest. Returns the reachable `(artifact_id, version)` pairs (excluding the
    /// root), sorted for determinism. Errors with [`ArtifactError::DependencyCycle`] if the
    /// dependency graph has a cycle reachable from the root.
    pub fn manifest(
        &self,
        artifact_id: &str,
        version: Version,
    ) -> Result<Vec<(String, Version)>, ArtifactError> {
        let root = (artifact_id.to_string(), version);
        if !self.artifacts.contains_key(&root) {
            return Err(ArtifactError::NotFound(root.0, root.1));
        }
        // Adjacency by `from` node.
        let mut adj: HashMap<&(String, Version), Vec<&(String, Version)>> = HashMap::new();
        for d in &self.deps {
            adj.entry(&d.from).or_default().push(&d.to);
        }

        let mut out: HashSet<(String, Version)> = HashSet::new();
        // DFS with on-stack tracking for cycle detection.
        let mut on_stack: HashSet<(String, Version)> = HashSet::new();
        self.dfs(&root, &adj, &mut out, &mut on_stack)?;

        let mut closure: Vec<(String, Version)> = out.into_iter().collect();
        closure.sort();
        Ok(closure)
    }

    fn dfs(
        &self,
        node: &(String, Version),
        adj: &HashMap<&(String, Version), Vec<&(String, Version)>>,
        out: &mut HashSet<(String, Version)>,
        on_stack: &mut HashSet<(String, Version)>,
    ) -> Result<(), ArtifactError> {
        on_stack.insert(node.clone());
        if let Some(children) = adj.get(node) {
            for child in children {
                if on_stack.contains(*child) {
                    return Err(ArtifactError::DependencyCycle(format!(
                        "{}@{}",
                        child.0, child.1
                    )));
                }
                // Record the dependency; recurse even if already recorded would re-walk, so
                // only recurse the first time we reach a node (closure is a set).
                let first_visit = out.insert((*child).clone());
                if first_visit {
                    self.dfs(child, adj, out, on_stack)?;
                } else {
                    // Already fully explored via another path; still must check it is not on
                    // the current stack (handled above), so nothing more to do.
                }
            }
        }
        on_stack.remove(node);
        Ok(())
    }

    // ── Arrow / Parquet (de)serialization ────────────────────────────────────

    /// Serialize the artifacts to a `knowledge_artifacts` RecordBatch (rows sorted by
    /// `(artifact_id, version)` for stable output).
    pub fn artifacts_batch(&self) -> RecordBatch {
        use arrow::array::ArrayRef;
        let mut rows: Vec<&KnowledgeArtifact> = self.artifacts.values().collect();
        rows.sort_by(|a, b| {
            (a.artifact_id.as_str(), a.version).cmp(&(b.artifact_id.as_str(), b.version))
        });

        let s = |get: &dyn Fn(&KnowledgeArtifact) -> String| -> ArrayRef {
            Arc::new(StringArray::from(
                rows.iter().map(|a| get(a)).collect::<Vec<_>>(),
            ))
        };
        let so = |get: &dyn Fn(&KnowledgeArtifact) -> Option<String>| -> ArrayRef {
            Arc::new(StringArray::from(
                rows.iter().map(|a| get(a)).collect::<Vec<_>>(),
            ))
        };
        let ts = |get: &dyn Fn(&KnowledgeArtifact) -> Option<i64>| -> ArrayRef {
            Arc::new(
                TimestampMillisecondArray::from(rows.iter().map(|a| get(a)).collect::<Vec<_>>())
                    .with_timezone("UTC"),
            )
        };

        RecordBatch::try_new(
            Arc::new(knowledge_artifacts_schema()),
            vec![
                s(&|a| a.artifact_id.clone()),
                s(&|a| a.artifact_type.clone()),
                s(&|a| a.version.to_string()),
                s(&|a| a.status.as_str().to_string()),
                s(&|a| a.canonical_url.clone()),
                s(&|a| a.steward.clone()),
                ts(&|a| Some(a.date)),
                ts(&|a| a.effective_start),
                ts(&|a| a.effective_end),
                so(&|a| a.supersedes.clone()),
            ],
        )
        .expect("artifact rows match knowledge_artifacts_schema")
    }

    /// Serialize the dependency edges to an `artifact_dependencies` RecordBatch. Edges store
    /// the version-pinned `from`/`to` as `artifact_id|version` so the table is flat Utf8.
    pub fn dependencies_batch(&self) -> RecordBatch {
        let pin = |k: &(String, Version)| format!("{}|{}", k.0, k.1);
        RecordBatch::try_new(
            Arc::new(artifact_dependencies_schema()),
            vec![
                Arc::new(StringArray::from(
                    self.deps.iter().map(|d| pin(&d.from)).collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from(
                    self.deps.iter().map(|d| pin(&d.to)).collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from(
                    self.deps
                        .iter()
                        .map(|d| d.dep_type.as_str().to_string())
                        .collect::<Vec<_>>(),
                )),
            ],
        )
        .expect("dependency rows match artifact_dependencies_schema")
    }

    /// Rebuild a store from its two RecordBatches (the read path).
    pub fn from_batches(
        artifacts: &RecordBatch,
        dependencies: &RecordBatch,
    ) -> Result<Self, ArtifactError> {
        let col = |b: &RecordBatch, i: usize| -> Result<StringArray, ArtifactError> {
            b.column(i)
                .as_any()
                .downcast_ref::<StringArray>()
                .cloned()
                .ok_or_else(|| ArtifactError::Decode(format!("column {i} not Utf8")))
        };
        let tcol =
            |b: &RecordBatch, i: usize| -> Result<TimestampMillisecondArray, ArtifactError> {
                b.column(i)
                    .as_any()
                    .downcast_ref::<TimestampMillisecondArray>()
                    .cloned()
                    .ok_or_else(|| ArtifactError::Decode(format!("column {i} not Timestamp")))
            };

        let id = col(artifacts, artifact_col::ARTIFACT_ID)?;
        let ty = col(artifacts, artifact_col::ARTIFACT_TYPE)?;
        let ver = col(artifacts, artifact_col::VERSION)?;
        let st = col(artifacts, artifact_col::STATUS)?;
        let url = col(artifacts, artifact_col::CANONICAL_URL)?;
        let stw = col(artifacts, artifact_col::STEWARD)?;
        let date = tcol(artifacts, artifact_col::DATE)?;
        let eff_s = tcol(artifacts, artifact_col::EFFECTIVE_START)?;
        let eff_e = tcol(artifacts, artifact_col::EFFECTIVE_END)?;
        let sup = col(artifacts, artifact_col::SUPERSEDES)?;

        let mut store = ArtifactStore::new();
        for i in 0..artifacts.num_rows() {
            let version = Version::parse(ver.value(i))?;
            let artifact = KnowledgeArtifact {
                artifact_id: id.value(i).to_string(),
                artifact_type: ty.value(i).to_string(),
                version,
                status: ArtifactStatus::parse(st.value(i))?,
                canonical_url: url.value(i).to_string(),
                steward: stw.value(i).to_string(),
                date: date.value(i),
                effective_start: (!eff_s.is_null(i)).then(|| eff_s.value(i)),
                effective_end: (!eff_e.is_null(i)).then(|| eff_e.value(i)),
                supersedes: (!sup.is_null(i)).then(|| sup.value(i).to_string()),
            };
            // Insert directly (bypass create()'s draft-forcing — we are reloading state).
            store.artifacts.insert(artifact.key(), artifact);
        }

        let from = col(dependencies, artifact_dep_col::FROM_ARTIFACT)?;
        let to = col(dependencies, artifact_dep_col::TO_ARTIFACT)?;
        let dt = col(dependencies, artifact_dep_col::DEP_TYPE)?;
        let unpin = |s: &str| -> Result<(String, Version), ArtifactError> {
            let (id, v) = s
                .rsplit_once('|')
                .ok_or_else(|| ArtifactError::Decode(format!("bad pin `{s}`")))?;
            Ok((id.to_string(), Version::parse(v)?))
        };
        for i in 0..dependencies.num_rows() {
            store.deps.push(Dependency {
                from: unpin(from.value(i))?,
                to: unpin(to.value(i))?,
                dep_type: DepType::parse(dt.value(i))?,
            });
        }
        Ok(store)
    }

    /// Persist both tables to Parquet (crash-safe, atomic) under `dir`:
    /// `knowledge_artifacts.parquet` and `artifact_dependencies.parquet`. Each file's
    /// Arrow schema metadata carries its table schema version for read-path migration.
    pub fn persist(&self, dir: &Path) -> std::io::Result<()> {
        write_table(
            &dir.join("knowledge_artifacts.parquet"),
            self.artifacts_batch(),
            KNOWLEDGE_ARTIFACTS_SCHEMA_VERSION,
        )?;
        write_table(
            &dir.join("artifact_dependencies.parquet"),
            self.dependencies_batch(),
            ARTIFACT_DEPENDENCIES_SCHEMA_VERSION,
        )
    }

    /// Load both tables from a directory previously written by [`persist`](Self::persist).
    pub fn load(dir: &Path) -> std::io::Result<Self> {
        let arts = read_table(&dir.join("knowledge_artifacts.parquet"))?;
        let deps = read_table(&dir.join("artifact_dependencies.parquet"))?;
        ArtifactStore::from_batches(&arts, &deps)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }
}

/// Stamp a table's schema-version into the Arrow schema metadata and write it atomically.
fn write_table(path: &Path, batch: RecordBatch, schema_version: &str) -> std::io::Result<()> {
    use arrow::datatypes::Field;
    let meta = std::collections::HashMap::from([(
        "schema_version".to_string(),
        schema_version.to_string(),
    )]);
    let fields: Vec<Field> = batch
        .schema()
        .fields()
        .iter()
        .map(|f| f.as_ref().clone())
        .collect();
    let schema = Arc::new(Schema::new(fields).with_metadata(meta));
    let stamped = RecordBatch::try_new(schema.clone(), batch.columns().to_vec())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    crate::write_parquet_atomic(path, |w| {
        use parquet::arrow::ArrowWriter;
        let mut writer = ArrowWriter::try_new(w, schema.clone(), None)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        writer
            .write(&stamped)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        writer
            .close()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(())
    })
}

fn read_table(path: &Path) -> std::io::Result<RecordBatch> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    let file = std::fs::File::open(path)?;
    let mut reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?
        .build()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    reader
        .next()
        .transpose()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "empty parquet"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn art(id: &str, v: Version) -> KnowledgeArtifact {
        KnowledgeArtifact {
            artifact_id: id.to_string(),
            artifact_type: "rule-set".to_string(),
            version: v,
            status: ArtifactStatus::Draft,
            canonical_url: format!("https://nusy.dev/ka/{id}"),
            steward: "Air".to_string(),
            date: 1_700_000_000_000,
            effective_start: None,
            effective_end: None,
            supersedes: None,
        }
    }

    #[test]
    fn lifecycle_legal_transitions() {
        let mut s = ArtifactStore::new();
        let v = Version::new(1, 0, 0);
        s.create(art("a", v)).unwrap();
        assert_eq!(s.get("a", v).unwrap().status, ArtifactStatus::Draft);
        s.activate("a", v).unwrap();
        assert_eq!(s.get("a", v).unwrap().status, ArtifactStatus::Active);
        s.retire("a", v).unwrap();
        assert_eq!(s.get("a", v).unwrap().status, ArtifactStatus::Retired);
    }

    #[test]
    fn lifecycle_illegal_transitions_error() {
        let mut s = ArtifactStore::new();
        let v = Version::new(1, 0, 0);
        s.create(art("a", v)).unwrap();
        // Cannot retire a draft (must activate first).
        assert!(matches!(
            s.retire("a", v),
            Err(ArtifactError::IllegalTransition { .. })
        ));
        s.activate("a", v).unwrap();
        // Cannot activate twice.
        assert!(matches!(
            s.activate("a", v),
            Err(ArtifactError::IllegalTransition { .. })
        ));
        s.retire("a", v).unwrap();
        // Retired is terminal.
        assert!(matches!(
            s.activate("a", v),
            Err(ArtifactError::IllegalTransition { .. })
        ));
    }

    #[test]
    fn active_artifacts_are_immutable_change_needs_new_version() {
        let mut s = ArtifactStore::new();
        let v1 = Version::new(1, 0, 0);
        s.create(art("a", v1)).unwrap();
        s.edit_draft("a", v1, |a| a.steward = "Mini".to_string())
            .unwrap(); // draft edit OK
        s.activate("a", v1).unwrap();
        // Editing an active artifact in place is rejected.
        assert!(matches!(
            s.edit_draft("a", v1, |a| a.steward = "DGX".to_string()),
            Err(ArtifactError::ImmutableActive(..))
        ));
        // The CRMI path: a new version that supersedes the old one.
        let v2 = Version::new(1, 1, 0);
        s.supersede("a", v1, art("a", v2)).unwrap();
        assert_eq!(s.get("a", v2).unwrap().supersedes.as_deref(), Some("a"));
        assert_eq!(s.get("a", v2).unwrap().status, ArtifactStatus::Draft);
    }

    #[test]
    fn create_rejects_duplicate_version() {
        let mut s = ArtifactStore::new();
        let v = Version::new(1, 0, 0);
        s.create(art("a", v)).unwrap();
        assert!(matches!(
            s.create(art("a", v)),
            Err(ArtifactError::Duplicate(..))
        ));
    }

    #[test]
    fn named_graph_handle_is_url_pipe_version() {
        let a = art("a", Version::new(2, 3, 1));
        assert_eq!(a.named_graph(), "https://nusy.dev/ka/a|2.3.1");
    }

    fn diamond() -> (ArtifactStore, Version) {
        // top → {left, right} → bottom  (a 3-level diamond)
        let mut s = ArtifactStore::new();
        let v = Version::new(1, 0, 0);
        for id in ["top", "left", "right", "bottom"] {
            s.create(art(id, v)).unwrap();
        }
        s.add_dependency(("top", v), ("left", v), DepType::DependsOn)
            .unwrap();
        s.add_dependency(("top", v), ("right", v), DepType::DependsOn)
            .unwrap();
        s.add_dependency(("left", v), ("bottom", v), DepType::DependsOn)
            .unwrap();
        s.add_dependency(("right", v), ("bottom", v), DepType::ComposedOf)
            .unwrap();
        (s, v)
    }

    #[test]
    fn manifest_closure_on_diamond_dedups_shared_dependency() {
        let (s, v) = diamond();
        let mut m = s.manifest("top", v).unwrap();
        m.sort();
        assert_eq!(
            m,
            vec![
                ("bottom".to_string(), v),
                ("left".to_string(), v),
                ("right".to_string(), v),
            ]
        );
        // A leaf's manifest is empty.
        assert!(s.manifest("bottom", v).unwrap().is_empty());
    }

    #[test]
    fn manifest_detects_cycles() {
        let mut s = ArtifactStore::new();
        let v = Version::new(1, 0, 0);
        for id in ["a", "b", "c"] {
            s.create(art(id, v)).unwrap();
        }
        s.add_dependency(("a", v), ("b", v), DepType::DependsOn)
            .unwrap();
        s.add_dependency(("b", v), ("c", v), DepType::DependsOn)
            .unwrap();
        s.add_dependency(("c", v), ("a", v), DepType::DependsOn)
            .unwrap(); // cycle
        assert!(matches!(
            s.manifest("a", v),
            Err(ArtifactError::DependencyCycle(_))
        ));
    }

    #[test]
    fn add_dependency_requires_known_endpoints() {
        let mut s = ArtifactStore::new();
        let v = Version::new(1, 0, 0);
        s.create(art("a", v)).unwrap();
        assert!(matches!(
            s.add_dependency(("a", v), ("ghost", v), DepType::DependsOn),
            Err(ArtifactError::NotFound(..))
        ));
    }

    #[test]
    fn record_batch_round_trip_preserves_state() {
        let (mut s, v) = diamond();
        s.activate("top", v).unwrap();
        s.edit_draft("left", v, |a| a.effective_start = Some(123))
            .unwrap();
        let reloaded =
            ArtifactStore::from_batches(&s.artifacts_batch(), &s.dependencies_batch()).unwrap();
        assert_eq!(reloaded.len(), s.len());
        assert_eq!(
            reloaded.get("top", v).unwrap().status,
            ArtifactStatus::Active
        );
        assert_eq!(reloaded.get("left", v).unwrap().effective_start, Some(123));
        // Dependency edges survive → manifest is identical.
        assert_eq!(
            reloaded.manifest("top", v).unwrap(),
            s.manifest("top", v).unwrap()
        );
    }

    #[test]
    fn parquet_round_trip_preserves_state() {
        let (s, v) = diamond();
        let dir = std::env::temp_dir().join(format!("ex4680_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        s.persist(&dir).unwrap();
        let loaded = ArtifactStore::load(&dir).unwrap();
        assert_eq!(loaded.len(), 4);
        assert_eq!(
            loaded.manifest("top", v).unwrap(),
            s.manifest("top", v).unwrap()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn status_rejects_fhir_unknown() {
        assert!(matches!(
            ArtifactStatus::parse("unknown"),
            Err(ArtifactError::InvalidStatus(_))
        ));
        assert_eq!(
            ArtifactStatus::parse("active").unwrap(),
            ArtifactStatus::Active
        );
    }
}
