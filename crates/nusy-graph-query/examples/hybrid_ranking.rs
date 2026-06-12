//! Hybrid ranking example — combine structural + semantic scores.
//!
//! Run: `cargo run --example hybrid_ranking`

use nusy_graph_query::{
    EmbeddedItem, EmbeddingProvider, HashEmbeddingProvider, HybridConfig, RankCandidate,
    hybrid_rank,
};

fn main() {
    let provider = HashEmbeddingProvider::new(384);

    // Simulate items with structural scores (e.g., from graph centrality)
    let candidates = vec![
        RankCandidate {
            id: "EX-3141".to_string(),
            structural_score: 0.9, // High structural importance
        },
        RankCandidate {
            id: "EX-3142".to_string(),
            structural_score: 0.5,
        },
        RankCandidate {
            id: "EX-3143".to_string(),
            structural_score: 0.3,
        },
    ];

    // Embeddings for each item (pre-computed from titles)
    let titles = vec![
        "Eviction scoring and selective promote",
        "PromoteHook and CertifiabilityBoundary",
        "TrainingTrigger and ConsolidationOrchestrator",
    ];
    let embeddings: Vec<EmbeddedItem> = candidates
        .iter()
        .zip(titles.iter())
        .map(|(c, title)| EmbeddedItem {
            id: c.id.clone(),
            vector: provider.embed(title).expect("embed"),
        })
        .collect();

    // Hybrid rank: 60% structural, 40% semantic
    let config = HybridConfig {
        structural_weight: 0.6,
        semantic_weight: 0.4,
    };

    let results = hybrid_rank(
        &candidates,
        &embeddings,
        "consolidation training pipeline",
        &provider,
        &config,
        10, // top_k
    )
    .expect("ranking");

    println!("Hybrid ranking for \"consolidation training pipeline\":");
    println!("{:<12} {:.4}  (structural + semantic)", "ID", "Score");
    println!("{}", "-".repeat(40));
    for r in &results {
        println!("{:<12} {:.4}", r.id, r.score);
    }
}
