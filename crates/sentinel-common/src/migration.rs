//! #497 — bounded migration class: typed eligibility, never a silent skip.
//!
//! A per-container migration (#501) is only valid for a RESTING container. Anything that would make
//! the snapshot torn or the move lossy is a TYPED rejection — V11 active inbound, V23 active
//! scheduled work, V30 a pending external side-effect — never a silent skip, so the operator/saga
//! sees exactly why a container was held back.

use std::fmt;

/// Whether a container may be per-container migrated, and if not, the typed reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationEligibility {
    /// Resting container — migratable.
    Eligible,
    /// Not migratable; carries the typed reason (never an unexplained skip).
    NotMigratable(NotMigratableReason),
}

impl MigrationEligibility {
    pub fn is_eligible(self) -> bool {
        matches!(self, MigrationEligibility::Eligible)
    }
}

/// Why a container is not migratable. Each variant maps to a bounded-class exclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotMigratableReason {
    /// V11 — active inbound cross-agent traffic (chat directed at the agent). The durable inbound
    /// queue + dedup is Track E/H; Track A excludes it rather than claiming queue semantics.
    ActiveInbound,
    /// V23 — active scheduled work (a task assigned to the agent is not `Done`).
    ScheduledWorkActive,
    /// V30 — a pending external side-effect (an active Voice-of-Gaia thought / delayed impulse).
    PendingSideEffect,
    /// The container is not spawned on this node.
    UnknownAgent,
    /// The local node is not ready/authorized to read the container under V19.
    OwnerFenceRejected,
}

impl fmt::Display for NotMigratableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            NotMigratableReason::ActiveInbound => "active inbound cross-agent traffic (V11)",
            NotMigratableReason::ScheduledWorkActive => "active scheduled work (V23)",
            NotMigratableReason::PendingSideEffect => "pending external side-effect (V30)",
            NotMigratableReason::UnknownAgent => "agent not spawned on this node",
            NotMigratableReason::OwnerFenceRejected => "owner fence rejected the container scope",
        };
        f.write_str(s)
    }
}

/// #497 G-EVENTHIST — per-agent time-travel / arbitrary-restore is NOT supported after a container
/// has been migrated across nodes (its event-log slice is stranded on the source node). A typed
/// error, never a silent skip; full continuity (event-slice transfer + retention pin) is Track E/H.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotSupportedForMigratedContainer {
    pub agent_id: u16,
    pub operation: &'static str,
}

impl fmt::Display for NotSupportedForMigratedContainer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} is not supported for migrated container agent {} (event history is on the source node; Track E/H)",
            self.operation, self.agent_id
        )
    }
}

impl std::error::Error for NotSupportedForMigratedContainer {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_are_typed_and_never_silent() {
        // Every rejection carries a human-readable, typed reason — there is no silent skip path.
        for r in [
            NotMigratableReason::ActiveInbound,
            NotMigratableReason::ScheduledWorkActive,
            NotMigratableReason::PendingSideEffect,
            NotMigratableReason::UnknownAgent,
            NotMigratableReason::OwnerFenceRejected,
        ] {
            assert!(!r.to_string().is_empty());
            assert!(!MigrationEligibility::NotMigratable(r).is_eligible());
        }
        assert!(MigrationEligibility::Eligible.is_eligible());
    }

    #[test]
    fn migrated_container_time_travel_is_typed_error() {
        let e = NotSupportedForMigratedContainer {
            agent_id: 7,
            operation: "arbitrary-restore",
        };
        assert!(e.to_string().contains("agent 7"));
        // It is a real std::error::Error, not a bool/None.
        let _: &dyn std::error::Error = &e;
    }
}
