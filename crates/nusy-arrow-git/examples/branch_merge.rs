//! Branch & Merge — create branches, diverge, merge with conflict resolution
//!
//! Run with: `cargo run --example branch_merge`

use nusy_arrow_core::{Namespace, Triple, YLayer};
use nusy_arrow_git::{
    CommitsTable, GitObjectStore, MergeResult, MergeStrategy, RefsTable, Resolution, checkout,
    create_commit, merge_with_strategy,
};

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
    let mut refs = RefsTable::new();

    // --- Initial commit on main ---
    obj.store.add_triple(
        &make_triple("alice", "knows", "bob"),
        Namespace::World,
        YLayer::Semantic,
    )?;

    let c1 = create_commit(
        &obj,
        &mut commits,
        vec![],
        "Initial: alice knows bob",
        "example",
    )?;
    refs.init_main(&c1.commit_id);
    println!("main: {} — '{}'", &c1.commit_id[..8], c1.message);

    // --- Create a feature branch ---
    refs.create_branch("feature", &c1.commit_id)?;
    println!("Created branch 'feature' at {}", &c1.commit_id[..8]);

    // --- Commit on main: alice's role = engineer ---
    obj.store.add_triple(
        &make_triple("alice", "role", "engineer"),
        Namespace::World,
        YLayer::Semantic,
    )?;

    let c_main = create_commit(
        &obj,
        &mut commits,
        vec![c1.commit_id.clone()],
        "main: alice is an engineer",
        "example",
    )?;
    refs.update_ref("main", &c_main.commit_id)?;
    println!("main: {} — '{}'", &c_main.commit_id[..8], c_main.message);

    // --- Switch to feature branch, commit different data ---
    checkout(&mut obj, &commits, &c1.commit_id)?; // Go back to branch point

    obj.store.add_triple(
        &make_triple("alice", "role", "architect"),
        Namespace::World,
        YLayer::Semantic,
    )?;
    obj.store.add_triple(
        &make_triple("bob", "role", "designer"),
        Namespace::World,
        YLayer::Semantic,
    )?;

    let c_feat = create_commit(
        &obj,
        &mut commits,
        vec![c1.commit_id.clone()],
        "feature: alice is architect, bob is designer",
        "example",
    )?;
    refs.update_ref("feature", &c_feat.commit_id)?;
    println!("feature: {} — '{}'", &c_feat.commit_id[..8], c_feat.message);

    // --- Merge with Manual strategy (detect conflicts) ---
    println!("\n--- Merging with Manual strategy ---");
    let result = merge_with_strategy(
        &mut obj,
        &mut commits,
        &c_main.commit_id,
        &c_feat.commit_id,
        "example",
        &MergeStrategy::Manual,
    )?;

    match &result {
        MergeResult::Clean(mc) => println!("Clean merge: {}", &mc.commit_id[..8]),
        MergeResult::Conflict(conflicts) => {
            println!("Found {} conflict(s):", conflicts.len());
            for c in conflicts {
                println!(
                    "  CONFLICT: {}.{} — main='{}' vs feature='{}'",
                    c.subject, c.predicate, c.object_a, c.object_b
                );
            }
        }
        MergeResult::NoCommonAncestor => println!("No common ancestor"),
    }

    // --- Merge with Custom strategy (resolve conflicts) ---
    println!("\n--- Merging with Custom strategy (keep feature's values) ---");

    // Need to re-checkout since merge may have changed state
    checkout(&mut obj, &commits, &c_main.commit_id)?;

    let result = merge_with_strategy(
        &mut obj,
        &mut commits,
        &c_main.commit_id,
        &c_feat.commit_id,
        "example",
        &MergeStrategy::Custom(Box::new(|_conflict| {
            // Always prefer the feature branch
            Resolution::KeepTheirs
        })),
    )?;

    match &result {
        MergeResult::Clean(mc) => {
            println!("Clean merge: {} — '{}'", &mc.commit_id[..8], mc.message);
            println!(
                "  Parents: {} + {}",
                &mc.parent_ids[0][..8],
                &mc.parent_ids[1][..8]
            );
            println!("  Store now has {} triples", obj.store.len());
        }
        MergeResult::Conflict(_) => println!("Unexpected conflict!"),
        MergeResult::NoCommonAncestor => println!("No common ancestor"),
    }

    // --- Show final branch state ---
    println!("\nBranches:");
    for r in refs.branches() {
        let head_marker = if r.is_head { " (HEAD)" } else { "" };
        println!("  {}: {}{}", r.ref_name, &r.commit_id[..8], head_marker);
    }

    println!("\nDone!");
    Ok(())
}
