//! Basic embedding example — hash provider, embed text, compute similarity.
//!
//! Run: `cargo run --example basic_embedding`

use nusy_graph_query::{EmbeddingProvider, HashEmbeddingProvider, cosine_similarity};

fn main() {
    // Create a deterministic hash-based embedding provider.
    // Good for testing and small datasets. For production, use
    // OllamaEmbeddingProvider (feature: ollama) or SubprocessEmbeddingProvider.
    let provider = HashEmbeddingProvider::new(384);

    // Embed some text
    let texts = vec![
        "Alice knows Bob".to_string(),
        "Alice is friends with Bob".to_string(),
        "The weather is sunny today".to_string(),
    ];

    let vectors = provider.embed_batch(&texts).expect("embedding failed");
    println!(
        "Embedded {} texts into {}-dim vectors",
        texts.len(),
        vectors[0].len()
    );

    // Compute pairwise similarities
    for i in 0..texts.len() {
        for j in (i + 1)..texts.len() {
            let sim = cosine_similarity(&vectors[i], &vectors[j]);
            println!("  sim(\"{}\", \"{}\") = {:.4}", texts[i], texts[j], sim);
        }
    }

    // Semantic search: find most similar to a query
    let query_vec = provider.embed("who knows whom").expect("embed query");
    println!("\nQuery: \"who knows whom\"");
    for (i, text) in texts.iter().enumerate() {
        let sim = cosine_similarity(&query_vec, &vectors[i]);
        println!("  {:.4}  {}", sim, text);
    }
}
