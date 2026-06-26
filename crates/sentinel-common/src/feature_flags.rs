//! Runtime Feature Flags via SENTINEL_* ENV-Variablen (Issue #233).
//!
//! Alle Flags defaulten zu `true` (Feature aktiv) ausser explizit anders dokumentiert.
//! Deaktivierung: `SENTINEL_FLAG_NAME=false` oder `SENTINEL_FLAG_NAME=0`.
//! Flags werden EINMAL beim Start gelesen (kein Hot-Path-Overhead).

use std::sync::OnceLock;
use tracing::warn;

/// Parsed runtime feature flags. Initialized once at startup.
#[derive(Debug, Clone)]
pub struct RuntimeFlags {
    pub chaos_enabled: bool,
    pub nightrun_enabled: bool,
    pub controlplane_enabled: bool,
    pub cloud_enabled: bool,
    pub sleep_cycle: bool,
    pub event_sourcing: bool,
    pub cqrs: bool,
    pub cortex_cb_enabled: bool,
    pub ebpf_enabled: bool,
    pub storage_ingest_enabled: bool,
    pub storage_chunk_cas: bool,
    pub platform_controlplane_enabled: bool,
    /// #497 (#8): Strangler gate for the per-container transfer path. Default OFF so the single-node
    /// prod path is unchanged until explicitly enabled on a cluster node.
    pub per_container_transfer_enabled: bool,
}

static FLAGS: OnceLock<RuntimeFlags> = OnceLock::new();

impl RuntimeFlags {
    /// Parses all SENTINEL_* ENV-Variablen. Call once at startup.
    pub fn init() -> &'static RuntimeFlags {
        FLAGS.get_or_init(|| {
            let flags = RuntimeFlags {
                chaos_enabled: env_flag("SENTINEL_CHAOS_ENABLED", true),
                nightrun_enabled: env_flag("SENTINEL_NIGHTRUN_ENABLED", true),
                controlplane_enabled: env_flag("SENTINEL_CONTROLPLANE_ENABLED", true),
                cloud_enabled: env_flag("SENTINEL_CLOUD_ENABLED", true),
                sleep_cycle: env_flag("SENTINEL_SLEEP_CYCLE", true),
                event_sourcing: env_flag("SENTINEL_EVENT_SOURCING", true),
                cqrs: env_flag("SENTINEL_CQRS", true),
                cortex_cb_enabled: env_flag("SENTINEL_CORTEX_CB_ENABLED", true),
                ebpf_enabled: env_flag("SENTINEL_EBPF_ENABLED", true),
                storage_ingest_enabled: env_flag("SENTINEL_STORAGE_INGEST_ENABLED", true),
                storage_chunk_cas: env_flag("SENTINEL_STORAGE_CHUNK_CAS", true),
                platform_controlplane_enabled: env_flag(
                    "SENTINEL_PLATFORM_CONTROLPLANE_ENABLED",
                    true,
                ),
                // #497: default OFF (Strangler) — the per-container transfer path stays inert in prod.
                per_container_transfer_enabled: env_flag(
                    "SENTINEL_PER_CONTAINER_TRANSFER_ENABLED",
                    false,
                ),
            };

            // Log disabled features at WARN level (AC-7)
            log_disabled("SENTINEL_CHAOS_ENABLED", flags.chaos_enabled);
            log_disabled("SENTINEL_NIGHTRUN_ENABLED", flags.nightrun_enabled);
            log_disabled("SENTINEL_CONTROLPLANE_ENABLED", flags.controlplane_enabled);
            log_disabled("SENTINEL_CLOUD_ENABLED", flags.cloud_enabled);
            log_disabled("SENTINEL_SLEEP_CYCLE", flags.sleep_cycle);
            log_disabled("SENTINEL_EVENT_SOURCING", flags.event_sourcing);
            log_disabled("SENTINEL_CQRS", flags.cqrs);
            log_disabled("SENTINEL_CORTEX_CB_ENABLED", flags.cortex_cb_enabled);
            log_disabled("SENTINEL_EBPF_ENABLED", flags.ebpf_enabled);
            log_disabled(
                "SENTINEL_STORAGE_INGEST_ENABLED",
                flags.storage_ingest_enabled,
            );
            log_disabled("SENTINEL_STORAGE_CHUNK_CAS", flags.storage_chunk_cas);

            flags
        })
    }

    /// Returns the global flags. Auto-initializes with defaults if init() was not called.
    pub fn global() -> &'static RuntimeFlags {
        FLAGS.get_or_init(|| RuntimeFlags {
            chaos_enabled: true,
            nightrun_enabled: true,
            controlplane_enabled: true,
            cloud_enabled: true,
            sleep_cycle: true,
            event_sourcing: true,
            cqrs: true,
            cortex_cb_enabled: true,
            ebpf_enabled: true,
            storage_ingest_enabled: true,
            storage_chunk_cas: true,
            platform_controlplane_enabled: true,
            // #497 (#8): per-container transfer defaults OFF (Strangler), unlike the others.
            per_container_transfer_enabled: false,
        })
    }
}

/// Reads an ENV variable as boolean. Default if not set.
/// Accepts: "false", "0", "no", "off" as false. Everything else = true.
fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(val) => !matches!(val.to_lowercase().as_str(), "false" | "0" | "no" | "off"),
        Err(_) => default,
    }
}

fn log_disabled(name: &str, enabled: bool) {
    if !enabled {
        warn!(flag = name, "Feature deaktiviert via ENV");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_flag_default_true() {
        // Unset ENV → default
        assert!(env_flag("SENTINEL_TEST_NONEXISTENT_FLAG_XYZ", true));
        assert!(!env_flag("SENTINEL_TEST_NONEXISTENT_FLAG_XYZ", false));
    }

    #[test]
    fn env_flag_false_values() {
        std::env::set_var("SENTINEL_TEST_FLAG_A", "false");
        assert!(!env_flag("SENTINEL_TEST_FLAG_A", true));

        std::env::set_var("SENTINEL_TEST_FLAG_B", "0");
        assert!(!env_flag("SENTINEL_TEST_FLAG_B", true));

        std::env::set_var("SENTINEL_TEST_FLAG_C", "no");
        assert!(!env_flag("SENTINEL_TEST_FLAG_C", true));

        std::env::set_var("SENTINEL_TEST_FLAG_D", "off");
        assert!(!env_flag("SENTINEL_TEST_FLAG_D", true));

        // Cleanup
        std::env::remove_var("SENTINEL_TEST_FLAG_A");
        std::env::remove_var("SENTINEL_TEST_FLAG_B");
        std::env::remove_var("SENTINEL_TEST_FLAG_C");
        std::env::remove_var("SENTINEL_TEST_FLAG_D");
    }

    #[test]
    fn env_flag_true_values() {
        std::env::set_var("SENTINEL_TEST_FLAG_E", "true");
        assert!(env_flag("SENTINEL_TEST_FLAG_E", false));

        std::env::set_var("SENTINEL_TEST_FLAG_F", "1");
        assert!(env_flag("SENTINEL_TEST_FLAG_F", false));

        std::env::set_var("SENTINEL_TEST_FLAG_G", "yes");
        assert!(env_flag("SENTINEL_TEST_FLAG_G", false));

        // Cleanup
        std::env::remove_var("SENTINEL_TEST_FLAG_E");
        std::env::remove_var("SENTINEL_TEST_FLAG_F");
        std::env::remove_var("SENTINEL_TEST_FLAG_G");
    }
}
