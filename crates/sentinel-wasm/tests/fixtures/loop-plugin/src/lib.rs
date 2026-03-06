//! Infinite-loop Plugin: testet Fuel-Exhaustion.
//!
//! execute() laeuft endlos — muss durch wasmtime Fuel-Limit gestoppt werden.

wit_bindgen::generate!({
    world: "sentinel-tool",
    path: "wit/",
});

struct LoopPlugin;

impl Guest for LoopPlugin {
    fn execute(_input: String) -> Result<String, String> {
        let mut i: u64 = 0;
        loop {
            i = i.wrapping_add(1);
            // black_box verhindert LLVM-Optimierung (Loop darf nicht wegoptimiert werden)
            std::hint::black_box(i);
        }
    }

    fn tool_name() -> String {
        "loop".to_string()
    }

    fn tool_description() -> String {
        "Infinite loop for fuel-exhaustion testing".to_string()
    }
}

export!(LoopPlugin);
