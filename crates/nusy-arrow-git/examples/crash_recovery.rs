//! Crash Recovery — save with WAL, simulate interruption, restore
//!
//! Run with: `cargo run --example crash_recovery`

use nusy_arrow_core::{Namespace, Triple, YLayer};
use nusy_arrow_git::{
    CommitsTable, GitObjectStore, RefsTable, create_commit, restore_full, save_full,
};
use std::fs;
use std::path::Path;

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
    let save_dir = tmp.path().join("state");
    let snap_dir = tmp.path().join("snapshots");

    // --- Build up state ---
    let mut obj = GitObjectStore::with_snapshot_dir(&snap_dir);
    let mut commits = CommitsTable::new();
    let mut refs = RefsTable::new();

    for i in 0..100 {
        obj.store.add_triple(
            &make_triple(&format!("entity-{i}"), "rdf:type", "Thing"),
            Namespace::World,
            YLayer::Semantic,
        )?;
    }

    let c1 = create_commit(&obj, &mut commits, vec![], "Add 100 entities", "example")?;
    refs.init_main(&c1.commit_id);

    println!(
        "Built state: {} triples, 1 commit, 1 branch",
        obj.store.len()
    );

    // --- Save everything (crash-safe) ---
    save_full(&obj, Some(&commits), Some(&refs), &save_dir)?;
    println!("Saved to {:?}", save_dir);

    // Verify save produced expected files
    println!("\nSave directory contents:");
    for entry in fs::read_dir(&save_dir)? {
        let entry = entry?;
        let size = entry.metadata()?.len();
        println!("  {} ({} bytes)", entry.file_name().to_string_lossy(), size);
    }

    // --- Simulate a crash: drop everything ---
    drop(obj);
    drop(commits);
    drop(refs);
    println!("\n--- Simulated crash: all in-memory state lost ---");

    // --- Simulate interrupted save: leave a WAL marker ---
    let wal_path = save_dir.join("_wal.json");
    fs::write(&wal_path, "[\"world\"]")?;
    println!("Left WAL marker (simulating interrupted second save)");
    assert!(wal_path.exists(), "WAL should exist");

    // --- Restore from the save point ---
    let mut obj2 = GitObjectStore::with_snapshot_dir(&snap_dir);
    let (restored_commits, restored_refs) = restore_full(&mut obj2, &save_dir)?;

    println!("\n--- Recovery complete ---");
    println!("  Graph: {} triples restored", obj2.store.len());

    if let Some(ct) = &restored_commits {
        println!("  Commits: {} restored", ct.len());
        let c = ct
            .get(&c1.commit_id)
            .expect("Original commit should be restored");
        println!("    {} — '{}'", &c.commit_id[..8], c.message);
    }

    if let Some(rt) = &restored_refs {
        println!("  Branches: {}", rt.branches().len());
        if let Some(head) = rt.head() {
            println!("    HEAD → {} at {}", head.ref_name, &head.commit_id[..8]);
        }
    }

    // WAL should be cleaned up during restore
    assert!(!wal_path.exists(), "WAL should be cleaned up after restore");
    println!("  WAL marker cleaned up: yes");

    // --- Verify data integrity ---
    assert_eq!(obj2.store.len(), 100, "All 100 triples should be restored");
    println!(
        "\nAll {} triples verified. Recovery successful!",
        obj2.store.len()
    );

    // --- Show the WAL crash-safety guarantee ---
    println!("\n--- WAL Crash Safety Explained ---");
    println!("1. save_full() writes _wal.json FIRST (lists namespaces)");
    println!("2. Each .parquet is written to .tmp, then atomically renamed");
    println!("3. _wal.json is removed LAST (marks save complete)");
    println!("4. If crash occurs mid-save, _wal.json survives");
    println!("5. On restore, _wal.json is detected and cleaned up");
    println!("6. Each .parquet is either fully old or fully new (atomic rename)");
    println!("   → No partial or corrupt files, ever.");

    println!("\nDone!");
    Ok(())
}

/// Helper to check if a path exists (used by assertions).
#[allow(dead_code)]
fn file_exists(dir: &Path, name: &str) -> bool {
    dir.join(name).exists()
}
