use nusy_arrow_core::{Namespace, Triple, YLayer};
use nusy_arrow_git::{CommitsTable, GitObjectStore, RefsTable, create_commit};
use nusy_graph_review::{
    CommentStore, CreateProposalInput, DiffStats, ProposalStatus, ProposalStore, proposal_diff,
    proposal_stats,
};

/// Helper: set up a git object store with an initial commit on main.
fn setup_git() -> (GitObjectStore, CommitsTable, RefsTable) {
    let obj_store = GitObjectStore::new();
    let mut commits_table = CommitsTable::new();
    let mut refs_table = RefsTable::new();

    // Add a baseline triple and commit
    let mut store = obj_store;
    store
        .store
        .add_triple(
            &Triple {
                subject: "being:alpha".into(),
                predicate: "type".into(),
                object: "Being".into(),
                graph: None,
                confidence: None,
                source_document: None,
                source_chunk_id: None,
                extracted_by: None,
                caused_by: None,
                derived_from: None,
                consolidated_at: None,
                certifiability_class: None,
                object_datatype: None,
            },
            Namespace::Self_,
            YLayer::Semantic,
        )
        .expect("add triple");

    let initial = create_commit(&store, &mut commits_table, vec![], "initial", "test")
        .expect("initial commit");
    refs_table.init_main(&initial.commit_id);

    (store, commits_table, refs_table)
}

/// Helper: create a proposal branch with an additional triple and commit.
fn add_proposal_branch(
    obj_store: &mut GitObjectStore,
    commits_table: &mut CommitsTable,
    refs_table: &mut RefsTable,
    branch_name: &str,
) -> String {
    let main_commit = refs_table.resolve("main").unwrap().to_string();
    refs_table
        .create_branch(branch_name, &main_commit)
        .expect("create branch");

    // Add a new triple on the proposal branch
    obj_store
        .store
        .add_triple(
            &Triple {
                subject: "being:alpha".into(),
                predicate: "knows".into(),
                object: "calculus".into(),
                graph: None,
                confidence: Some(0.9),
                source_document: None,
                source_chunk_id: None,
                extracted_by: None,
                caused_by: None,
                derived_from: None,
                consolidated_at: None,
                certifiability_class: None,
                object_datatype: None,
            },
            Namespace::Self_,
            YLayer::Semantic,
        )
        .expect("add triple");

    let commit = create_commit(
        obj_store,
        commits_table,
        vec![main_commit],
        "add knowledge",
        "being-alpha",
    )
    .expect("commit");

    refs_table
        .update_ref(branch_name, &commit.commit_id)
        .expect("update ref");

    commit.commit_id
}

#[test]
fn test_full_lifecycle_happy_path() {
    let (mut obj_store, mut commits_table, mut refs_table) = setup_git();

    // 1. Create a proposal branch with changes
    add_proposal_branch(
        &mut obj_store,
        &mut commits_table,
        &mut refs_table,
        "proposal/add-knowledge",
    );

    // 2. Create proposal in store
    let mut proposal_store = ProposalStore::new();
    let mut comment_store = CommentStore::new();

    let prop_id = proposal_store
        .create_proposal(&CreateProposalInput {
            author: "being-alpha",
            title: "Add calculus knowledge",
            source_branch: "proposal/add-knowledge",
            target_branch: "main",
            namespace: "self",
            proposal_type: "knowledge_change",
            description: Some("Adding Y1 semantic knowledge from experiment"),
        })
        .expect("create proposal");

    assert_eq!(
        proposal_store.get_status(&prop_id).unwrap(),
        ProposalStatus::Draft
    );

    // 3. Open and assign reviewer
    proposal_store.open_proposal(&prop_id).unwrap();
    proposal_store.add_reviewer(&prop_id, "captain").unwrap();

    assert_eq!(
        proposal_store.get_status(&prop_id).unwrap(),
        ProposalStatus::Reviewing
    );

    // 4. Compute diff
    let diff_batch = proposal_diff(
        &proposal_store,
        &prop_id,
        &mut obj_store,
        &commits_table,
        &refs_table,
    )
    .expect("diff");

    assert!(diff_batch.num_rows() > 0, "diff should show changes");

    let stats = proposal_stats(
        &proposal_store,
        &prop_id,
        &mut obj_store,
        &commits_table,
        &refs_table,
    )
    .expect("stats");

    assert_eq!(
        stats,
        DiffStats {
            additions: 1,
            deletions: 0,
            total: 1,
        }
    );

    // 5. Add a review comment (blocks approval)
    let comment_id = comment_store
        .add_comment(&prop_id, "captain", "Verify confidence value", None, None)
        .unwrap();

    let unresolved = comment_store.unresolved_count(&prop_id).unwrap();
    assert_eq!(unresolved, 1);

    // Attempt to approve — should fail due to unresolved comment
    let err = proposal_store
        .approve(&prop_id, "captain", unresolved)
        .unwrap_err();
    assert!(matches!(
        err,
        nusy_graph_review::ProposalError::UnresolvedComments(1)
    ));

    // 6. Resolve the comment
    comment_store.resolve_comment(&comment_id).unwrap();
    assert_eq!(comment_store.unresolved_count(&prop_id).unwrap(), 0);

    // 7. Approve and merge
    proposal_store
        .approve(
            &prop_id,
            "captain",
            comment_store.unresolved_count(&prop_id).unwrap(),
        )
        .unwrap();
    assert_eq!(
        proposal_store.get_status(&prop_id).unwrap(),
        ProposalStatus::Approved
    );

    // Perform the actual git merge
    let source_branch = proposal_store.get_source_branch(&prop_id).unwrap();
    let target_branch = proposal_store.get_target_branch(&prop_id).unwrap();
    let source_commit = refs_table.resolve(&source_branch).unwrap().to_string();
    let target_commit = refs_table.resolve(&target_branch).unwrap().to_string();

    let merge_result = nusy_arrow_git::merge(
        &mut obj_store,
        &mut commits_table,
        &target_commit,
        &source_commit,
        "captain",
    )
    .expect("merge");

    match merge_result {
        nusy_arrow_git::MergeResult::Clean(merge_commit) => {
            refs_table
                .update_ref("main", &merge_commit.commit_id)
                .expect("update main");
        }
        other => panic!("expected clean merge, got: {other:?}"),
    }

    // Delete the source branch
    refs_table
        .delete_branch(&source_branch)
        .expect("delete branch");

    // Mark merged in proposal store
    proposal_store
        .mark_merged(&prop_id, "captain", None, None)
        .unwrap();

    // 8. Verify final state
    assert_eq!(
        proposal_store.get_status(&prop_id).unwrap(),
        ProposalStatus::Merged
    );

    // Verify main branch has the merged changes
    assert!(
        obj_store.store.len() >= 2,
        "store should have at least 2 triples"
    );

    // Verify source branch was cleaned up
    assert!(
        refs_table.get(&source_branch).is_none(),
        "source branch should be deleted"
    );
}

#[test]
fn test_reject_revise_approve_lifecycle() {
    let (mut obj_store, mut commits_table, mut refs_table) = setup_git();
    add_proposal_branch(
        &mut obj_store,
        &mut commits_table,
        &mut refs_table,
        "proposal/ontology-fix",
    );

    let mut proposal_store = ProposalStore::new();
    let prop_id = proposal_store
        .create_proposal(&CreateProposalInput {
            author: "being-beta",
            title: "Fix ontology relationships",
            source_branch: "proposal/ontology-fix",
            target_branch: "main",
            namespace: "world",
            proposal_type: "ontology_change",
            description: None,
        })
        .unwrap();

    // Open → review → reject
    proposal_store.open_proposal(&prop_id).unwrap();
    proposal_store.add_reviewer(&prop_id, "captain").unwrap();
    proposal_store.reject(&prop_id, "captain").unwrap();

    assert_eq!(
        proposal_store.get_status(&prop_id).unwrap(),
        ProposalStatus::Rejected
    );

    // Author revises (rejected → revised → reviewing)
    proposal_store.revise(&prop_id, "being-beta").unwrap();
    assert_eq!(
        proposal_store.get_status(&prop_id).unwrap(),
        ProposalStatus::Reviewing
    );

    // Second review pass → approve → merge
    proposal_store.approve(&prop_id, "captain", 0).unwrap();
    proposal_store
        .mark_merged(&prop_id, "captain", None, None)
        .unwrap();

    assert_eq!(
        proposal_store.get_status(&prop_id).unwrap(),
        ProposalStatus::Merged
    );
}

#[test]
fn test_close_after_rejection() {
    let mut proposal_store = ProposalStore::new();
    let prop_id = proposal_store
        .create_proposal(&CreateProposalInput {
            author: "being-gamma",
            title: "Bad idea",
            source_branch: "proposal/bad",
            target_branch: "main",
            namespace: "research",
            proposal_type: "safety_rule_change",
            description: None,
        })
        .unwrap();

    proposal_store.open_proposal(&prop_id).unwrap();
    proposal_store.add_reviewer(&prop_id, "captain").unwrap();
    proposal_store.reject(&prop_id, "captain").unwrap();

    // Author decides to abandon
    proposal_store
        .close_proposal(&prop_id, "being-gamma", None)
        .unwrap();
    assert_eq!(
        proposal_store.get_status(&prop_id).unwrap(),
        ProposalStatus::Closed
    );
}

#[test]
fn test_multiple_proposals_independent_state() {
    let mut store = ProposalStore::new();

    let p1 = store
        .create_proposal(&CreateProposalInput {
            author: "alpha",
            title: "P1",
            source_branch: "b1",
            target_branch: "main",
            namespace: "self",
            proposal_type: "knowledge_change",
            description: None,
        })
        .unwrap();
    let p2 = store
        .create_proposal(&CreateProposalInput {
            author: "beta",
            title: "P2",
            source_branch: "b2",
            target_branch: "main",
            namespace: "world",
            proposal_type: "ontology_change",
            description: None,
        })
        .unwrap();

    store.open_proposal(&p1).unwrap();
    store.add_reviewer(&p1, "captain").unwrap();
    store.approve(&p1, "captain", 0).unwrap();

    // P1 is approved, P2 is still draft
    assert_eq!(store.get_status(&p1).unwrap(), ProposalStatus::Approved);
    assert_eq!(store.get_status(&p2).unwrap(), ProposalStatus::Draft);
}
