//! Integration test: Full lifecycle exercising all new operations.
//!
//! Scenario from EXP-1284 Phase 5:
//! 1. Init repo, commit initial data
//! 2. Create branch `feature`, switch to it
//! 3. Make two commits on `feature`
//! 4. Tag the second commit as `v1.0`
//! 5. Switch back to `main`, cherry-pick the first `feature` commit
//! 6. Verify `main` has the cherry-picked changes
//! 7. Revert the cherry-picked commit on `main`
//! 8. Verify `main` is back to its original state
//! 9. Delete branch `feature`
//! 10. Verify tag `v1.0` still resolves to the correct commit
//! 11. All pre-existing tests still pass (verified by cargo test --workspace)

mod common;

use common::sample_triple;
use nusy_arrow_core::{Namespace, YLayer};
use nusy_arrow_git::{
    CommitsTable, GitObjectStore, RefsTable, checkout, cherry_pick, create_commit, revert,
};

#[test]
fn test_full_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();
    let mut refs = RefsTable::new();

    // 1. Init repo, commit initial data
    obj.store
        .add_triple(
            &sample_triple("initial", "Base"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();
    let c0 = create_commit(&obj, &mut commits, vec![], "initial commit", "DGX").unwrap();
    refs.init_main(&c0.commit_id);
    assert_eq!(obj.store.len(), 1);

    // 2. Create branch `feature`, switch to it
    refs.create_branch("feature", &c0.commit_id).unwrap();
    refs.switch_head("feature").unwrap();
    assert_eq!(refs.head().unwrap().ref_name, "feature");

    // 3. Make two commits on `feature`
    obj.store
        .add_triple(
            &sample_triple("feat1", "F1"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();
    let f1 = create_commit(
        &obj,
        &mut commits,
        vec![c0.commit_id.clone()],
        "feature commit 1",
        "DGX",
    )
    .unwrap();
    refs.update_ref("feature", &f1.commit_id).unwrap();

    obj.store
        .add_triple(
            &sample_triple("feat2", "F2"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();
    let f2 = create_commit(
        &obj,
        &mut commits,
        vec![f1.commit_id.clone()],
        "feature commit 2",
        "DGX",
    )
    .unwrap();
    refs.update_ref("feature", &f2.commit_id).unwrap();
    assert_eq!(obj.store.len(), 3); // initial + feat1 + feat2

    // 4. Tag the second commit as `v1.0`
    refs.create_tag("v1.0", &f2.commit_id).unwrap();
    assert_eq!(refs.tags().len(), 1);
    assert_eq!(refs.resolve("v1.0"), Some(f2.commit_id.as_str()));

    // 5. Switch back to `main`, cherry-pick the first `feature` commit
    refs.switch_head("main").unwrap();
    checkout(&mut obj, &commits, &c0.commit_id).unwrap();
    assert_eq!(obj.store.len(), 1); // only initial

    let cp_id = cherry_pick(&mut obj, &mut commits, &f1.commit_id, &c0.commit_id, "DGX").unwrap();
    refs.update_ref("main", &cp_id).unwrap();

    // 6. Verify `main` has the cherry-picked changes
    assert_eq!(obj.store.len(), 2); // initial + feat1

    // 7. Revert the cherry-picked commit on `main`
    let revert_id = revert(&mut obj, &mut commits, &cp_id, &cp_id, "DGX").unwrap();
    refs.update_ref("main", &revert_id).unwrap();

    // 8. Verify `main` is back to its original state
    assert_eq!(obj.store.len(), 1); // only initial

    // 9. Delete branch `feature`
    refs.delete_branch("feature").unwrap();
    assert_eq!(refs.branches().len(), 1); // only main
    assert!(refs.get("feature").is_none());

    // 10. Verify tag `v1.0` still resolves to the correct commit
    assert_eq!(refs.resolve("v1.0"), Some(f2.commit_id.as_str()));
    assert_eq!(refs.tags().len(), 1);

    // Verify commits table has all commits: c0, f1, f2, cherry-pick, revert
    assert!(commits.len() >= 5);
}

#[test]
fn test_tag_and_branch_namespace_isolation() {
    let mut refs = RefsTable::new();
    refs.init_main("c1");
    refs.create_branch("dev", "c1").unwrap();
    refs.create_tag("v1.0", "c1").unwrap();
    refs.create_tag("v2.0", "c2").unwrap();

    // branches() returns only branches
    assert_eq!(refs.branches().len(), 2);
    // tags() returns only tags
    assert_eq!(refs.tags().len(), 2);
    // get() returns both
    assert!(refs.get("dev").is_some());
    assert!(refs.get("v1.0").is_some());
}

#[test]
fn test_delete_branch_leaves_commits_accessible() {
    let tmp = tempfile::tempdir().unwrap();
    let mut obj = GitObjectStore::with_snapshot_dir(tmp.path());
    let mut commits = CommitsTable::new();
    let mut refs = RefsTable::new();

    // Create data and commit
    obj.store
        .add_triple(
            &sample_triple("s1", "A"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();
    let c1 = create_commit(&obj, &mut commits, vec![], "c1", "DGX").unwrap();
    refs.init_main(&c1.commit_id);

    // Create branch and commit on it
    refs.create_branch("ephemeral", &c1.commit_id).unwrap();
    obj.store
        .add_triple(
            &sample_triple("s2", "B"),
            Namespace::World,
            YLayer::Semantic,
        )
        .unwrap();
    let c2 = create_commit(
        &obj,
        &mut commits,
        vec![c1.commit_id.clone()],
        "ephemeral work",
        "DGX",
    )
    .unwrap();
    refs.update_ref("ephemeral", &c2.commit_id).unwrap();

    // Delete the branch
    refs.delete_branch("ephemeral").unwrap();

    // The commit is still in the commits table (unreachable via refs, but not deleted)
    assert!(commits.get(&c2.commit_id).is_some());

    // Can still checkout the orphaned commit directly
    checkout(&mut obj, &commits, &c2.commit_id).unwrap();
    assert_eq!(obj.store.len(), 2);
}
