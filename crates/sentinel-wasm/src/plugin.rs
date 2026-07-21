//! Plugin lifecycle management for WASM Component Model.
//!
//! `PluginHost` manages the wasmtime engine, linker, and cached
//! pre-linked components (`InstancePre`). Each plugin is compiled
//! once (expensive) and instantiated per-call (cheap).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use wasmtime::component::Component;
use wasmtime::{Engine, Store, StoreLimitsBuilder};

use crate::host::{AgentSnapshot, PluginState, RoomSnapshot, SentinelTool};

/// Configuration for loading a single WASM plugin.
#[derive(Debug, Clone)]
pub struct PluginConfig {
    /// Path to the `.wasm` component file.
    pub wasm_path: PathBuf,
    /// Maximum memory per plugin invocation (bytes). Default: 64 MB.
    pub memory_limit_bytes: usize,
    /// Maximum fuel (instruction count) per invocation. Default: 10M.
    pub fuel_limit: u64,
    /// WASI preopened directories: (host_path, guest_path).
    pub allowed_paths: Vec<(PathBuf, PathBuf)>,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            wasm_path: PathBuf::new(),
            memory_limit_bytes: 64 * 1024 * 1024, // 64 MB
            fuel_limit: 10_000_000,               // 10M instructions
            allowed_paths: Vec::new(),
        }
    }
}

/// Result of querying a loaded plugin's metadata exports.
#[derive(Debug, Clone)]
pub struct PluginMeta {
    pub tool_name: String,
    pub tool_description: String,
}

/// Manages WASM Component Model plugins.
///
/// Lifecycle:
/// 1. `new()` — Create engine + linker (once).
/// 2. `load(config)` — Compile component + cache (expensive, once per .wasm).
/// 3. `execute(path, input, state)` — New `Store` per call (cheap), fuel-reset, fresh WASI ctx.
pub struct PluginHost {
    engine: Engine,
    linker: wasmtime::component::Linker<PluginState>,
    /// Cached compiled components (expensive compile once, cheap instantiate per call).
    cache: HashMap<PathBuf, Component>,
    /// Plugin configs for memory/fuel limits.
    configs: HashMap<PathBuf, PluginConfig>,
}

impl PluginHost {
    /// Creates a new PluginHost with Component Model engine.
    pub fn new() -> wasmtime::Result<Self> {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        config.wasm_component_model(true);
        let engine = Engine::new(&config)?;

        let mut linker = wasmtime::component::Linker::<PluginState>::new(&engine);
        // Register WASI host functions (sync mode).
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
        // Register sentinel:plugin host-api functions.
        SentinelTool::add_to_linker::<PluginState, wasmtime::component::HasSelf<PluginState>>(
            &mut linker,
            |state: &mut PluginState| state,
        )?;

        Ok(Self {
            engine,
            linker,
            cache: HashMap::new(),
            configs: HashMap::new(),
        })
    }

    /// Returns a reference to the engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Loads and caches a plugin component.
    ///
    /// This is the expensive operation — component compilation.
    /// Call once per `.wasm` file, then reuse via `execute()`.
    pub fn load(&mut self, config: PluginConfig) -> wasmtime::Result<()> {
        let component = Component::from_file(&self.engine, &config.wasm_path)?;
        // Kanonischen Pfad als Cache-Key verwenden, damit PathBuf<->String
        // Roundtrips (z.B. in ToolDefinition::wasm_path) keinen Cache-Miss verursachen.
        let canonical = Self::canonical(&config.wasm_path);
        self.cache.insert(canonical.clone(), component);
        let canonical_config = PluginConfig {
            wasm_path: canonical.clone(),
            ..config
        };
        self.configs.insert(canonical, canonical_config);
        Ok(())
    }

    /// Checks if a plugin is loaded (cached).
    pub fn is_loaded(&self, wasm_path: &Path) -> bool {
        self.cache.contains_key(&Self::canonical(wasm_path))
    }

    /// Removes one compiled component and its execution limits from the cache.
    /// Callers must ensure no remaining workload references the path.
    pub fn unload(&mut self, wasm_path: &Path) -> bool {
        let canonical = Self::canonical(wasm_path);
        let removed_component = self.cache.remove(&canonical).is_some();
        let removed_config = self.configs.remove(&canonical).is_some();
        removed_component || removed_config
    }

    /// Kanonisiert einen Pfad fuer konsistente Cache-Key-Lookups.
    fn canonical(path: &Path) -> PathBuf {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }

    /// Returns the number of cached plugins.
    pub fn cached_count(&self) -> usize {
        self.cache.len()
    }

    /// Queries a loaded plugin's metadata (tool-name, tool-description).
    ///
    /// Creates a temporary store to call the export functions.
    pub fn query_meta(
        &self,
        wasm_path: &Path,
        agent_home: PathBuf,
    ) -> wasmtime::Result<PluginMeta> {
        let canonical = Self::canonical(wasm_path);
        let component = self
            .cache
            .get(&canonical)
            .ok_or_else(|| wasmtime::Error::msg("Plugin not loaded"))?;

        let config = self
            .configs
            .get(&canonical)
            .ok_or_else(|| wasmtime::Error::msg("Plugin config not found"))?;

        let state = self.build_state(
            config,
            AgentSnapshot::default(),
            HashMap::new(),
            0,
            agent_home,
        )?;
        let mut store = self.build_store(state, config)?;

        let bindings = SentinelTool::instantiate(&mut store, component, &self.linker)?;
        let tool_name = bindings.call_tool_name(&mut store)?;
        let tool_description = bindings.call_tool_description(&mut store)?;

        Ok(PluginMeta {
            tool_name,
            tool_description,
        })
    }

    /// Executes a plugin with the given input.
    ///
    /// Creates a fresh `Store` per invocation with fuel limits and WASI context.
    /// Returns `Ok(output)` or `Err(plugin_error)` from the plugin's `execute()`.
    pub fn execute(
        &self,
        wasm_path: &Path,
        input: &str,
        agent_snapshot: AgentSnapshot,
        rooms: HashMap<String, RoomSnapshot>,
        tick: u64,
        agent_home: PathBuf,
    ) -> wasmtime::Result<Result<String, String>> {
        let canonical = Self::canonical(wasm_path);
        let component = self
            .cache
            .get(&canonical)
            .ok_or_else(|| wasmtime::Error::msg("Plugin not loaded"))?;

        let config = self
            .configs
            .get(&canonical)
            .ok_or_else(|| wasmtime::Error::msg("Plugin config not found"))?;

        let state = self.build_state(config, agent_snapshot, rooms, tick, agent_home)?;
        let mut store = self.build_store(state, config)?;

        let bindings = SentinelTool::instantiate(&mut store, component, &self.linker)?;
        bindings.call_execute(&mut store, input)
    }

    /// Builds a `PluginState` for a single invocation.
    fn build_state(
        &self,
        config: &PluginConfig,
        agent_snapshot: AgentSnapshot,
        rooms: HashMap<String, RoomSnapshot>,
        tick: u64,
        agent_home: PathBuf,
    ) -> wasmtime::Result<PluginState> {
        let mut wasi_builder = wasmtime_wasi::WasiCtxBuilder::new();

        // Preopened directories for WASI filesystem access.
        for (host_path, guest_path) in &config.allowed_paths {
            let guest_str = guest_path.to_str().unwrap_or("/data");
            wasi_builder.preopened_dir(
                host_path,
                guest_str,
                wasmtime_wasi::DirPerms::all(),
                wasmtime_wasi::FilePerms::all(),
            )?;
        }

        let limits = StoreLimitsBuilder::new()
            .memory_size(config.memory_limit_bytes)
            .instances(10)
            .tables(10)
            .memories(10)
            .build();

        Ok(PluginState {
            wasi_ctx: wasi_builder.build(),
            resource_table: wasmtime::component::ResourceTable::new(),
            limits,
            agent_snapshot,
            rooms,
            tick,
            agent_home,
        })
    }

    /// Builds a `Store` with fuel limits and resource limiter.
    fn build_store(
        &self,
        state: PluginState,
        config: &PluginConfig,
    ) -> wasmtime::Result<Store<PluginState>> {
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        store.set_fuel(config.fuel_limit)?;
        Ok(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_host_creates_successfully() {
        let host = PluginHost::new();
        assert!(host.is_ok());
        let host = host.unwrap();
        assert_eq!(host.cached_count(), 0);
    }

    #[test]
    fn load_nonexistent_fails() {
        let mut host = PluginHost::new().unwrap();
        let config = PluginConfig {
            wasm_path: PathBuf::from("/nonexistent/plugin.wasm"),
            ..Default::default()
        };
        let result = host.load(config);
        assert!(result.is_err());
    }
}
