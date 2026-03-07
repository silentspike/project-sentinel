//! Echo-Plugin: minimales sentinel-tool Component fuer Tests.
//!
//! Implementiert die `sentinel-tool` World aus `wit/world.wit`.
//! Wird als `.wasm` Component kompiliert und als Test-Fixture genutzt.

wit_bindgen::generate!({
    world: "sentinel-tool",
    path: "wit/",
});

struct EchoPlugin;

impl Guest for EchoPlugin {
    fn execute(input: String) -> Result<String, String> {
        // Rufe Host-APIs auf um den Roundtrip zu testen.
        let agent = sentinel::plugin::host_api::get_agent_info();
        let tick = sentinel::plugin::host_api::get_tick();

        sentinel::plugin::host_api::log(
            sentinel::plugin::types::LogLevel::Info,
            &format!("Echo plugin called by {} at tick {}", agent.name, tick),
        );

        if input.is_empty() {
            return Err("empty input".to_string());
        }

        Ok(format!("echo: {}", input))
    }

    fn tool_name() -> String {
        "echo".to_string()
    }

    fn tool_description() -> String {
        "Echoes input back, calling host-api for agent info and logging".to_string()
    }
}

export!(EchoPlugin);
