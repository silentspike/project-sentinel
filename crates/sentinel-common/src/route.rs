//! #497 / G-ROUTE — agent locator (`RouteRegistry`), V12 reference-integrity.
//!
//! Cross-agent (and later cross-node) references resolve by `agent_id` → location, NEVER by a
//! cached local `EntityId`. After a per-container restore the agent's entity gets a new `EntityId`;
//! the `RouteRegistry` keeps `agent_id` mapped to its node + owner-epoch, so a holder resolves the
//! *current* location and looks the entity up fresh by `agent_id`. This is also the cross-node hook
//! for #501 (on migration `agent_id` is re-registered to the target node, state `Remote`).

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::cluster::NodeId;

/// Where a container's owner currently lives, from the resolving node's view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteState {
    /// Owned and routable on this node — resolve the live entity locally by `agent_id`.
    Local,
    /// Mid-migration: route is quiescing (V17), holders must not assume a stable entity yet.
    Migrating,
    /// Owned on another node — resolve cross-node (#501), never via a local entity.
    Remote,
}

/// One agent's route: which node owns it, under which owner-epoch, and its route state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteEntry {
    pub node_id: NodeId,
    pub owner_epoch: u64,
    pub state: RouteState,
}

/// Maps `agent_id` → [`RouteEntry`]. The single place cross-agent/cross-node references are
/// resolved — callers never hold a foreign `EntityId`, so a per-container despawn+respawn (#497)
/// cannot leave a stale reference.
#[derive(Default)]
pub struct RouteRegistry {
    routes: RwLock<HashMap<u16, RouteEntry>>,
}

static GLOBAL: OnceLock<RouteRegistry> = OnceLock::new();

impl RouteRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process-global registry (mirrors `OwnerRegistry::global`).
    pub fn global() -> &'static RouteRegistry {
        GLOBAL.get_or_init(RouteRegistry::new)
    }

    /// Register/overwrite an agent's route. Called on spawn, `OwnerCommit`, and `RouteSwitch`.
    pub fn register(&self, agent_id: u16, node_id: NodeId, owner_epoch: u64, state: RouteState) {
        self.routes.write().unwrap().insert(
            agent_id,
            RouteEntry {
                node_id,
                owner_epoch,
                state,
            },
        );
    }

    /// Resolve an agent's current route by `agent_id` (never by `EntityId`). `None` = unknown agent.
    pub fn resolve(&self, agent_id: u16) -> Option<RouteEntry> {
        self.routes.read().unwrap().get(&agent_id).cloned()
    }

    /// Update only the route state (Local→Migrating on `PrepareHandoff`, →Remote on `RouteSwitch`).
    /// Returns `false` if the agent is unknown.
    pub fn set_route_state(&self, agent_id: u16, state: RouteState) -> bool {
        match self.routes.write().unwrap().get_mut(&agent_id) {
            Some(e) => {
                e.state = state;
                true
            }
            None => false,
        }
    }

    /// Cache-invalidate one agent (on despawn/decommission). Returns the removed entry, if any.
    pub fn invalidate(&self, agent_id: u16) -> Option<RouteEntry> {
        self.routes.write().unwrap().remove(&agent_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> NodeId {
        NodeId(uuid::Uuid::nil())
    }

    #[test]
    fn resolve_register_invalidate() {
        let reg = RouteRegistry::new();
        assert!(reg.resolve(1).is_none(), "unknown agent resolves to None");

        reg.register(1, node(), 7, RouteState::Local);
        let e = reg.resolve(1).expect("registered");
        assert_eq!(e.owner_epoch, 7);
        assert_eq!(e.state, RouteState::Local);

        // State transition (e.g. PrepareHandoff): Local -> Migrating.
        assert!(reg.set_route_state(1, RouteState::Migrating));
        assert_eq!(reg.resolve(1).unwrap().state, RouteState::Migrating);
        assert!(
            !reg.set_route_state(99, RouteState::Migrating),
            "unknown agent"
        );

        // Invalidate removes the route.
        assert!(reg.invalidate(1).is_some());
        assert!(reg.resolve(1).is_none());
    }
}
