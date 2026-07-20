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

fn install_follower_authority(seed: NodeId, follower: NodeId) {
    assert!(OwnerRegistry::init_cluster(follower));
    let term = OwnerTerm {
        scope: StateTransferScope::World,
        owner_node: seed,
        epoch: 1,
        coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
    };
    let global =
        OwnerTermSnapshot::new(TRACK_A_COORDINATOR_GENERATION, 1, vec![term.clone()]).unwrap();
    let local = LocalOwnerStateSnapshot::new(
        follower,
        TRACK_A_COORDINATOR_GENERATION,
        1,
        vec![LocalOwnerBaseState {
            scope: StateTransferScope::World,
            recipient_node: follower,
            owner_term: term,
            base_role: LocalOwnerBaseRole::Follower,
            activation_state: ActivationState::NotRoutable,
        }],
    )
    .unwrap();
    OwnerRegistry::global()
        .rebuild_from_owner_snapshot(&global, &local, vec![])
        .unwrap();
}

#[test]
fn empty_follower_bootstraps_only_node_local_structure() {
    install_follower_authority(node(1), node(2));
    let dir = tempfile::tempdir().unwrap();
    let metadata_path = dir.path().join("metadata.redb");
    let layer = LayerManager::new(
        CasStore::open(dir.path()).unwrap(),
        MetadataStore::open(&metadata_path).unwrap(),
    );

    layer
        .init_base_root()
        .expect("a fresh follower must create its node-local base root");
    assert!(layer
        .meta()
        .get_inode(SHARED_BASE_LAYER_ID, 1)
        .unwrap()
        .is_some());

    let error = layer
        .populate_base_file(1, "forbidden.txt", b"world-content", 0o644)
        .expect_err("a follower must not change shared base content");
    assert!(matches!(
        error.downcast_ref::<OwnerIssueError>(),
        Some(OwnerIssueError::NotOwner { .. })
    ));
    assert_eq!(layer.cas().stats().unwrap().blob_count, 0);

    layer.init_base_root().unwrap();
    assert!(layer
        .meta()
        .get_inode(SHARED_BASE_LAYER_ID, 1)
        .unwrap()
        .is_some());
}
