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

/// Topic for sentinel judge alerts
pub const JUDGE_ALERT: &str = "sentinel/judge/alert";

/// Build topic for cortex gateway injection per agent
pub fn cortex_inject(name: &str) -> String {
    format!("{PREFIX}/cortex/inject/{name}")
}

/// Topic for model swap requests (invisible to agents)
pub const MODEL_SWAP: &str = "sentinel/meta/model-swap";

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
    fn test_cortex_inject() {
        assert_eq!(cortex_inject("thomas"), "sentinel/cortex/inject/thomas");
    }

    #[test]
    fn test_constants() {
        assert_eq!(CHAOS_EVENT, "sentinel/chaos/event");
        assert_eq!(JUDGE_ALERT, "sentinel/judge/alert");
        assert_eq!(MODEL_SWAP, "sentinel/meta/model-swap");
        assert_eq!(PREFIX, "sentinel");
    }
}
