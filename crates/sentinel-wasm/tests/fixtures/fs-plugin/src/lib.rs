//! FS-Plugin: sentinel-tool Component das ALLE 7 Host-API Functions nutzt.
//!
//! Realistisches "file-processor" Tool:
//! - Liest Dateien via fs-read
//! - Schreibt Ergebnisse via fs-write
//! - Listet Verzeichnisse via fs-list
//! - Fragt Agent-Info, Room-Info, Tick ab
//! - Loggt alle Operationen
//!
//! Input-Format (Kommando-basiert):
//!   "read <path>"           - Datei lesen, Inhalt zurueckgeben
//!   "write <path> <data>"   - Datei schreiben, Bytes-Written zurueckgeben
//!   "list <path>"           - Verzeichnis auflisten
//!   "room <room-id>"        - Raum-Info abfragen
//!   "status"                - Agent-Info + Tick + Room-Info als JSON-Report
//!   "process <in> <out>"    - Datei lesen, Woerter zaehlen, Report schreiben

wit_bindgen::generate!({
    world: "sentinel-tool",
    path: "wit/",
});

use sentinel::plugin::host_api;
use sentinel::plugin::types::LogLevel;

struct FsPlugin;

impl Guest for FsPlugin {
    fn execute(input: String) -> Result<String, String> {
        if input.is_empty() {
            return Err("empty input".to_string());
        }

        let parts: Vec<&str> = input.splitn(3, ' ').collect();
        let cmd = parts[0];

        match cmd {
            "read" => {
                let path = parts.get(1).ok_or("read: missing path")?;
                host_api::log(LogLevel::Info, &format!("fs-read: {path}"));
                let data = host_api::fs_read(path)?;
                // Return content as UTF-8 string
                String::from_utf8(data).map_err(|e| format!("utf8 error: {e}"))
            }

            "write" => {
                if parts.len() < 3 {
                    return Err("write: usage: write <path> <data>".to_string());
                }
                let path = parts[1];
                let data = parts[2];
                host_api::log(LogLevel::Info, &format!("fs-write: {path} ({} bytes)", data.len()));
                let written = host_api::fs_write(path, data.as_bytes())?;
                Ok(format!("written: {written} bytes to {path}"))
            }

            "list" => {
                let path = parts.get(1).copied().unwrap_or(".");
                host_api::log(LogLevel::Info, &format!("fs-list: {path}"));
                let entries = host_api::fs_list(path)?;
                Ok(entries.join("\n"))
            }

            "room" => {
                let room_id = parts.get(1).ok_or("room: missing room-id")?;
                host_api::log(LogLevel::Info, &format!("get-room-info: {room_id}"));
                match host_api::get_room_info(room_id) {
                    Some(info) => Ok(format!(
                        "room:{} name:{} floor:{} temp:{:.1} noise:{:.1} occupants:{}",
                        info.room_id, info.name, info.floor,
                        info.temperature, info.noise_db, info.occupant_count,
                    )),
                    None => Err(format!("room not found: {room_id}")),
                }
            }

            "status" => {
                // Full status report using ALL host functions
                let agent = host_api::get_agent_info();
                let tick = host_api::get_tick();
                let room = host_api::get_room_info(&agent.room_id);

                host_api::log(LogLevel::Info, &format!(
                    "status report for {} at tick {tick}", agent.name
                ));

                let room_line = match room {
                    Some(r) => format!(
                        "room:{} temp:{:.1}C noise:{:.1}dB occupants:{}",
                        r.name, r.temperature, r.noise_db, r.occupant_count,
                    ),
                    None => format!("room:{} (no data)", agent.room_id),
                };

                Ok(format!(
                    "agent:{} role:{} tick:{}\n\
                     hunger:{:.2} energy:{:.2} stress:{:.2}\n\
                     social:{:.2} caffeine:{:.2} bladder:{:.2}\n\
                     {room_line}",
                    agent.name, agent.role, tick,
                    agent.hunger, agent.energy, agent.stress,
                    agent.social_need, agent.caffeine, agent.bladder,
                ))
            }

            "process" => {
                // Full pipeline: read file -> count words -> write report
                if parts.len() < 3 {
                    return Err("process: usage: process <input-file> <output-file>".to_string());
                }
                let in_path = parts[1];
                let out_path = parts[2];

                host_api::log(LogLevel::Info, &format!("process: {in_path} -> {out_path}"));

                // 1. Read input file
                let data = host_api::fs_read(in_path)?;
                let content = String::from_utf8(data)
                    .map_err(|e| format!("utf8 error: {e}"))?;

                // 2. Process: count words, lines, bytes
                let words = content.split_whitespace().count();
                let lines = content.lines().count();
                let bytes = content.len();

                // 3. Get context
                let agent = host_api::get_agent_info();
                let tick = host_api::get_tick();

                // 4. Write report
                let report = format!(
                    "File Analysis Report\n\
                     ====================\n\
                     Source: {in_path}\n\
                     Agent: {} ({})\n\
                     Tick: {tick}\n\
                     ---\n\
                     Words: {words}\n\
                     Lines: {lines}\n\
                     Bytes: {bytes}\n",
                    agent.name, agent.role,
                );
                host_api::fs_write(out_path, report.as_bytes())?;

                // 5. List directory to confirm output exists
                // Extract parent dir or use "."
                let parent = if let Some(pos) = out_path.rfind('/') {
                    &out_path[..pos]
                } else {
                    "."
                };
                let entries = host_api::fs_list(parent)?;
                let out_filename = if let Some(pos) = out_path.rfind('/') {
                    &out_path[pos + 1..]
                } else {
                    out_path
                };
                if !entries.iter().any(|e| e == out_filename) {
                    return Err(format!("output file {out_path} not in directory listing"));
                }

                host_api::log(LogLevel::Info, &format!(
                    "process complete: {words} words, report written to {out_path}"
                ));

                Ok(format!("processed: {words} words, {lines} lines, {bytes} bytes -> {out_path}"))
            }

            _ => Err(format!("unknown command: {cmd}")),
        }
    }

    fn tool_name() -> String {
        "fs-processor".to_string()
    }

    fn tool_description() -> String {
        "File processor tool: reads, writes, lists files and queries agent/room state".to_string()
    }
}

export!(FsPlugin);
