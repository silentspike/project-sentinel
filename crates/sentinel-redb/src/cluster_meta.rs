//! Durable cluster owner authority and recipient-local state (ADR-3 / #615).
//!
//! This database is deliberately outside the data-plane owner fence: owner authority
//! and local transition state are control-plane operations and cannot be authorized by
//! the guard they replace. Bootstrap and replication use one atomic full-snapshot API;
//! individual row writes remain only for explicit coordinator/handoff transitions.

use anyhow::Context;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use sentinel_common::{
    validate_owner_snapshot_pair, LocalOwnerBaseState, LocalOwnerOperationKind, LocalOwnerRole,
    LocalOwnerSagaRole, LocalOwnerSagaState, LocalOwnerState, LocalOwnerStateSnapshot,
    OwnerSnapshotInstallOutcome, OwnerTerm, OwnerTermSnapshot, StateTransferScope,
    TRACK_A_COORDINATOR_GENERATION,
};
use serde::{Deserialize, Serialize};

const CLUSTER_OWNER: TableDefinition<&str, &[u8]> = TableDefinition::new("cluster_owner");
const LOCAL_OWNER: TableDefinition<&str, &[u8]> = TableDefinition::new("local_owner");
const LOCAL_OWNER_SAGA: TableDefinition<&str, &[u8]> = TableDefinition::new("local_owner_saga");
const OWNER_TERM_SNAPSHOT_META: TableDefinition<&str, &[u8]> =
    TableDefinition::new("owner_term_snapshot_meta");
const INSTALL_MARKER_KEY: &str = "installed";

fn scope_key(scope: &StateTransferScope) -> String {
    scope.to_wire()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerSnapshotInstallStatus {
    Installed,
    SnapshotConflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerSnapshotInstallMarker {
    pub schema_version: u32,
    pub recipient_node: sentinel_common::NodeId,
    pub coordinator_generation: u64,
    pub term_snapshot_revision: u64,
    pub global_checksum: [u8; 32],
    pub local_checksum: [u8; 32],
    pub status: OwnerSnapshotInstallStatus,
    #[serde(default)]
    pub conflict_global_checksum: Option<[u8; 32]>,
    #[serde(default)]
    pub conflict_local_checksum: Option<[u8; 32]>,
}

pub type InstallOutcome = OwnerSnapshotInstallOutcome;

pub struct ClusterMetaStore {
    db: Database,
}

impl ClusterMetaStore {
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let db = Database::create(path)
            .map_err(|e| anyhow::anyhow!("ClusterMetaStore open at {path}: {e}"))?;
        let txn = db.begin_write()?;
        {
            txn.open_table(CLUSTER_OWNER)?;
            txn.open_table(LOCAL_OWNER)?;
            txn.open_table(LOCAL_OWNER_SAGA)?;
            txn.open_table(OWNER_TERM_SNAPSHOT_META)?;
        }
        txn.commit()?;
        let store = Self { db };
        store.migrate_legacy_overlays()?;
        Ok(store)
    }

    /// Before the first full snapshot, preserve legacy transition rows as a scope-keyed
    /// reconciliation overlay. Stable Owner/Follower rows are replaced by the seed
    /// snapshot and are intentionally not promoted to authority here.
    fn migrate_legacy_overlays(&self) -> anyhow::Result<()> {
        if self
            .install_marker()?
            .is_some_and(|marker| marker.coordinator_generation != 0)
        {
            return Ok(());
        }
        let legacy_rows = {
            let read = self.db.begin_read()?;
            let table = read.open_table(LOCAL_OWNER)?;
            let mut rows = Vec::new();
            for entry in table.iter()? {
                let (_, value) = entry?;
                if let Ok(state) = serde_json::from_slice::<LocalOwnerState>(value.value()) {
                    rows.push(state);
                }
            }
            rows
        };
        if legacy_rows.is_empty() {
            return Ok(());
        }
        let txn = self.db.begin_write()?;
        {
            let mut sagas = txn.open_table(LOCAL_OWNER_SAGA)?;
            for state in legacy_rows {
                let role = match state.role {
                    LocalOwnerRole::Retiring => LocalOwnerSagaRole::Retiring,
                    LocalOwnerRole::Retired => LocalOwnerSagaRole::Retired,
                    LocalOwnerRole::PreparedTarget => LocalOwnerSagaRole::PreparedTarget,
                    LocalOwnerRole::OwnerActivating => LocalOwnerSagaRole::OwnerActivating,
                    LocalOwnerRole::Owner | LocalOwnerRole::Follower => continue,
                };
                let key = scope_key(&state.scope);
                if sagas.get(key.as_str())?.is_some() {
                    continue;
                }
                let overlay = LocalOwnerSagaState {
                    scope: state.scope.clone(),
                    operation_kind: LocalOwnerOperationKind::LegacyReconciliation,
                    op_id: None,
                    owner_term: OwnerTerm {
                        scope: state.scope,
                        owner_node: state.node_id,
                        epoch: state.epoch,
                        coordinator_generation: 0,
                    },
                    role,
                    transition_seq: 0,
                };
                let bytes = serde_json::to_vec(&overlay)?;
                sagas.insert(key.as_str(), bytes.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    pub fn install_marker(&self) -> anyhow::Result<Option<OwnerSnapshotInstallMarker>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(OWNER_TERM_SNAPSHOT_META)?;
        table
            .get(INSTALL_MARKER_KEY)?
            .map(|value| {
                serde_json::from_slice(value.value()).context("deserialize owner install marker")
            })
            .transpose()
    }

    /// Atomically install a complete global and recipient-local owner snapshot. The
    /// scope-keyed saga table is deliberately not opened by the replacement transaction.
    pub fn install_owner_snapshot(
        &self,
        global: &OwnerTermSnapshot,
        local: &LocalOwnerStateSnapshot,
    ) -> anyhow::Result<InstallOutcome> {
        validate_owner_snapshot_pair(global, local)?;
        if global.coordinator_generation != TRACK_A_COORDINATOR_GENERATION {
            return Ok(InstallOutcome::GenerationMismatch {
                installed_generation: TRACK_A_COORDINATOR_GENERATION,
                received_generation: global.coordinator_generation,
            });
        }

        let txn = self.db.begin_write()?;
        let existing_marker = {
            let table = txn.open_table(OWNER_TERM_SNAPSHOT_META)?;
            let marker = table
                .get(INSTALL_MARKER_KEY)?
                .map(|value| serde_json::from_slice::<OwnerSnapshotInstallMarker>(value.value()))
                .transpose()
                .context("deserialize owner install marker")?;
            marker
        };

        if let Some(mut marker) = existing_marker.clone() {
            // Generation zero is legacy bootstrap metadata, not Track-A authority. It
            // is replaced by the first valid generation-one full snapshot.
            if marker.coordinator_generation != 0 {
                if marker.status == OwnerSnapshotInstallStatus::SnapshotConflict {
                    return Ok(InstallOutcome::SnapshotConflict);
                }
                if marker.coordinator_generation != global.coordinator_generation {
                    return Ok(InstallOutcome::GenerationMismatch {
                        installed_generation: marker.coordinator_generation,
                        received_generation: global.coordinator_generation,
                    });
                }
                if marker.recipient_node != local.recipient_node {
                    anyhow::bail!(
                        "recipient-local owner snapshot cannot change recipient from {} to {}",
                        marker.recipient_node,
                        local.recipient_node
                    );
                }
                if global.term_snapshot_revision < marker.term_snapshot_revision {
                    return Ok(InstallOutcome::StaleSnapshot {
                        installed_revision: marker.term_snapshot_revision,
                        received_revision: global.term_snapshot_revision,
                    });
                }
                if global.term_snapshot_revision == marker.term_snapshot_revision {
                    if global.checksum == marker.global_checksum
                        && local.checksum == marker.local_checksum
                    {
                        return Ok(InstallOutcome::AlreadyInstalled);
                    }
                    marker.status = OwnerSnapshotInstallStatus::SnapshotConflict;
                    marker.conflict_global_checksum = Some(global.checksum);
                    marker.conflict_local_checksum = Some(local.checksum);
                    let bytes = serde_json::to_vec(&marker)?;
                    {
                        let mut table = txn.open_table(OWNER_TERM_SNAPSHOT_META)?;
                        table.insert(INSTALL_MARKER_KEY, bytes.as_slice())?;
                    }
                    txn.commit()?;
                    return Ok(InstallOutcome::SnapshotConflict);
                }
            }
        }

        // Epochs may never move backwards, including across a higher snapshot revision.
        {
            let table = txn.open_table(CLUSTER_OWNER)?;
            for term in &global.sorted_terms {
                if let Some(value) = table.get(scope_key(&term.scope).as_str())? {
                    let old: OwnerTerm =
                        serde_json::from_slice(value.value()).context("deserialize OwnerTerm")?;
                    if old.coordinator_generation == global.coordinator_generation
                        && term.epoch < old.epoch
                    {
                        return Ok(InstallOutcome::StaleSnapshot {
                            installed_revision: existing_marker
                                .as_ref()
                                .map_or(0, |marker| marker.term_snapshot_revision),
                            received_revision: global.term_snapshot_revision,
                        });
                    }
                }
            }
        }

        {
            let mut table = txn.open_table(CLUSTER_OWNER)?;
            for removed in table.extract_if(|_, _| true)? {
                removed?;
            }
            for term in &global.sorted_terms {
                let bytes = serde_json::to_vec(term).context("serialize OwnerTerm")?;
                table.insert(scope_key(&term.scope).as_str(), bytes.as_slice())?;
            }
        }
        {
            let mut table = txn.open_table(LOCAL_OWNER)?;
            for removed in table.extract_if(|_, _| true)? {
                removed?;
            }
            for state in &local.sorted_base_states {
                let bytes = serde_json::to_vec(state).context("serialize LocalOwnerBaseState")?;
                table.insert(scope_key(&state.scope).as_str(), bytes.as_slice())?;
            }
        }
        let marker = OwnerSnapshotInstallMarker {
            schema_version: global.schema_version,
            recipient_node: local.recipient_node,
            coordinator_generation: global.coordinator_generation,
            term_snapshot_revision: global.term_snapshot_revision,
            global_checksum: global.checksum,
            local_checksum: local.checksum,
            status: OwnerSnapshotInstallStatus::Installed,
            conflict_global_checksum: None,
            conflict_local_checksum: None,
        };
        {
            let bytes = serde_json::to_vec(&marker)?;
            let mut table = txn.open_table(OWNER_TERM_SNAPSHOT_META)?;
            table.insert(INSTALL_MARKER_KEY, bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(InstallOutcome::Installed)
    }

    /// Reconstruct the exact installed envelopes for a restart cache rebuild.
    pub fn installed_owner_snapshot(
        &self,
    ) -> anyhow::Result<Option<(OwnerTermSnapshot, LocalOwnerStateSnapshot)>> {
        let Some(marker) = self.install_marker()? else {
            return Ok(None);
        };
        if marker.status == OwnerSnapshotInstallStatus::SnapshotConflict {
            anyhow::bail!("owner snapshot conflict requires manual recovery");
        }
        let global = OwnerTermSnapshot {
            schema_version: marker.schema_version,
            coordinator_generation: marker.coordinator_generation,
            term_snapshot_revision: marker.term_snapshot_revision,
            sorted_terms: self.list_owner_terms()?,
            checksum: marker.global_checksum,
        };
        let local = LocalOwnerStateSnapshot {
            schema_version: marker.schema_version,
            recipient_node: marker.recipient_node,
            coordinator_generation: marker.coordinator_generation,
            term_snapshot_revision: marker.term_snapshot_revision,
            sorted_base_states: self.list_local_base_states()?,
            checksum: marker.local_checksum,
        };
        validate_owner_snapshot_pair(&global, &local)?;
        Ok(Some((global, local)))
    }

    // Legacy-fixture helper only. Bootstrap and replication must use the atomic
    // `install_owner_snapshot` API so global authority, recipient-local state and
    // the install marker can never be observed as a partial update.
    #[cfg(test)]
    fn put_owner_term(&self, term: &OwnerTerm) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(term).context("serialize OwnerTerm")?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(CLUSTER_OWNER)?;
            table.insert(scope_key(&term.scope).as_str(), bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn get_owner_term(&self, scope: &StateTransferScope) -> anyhow::Result<Option<OwnerTerm>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(CLUSTER_OWNER)?;
        table
            .get(scope_key(scope).as_str())?
            .map(|value| serde_json::from_slice(value.value()).context("deserialize OwnerTerm"))
            .transpose()
    }

    pub fn list_owner_terms(&self) -> anyhow::Result<Vec<OwnerTerm>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(CLUSTER_OWNER)?;
        let mut terms = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            terms.push(serde_json::from_slice(value.value()).context("deserialize OwnerTerm")?);
        }
        terms.sort_by_key(|term: &OwnerTerm| term.scope.to_wire());
        Ok(terms)
    }

    pub fn list_local_base_states(&self) -> anyhow::Result<Vec<LocalOwnerBaseState>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(LOCAL_OWNER)?;
        let mut states = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            if let Ok(state) = serde_json::from_slice::<LocalOwnerBaseState>(value.value()) {
                states.push(state);
            }
        }
        states.sort_by_key(|state| state.scope.to_wire());
        Ok(states)
    }

    pub fn put_local_saga_state(&self, state: &LocalOwnerSagaState) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(state).context("serialize LocalOwnerSagaState")?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(LOCAL_OWNER_SAGA)?;
            table.insert(scope_key(&state.scope).as_str(), bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// CAS-style handoff overlay transition. An idempotent replay of the same state is
    /// accepted; a newer authorized counter-handoff may replace an older handoff or
    /// legacy reconciliation state. A concurrent migration or newer term is never
    /// selected arbitrarily.
    pub fn put_handoff_saga_state_cas(&self, state: &LocalOwnerSagaState) -> anyhow::Result<()> {
        if state.operation_kind != LocalOwnerOperationKind::Handoff {
            anyhow::bail!("handoff overlay API requires operation_kind=Handoff");
        }
        let key = scope_key(&state.scope);
        let bytes = serde_json::to_vec(state).context("serialize LocalOwnerSagaState")?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(LOCAL_OWNER_SAGA)?;
            let existing = table
                .get(key.as_str())?
                .map(|value| serde_json::from_slice::<LocalOwnerSagaState>(value.value()))
                .transpose()
                .context("deserialize LocalOwnerSagaState")?;
            if let Some(existing) = existing {
                if &existing == state {
                    return Ok(());
                }
                if existing.operation_kind == LocalOwnerOperationKind::Migration {
                    anyhow::bail!(
                        "ManualRecoveryRequired: migration overlay conflicts with handoff for {}",
                        key
                    );
                }
                let existing_term = (
                    existing.owner_term.coordinator_generation,
                    existing.owner_term.epoch,
                );
                let requested_term = (
                    state.owner_term.coordinator_generation,
                    state.owner_term.epoch,
                );
                if existing_term > requested_term {
                    anyhow::bail!(
                        "ManualRecoveryRequired: newer handoff overlay exists for {}",
                        key
                    );
                }
                if existing_term == requested_term {
                    if existing.op_id != state.op_id {
                        anyhow::bail!(
                            "ManualRecoveryRequired: concurrent handoff overlays for {}",
                            key
                        );
                    }
                    if state.transition_seq <= existing.transition_seq {
                        anyhow::bail!(
                            "ManualRecoveryRequired: conflicting handoff transition for {}",
                            key
                        );
                    }
                }
            }
            table.insert(key.as_str(), bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Complete an authorized ownership-only handoff after the full snapshot carrying
    /// `committed_term` has been installed. General snapshot replication never removes
    /// overlays; this handoff-specific CAS is the only path that may clear an older
    /// handoff/legacy reconciliation overlay for the scope.
    pub fn complete_handoff_overlay(&self, committed_term: &OwnerTerm) -> anyhow::Result<()> {
        let key = scope_key(&committed_term.scope);
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(LOCAL_OWNER_SAGA)?;
            let existing = table
                .get(key.as_str())?
                .map(|value| serde_json::from_slice::<LocalOwnerSagaState>(value.value()))
                .transpose()
                .context("deserialize LocalOwnerSagaState")?;
            if let Some(existing) = existing {
                if existing.operation_kind == LocalOwnerOperationKind::Migration {
                    anyhow::bail!(
                        "ManualRecoveryRequired: migration overlay conflicts with handoff for {}",
                        key
                    );
                }
                if existing.owner_term.coordinator_generation
                    > committed_term.coordinator_generation
                    || (existing.owner_term.coordinator_generation
                        == committed_term.coordinator_generation
                        && existing.owner_term.epoch > committed_term.epoch)
                {
                    anyhow::bail!(
                        "ManualRecoveryRequired: newer owner overlay conflicts with handoff for {}",
                        key
                    );
                }
                table.remove(key.as_str())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    pub fn list_local_saga_states(&self) -> anyhow::Result<Vec<LocalOwnerSagaState>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(LOCAL_OWNER_SAGA)?;
        let mut states = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            states.push(
                serde_json::from_slice(value.value()).context("deserialize LocalOwnerSagaState")?,
            );
        }
        states.sort_by_key(|state: &LocalOwnerSagaState| state.scope.to_wire());
        Ok(states)
    }

    /// Legacy compatibility for the existing handoff while it is moved to the scoped
    /// overlay API in this A5 change.
    pub fn put_local_state(&self, state: &LocalOwnerState) -> anyhow::Result<()> {
        let role = match state.role {
            LocalOwnerRole::Retiring => LocalOwnerSagaRole::Retiring,
            LocalOwnerRole::Retired => LocalOwnerSagaRole::Retired,
            LocalOwnerRole::PreparedTarget => LocalOwnerSagaRole::PreparedTarget,
            LocalOwnerRole::OwnerActivating => LocalOwnerSagaRole::OwnerActivating,
            LocalOwnerRole::Owner | LocalOwnerRole::Follower => {
                anyhow::bail!("stable local owner state must be installed by full snapshot")
            }
        };
        self.put_handoff_saga_state_cas(&LocalOwnerSagaState {
            scope: state.scope.clone(),
            operation_kind: LocalOwnerOperationKind::Handoff,
            op_id: None,
            owner_term: OwnerTerm {
                scope: state.scope.clone(),
                owner_node: state.node_id,
                epoch: state.epoch,
                coordinator_generation: if state.epoch == 0 {
                    0
                } else {
                    TRACK_A_COORDINATOR_GENERATION
                },
            },
            role,
            transition_seq: 0,
        })
    }

    pub fn get_local_state(
        &self,
        scope: &StateTransferScope,
    ) -> anyhow::Result<Option<LocalOwnerState>> {
        Ok(self
            .list_local_saga_states()?
            .into_iter()
            .find(|state| &state.scope == scope)
            .map(|state| LocalOwnerState {
                scope: state.scope,
                node_id: state.owner_term.owner_node,
                epoch: state.owner_term.epoch,
                role: match state.role {
                    LocalOwnerSagaRole::Retiring => LocalOwnerRole::Retiring,
                    LocalOwnerSagaRole::Retired => LocalOwnerRole::Retired,
                    LocalOwnerSagaRole::PreparedTarget => LocalOwnerRole::PreparedTarget,
                    LocalOwnerSagaRole::OwnerActivating => LocalOwnerRole::OwnerActivating,
                },
            }))
    }

    pub fn list_local_states(&self) -> anyhow::Result<Vec<LocalOwnerState>> {
        self.list_local_saga_states().map(|states| {
            states
                .into_iter()
                .map(|state| LocalOwnerState {
                    scope: state.scope,
                    node_id: state.owner_term.owner_node,
                    epoch: state.owner_term.epoch,
                    role: match state.role {
                        LocalOwnerSagaRole::Retiring => LocalOwnerRole::Retiring,
                        LocalOwnerSagaRole::Retired => LocalOwnerRole::Retired,
                        LocalOwnerSagaRole::PreparedTarget => LocalOwnerRole::PreparedTarget,
                        LocalOwnerSagaRole::OwnerActivating => LocalOwnerRole::OwnerActivating,
                    },
                })
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::{
        ActivationState, LocalOwnerBaseRole, LocalOwnerBaseState, NodeId,
        OWNER_SNAPSHOT_SCHEMA_VERSION,
    };
    use sha2::Digest;

    fn node(byte: u8) -> NodeId {
        NodeId(uuid::Uuid::from_bytes([byte; 16]))
    }

    fn pair(
        recipient: NodeId,
        revision: u64,
        scopes: &[StateTransferScope],
    ) -> (OwnerTermSnapshot, LocalOwnerStateSnapshot) {
        let terms: Vec<_> = scopes
            .iter()
            .cloned()
            .map(|scope| OwnerTerm {
                scope,
                owner_node: recipient,
                epoch: 1,
                coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
            })
            .collect();
        let global =
            OwnerTermSnapshot::new(TRACK_A_COORDINATOR_GENERATION, revision, terms.clone())
                .unwrap();
        let local = LocalOwnerStateSnapshot::new(
            recipient,
            TRACK_A_COORDINATOR_GENERATION,
            revision,
            terms
                .into_iter()
                .map(|owner_term| LocalOwnerBaseState {
                    scope: owner_term.scope.clone(),
                    recipient_node: recipient,
                    owner_term,
                    base_role: LocalOwnerBaseRole::Owner,
                    activation_state: ActivationState::Routable,
                })
                .collect(),
        )
        .unwrap();
        (global, local)
    }

    #[test]
    fn full_install_replaces_rows_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cluster_meta.redb");
        let recipient = node(1);
        let first = pair(
            recipient,
            1,
            &[
                StateTransferScope::World,
                StateTransferScope::for_agent("AGENT-01"),
            ],
        );
        let second = pair(recipient, 2, &[StateTransferScope::World]);
        {
            let store = ClusterMetaStore::open(path.to_str().unwrap()).unwrap();
            assert_eq!(
                store.install_owner_snapshot(&first.0, &first.1).unwrap(),
                InstallOutcome::Installed
            );
            assert_eq!(
                store.install_owner_snapshot(&first.0, &first.1).unwrap(),
                InstallOutcome::AlreadyInstalled
            );
            assert_eq!(
                store.install_owner_snapshot(&second.0, &second.1).unwrap(),
                InstallOutcome::Installed
            );
            assert_eq!(store.list_owner_terms().unwrap().len(), 1);
            assert_eq!(store.list_local_base_states().unwrap().len(), 1);
        }
        let reopened = ClusterMetaStore::open(path.to_str().unwrap()).unwrap();
        let installed = reopened.installed_owner_snapshot().unwrap().unwrap();
        assert_eq!(installed, second);
        assert_eq!(installed.0.schema_version, OWNER_SNAPSHOT_SCHEMA_VERSION);
    }

    #[test]
    fn stale_and_conflicting_snapshots_have_typed_outcomes() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            ClusterMetaStore::open(dir.path().join("cluster_meta.redb").to_str().unwrap()).unwrap();
        let recipient = node(1);
        let current = pair(recipient, 2, &[StateTransferScope::World]);
        let stale = pair(recipient, 1, &[StateTransferScope::World]);
        store
            .install_owner_snapshot(&current.0, &current.1)
            .unwrap();
        assert!(matches!(
            store.install_owner_snapshot(&stale.0, &stale.1).unwrap(),
            InstallOutcome::StaleSnapshot { .. }
        ));

        let mut conflict = current.1.clone();
        conflict.sorted_base_states[0].activation_state = ActivationState::NotRoutable;
        conflict.checksum = sha2::Sha256::digest(conflict.canonical_payload().unwrap()).into();
        assert_eq!(
            store.install_owner_snapshot(&current.0, &conflict).unwrap(),
            InstallOutcome::SnapshotConflict
        );
        assert_eq!(
            store
                .install_owner_snapshot(&current.0, &current.1)
                .unwrap(),
            InstallOutcome::SnapshotConflict
        );
    }

    #[test]
    fn full_install_preserves_legacy_transition_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cluster_meta.redb");
        let recipient = node(1);
        let store = ClusterMetaStore::open(path.to_str().unwrap()).unwrap();
        let overlay = LocalOwnerSagaState {
            scope: StateTransferScope::for_agent("AGENT-07"),
            operation_kind: LocalOwnerOperationKind::LegacyReconciliation,
            op_id: None,
            owner_term: OwnerTerm {
                scope: StateTransferScope::for_agent("AGENT-07"),
                owner_node: recipient,
                epoch: 1,
                coordinator_generation: 0,
            },
            role: LocalOwnerSagaRole::Retired,
            transition_seq: 0,
        };
        store.put_local_saga_state(&overlay).unwrap();
        let snapshot = pair(recipient, 1, &[StateTransferScope::World]);
        store
            .install_owner_snapshot(&snapshot.0, &snapshot.1)
            .unwrap();
        assert_eq!(store.list_local_saga_states().unwrap(), vec![overlay]);
    }

    #[test]
    fn first_full_install_replaces_generation_zero_and_migrates_legacy_role() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cluster_meta.redb");
        let recipient = node(1);
        let scope = StateTransferScope::for_agent("AGENT-07");
        {
            let store = ClusterMetaStore::open(path.to_str().unwrap()).unwrap();
            store
                .put_owner_term(&OwnerTerm {
                    scope: scope.clone(),
                    owner_node: recipient,
                    epoch: 8,
                    coordinator_generation: 0,
                })
                .unwrap();
            let legacy = LocalOwnerState {
                scope: scope.clone(),
                node_id: recipient,
                epoch: 8,
                role: LocalOwnerRole::Retired,
            };
            let bytes = serde_json::to_vec(&legacy).unwrap();
            let txn = store.db.begin_write().unwrap();
            {
                let mut table = txn.open_table(LOCAL_OWNER).unwrap();
                table
                    .insert(scope_key(&scope).as_str(), bytes.as_slice())
                    .unwrap();
            }
            txn.commit().unwrap();
        }

        let store = ClusterMetaStore::open(path.to_str().unwrap()).unwrap();
        let overlay = store.list_local_saga_states().unwrap().pop().unwrap();
        assert_eq!(
            overlay.operation_kind,
            LocalOwnerOperationKind::LegacyReconciliation
        );
        assert_eq!(overlay.role, LocalOwnerSagaRole::Retired);
        let current = pair(recipient, 1, &[StateTransferScope::World]);
        assert_eq!(
            store
                .install_owner_snapshot(&current.0, &current.1)
                .unwrap(),
            InstallOutcome::Installed
        );
        assert_eq!(store.list_owner_terms().unwrap(), current.0.sorted_terms);
        assert_eq!(store.list_local_saga_states().unwrap(), vec![overlay]);
    }

    #[test]
    fn generation_zero_install_marker_is_replaced_by_first_track_a_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cluster_meta.redb");
        let store = ClusterMetaStore::open(path.to_str().unwrap()).unwrap();
        let recipient = node(1);
        let legacy_scope = StateTransferScope::for_agent("AGENT-07");
        let legacy_state = LocalOwnerState {
            scope: legacy_scope.clone(),
            node_id: recipient,
            epoch: 7,
            role: LocalOwnerRole::Retired,
        };
        let legacy_marker = OwnerSnapshotInstallMarker {
            schema_version: OWNER_SNAPSHOT_SCHEMA_VERSION,
            recipient_node: recipient,
            coordinator_generation: 0,
            term_snapshot_revision: 99,
            global_checksum: [1; 32],
            local_checksum: [2; 32],
            status: OwnerSnapshotInstallStatus::Installed,
            conflict_global_checksum: None,
            conflict_local_checksum: None,
        };
        let bytes = serde_json::to_vec(&legacy_marker).unwrap();
        let legacy_state_bytes = serde_json::to_vec(&legacy_state).unwrap();
        let txn = store.db.begin_write().unwrap();
        {
            let mut table = txn.open_table(OWNER_TERM_SNAPSHOT_META).unwrap();
            table.insert(INSTALL_MARKER_KEY, bytes.as_slice()).unwrap();
        }
        {
            let mut table = txn.open_table(LOCAL_OWNER).unwrap();
            table
                .insert(
                    scope_key(&legacy_scope).as_str(),
                    legacy_state_bytes.as_slice(),
                )
                .unwrap();
        }
        txn.commit().unwrap();
        drop(store);

        let store = ClusterMetaStore::open(path.to_str().unwrap()).unwrap();
        let overlay = store.list_local_saga_states().unwrap().pop().unwrap();
        assert_eq!(
            overlay.operation_kind,
            LocalOwnerOperationKind::LegacyReconciliation
        );
        assert_eq!(overlay.role, LocalOwnerSagaRole::Retired);

        let current = pair(recipient, 1, &[StateTransferScope::World]);
        assert_eq!(
            store
                .install_owner_snapshot(&current.0, &current.1)
                .unwrap(),
            InstallOutcome::Installed
        );
        let installed = store.install_marker().unwrap().unwrap();
        assert_eq!(
            installed.coordinator_generation,
            TRACK_A_COORDINATOR_GENERATION
        );
        assert_eq!(installed.term_snapshot_revision, 1);
        assert_eq!(store.list_local_saga_states().unwrap(), vec![overlay]);
    }

    #[test]
    fn handoff_overlay_cas_rejects_concurrency_and_allows_newer_counter_handoff() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            ClusterMetaStore::open(dir.path().join("cluster_meta.redb").to_str().unwrap()).unwrap();
        let scope = StateTransferScope::for_agent("AGENT-07");
        let op1 = uuid::Uuid::from_bytes([1; 16]);
        let op2 = uuid::Uuid::from_bytes([2; 16]);
        let state = LocalOwnerSagaState {
            scope: scope.clone(),
            operation_kind: LocalOwnerOperationKind::Handoff,
            op_id: Some(op1),
            owner_term: OwnerTerm {
                scope: scope.clone(),
                owner_node: node(1),
                epoch: 1,
                coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
            },
            role: LocalOwnerSagaRole::Retiring,
            transition_seq: 1,
        };
        store.put_handoff_saga_state_cas(&state).unwrap();

        let mut advanced = state.clone();
        advanced.role = LocalOwnerSagaRole::Retired;
        advanced.transition_seq = 2;
        store.put_handoff_saga_state_cas(&advanced).unwrap();

        let mut concurrent = advanced.clone();
        concurrent.op_id = Some(op2);
        assert!(store.put_handoff_saga_state_cas(&concurrent).is_err());

        let mut counter = concurrent;
        counter.owner_term.epoch = 2;
        counter.transition_seq = 1;
        store.put_handoff_saga_state_cas(&counter).unwrap();
        assert_eq!(store.list_local_saga_states().unwrap(), vec![counter]);
    }

    #[test]
    fn track_a_rejects_other_coordinator_generation_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            ClusterMetaStore::open(dir.path().join("cluster_meta.redb").to_str().unwrap()).unwrap();
        let recipient = node(1);
        let term = OwnerTerm {
            scope: StateTransferScope::World,
            owner_node: recipient,
            epoch: 1,
            coordinator_generation: 2,
        };
        let global = OwnerTermSnapshot::new(2, 1, vec![term.clone()]).unwrap();
        let local = LocalOwnerStateSnapshot::new(
            recipient,
            2,
            1,
            vec![LocalOwnerBaseState {
                scope: term.scope.clone(),
                recipient_node: recipient,
                owner_term: term,
                base_role: LocalOwnerBaseRole::Owner,
                activation_state: ActivationState::Routable,
            }],
        )
        .unwrap();
        assert!(matches!(
            store.install_owner_snapshot(&global, &local).unwrap(),
            InstallOutcome::GenerationMismatch {
                installed_generation: TRACK_A_COORDINATOR_GENERATION,
                received_generation: 2
            }
        ));
        assert!(store.install_marker().unwrap().is_none());
        assert!(store.list_owner_terms().unwrap().is_empty());
    }
}
