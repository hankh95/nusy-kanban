//! KgStore — full-featured Arrow-native knowledge graph store.
//!
//! Replacement for Python `brain/reasoning/kg_store.py`.
//! Adds namespace prefix management, keyword search, knowledge gap tracking,
//! and bulk operations on top of [`ArrowGraphStore`].
//!
//! # Quick Start
//!
//! ```rust
//! use nusy_arrow_core::kg_store::KgStore;
//!
//! let mut store = KgStore::new();
//! store.bind_prefix("nusy", "https://nusy.dev/");
//! store.bind_prefix("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#");
//!
//! store.add_triple("nusy:Alice", "rdf:type", "nusy:Person", None, 1.0).unwrap();
//!
//! let results = store.search_by_keywords(&["Alice"]);
//! assert_eq!(results.len(), 1);
//! ```

use crate::namespace::Namespace;
use crate::schema::col;
use crate::store::{ArrowGraphStore, QuerySpec, StoreError, Triple};
use crate::triple_store::{StoredTriple, batches_to_stored_triples};
use crate::y_layer::YLayer;

use arrow::array::StringArray;
use std::collections::HashMap;

/// Default namespace prefixes (matching Python KGStore).
fn default_prefixes() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert(
        "rdf".into(),
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#".into(),
    );
    m.insert(
        "rdfs".into(),
        "http://www.w3.org/2000/01/rdf-schema#".into(),
    );
    m.insert("owl".into(), "http://www.w3.org/2002/07/owl#".into());
    m.insert("xsd".into(), "http://www.w3.org/2001/XMLSchema#".into());
    m.insert("foaf".into(), "http://xmlns.com/foaf/0.1/".into());
    m.insert("prov".into(), "http://www.w3.org/ns/prov#".into());
    m.insert("santiago".into(), "https://nusy.dev/santiago/".into());
    m.insert("ethics".into(), "https://nusy.dev/ethics/".into());
    m.insert("pm".into(), "https://nusy.dev/pm/".into());
    m.insert("dev".into(), "https://nusy.dev/dev/".into());
    m.insert("nusy".into(), "https://nusy.dev/".into());
    m
}

/// A knowledge gap — something the being doesn't know.
#[derive(Debug, Clone)]
pub struct KnowledgeGap {
    pub question: String,
    pub keywords: Vec<String>,
    pub confidence: f64,
    pub missing_concepts: Vec<String>,
    pub resolved: bool,
}

/// Statistics about the knowledge graph.
#[derive(Debug, Clone)]
pub struct KgStats {
    pub total_triples: usize,
    pub unique_subjects: usize,
    pub unique_predicates: usize,
    pub unique_objects: usize,
    pub namespace_count: usize,
}

/// Full-featured Arrow-native knowledge graph store.
///
/// Provides:
/// - Namespace prefix management (expand/compact URIs)
/// - Pattern-based triple queries
/// - Keyword search (case-insensitive substring matching)
/// - Knowledge gap tracking
/// - Bulk add operations
pub struct KgStore {
    inner: ArrowGraphStore,
    prefixes: HashMap<String, String>,
    gaps: Vec<KnowledgeGap>,
    default_namespace: Namespace,
    default_y_layer: YLayer,
}

impl KgStore {
    /// Create a new store with default NuSy prefixes.
    pub fn new() -> Self {
        Self {
            inner: ArrowGraphStore::new(),
            prefixes: default_prefixes(),
            gaps: Vec::new(),
            default_namespace: Namespace::World,
            default_y_layer: YLayer::Semantic,
        }
    }

    /// Create with custom defaults.
    pub fn with_defaults(namespace: Namespace, y_layer: YLayer) -> Self {
        Self {
            inner: ArrowGraphStore::new(),
            prefixes: default_prefixes(),
            gaps: Vec::new(),
            default_namespace: namespace,
            default_y_layer: y_layer,
        }
    }

    // ── Prefix management ─────────────────────────────────────────────

    /// Bind a namespace prefix (e.g., "rdf" → "http://www.w3.org/...").
    pub fn bind_prefix(&mut self, prefix: &str, uri: &str) {
        self.prefixes.insert(prefix.to_string(), uri.to_string());
    }

    /// Expand a prefixed URI (e.g., "rdf:type" → "http://www.w3.org/.../type").
    /// Returns the original string if no prefix matches.
    pub fn expand_uri(&self, value: &str) -> String {
        if let Some(idx) = value.find(':') {
            let prefix = &value[..idx];
            let local = &value[idx + 1..];
            if let Some(ns_uri) = self.prefixes.get(prefix) {
                return format!("{ns_uri}{local}");
            }
        }
        value.to_string()
    }

    /// Compact a full URI to prefixed form (e.g., "http://www.w3.org/.../type" → "rdf:type").
    /// Returns the original string if no prefix matches.
    /// Matches longest prefix first to avoid ambiguity (e.g., "santiago:" before "nusy:").
    pub fn compact_uri(&self, uri: &str) -> String {
        // Sort by URI length descending so longest prefix matches first
        let mut sorted: Vec<_> = self.prefixes.iter().collect();
        sorted.sort_by_key(|e| std::cmp::Reverse(e.1.len()));

        for (prefix, ns_uri) in sorted {
            if let Some(local) = uri.strip_prefix(ns_uri.as_str()) {
                return format!("{prefix}:{local}");
            }
        }
        uri.to_string()
    }

    /// Get all bound prefixes.
    pub fn prefixes(&self) -> &HashMap<String, String> {
        &self.prefixes
    }

    // ── Triple operations ─────────────────────────────────────────────

    /// Add a triple with optional namespace expansion.
    pub fn add_triple(
        &mut self,
        subject: &str,
        predicate: &str,
        object: &str,
        source: Option<&str>,
        confidence: f64,
    ) -> Result<String, StoreError> {
        let triple = Triple {
            subject: self.expand_uri(subject),
            predicate: self.expand_uri(predicate),
            object: self.expand_uri(object),
            graph: None,
            confidence: Some(confidence),
            source_document: source.map(|s| s.to_string()),
            source_chunk_id: None,
            extracted_by: source.map(|s| s.to_string()),
            caused_by: None,
            derived_from: None,
            consolidated_at: None,
            certifiability_class: None,
            object_datatype: None,
        };
        self.inner
            .add_triple(&triple, self.default_namespace, self.default_y_layer)
    }

    /// Add multiple triples in batch.
    pub fn add_triples(
        &mut self,
        triples: &[(&str, &str, &str, f64)],
        source: Option<&str>,
    ) -> Result<Vec<String>, StoreError> {
        let ts: Vec<Triple> = triples
            .iter()
            .map(|(s, p, o, conf)| Triple {
                subject: self.expand_uri(s),
                predicate: self.expand_uri(p),
                object: self.expand_uri(o),
                graph: None,
                confidence: Some(*conf),
                source_document: source.map(|s| s.to_string()),
                source_chunk_id: None,
                extracted_by: source.map(|s| s.to_string()),
                caused_by: None,
                derived_from: None,
                consolidated_at: None,
                certifiability_class: None,
                object_datatype: None,
            })
            .collect();
        self.inner
            .add_batch(&ts, self.default_namespace, self.default_y_layer)
    }

    /// Query by (s, p, o) pattern. None means wildcard. URIs are expanded.
    pub fn query(
        &self,
        subject: Option<&str>,
        predicate: Option<&str>,
        object: Option<&str>,
    ) -> Result<Vec<StoredTriple>, StoreError> {
        let spec = QuerySpec {
            subject: subject.map(|s| self.expand_uri(s)),
            predicate: predicate.map(|s| self.expand_uri(s)),
            object: object.map(|s| self.expand_uri(s)),
            ..Default::default()
        };
        let batches = self.inner.query(&spec)?;
        Ok(batches_to_stored_triples(&batches))
    }

    /// Search by keywords (case-insensitive substring match on s/p/o).
    /// Returns matching triples with the matched keyword noted.
    pub fn search_by_keywords(&self, keywords: &[&str]) -> Vec<(StoredTriple, String)> {
        let spec = QuerySpec::default();
        let batches = self.inner.query(&spec).unwrap_or_default();
        let mut results = Vec::new();

        for batch in &batches {
            let subjects = batch
                .column(col::SUBJECT)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("subject column");
            let predicates = batch
                .column(col::PREDICATE)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("predicate column");
            let objects = batch
                .column(col::OBJECT)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("object column");

            for i in 0..batch.num_rows() {
                let s = subjects.value(i).to_lowercase();
                let p = predicates.value(i).to_lowercase();
                let o = objects.value(i).to_lowercase();

                for kw in keywords {
                    let kw_lower = kw.to_lowercase();
                    if s.contains(&kw_lower) || p.contains(&kw_lower) || o.contains(&kw_lower) {
                        results.push((
                            crate::triple_store::extract_stored_triple(batch, i),
                            kw.to_string(),
                        ));
                        break; // Don't duplicate for multiple keyword matches
                    }
                }
            }
        }
        results
    }

    /// Clear all triples.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    // ── Knowledge gap tracking ────────────────────────────────────────

    /// Record a knowledge gap.
    pub fn record_knowledge_gap(
        &mut self,
        question: &str,
        keywords: &[&str],
        confidence: f64,
        missing_concepts: &[&str],
    ) -> usize {
        let gap = KnowledgeGap {
            question: question.to_string(),
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
            confidence,
            missing_concepts: missing_concepts.iter().map(|s| s.to_string()).collect(),
            resolved: false,
        };
        self.gaps.push(gap);
        self.gaps.len() - 1
    }

    /// Get unresolved knowledge gaps.
    pub fn unresolved_gaps(&self) -> Vec<&KnowledgeGap> {
        self.gaps.iter().filter(|g| !g.resolved).collect()
    }

    /// Resolve a knowledge gap by index.
    pub fn resolve_gap(&mut self, index: usize) -> bool {
        if let Some(gap) = self.gaps.get_mut(index) {
            gap.resolved = true;
            true
        } else {
            false
        }
    }

    // ── Statistics ────────────────────────────────────────────────────

    /// Get store statistics.
    pub fn statistics(&self) -> KgStats {
        let spec = QuerySpec::default();
        let batches = self.inner.query(&spec).unwrap_or_default();
        let triples = batches_to_stored_triples(&batches);

        let mut subjects = std::collections::HashSet::new();
        let mut predicates = std::collections::HashSet::new();
        let mut objects = std::collections::HashSet::new();

        for t in &triples {
            subjects.insert(t.subject.clone());
            predicates.insert(t.predicate.clone());
            objects.insert(t.object.clone());
        }

        KgStats {
            total_triples: triples.len(),
            unique_subjects: subjects.len(),
            unique_predicates: predicates.len(),
            unique_objects: objects.len(),
            namespace_count: self.prefixes.len(),
        }
    }

    /// Total triple count.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Get reference to underlying ArrowGraphStore.
    pub fn inner(&self) -> &ArrowGraphStore {
        &self.inner
    }

    /// Get mutable reference to underlying ArrowGraphStore.
    pub fn inner_mut(&mut self) -> &mut ArrowGraphStore {
        &mut self.inner
    }
}

impl Default for KgStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefix_expand() {
        let store = KgStore::new();
        assert_eq!(
            store.expand_uri("rdf:type"),
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
        );
        assert_eq!(
            store.expand_uri("santiago:Alice"),
            "https://nusy.dev/santiago/Alice"
        );
        assert_eq!(store.expand_uri("no_prefix"), "no_prefix");
    }

    #[test]
    fn test_prefix_compact() {
        let store = KgStore::new();
        assert_eq!(
            store.compact_uri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            "rdf:type"
        );
        assert_eq!(
            store.compact_uri("https://nusy.dev/santiago/Alice"),
            "santiago:Alice"
        );
        assert_eq!(
            store.compact_uri("http://unknown/foo"),
            "http://unknown/foo"
        );
    }

    #[test]
    fn test_add_with_prefix_expansion() {
        let mut store = KgStore::new();
        store
            .add_triple("santiago:Alice", "rdf:type", "santiago:Person", None, 1.0)
            .unwrap();

        // Query with expanded URI
        let results = store
            .query(Some("https://nusy.dev/santiago/Alice"), None, None)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].predicate,
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
        );
    }

    #[test]
    fn test_query_with_prefix() {
        let mut store = KgStore::new();
        store
            .add_triple("santiago:Alice", "rdf:type", "santiago:Person", None, 1.0)
            .unwrap();

        // Query with prefixed URI (auto-expanded)
        let results = store.query(Some("santiago:Alice"), None, None).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_keyword_search() {
        let mut store = KgStore::new();
        store
            .add_triple(
                "santiago:Alice",
                "santiago:knows",
                "santiago:Bob",
                None,
                1.0,
            )
            .unwrap();
        store
            .add_triple(
                "santiago:Carol",
                "santiago:likes",
                "santiago:Dave",
                None,
                1.0,
            )
            .unwrap();

        let results = store.search_by_keywords(&["Alice"]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, "Alice");

        let results = store.search_by_keywords(&["santiago"]);
        assert_eq!(results.len(), 2); // Both triples contain "santiago"
    }

    #[test]
    fn test_keyword_search_case_insensitive() {
        let mut store = KgStore::new();
        store
            .add_triple("Alice", "knows", "Bob", None, 1.0)
            .unwrap();

        let results = store.search_by_keywords(&["alice"]);
        assert_eq!(results.len(), 1);

        let results = store.search_by_keywords(&["ALICE"]);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_knowledge_gaps() {
        let mut store = KgStore::new();
        let idx = store.record_knowledge_gap(
            "What is photosynthesis?",
            &["photosynthesis", "plants"],
            0.3,
            &["chloroplast", "light_reaction"],
        );

        assert_eq!(store.unresolved_gaps().len(), 1);
        assert_eq!(
            store.unresolved_gaps()[0].question,
            "What is photosynthesis?"
        );

        assert!(store.resolve_gap(idx));
        assert_eq!(store.unresolved_gaps().len(), 0);
    }

    #[test]
    fn test_bulk_add() {
        let mut store = KgStore::new();
        let ids = store
            .add_triples(
                &[
                    ("santiago:A", "rdf:type", "santiago:Person", 1.0),
                    ("santiago:B", "rdf:type", "santiago:Person", 1.0),
                    ("santiago:C", "rdf:type", "santiago:Person", 1.0),
                ],
                Some("bulk_import"),
            )
            .unwrap();
        assert_eq!(ids.len(), 3);
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn test_custom_prefix() {
        let mut store = KgStore::new();
        store.bind_prefix("med", "https://nusy.dev/medical/");
        store
            .add_triple("med:Aspirin", "rdf:type", "med:Drug", None, 1.0)
            .unwrap();

        let results = store.query(Some("med:Aspirin"), None, None).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].subject.starts_with("https://nusy.dev/medical/"));
    }

    #[test]
    fn test_statistics() {
        let mut store = KgStore::new();
        store.add_triple("s1", "p1", "o1", None, 1.0).unwrap();
        store.add_triple("s2", "p1", "o2", None, 1.0).unwrap();

        let stats = store.statistics();
        assert_eq!(stats.total_triples, 2);
        assert_eq!(stats.unique_subjects, 2);
        assert_eq!(stats.unique_predicates, 1);
        assert!(stats.namespace_count >= 11); // Default prefixes
    }

    #[test]
    fn test_clear() {
        let mut store = KgStore::new();
        store.add_triple("s", "p", "o", None, 1.0).unwrap();
        assert_eq!(store.len(), 1);

        store.clear();
        assert_eq!(store.len(), 0);
    }
}
