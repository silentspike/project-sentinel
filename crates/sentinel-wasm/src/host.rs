//! WIT Component Model host implementation.
//!
//! Uses `bindgen!` to generate typed traits from `wit/world.wit`.
//! Implements the `sentinel:plugin/host-api` interface, bridging
//! ECS world-state to WASM plugins via typed host functions.

use std::collections::HashMap;
use std::path::PathBuf;

// Generate typed bindings from WIT definition.
// Let bindgen generate ALL types (no custom `with` mappings).
wasmtime::component::bindgen!({
    world: "sentinel-tool",
    path: "wit/",
});

/// ECS snapshot for a single agent (read-only, passed to plugin).
#[derive(Clone, Debug, Default)]
pub struct AgentSnapshot {
    pub agent_id: String,
    pub name: String,
    pub role: String,
    pub hunger: f32,
    pub energy: f32,
    pub stress: f32,
    pub social_need: f32,
    pub caffeine: f32,
    pub bladder: f32,
    pub room_id: String,
}

/// ECS snapshot for a room (read-only, passed to plugin).
#[derive(Clone, Debug)]
pub struct RoomSnapshot {
    pub room_id: String,
    pub name: String,
    pub floor: u32,
    pub temperature: f32,
    pub noise_db: f32,
    pub occupant_count: u32,
}

/// State stored in `wasmtime::Store` per plugin execution.
///
/// Contains WASI context, resource limits, and ECS data snapshots.
/// A new `PluginState` is created for each tool invocation.
pub struct PluginState {
    pub wasi_ctx: wasmtime_wasi::WasiCtx,
    pub resource_table: wasmtime::component::ResourceTable,
    pub limits: wasmtime::StoreLimits,
    /// Read-only ECS snapshot for this agent.
    pub agent_snapshot: AgentSnapshot,
    /// Room data keyed by room-id.
    pub rooms: HashMap<String, RoomSnapshot>,
    /// Current simulation tick.
    pub tick: u64,
    /// Base path for agent filesystem (WASI preopened dir).
    pub agent_home: PathBuf,
}

// Required by wasmtime-wasi for WASI host function delegation.
impl wasmtime_wasi::WasiView for PluginState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.resource_table,
        }
    }
}

/// Resolves a plugin-provided path safely within agent_home.
///
/// Rejects absolute paths and `..` components to prevent directory traversal.
fn safe_resolve(agent_home: &std::path::Path, path: &str) -> Result<PathBuf, String> {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return Err(format!("absolute path not allowed: '{}'", path));
    }
    for component in p.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(format!("path traversal not allowed: '{}'", path));
        }
    }
    Ok(agent_home.join(p))
}

/// Host-API implementation: bridges ECS world-state to WASM plugins.
///
/// Each function maps a WIT host-api call to operations on `PluginState`.
/// FS operations use the agent's home directory. Paths are sandboxed:
/// absolute paths and `..` traversal are rejected.
impl sentinel::plugin::host_api::Host for PluginState {
    fn fs_read(&mut self, path: String) -> Result<Vec<u8>, String> {
        let full_path = safe_resolve(&self.agent_home, &path)?;
        std::fs::read(&full_path).map_err(|e| format!("fs-read '{}': {}", path, e))
    }

    fn fs_write(&mut self, path: String, data: Vec<u8>) -> Result<u64, String> {
        let full_path = safe_resolve(&self.agent_home, &path)?;
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("fs-write mkdir '{}': {}", path, e))?;
        }
        let len = data.len() as u64;
        std::fs::write(&full_path, &data).map_err(|e| format!("fs-write '{}': {}", path, e))?;
        Ok(len)
    }

    fn fs_list(&mut self, path: String) -> Result<Vec<String>, String> {
        let full_path = safe_resolve(&self.agent_home, &path)?;
        let entries =
            std::fs::read_dir(&full_path).map_err(|e| format!("fs-list '{}': {}", path, e))?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| format!("fs-list entry: {}", e))?;
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
        Ok(names)
    }

    fn get_agent_info(&mut self) -> sentinel::plugin::types::AgentInfo {
        let s = &self.agent_snapshot;
        sentinel::plugin::types::AgentInfo {
            agent_id: s.agent_id.clone(),
            name: s.name.clone(),
            role: s.role.clone(),
            hunger: s.hunger,
            energy: s.energy,
            stress: s.stress,
            social_need: s.social_need,
            caffeine: s.caffeine,
            bladder: s.bladder,
            room_id: s.room_id.clone(),
        }
    }

    fn get_room_info(&mut self, room_id: String) -> Option<sentinel::plugin::types::RoomInfo> {
        self.rooms
            .get(&room_id)
            .map(|r| sentinel::plugin::types::RoomInfo {
                room_id: r.room_id.clone(),
                name: r.name.clone(),
                floor: r.floor,
                temperature: r.temperature,
                noise_db: r.noise_db,
                occupant_count: r.occupant_count,
            })
    }

    fn log(&mut self, level: sentinel::plugin::types::LogLevel, msg: String) {
        match level {
            sentinel::plugin::types::LogLevel::Debug => tracing::debug!(plugin = true, "{}", msg),
            sentinel::plugin::types::LogLevel::Info => tracing::info!(plugin = true, "{}", msg),
            sentinel::plugin::types::LogLevel::Warn => tracing::warn!(plugin = true, "{}", msg),
            sentinel::plugin::types::LogLevel::Error => tracing::error!(plugin = true, "{}", msg),
        }
    }

    fn get_tick(&mut self) -> u64 {
        self.tick
    }
}

/// Empty trait impl for the types interface (required by bindgen).
impl sentinel::plugin::types::Host for PluginState {}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel::plugin::host_api::Host;

    fn make_snapshot() -> AgentSnapshot {
        AgentSnapshot {
            agent_id: "AGENT-01".to_string(),
            name: "Thomas Mueller".to_string(),
            role: "CEO".to_string(),
            hunger: 0.3,
            energy: 0.7,
            stress: 0.2,
            social_need: 0.5,
            caffeine: 0.4,
            bladder: 0.1,
            room_id: "buero-ceo".to_string(),
        }
    }

    fn make_rooms() -> HashMap<String, RoomSnapshot> {
        let mut rooms = HashMap::new();
        rooms.insert(
            "buero-ceo".to_string(),
            RoomSnapshot {
                room_id: "buero-ceo".to_string(),
                name: "CEO Buero".to_string(),
                floor: 1,
                temperature: 22.0,
                noise_db: 35.0,
                occupant_count: 1,
            },
        );
        rooms
    }

    fn make_state(agent_home: PathBuf) -> PluginState {
        let wasi_ctx = wasmtime_wasi::WasiCtxBuilder::new().build();
        PluginState {
            wasi_ctx,
            resource_table: wasmtime::component::ResourceTable::new(),
            limits: wasmtime::StoreLimitsBuilder::new()
                .memory_size(64 * 1024 * 1024)
                .build(),
            agent_snapshot: make_snapshot(),
            rooms: make_rooms(),
            tick: 42,
            agent_home,
        }
    }

    #[test]
    fn host_get_agent_info() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = make_state(dir.path().to_path_buf());
        let info = state.get_agent_info();
        assert_eq!(info.agent_id, "AGENT-01");
        assert_eq!(info.name, "Thomas Mueller");
        assert_eq!(info.role, "CEO");
        assert!((info.hunger - 0.3).abs() < f32::EPSILON);
        assert!((info.energy - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn host_get_room_info() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = make_state(dir.path().to_path_buf());
        let room = state.get_room_info("buero-ceo".to_string());
        assert!(room.is_some());
        let room = room.unwrap();
        assert_eq!(room.name, "CEO Buero");
        assert_eq!(room.floor, 1);

        let none = state.get_room_info("nonexistent".to_string());
        assert!(none.is_none());
    }

    #[test]
    fn host_get_tick() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = make_state(dir.path().to_path_buf());
        assert_eq!(state.get_tick(), 42);
    }

    #[test]
    fn host_fs_read_write() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = make_state(dir.path().to_path_buf());

        let data = b"Hello from plugin!".to_vec();
        let bytes_written = state.fs_write("test.txt".to_string(), data.clone());
        assert!(bytes_written.is_ok());
        assert_eq!(bytes_written.unwrap(), data.len() as u64);

        let read_back = state.fs_read("test.txt".to_string());
        assert!(read_back.is_ok());
        assert_eq!(read_back.unwrap(), data);
    }

    #[test]
    fn host_fs_list() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = make_state(dir.path().to_path_buf());

        state.fs_write("a.txt".to_string(), b"a".to_vec()).unwrap();
        state.fs_write("b.txt".to_string(), b"b".to_vec()).unwrap();

        let list = state.fs_list(".".to_string());
        assert!(list.is_ok());
        let mut names = list.unwrap();
        names.sort();
        assert_eq!(names, vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn host_fs_read_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = make_state(dir.path().to_path_buf());
        let result = state.fs_read("does_not_exist.txt".to_string());
        assert!(result.is_err());
    }

    #[test]
    fn host_log_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = make_state(dir.path().to_path_buf());
        state.log(
            sentinel::plugin::types::LogLevel::Debug,
            "debug msg".to_string(),
        );
        state.log(
            sentinel::plugin::types::LogLevel::Info,
            "info msg".to_string(),
        );
        state.log(
            sentinel::plugin::types::LogLevel::Warn,
            "warn msg".to_string(),
        );
        state.log(
            sentinel::plugin::types::LogLevel::Error,
            "error msg".to_string(),
        );
    }

    #[test]
    fn host_fs_write_creates_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = make_state(dir.path().to_path_buf());
        let result = state.fs_write("sub/dir/file.txt".to_string(), b"nested".to_vec());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 6);
        let read_back = state.fs_read("sub/dir/file.txt".to_string());
        assert_eq!(read_back.unwrap(), b"nested");
    }

    #[test]
    fn host_fs_absolute_path_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = make_state(dir.path().to_path_buf());
        let result = state.fs_read("/etc/passwd".to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("absolute path not allowed"));
    }

    #[test]
    fn host_fs_traversal_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = make_state(dir.path().to_path_buf());
        let result = state.fs_read("../../../etc/passwd".to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("path traversal not allowed"));
    }

    #[test]
    fn host_fs_write_absolute_path_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = make_state(dir.path().to_path_buf());
        let result = state.fs_write("/tmp/evil.txt".to_string(), b"hack".to_vec());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("absolute path not allowed"));
    }

    #[test]
    fn host_fs_list_traversal_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = make_state(dir.path().to_path_buf());
        let result = state.fs_list("../../".to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("path traversal not allowed"));
    }
}
