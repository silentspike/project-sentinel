//! Zenoh topic hierarchy for Project Sentinel.
//!
//! Topics follow A2A-compatible naming (Google/Linux Foundation Agent2Agent Protocol).
//! Hierarchical structure: sentinel/{domain}/{entity}/{channel}

/// Topic prefix for all sentinel messages
pub const PREFIX: &str = "sentinel";

/// Build topic for agent action channel
pub fn agent_action(name: &str) -> String {
    format!("{PREFIX}/agent/{name}/action")
}

/// Build topic for agent perception channel
pub fn agent_perception(name: &str) -> String {
    format!("{PREFIX}/agent/{name}/perception")
}

/// Build topic for agent state updates
pub fn agent_state(name: &str) -> String {
    format!("{PREFIX}/agent/{name}/state")
}

/// Build topic for room audio events
pub fn room_audio(room_id: &str) -> String {
    format!("{PREFIX}/room/{room_id}/audio")
}

/// Build topic for room smell events
pub fn room_smell(room_id: &str) -> String {
    format!("{PREFIX}/room/{room_id}/smell")
}

/// Build topic for room presence changes
pub fn room_presence(room_id: &str) -> String {
    format!("{PREFIX}/room/{room_id}/presence")
}

/// Build topic for global simulation tick
pub fn physics_tick(tick_number: u64) -> String {
    format!("{PREFIX}/physics/tick/{tick_number}")
}

/// Topic for chaos monkey events
pub const CHAOS_EVENT: &str = "sentinel/chaos/event";

/// Topic for sentinel judge alerts.
///
/// ADR-001: Judge publishes alerts via NATS (`sentinel.judge.alert.*`), not Zenoh.
/// This constant is retained for type-signature compatibility; no active subscribers.
#[deprecated(note = "ADR-001: Judge alerts flow via NATS, not Zenoh")]
pub const JUDGE_ALERT: &str = "sentinel/judge/alert";

/// Build topic for agent PSI (Pressure Stall Information) metrics
pub fn agent_psi(name: &str) -> String {
    format!("{PREFIX}/agent/{name}/psi")
}

/// Build topic for cortex gateway injection per agent
pub fn cortex_inject(name: &str) -> String {
    format!("{PREFIX}/cortex/inject/{name}")
}

/// Topic for model swap requests (invisible to agents).
///
/// ADR-001: Model-swap flows via NATS alert (type: "swap") + Daemon HTTP to Gateway.
/// This constant is retained for type-signature compatibility; no active subscribers.
#[deprecated(note = "ADR-001: Model-swap flows via NATS + HTTP, not Zenoh")]
pub const MODEL_SWAP: &str = "sentinel/meta/model-swap";

/// Topic for eBPF agent health metrics.
pub const EBPF_AGENT_HEALTH: &str = "sentinel/ebpf/agent-health";

/// Topic for eBPF I/O profiling metrics.
pub const EBPF_IO_PROFILE: &str = "sentinel/ebpf/io-profile";

/// Topic for eBPF network monitoring metrics.
pub const EBPF_NETWORK: &str = "sentinel/ebpf/network";

/// Topic for eBPF PSI (Pressure Stall Information) metrics.
pub const EBPF_PSI: &str = "sentinel/ebpf/psi";

/// Topic for eBPF monitoring mode status.
pub const EBPF_STATUS: &str = "sentinel/ebpf/status";

/// Build topic for scoped query requests per agent.
pub fn query_request_agent(name: &str) -> String {
    format!("{PREFIX}/query/agent/{name}/request")
}

/// Build topic for scoped query requests per room.
pub fn query_request_room(room_id: &str) -> String {
    format!("{PREFIX}/query/room/{room_id}/request")
}

/// Topic for global scoped query requests.
pub const QUERY_REQUEST_GLOBAL: &str = "sentinel/query/global/request";

/// Build topic for query responses per agent.
pub fn query_response_agent(name: &str) -> String {
    format!("{PREFIX}/query/response/{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_topics() {
        assert_eq!(agent_action("thomas"), "sentinel/agent/thomas/action");
        assert_eq!(agent_perception("lisa"), "sentinel/agent/lisa/perception");
        assert_eq!(agent_state("andreas"), "sentinel/agent/andreas/state");
    }

    #[test]
    fn test_room_topics() {
        assert_eq!(room_audio("kueche"), "sentinel/room/kueche/audio");
        assert_eq!(room_smell("lobby"), "sentinel/room/lobby/smell");
        assert_eq!(
            room_presence("grossraum"),
            "sentinel/room/grossraum/presence"
        );
    }

    #[test]
    fn test_physics_tick() {
        assert_eq!(physics_tick(0), "sentinel/physics/tick/0");
        assert_eq!(physics_tick(42), "sentinel/physics/tick/42");
        assert_eq!(
            physics_tick(u64::MAX),
            format!("sentinel/physics/tick/{}", u64::MAX)
        );
    }

    #[test]
    fn test_agent_psi() {
        assert_eq!(agent_psi("thomas"), "sentinel/agent/thomas/psi");
        assert_eq!(agent_psi("lisa"), "sentinel/agent/lisa/psi");
    }

    #[test]
    fn test_cortex_inject() {
        assert_eq!(cortex_inject("thomas"), "sentinel/cortex/inject/thomas");
    }

    #[test]
    #[allow(deprecated)]
    fn test_constants() {
        assert_eq!(CHAOS_EVENT, "sentinel/chaos/event");
        assert_eq!(JUDGE_ALERT, "sentinel/judge/alert");
        assert_eq!(MODEL_SWAP, "sentinel/meta/model-swap");
        assert_eq!(QUERY_REQUEST_GLOBAL, "sentinel/query/global/request");
        assert_eq!(PREFIX, "sentinel");
    }

    #[test]
    fn test_ebpf_topics() {
        assert_eq!(EBPF_AGENT_HEALTH, "sentinel/ebpf/agent-health");
        assert_eq!(EBPF_IO_PROFILE, "sentinel/ebpf/io-profile");
        assert_eq!(EBPF_NETWORK, "sentinel/ebpf/network");
        assert_eq!(EBPF_PSI, "sentinel/ebpf/psi");
        assert_eq!(EBPF_STATUS, "sentinel/ebpf/status");
    }

    #[test]
    fn test_query_topics() {
        assert_eq!(
            query_request_agent("thomas"),
            "sentinel/query/agent/thomas/request"
        );
        assert_eq!(
            query_request_room("kueche"),
            "sentinel/query/room/kueche/request"
        );
        assert_eq!(
            query_response_agent("thomas"),
            "sentinel/query/response/thomas"
        );
    }
}
