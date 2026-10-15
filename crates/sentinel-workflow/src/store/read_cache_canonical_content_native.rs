use super::*;
use crate::{AdaptiveWorkFundingExpiredReplacementSourceV1, RuntimeAuthoritySnapshotV1, TenantId, WorkflowStore};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use uuid::Uuid;

const MAX_DB_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 256 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_RUNTIME: Duration = Duration::from_secs(300);
const MAX_DIGEST_ROWS: usize = 65;
const MAX_DIGEST_DOMAIN_BYTES: usize = 128;

struct Config {
    path: PathBuf,
    db_bytes: u64,
    modified: SystemTime,
    tenant: TenantId,
    session_id: Uuid,
    funding_operation_id: Uuid,
    authority: RuntimeAuthoritySnapshotV1,
    samples: usize,
}

fn explicit_text(name: &str, limit: usize) -> String {
    let value = std::env::var(name).unwrap_or_else(|_| panic!("missing explicit {name}"));
    assert!(!value.is_empty() && value.len() <= limit, "invalid bounded {name}");
    value
}

fn explicit_uuid(name: &str) -> Uuid {
    let value = explicit_text(name, 36);
    let id = Uuid::parse_str(&value).unwrap_or_else(|_| panic!("invalid {name}"));
    assert!(!id.is_nil() && id.to_string() == value, "noncanonical {name}");
    id
}

fn regular_file(path: &Path, limit: u64) -> std::fs::Metadata {
    let info = std::fs::symlink_metadata(path)
        .unwrap_or_else(|_| panic!("explicit diagnostic file unavailable"));
    assert!(info.file_type().is_file() && info.len() > 0 && info.len() <= limit,
        "diagnostic input must be a bounded regular file, not a symlink");
    info
}

impl Config {
    fn read() -> Self {
        let path = PathBuf::from(explicit_text("SENTINEL_DIAGNOSTIC_READ_ONLY_DB", 4096));
        let metadata_path = PathBuf::from(explicit_text("SENTINEL_DIAGNOSTIC_FUNDING_METADATA", 4096));
        assert!(path.is_absolute() && metadata_path.is_absolute(), "explicit absolute paths required");
        let db = regular_file(&path, MAX_DB_BYTES);
        let info = regular_file(&metadata_path, MAX_METADATA_BYTES);
        let mut bytes = Vec::new();
        std::fs::File::open(&metadata_path)
            .unwrap_or_else(|_| panic!("diagnostic metadata unavailable"))
            .take(MAX_METADATA_BYTES + 1)
            .read_to_end(&mut bytes)
            .unwrap_or_else(|_| panic!("diagnostic metadata read failed"));
        assert!(bytes.len() as u64 == info.len(), "diagnostic metadata changed or exceeded its bound");
        let metadata: serde_json::Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| panic!("invalid diagnostic metadata JSON"));
        let source: AdaptiveWorkFundingExpiredReplacementSourceV1 =
            serde_json::from_value(metadata["source"].clone())
                .unwrap_or_else(|_| panic!("missing typed retained funding source metadata"));
        source.validate().unwrap_or_else(|_| panic!("invalid retained funding source metadata"));
        let tenant_text = explicit_text("SENTINEL_DIAGNOSTIC_TENANT_ID", 128);
        let tenant = TenantId::parse(&tenant_text)
            .unwrap_or_else(|_| panic!("invalid diagnostic tenant identity"));
        let session_id = explicit_uuid("SENTINEL_DIAGNOSTIC_SESSION_ID");
        let funding_operation_id = explicit_uuid("SENTINEL_DIAGNOSTIC_FUNDING_OPERATION_ID");
        assert!(source.tenant_id == tenant && source.session_id == session_id,
            "explicit identities differ from retained source metadata");
        let authority = source.current_resume_source.assignee_authority;
        authority.validate().unwrap_or_else(|_| panic!("invalid supplied authority shape"));
        let samples = std::env::var_os("SENTINEL_DIAGNOSTIC_REPEAT")
            .map(|value| {
                let value = value.into_string().unwrap_or_else(|_| panic!("repeat must be in 1..=3"));
                assert!(value.len() <= 1, "repeat must be in 1..=3");
                value.parse::<usize>().unwrap_or_else(|_| panic!("repeat must be in 1..=3"))
            })
            .unwrap_or(2);
        assert!((1..=3).contains(&samples), "repeat must be in 1..=3");
        Self {
            path, db_bytes: db.len(),
            modified: db.modified().unwrap_or_else(|_| panic!("snapshot timestamp unavailable")),
            tenant, session_id, funding_operation_id, authority, samples,
        }
    }

    fn frozen(&self, started: Instant) {
        assert!(started.elapsed() <= MAX_RUNTIME, "native comparison exceeded its wall-time bound");
        let info = regular_file(&self.path, MAX_DB_BYTES);
        assert!(info.len() == self.db_bytes && info.modified().ok() == Some(self.modified),
            "snapshot changed; no matched comparison permitted");
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut sidecar = self.path.as_os_str().to_os_string();
            sidecar.push(suffix);
            assert!(std::fs::symlink_metadata(PathBuf::from(sidecar))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
                "a standalone frozen snapshot without SQLite sidecars is required");
        }
    }

    fn open_read_only(&self) -> WorkflowStore {
        // Same construction contract as store::tests' existing native RO probe:
        // no WorkflowStore::open, initialization, migration, repair, or RW flag.
        let connection = Connection::open_with_flags(&self.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .unwrap_or_else(|_| panic!("read-only snapshot open failed"));
        connection.pragma_update(None, "query_only", true)
            .unwrap_or_else(|_| panic!("query_only setup failed"));
        connection.busy_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("read-only busy-timeout setup failed"));
        assert!(connection.is_readonly("main").unwrap_or(false), "snapshot connection must be RO");
        crate::store::validate_store_schema(&connection, crate::store::WORKFLOW_STORE_SCHEMA_VERSION)
            .unwrap_or_else(|_| panic!("snapshot schema rejected; migration is forbidden"));
        WorkflowStore {
            connection: std::sync::Mutex::new(connection),
            validated_reads: std::sync::Mutex::new(ReadCache::default()),
        }
    }
}

fn unchanged_read_only(store: &WorkflowStore, data_version: Option<i64>) -> i64 {
    let connection = store.connection.lock().unwrap_or_else(|_| panic!("diagnostic connection poisoned"));
    assert!(connection.is_readonly("main").unwrap_or(false) && connection.is_autocommit(),
        "read-only snapshot or transaction cleanup contract violated");
    let scalar = |sql: &str| -> i64 {
        connection.query_row(sql, [], |row| row.get(0))
            .unwrap_or_else(|_| panic!("read-only state check failed"))
    };
    let query_only = scalar("PRAGMA query_only");
    let changes = scalar("SELECT total_changes()");
    let version = scalar("PRAGMA data_version");
    assert!(query_only == 1 && changes == 0, "native diagnostic must remain query_only with total_changes=0");
    assert!(data_version.is_none_or(|before| before == version),
        "snapshot changed during a leg; no matched comparison permitted");
    version
}

fn phase<T: Serialize>(
    sample: usize,
    legacy: bool,
    name: &'static str,
    remaining: &mut usize,
    overall_started: Instant,
    read: impl FnOnce() -> Result<T, crate::WorkflowError>,
) -> Vec<u8> {
    assert!(overall_started.elapsed() <= MAX_RUNTIME, "native comparison exceeded its wall-time bound");
    let started = Instant::now();
    let (result, digests) = crate::digest::with_canonical_digest_cache_enabled(!legacy, || {
        crate::digest::with_digest_diagnostics(read)
    });
    let elapsed_us = started.elapsed().as_micros();
    let mode = if legacy { "legacy" } else { "candidate" };
    println!("CANONICAL_CONTENT_NATIVE_PHASE sample={sample} mode={mode} phase={name} elapsed_us={elapsed_us} digest_rows={} succeeded={} error_code={:?}",
        digests.len(), result.is_ok(), result.as_ref().err().map(|error| error.code));
    assert!(overall_started.elapsed() <= MAX_RUNTIME, "native comparison exceeded its wall-time bound");
    assert!(digests.len() <= MAX_DIGEST_ROWS, "digest diagnostic inventory exceeded its bound");
    for digest in &digests {
        let domain = match digest.domain {
            Some(domain) if domain.len() <= MAX_DIGEST_DOMAIN_BYTES && domain.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')) => domain,
            Some(_) => "redacted_domain",
            None => "overflow",
        };
        let canonical = &digest.canonical;
        let byte_array = &digest.byte_array;
        println!("CANONICAL_CONTENT_NATIVE_DIGEST sample={sample} mode={mode} phase={name} domain={domain} overflow={} canonical_calls={} canonical_cache_hits={} canonical_failures={} canonical_serialized_bytes={} canonical_hashed_bytes={} canonical_elapsed_us={} uncached_byte_array_calls={} uncached_byte_array_failures={} uncached_byte_array_serialized_bytes={} uncached_byte_array_hashed_bytes={} uncached_byte_array_elapsed_us={}",
            digest.domain.is_none(), canonical.calls, canonical.cache_hits, canonical.failures, canonical.serialized_bytes,
            canonical.hashed_bytes, canonical.elapsed.as_micros(), byte_array.calls, byte_array.failures,
            byte_array.serialized_bytes, byte_array.hashed_bytes, byte_array.elapsed.as_micros());
    }
    let value = result.unwrap_or_else(|_| panic!("public read failed; see metadata-only phase result"));
    let size = crate::digest::serialized_json_size(&value)
        .unwrap_or_else(|_| panic!("result size accounting failed"));
    assert!(size <= *remaining, "serialized workload outputs exceed the diagnostic bound");
    let bytes = serde_json::to_vec(&value).unwrap_or_else(|_| panic!("result serialization failed"));
    assert!(bytes.len() == size, "serialized result size changed");
    *remaining -= size;
    bytes
}

fn leg(config: &Config, sample: usize, legacy: bool, started: Instant) -> Vec<Vec<u8>> {
    config.frozen(started);
    let store = config.open_read_only();
    let version = unchanged_read_only(&store, None);
    let mut remaining = MAX_OUTPUT_BYTES;
    let mut outputs = Vec::with_capacity(3);
    outputs.push(phase(sample, legacy, "session", &mut remaining, started, || {
        let session = store.adaptive_session(config.session_id, &config.authority)?;
        assert!(session.as_ref().is_some_and(|session|
            session.grant.session_id == config.session_id && session.grant.authority.tenant_id == config.tenant),
            "explicit session unavailable or differently bound");
        Ok(session)
    }));
    unchanged_read_only(&store, Some(version));
    config.frozen(started);
    outputs.push(phase(sample, legacy, "funding_receipt", &mut remaining, started, || {
        let receipt = store.adaptive_work_funding(&config.tenant, config.session_id, config.funding_operation_id)?;
        assert!(receipt.is_some(), "explicit funding receipt unavailable");
        Ok(receipt)
    }));
    unchanged_read_only(&store, Some(version));
    config.frozen(started);
    outputs.push(phase(sample, legacy, "session_review_history", &mut remaining, started, || {
        let calls = store.adaptive_leadership_review_calls(&config.tenant, config.session_id)?;
        assert!(!calls.is_empty(), "explicit session has no retained leadership-review history");
        Ok(calls)
    }));
    unchanged_read_only(&store, Some(version));
    config.frozen(started);
    outputs
}

#[test]
#[ignore = "Explicit frozen protected-host RO snapshot A/B diagnostic; not runtime acceptance"]
fn diagnose_frozen_canonical_content_ab_read_only() {
    let started = Instant::now();
    let config = Config::read();
    println!("CANONICAL_CONTENT_NATIVE_BEGIN samples={} max_runtime_s={} max_output_bytes={} snapshot_bytes={} writes=0",
        config.samples, MAX_RUNTIME.as_secs(), MAX_OUTPUT_BYTES, config.db_bytes);
    for sample in 1..=config.samples {
        // Alternate order without flushing OS caches or mutating the host.
        let legacy_first = sample % 2 == 1;
        let first = leg(&config, sample, legacy_first, started);
        let second = leg(&config, sample, !legacy_first, started);
        assert!(first == second, "exact serialized workload outputs differ; no matched comparison permitted");
        println!("CANONICAL_CONTENT_NATIVE_COMPARE sample={sample} legacy_first={legacy_first} serialized_outputs_identical=true total_changes=0");
    }
    config.frozen(started);
    println!("CANONICAL_CONTENT_NATIVE_END elapsed_us={} total_changes=0", started.elapsed().as_micros());
}
