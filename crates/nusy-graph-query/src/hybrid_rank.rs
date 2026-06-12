//! Hybrid ranking — combine structural and semantic scores.
//!
//! Used by kanban query (text match + semantic similarity) and
//! codegraph search (structural graph position + semantic similarity).

use crate::embedding::{EmbeddedItem, EmbeddingProvider, cosine_similarity};

/// Configuration for hybrid ranking.
#[derive(Debug, Clone)]
pub struct HybridConfig {
    /// Weight for the structural/text score (default 0.6).
    pub structural_weight: f32,
    /// Weight for the semantic score (default 0.4).
    pub semantic_weight: f32,
}

impl Default for HybridConfig {
    fn default() -> Self {
        Self {
            structural_weight: 0.6,
            semantic_weight: 0.4,
        }
    }
}

/// A candidate for hybrid ranking — has an ID and a structural score.
#[derive(Debug, Clone)]
pub struct RankCandidate {
    pub id: String,
    /// Structural score (e.g., text match, graph centrality).
    pub structural_score: f32,
}

/// A ranked result after hybrid scoring.
#[derive(Debug, Clone)]
pub struct RankedResult {
    pub id: String,
    /// Combined hybrid score.
    pub score: f32,
    /// Original structural score component.
    pub structural_score: f32,
    /// Semantic similarity score component.
    pub semantic_score: f32,
}

/// Rank candidates by combining structural scores with semantic similarity.
///
/// For each candidate:
/// 1. Look up its semantic score from the embeddings
/// 2. Combine: `score = structural_weight * structural + semantic_weight * semantic`
/// 3. Sort by combined score descending
pub fn hybrid_rank(
    candidates: &[RankCandidate],
    embeddings: &[EmbeddedItem],
    query: &str,
    provider: &dyn EmbeddingProvider,
    config: &HybridConfig,
    top_k: usize,
) -> Result<Vec<RankedResult>, crate::embedding::EmbeddingError> {
    let query_vec = provider.embed(query)?;

    // Build lookup: id → semantic score
    let semantic_scores: std::collections::HashMap<&str, f32> = embeddings
        .iter()
        .map(|item| {
            (
                item.id.as_str(),
                cosine_similarity(&query_vec, &item.vector),
            )
        })
        .collect();

    let mut results: Vec<RankedResult> = candidates
        .iter()
        .map(|c| {
            let semantic_score = semantic_scores.get(c.id.as_str()).copied().unwrap_or(0.0);
            let combined = config.structural_weight * c.structural_score
                + config.semantic_weight * semantic_score;
            RankedResult {
                id: c.id.clone(),
                score: combined,
                structural_score: c.structural_score,
                semantic_score,
            }
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::HashEmbeddingProvider;

    fn provider() -> HashEmbeddingProvider {
        HashEmbeddingProvider::new(384)
    }

    #[test]
    fn test_hybrid_rank_basic() {
        let p = provider();
        let items: Vec<EmbeddedItem> = ["arrow kanban", "signal fusion", "graph query"]
            .iter()
            .map(|text| EmbeddedItem {
                id: text.to_string(),
                vector: p.embed(text).unwrap(),
            })
            .collect();

        let candidates = vec![
            RankCandidate {
                id: "arrow kanban".to_string(),
                structural_score: 1.0,
            },
            RankCandidate {
                id: "signal fusion".to_string(),
                structural_score: 0.5,
            },
            RankCandidate {
                id: "graph query".to_string(),
                structural_score: 0.0,
            },
        ];

        let config = HybridConfig::default();
        let results = hybrid_rank(&candidates, &items, "arrow", &p, &config, 10).unwrap();

        assert_eq!(results.len(), 3);
        // "arrow kanban" has structural_score=1.0, should rank highest
        assert_eq!(results[0].id, "arrow kanban");
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn test_hybrid_rank_top_k() {
        let p = provider();
        let items: Vec<EmbeddedItem> = (0..10)
            .map(|i| EmbeddedItem {
                id: format!("item-{i}"),
                vector: p.embed(&format!("item {i}")).unwrap(),
            })
            .collect();

        let candidates: Vec<RankCandidate> = (0..10)
            .map(|i| RankCandidate {
                id: format!("item-{i}"),
                structural_score: 0.5,
            })
            .collect();

        let config = HybridConfig::default();
        let results = hybrid_rank(&candidates, &items, "test", &p, &config, 3).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_hybrid_rank_custom_weights() {
        let p = provider();
        let items = vec![EmbeddedItem {
            id: "A".to_string(),
            vector: p.embed("A").unwrap(),
        }];
        let candidates = vec![RankCandidate {
            id: "A".to_string(),
            structural_score: 1.0,
        }];

        // All structural weight
        let config = HybridConfig {
            structural_weight: 1.0,
            semantic_weight: 0.0,
        };
        let results = hybrid_rank(&candidates, &items, "test", &p, &config, 10).unwrap();
        assert!((results[0].score - 1.0).abs() < 1e-6);

        // All semantic weight
        let config = HybridConfig {
            structural_weight: 0.0,
            semantic_weight: 1.0,
        };
        let results = hybrid_rank(&candidates, &items, "test", &p, &config, 10).unwrap();
        assert_eq!(results[0].structural_score, 1.0);
        assert!(results[0].score < 1.0); // semantic score < 1.0 for different text
    }

    #[test]
    fn test_hybrid_rank_missing_embedding() {
        let p = provider();
        // Candidate exists but has no embedding
        let candidates = vec![RankCandidate {
            id: "missing".to_string(),
            structural_score: 0.8,
        }];

        let config = HybridConfig::default();
        let results = hybrid_rank(&candidates, &[], "test", &p, &config, 10).unwrap();
        assert_eq!(results.len(), 1);
        // Semantic score should be 0, combined = 0.6 * 0.8 + 0.4 * 0 = 0.48
        assert!((results[0].score - 0.48).abs() < 1e-6);
    }
}
