//! Basic Workflow — add triples, commit, checkout, diff
//!
//! Run with: `cargo run --example basic_workflow`

use nusy_arrow_core::{Namespace, Triple, YLayer};
use nusy_arrow_git::{CommitsTable, GitObjectStore, checkout, create_commit, diff};

fn make_triple(subject: &str, predicate: &str, object: &str) -> Triple {
    Triple {
        subject: subject.to_string(),
        predicate: predicate.to_string(),
        object: object.to_string(),
        graph: None,
        confidence: Some(1.0),
        source_document: None,
        source_chunk_id: None,
        extracted_by: None,
        caused_by: None,
        derived_from: None,
        consolidated_at: None,
        certifiability_class: None,
        object_datatype: None,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path().join("snapshots"));
    let mut commits = CommitsTable::new();

    // --- Commit 1: Add initial knowledge ---
    obj.store.add_triple(
        &make_triple("alice", "knows", "bob"),
        Namespace::World,
        YLayer::Semantic,
    )?;
    obj.store.add_triple(
        &make_triple("alice", "role", "engineer"),
        Namespace::World,
        YLayer::Semantic,
    )?;

    let c1 = create_commit(
        &obj,
        &mut commits,
        vec![],
        "Add alice's relationships",
        "example",
    )?;
    println!("Commit 1: {} — '{}'", &c1.commit_id[..8], c1.message);
    println!("  Store has {} triples", obj.store.len());

    // --- Commit 2: Add more knowledge ---
    obj.store.add_triple(
        &make_triple("bob", "knows", "carol"),
        Namespace::World,
        YLayer::Semantic,
    )?;
    obj.store.add_triple(
        &make_triple("bob", "role", "designer"),
        Namespace::World,
        YLayer::Semantic,
    )?;

    let c2 = create_commit(
        &obj,
        &mut commits,
        vec![c1.commit_id.clone()],
        "Add bob's relationships",
        "example",
    )?;
    println!("Commit 2: {} — '{}'", &c2.commit_id[..8], c2.message);
    println!("  Store has {} triples", obj.store.len());

    // --- Diff between commits ---
    let changes = diff(&mut obj, &commits, &c1.commit_id, &c2.commit_id)?;
    println!("\nDiff (commit 1 → commit 2):");
    println!("  Added: {} triples", changes.added.len());
    for entry in &changes.added {
        println!(
            "    + {} → {} → {}",
            entry.subject, entry.predicate, entry.object
        );
    }
    println!("  Removed: {} triples", changes.removed.len());

    // --- Checkout commit 1 (time travel) ---
    checkout(&mut obj, &commits, &c1.commit_id)?;
    println!("\nAfter checkout to commit 1:");
    println!(
        "  Store has {} triples (bob's data is gone)",
        obj.store.len()
    );

    // --- Checkout commit 2 (back to latest) ---
    checkout(&mut obj, &commits, &c2.commit_id)?;
    println!("\nAfter checkout to commit 2:");
    println!(
        "  Store has {} triples (everything restored)",
        obj.store.len()
    );

    // --- View history ---
    let history = nusy_arrow_git::log(&commits, &c2.commit_id, 0);
    println!("\nCommit history (newest first):");
    for commit in &history {
        println!(
            "  {} — {} (by {})",
            &commit.commit_id[..8],
            commit.message,
            commit.author
        );
    }

    println!("\nDone!");
    Ok(())
}
