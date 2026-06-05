use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use sentinel_limbo::rusqlite::{params, Connection};
use sentinel_limbo::EventStore;
use serde_json::json;

const MAX_PLAUSIBLE_SIM_TICK_EXCLUSIVE: i64 = 1_000_000_000;
const EVOLUTION_PER_AGENT_FIELD_KEEP: i64 = 2000;
const EVOLUTION_GLOBAL_HIGH_WATER: i64 = 499_000;
const EVOLUTION_GLOBAL_RETAIN: i64 = 490_000;
const EVENTS_DB_MAX_BYTES: u64 = 200 * 1024 * 1024;
const EVOLUTION_DB_MAX_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Parser)]
#[command(name = "sentinel-db-maint")]
#[command(about = "Offline SQLite maintenance for Sentinel data stores")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    InspectEvents {
        #[arg(long)]
        path: PathBuf,
    },
    CompactEvents {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    InspectEvolution {
        #[arg(long)]
        path: PathBuf,
    },
    CompactEvolution {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::InspectEvents { path } => inspect_events(&path),
        Command::CompactEvents { input, output } => compact_events(&input, &output),
        Command::InspectEvolution { path } => inspect_evolution(&path),
        Command::CompactEvolution { input, output } => compact_evolution(&input, &output),
    }
}

fn inspect_events(path: &Path) -> Result<()> {
    let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    let events = scalar_i64(&conn, "SELECT count(*) FROM events")?;
    let outbox = scalar_i64(&conn, "SELECT count(*) FROM outbox")?;
    let orphan_outbox = scalar_i64(
        &conn,
        "SELECT count(*) FROM outbox
         WHERE event_id NOT IN (SELECT event_id FROM events)",
    )?;
    let snapshots = scalar_i64(&conn, "SELECT count(*) FROM snapshots")?;
    let duplicate_snapshot_aggregates = scalar_i64(
        &conn,
        "SELECT count(*) FROM (
             SELECT aggregate_id FROM snapshots GROUP BY aggregate_id HAVING count(*) > 1
         )",
    )?;
    let immutable_trigger = scalar_i64(
        &conn,
        "SELECT count(*) FROM sqlite_master
         WHERE type = 'trigger'
           AND name = 'protect_recent_snapshots'
           AND sql LIKE '%BEFORE DELETE ON world_snapshots%'",
    )?;
    let events_event_id_index = index_exists(&conn, "idx_events_event_id")?;
    let outbox_event_id_index = index_exists(&conn, "idx_outbox_event_id")?;
    let integrity = integrity_check(&conn)?;
    println!(
        "{}",
        json!({
            "events": events,
            "outbox": outbox,
            "orphan_outbox": orphan_outbox,
            "snapshots": snapshots,
            "duplicate_snapshot_aggregates": duplicate_snapshot_aggregates,
            "protect_recent_snapshots_trigger": immutable_trigger == 1,
            "idx_events_event_id": events_event_id_index,
            "idx_outbox_event_id": outbox_event_id_index,
            "integrity_check": integrity,
        })
    );
    Ok(())
}

fn compact_events(input: &Path, output: &Path) -> Result<()> {
    require_output_absent(output)?;
    let output_str = output
        .to_str()
        .with_context(|| format!("non-UTF8 output path {}", output.display()))?;
    let store = EventStore::open(output_str)?;
    drop(store);

    let mut conn = Connection::open(output)?;
    conn.execute(
        "ATTACH DATABASE ?1 AS src",
        params![input.display().to_string()],
    )?;

    let source_has_retry_count = table_has_column(&conn, "src", "outbox", "retry_count")?;
    let source_has_last_error = table_has_column(&conn, "src", "outbox", "last_error")?;

    let tx = conn.transaction()?;
    tx.execute_batch(
        "INSERT INTO events
         (id, event_id, event_type, aggregate_id, payload, correlation_id, causation_id,
          operation_id, tick, timestamp_ms, schema_version, compensation_type)
         SELECT id, event_id, event_type, aggregate_id, payload, correlation_id, causation_id,
                operation_id, tick, timestamp_ms, schema_version, compensation_type
         FROM src.events
         ORDER BY id;

         INSERT INTO snapshots
         (id, aggregate_id, snapshot_type, payload, last_event_id, version, created_at)
         SELECT s.id, s.aggregate_id, s.snapshot_type, s.payload, s.last_event_id, s.version, s.created_at
         FROM src.snapshots s
         WHERE s.id IN (
             SELECT (
                 SELECT s2.id
                 FROM src.snapshots s2
                 WHERE s2.aggregate_id = aggregates.aggregate_id
                 ORDER BY s2.version DESC, s2.id DESC
                 LIMIT 1
             )
             FROM (SELECT DISTINCT aggregate_id FROM src.snapshots) aggregates
         )
         ORDER BY s.id;

         INSERT INTO world_snapshots
         (id, tier, tick, sim_hour, last_event_id, payload_size, payload, created_at)
         SELECT id, tier, tick, sim_hour, last_event_id, payload_size, payload, created_at
         FROM src.world_snapshots
         ORDER BY tick;

         INSERT INTO projection_offsets
         (projection_name, last_event_id, updated_at)
         SELECT projection_name, last_event_id, updated_at
         FROM src.projection_offsets;",
    )?;

    let retry_count_expr = if source_has_retry_count {
        "COALESCE(o.retry_count, 0)"
    } else {
        "0"
    };
    let last_error_expr = if source_has_last_error {
        "o.last_error"
    } else {
        "NULL"
    };
    tx.execute_batch(&format!(
        "INSERT INTO outbox
         (id, event_id, topic, payload, status, created_at, published_at, retry_count, last_error)
         SELECT o.id, o.event_id, o.topic, o.payload, o.status, o.created_at, o.published_at,
                {retry_count_expr}, {last_error_expr}
         FROM src.outbox o
         WHERE o.status IN ('pending', 'failed')
           AND o.event_id IN (SELECT event_id FROM src.events)
         ORDER BY o.id;"
    ))?;

    preserve_sqlite_sequence(&tx, "events")?;
    preserve_sqlite_sequence(&tx, "outbox")?;
    preserve_sqlite_sequence(&tx, "snapshots")?;
    tx.commit()?;

    checkpoint_wal(&conn)?;
    validate_events_output(&conn, output)?;
    inspect_events(output)
}

fn inspect_evolution(path: &Path) -> Result<()> {
    let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    let rows = scalar_i64(&conn, "SELECT count(*) FROM personality_evolution")?;
    let invalid_ticks = scalar_i64(
        &conn,
        "SELECT count(*) FROM personality_evolution
         WHERE tick < 0 OR tick >= 1000000000",
    )?;
    let over_limit_groups = scalar_i64(
        &conn,
        "SELECT count(*) FROM (
             SELECT agent_id, field
             FROM personality_evolution
             GROUP BY agent_id, field
             HAVING count(*) > 2000
         )",
    )?;
    let max_tick = optional_i64(&conn, "SELECT max(tick) FROM personality_evolution")?;
    let integrity = integrity_check(&conn)?;
    println!(
        "{}",
        json!({
            "personality_evolution": rows,
            "invalid_ticks": invalid_ticks,
            "over_limit_groups": over_limit_groups,
            "max_tick": max_tick,
            "integrity_check": integrity,
        })
    );
    Ok(())
}

fn compact_evolution(input: &Path, output: &Path) -> Result<()> {
    require_output_absent(output)?;
    let mut conn = Connection::open(output)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE personality_evolution (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             agent_id TEXT NOT NULL,
             tick INTEGER NOT NULL,
             field TEXT NOT NULL,
             change_type TEXT NOT NULL,
             old_value TEXT,
             new_value TEXT NOT NULL,
             reason TEXT NOT NULL,
             nmda_score REAL,
             source TEXT NOT NULL DEFAULT 'realtime_judge',
             created_at_ms INTEGER NOT NULL
         );
         CREATE INDEX idx_evolution_agent ON personality_evolution(agent_id, tick);
         CREATE INDEX idx_evolution_source ON personality_evolution(source);
         CREATE INDEX idx_evolution_agent_field_id
             ON personality_evolution(agent_id, field, id DESC);",
    )?;
    conn.execute(
        "ATTACH DATABASE ?1 AS src",
        params![input.display().to_string()],
    )?;

    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO personality_evolution
         (id, agent_id, tick, field, change_type, old_value, new_value, reason,
          nmda_score, source, created_at_ms)
         SELECT id, agent_id, tick, field, change_type, old_value, new_value, reason,
                nmda_score, source, created_at_ms
         FROM (
             SELECT *,
                    row_number() OVER (
                        PARTITION BY agent_id, field
                        ORDER BY id DESC
                    ) AS retention_rank
             FROM src.personality_evolution
             WHERE tick >= 0 AND tick < ?1
         )
         WHERE retention_rank <= ?2
         ORDER BY id",
        params![
            MAX_PLAUSIBLE_SIM_TICK_EXCLUSIVE,
            EVOLUTION_PER_AGENT_FIELD_KEEP
        ],
    )?;

    let count: i64 = tx.query_row("SELECT COUNT(*) FROM personality_evolution", [], |row| {
        row.get(0)
    })?;
    if count > EVOLUTION_GLOBAL_HIGH_WATER {
        tx.execute(
            "DELETE FROM personality_evolution
             WHERE id <= (
                 SELECT id
                 FROM personality_evolution
                 ORDER BY id DESC
                 LIMIT 1 OFFSET ?1
             )",
            params![EVOLUTION_GLOBAL_RETAIN],
        )?;
    }
    preserve_sqlite_sequence(&tx, "personality_evolution")?;
    tx.commit()?;

    checkpoint_wal(&conn)?;
    validate_evolution_output(&conn, output)?;
    inspect_evolution(output)
}

fn require_output_absent(path: &Path) -> Result<()> {
    if path.exists() {
        bail!("output already exists: {}", path.display());
    }
    Ok(())
}

fn scalar_i64(conn: &Connection, sql: &str) -> Result<i64> {
    conn.query_row(sql, [], |row| row.get(0))
        .with_context(|| format!("query scalar failed: {sql}"))
}

fn optional_i64(conn: &Connection, sql: &str) -> Result<Option<i64>> {
    conn.query_row(sql, [], |row| row.get(0))
        .with_context(|| format!("query optional scalar failed: {sql}"))
}

fn index_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
            params![name],
            |row| row.get(0),
        )
        .with_context(|| format!("query index existence failed: {name}"))?;
    Ok(count == 1)
}

fn integrity_check(conn: &Connection) -> Result<String> {
    conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .context("integrity_check failed")
}

fn checkpoint_wal(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .context("wal checkpoint failed")
}

fn table_has_column(conn: &Connection, schema: &str, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA {schema}.table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn preserve_sqlite_sequence(
    tx: &sentinel_limbo::rusqlite::Transaction<'_>,
    table: &str,
) -> Result<()> {
    let fallback = match table {
        "events" => "SELECT COALESCE(max(id), 0) FROM events",
        "outbox" => "SELECT COALESCE(max(id), 0) FROM outbox",
        "snapshots" => "SELECT COALESCE(max(id), 0) FROM snapshots",
        "personality_evolution" => "SELECT COALESCE(max(id), 0) FROM personality_evolution",
        _ => bail!("unsupported sqlite_sequence table: {table}"),
    };
    tx.execute(
        "DELETE FROM sqlite_sequence WHERE name = ?1",
        params![table],
    )?;
    tx.execute(
        &format!(
            "INSERT INTO sqlite_sequence(name, seq)
             SELECT ?1, COALESCE(
                 (SELECT seq FROM src.sqlite_sequence WHERE name = ?1),
                 ({fallback})
             )"
        ),
        params![table],
    )?;
    Ok(())
}

fn validate_events_output(conn: &Connection, path: &Path) -> Result<()> {
    let integrity = integrity_check(conn)?;
    if integrity != "ok" {
        bail!("events output integrity_check failed: {integrity}");
    }
    let orphan_outbox = scalar_i64(
        conn,
        "SELECT count(*) FROM outbox
         WHERE event_id NOT IN (SELECT event_id FROM events)",
    )?;
    if orphan_outbox != 0 {
        bail!("events output has {orphan_outbox} orphan outbox rows");
    }
    let duplicate_snapshot_aggregates = scalar_i64(
        conn,
        "SELECT count(*) FROM (
             SELECT aggregate_id FROM snapshots GROUP BY aggregate_id HAVING count(*) > 1
         )",
    )?;
    if duplicate_snapshot_aggregates != 0 {
        bail!("events output has duplicate snapshot aggregates: {duplicate_snapshot_aggregates}");
    }
    let immutable_trigger = scalar_i64(
        conn,
        "SELECT count(*) FROM sqlite_master
         WHERE type = 'trigger'
           AND name = 'protect_recent_snapshots'
           AND sql LIKE '%BEFORE DELETE ON world_snapshots%'",
    )?;
    if immutable_trigger != 1 {
        bail!("protect_recent_snapshots trigger missing from events output");
    }
    if !index_exists(conn, "idx_events_event_id")? {
        bail!("idx_events_event_id missing from events output");
    }
    if !index_exists(conn, "idx_outbox_event_id")? {
        bail!("idx_outbox_event_id missing from events output");
    }
    let size = std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    if size >= EVENTS_DB_MAX_BYTES {
        bail!("events output too large: {size} bytes, limit is {EVENTS_DB_MAX_BYTES} bytes");
    }
    Ok(())
}

fn validate_evolution_output(conn: &Connection, path: &Path) -> Result<()> {
    let integrity = integrity_check(conn)?;
    if integrity != "ok" {
        bail!("evolution output integrity_check failed: {integrity}");
    }
    let invalid_ticks = scalar_i64(
        conn,
        "SELECT count(*) FROM personality_evolution
         WHERE tick < 0 OR tick >= 1000000000",
    )?;
    if invalid_ticks != 0 {
        bail!("evolution output has {invalid_ticks} invalid ticks");
    }
    let over_limit_groups = scalar_i64(
        conn,
        "SELECT count(*) FROM (
             SELECT agent_id, field
             FROM personality_evolution
             GROUP BY agent_id, field
             HAVING count(*) > 2000
         )",
    )?;
    if over_limit_groups != 0 {
        bail!("evolution output has {over_limit_groups} over-limit groups");
    }
    let size = std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    if size >= EVOLUTION_DB_MAX_BYTES {
        bail!("evolution output too large: {size} bytes, limit is {EVOLUTION_DB_MAX_BYTES} bytes");
    }
    Ok(())
}
