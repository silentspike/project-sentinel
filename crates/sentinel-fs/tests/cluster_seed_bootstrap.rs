use sentinel_common::{
    ActivationState, LocalOwnerBaseRole, LocalOwnerBaseState, LocalOwnerStateSnapshot, NodeId,
    OwnerIssueError, OwnerRegistry, OwnerTerm, OwnerTermSnapshot, StateTransferScope,
    TRACK_A_COORDINATOR_GENERATION,
};
use sentinel_fs::cas::CasStore;
use sentinel_fs::layer::LayerManager;
use sentinel_fs::metadata::MetadataStore;
use sentinel_fs::SHARED_BASE_LAYER_ID;

fn node(byte: u8) -> NodeId {
    NodeId(uuid::Uuid::from_bytes([byte; 16]))
}

fn install_seed_authority(seed: NodeId) {
    assert!(OwnerRegistry::init_cluster(seed));
    let terms = vec![
        OwnerTerm {
            scope: StateTransferScope::World,
            owner_node: seed,
            epoch: 1,
            coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
        },
        OwnerTerm {
            scope: StateTransferScope::for_agent("AGENT-01"),
            owner_node: seed,
            epoch: 1,
            coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
        },
    ];
    let global = OwnerTermSnapshot::new(TRACK_A_COORDINATOR_GENERATION, 1, terms).unwrap();
    let local = LocalOwnerStateSnapshot::new(
        seed,
        TRACK_A_COORDINATOR_GENERATION,
        1,
        global
            .sorted_terms
            .iter()
            .cloned()
            .map(|owner_term| LocalOwnerBaseState {
                scope: owner_term.scope.clone(),
                recipient_node: seed,
                owner_term,
                base_role: LocalOwnerBaseRole::Owner,
                activation_state: ActivationState::Routable,
            })
            .collect(),
    )
    .unwrap();
    OwnerRegistry::global()
        .rebuild_from_owner_snapshot(&global, &local, vec![])
        .unwrap();
}

#[test]
fn empty_seed_bootstrap_is_idempotent_and_keeps_content_fenced() {
    install_seed_authority(node(1));
    let dir = tempfile::tempdir().unwrap();
    let metadata_path = dir.path().join("metadata.redb");

    let layer = LayerManager::new(
        CasStore::open(dir.path()).unwrap(),
        MetadataStore::open(&metadata_path).unwrap(),
    );
    layer.init_base_root().unwrap();
    let root = layer
        .meta()
        .get_inode(SHARED_BASE_LAYER_ID, 1)
        .unwrap()
        .expect("base root must exist on an empty seed");
    layer.init_base_root().unwrap();
    let second = layer
        .meta()
        .get_inode(SHARED_BASE_LAYER_ID, 1)
        .unwrap()
        .expect("base root must survive repeated initialization");
    assert_eq!(root.mode, second.mode);
    assert_eq!(root.mtime, second.mtime);

    let inode = layer
        .populate_base_file(1, "seed.txt", b"seed-owned", 0o644)
        .expect("the World owner may populate shared content");
    assert_eq!(
        layer.read_file(SHARED_BASE_LAYER_ID, inode).unwrap(),
        b"seed-owned"
    );

    let blobs_before = layer.cas().stats().unwrap().blob_count;
    let error = layer
        .write_file("AGENT-99", 1, "unknown.txt", b"must-not-land", 0o600)
        .expect_err("an unknown agent scope must remain fail closed");
    assert!(matches!(
        error.downcast_ref::<OwnerIssueError>(),
        Some(OwnerIssueError::UnknownScope { .. })
    ));
    assert_eq!(layer.cas().stats().unwrap().blob_count, blobs_before);

    drop(layer);
    let reopened = LayerManager::new(
        CasStore::open(dir.path()).unwrap(),
        MetadataStore::open(&metadata_path).unwrap(),
    );
    reopened.init_base_root().unwrap();
    let after_restart = reopened
        .meta()
        .get_inode(SHARED_BASE_LAYER_ID, 1)
        .unwrap()
        .expect("base root must survive restart initialization");
    assert_eq!(root.mode, after_restart.mode);
    assert_eq!(root.mtime, after_restart.mtime);
}
