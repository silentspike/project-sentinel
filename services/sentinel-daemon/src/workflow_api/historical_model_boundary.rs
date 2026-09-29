//! Root-verified historical deployment evidence is distinct from model authority.
use super::*;

const BOUNDARY_FILE: &str = "/etc/sentinel/model-boundary-evidence.json";
const MAX_BOUNDARY_BYTES: u64 = 256 * 1024;
const MAX_BOUNDARY_RECORDS: usize = 32;
const PINNED_CLI_VERSION: &str = "0.151.0";
// Exact pinned Gateway profile, cmd/cortex-gateway/internal/proxy/codex_cli.go.
const DISABLED_FEATURES: [&str; 20] = [
    "apps", "auth_elicitation", "browser_use", "code_mode", "code_mode_host",
    "computer_use", "goals", "hooks", "image_generation", "memories", "multi_agent",
    "plugins", "shell_snapshot", "shell_snapshot_v2", "shell_tool", "skill_search",
    "tool_suggest", "unbounded_connection_retries", "view_image", "workspace_dependencies",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalBoundaryFile {
    schema_version: u16,
    records: Vec<HistoricalBoundaryRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalBoundaryRecord {
    request_id: String,
    request_digest: String,
    boundary: sentinel_limbo::LlmHistoricalInferenceBoundaryV1,
    proof: serde_json::Value,
}

// Missing fields must fail; deserialization never fills in historical evidence.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalCliProfile {
    cli_version: String,
    ephemeral: bool,
    strict_config: bool,
    ignore_user_config: bool,
    ignore_rules: bool,
    sandbox: String,
    shell_environment_inherit: String,
    request_max_retries: u32,
    stream_max_retries: u32,
    supports_websockets: bool,
    web_search: String,
    sentinel_tool_authority: bool,
    shell_tool: bool,
    code_mode: bool,
    disabled_features: Vec<String>,
}

fn canonical_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric()
            || matches!(byte, b'_' | b'.' | b':' | b'-'))
}

fn validate_cli_profile(profile: &serde_json::Value) -> Result<(), &'static str> {
    let profile: HistoricalCliProfile = serde_json::from_value(profile.clone())
        .map_err(|_| "historical CLI profile shape invalid")?;
    let disabled: BTreeSet<&str> = profile.disabled_features.iter().map(String::as_str).collect();
    if profile.cli_version != PINNED_CLI_VERSION || !profile.ephemeral
        || !profile.strict_config || !profile.ignore_user_config || !profile.ignore_rules
        || profile.sandbox != "read-only" || profile.shell_environment_inherit != "none"
        || profile.request_max_retries != 0 || profile.stream_max_retries != 0
        || profile.supports_websockets || profile.web_search != "disabled"
        || profile.sentinel_tool_authority || profile.shell_tool || profile.code_mode
        || profile.disabled_features.len() != DISABLED_FEATURES.len()
        || disabled.len() != DISABLED_FEATURES.len()
        || DISABLED_FEATURES.iter().any(|feature| !disabled.contains(feature))
    {
        return Err("historical CLI inference profile unsafe");
    }
    Ok(())
}

fn validate_record(record: &HistoricalBoundaryRecord) -> Result<(), &'static str> {
    let proof = &record.proof;
    let boundary = &record.boundary;
    let claim = proof.get("claimed_at_ms").and_then(serde_json::Value::as_u64)
        .ok_or("historical proof claim time missing")?;
    if !valid_request_id(&record.request_id) || !canonical_hex(&record.request_digest, 64)
        || !canonical_hex(&boundary.release_git_sha, 40)
        || boundary.valid_from_ms >= boundary.valid_until_ms
        || claim < boundary.valid_from_ms || claim >= boundary.valid_until_ms
        || i64::try_from(boundary.valid_until_ms).is_err()
    {
        return Err("historical boundary identity or interval invalid");
    }
    for digest in [&boundary.release_manifest_sha256, &boundary.gateway_binary_sha256,
        &boundary.cli_binary_sha256, &boundary.cli_profile_sha256, &boundary.boundary_receipt_sha256] {
        if !canonical_hex(digest, 64) {
            return Err("historical boundary hash invalid");
        }
    }
    let profile = proof.get("cli_profile").ok_or("historical CLI profile missing")?;
    validate_cli_profile(profile)?;
    let canonical = sentinel_common::canonical_json(proof).map_err(|_| "historical proof encoding invalid")?;
    let profile_bytes = sentinel_common::canonical_json(profile).map_err(|_| "historical profile encoding invalid")?;
    if sentinel_common::sha256_hex(&canonical) != boundary.boundary_receipt_sha256
        || sentinel_common::sha256_hex(&profile_bytes) != boundary.cli_profile_sha256
        || proof.get("provenance").and_then(|value| value.as_str()) != Some("retrospective_deployment_verification")
        || proof.get("request_id").and_then(|value| value.as_str()) != Some(record.request_id.as_str())
        || proof.get("request_digest").and_then(|value| value.as_str()) != Some(record.request_digest.as_str())
        || proof.get("release_git_sha").and_then(|value| value.as_str()) != Some(boundary.release_git_sha.as_str())
        || proof.get("release_manifest_sha256").and_then(|value| value.as_str()) != Some(boundary.release_manifest_sha256.as_str())
        || proof.get("gateway_binary_sha256").and_then(|value| value.as_str()) != Some(boundary.gateway_binary_sha256.as_str())
        || proof.get("cli_binary_sha256").and_then(|value| value.as_str()) != Some(boundary.cli_binary_sha256.as_str())
        || proof.get("valid_from_ms").and_then(serde_json::Value::as_u64) != Some(boundary.valid_from_ms)
        || proof.get("valid_until_ms").and_then(serde_json::Value::as_u64) != Some(boundary.valid_until_ms)
    {
        return Err("historical boundary proof binding invalid");
    }
    Ok(())
}

/// Pure validation checks the whole bounded file, not just the selected record.
/// Protected root publication is the trust boundary; hashes alone prove no isolation.
fn validate_boundary_records(
    bytes: &[u8], request_id: &str, request_digest: &str, claimed_at_ms: u64,
) -> Result<Option<sentinel_limbo::LlmHistoricalInferenceBoundaryV1>, &'static str> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_BOUNDARY_BYTES
        || !valid_request_id(request_id) || !canonical_hex(request_digest, 64)
        || i64::try_from(claimed_at_ms).is_err()
    {
        return Err("historical boundary input or byte bound invalid");
    }
    let records: HistoricalBoundaryFile = serde_json::from_slice(bytes)
        .map_err(|_| "historical boundary JSON invalid")?;
    if records.schema_version != 1 || records.records.len() > MAX_BOUNDARY_RECORDS {
        return Err("historical boundary schema or bound invalid");
    }
    let mut seen = BTreeSet::new();
    let mut selected = None;
    for record in records.records {
        if !seen.insert(record.request_id.clone()) {
            return Err("historical boundary duplicate request");
        }
        validate_record(&record)?;
        if record.request_id != request_id {
            continue;
        }
        if record.request_digest != request_digest
            || record.proof.get("claimed_at_ms").and_then(serde_json::Value::as_u64) != Some(claimed_at_ms)
            || claimed_at_ms < record.boundary.valid_from_ms
            || claimed_at_ms >= record.boundary.valid_until_ms
        {
            return Err("historical boundary request or claim conflict");
        }
        selected = Some(record.boundary);
    }
    Ok(selected)
}

pub(super) fn verified_historical_boundary(
    request_id: &str,
    request_digest: &str,
    claimed_at_ms: u64,
) -> Result<Option<sentinel_limbo::LlmHistoricalInferenceBoundaryV1>, &'static str> {
    let path = Path::new(BOUNDARY_FILE);
    let inspected = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("historical boundary unavailable"),
    };
    for parent in [Path::new("/etc"), Path::new("/etc/sentinel")] {
        let metadata = fs::symlink_metadata(parent).map_err(|_| "historical boundary parent unavailable")?;
        if !metadata.is_dir() || metadata.file_type().is_symlink()
            || metadata.uid() != 0 || metadata.mode() & 0o022 != 0
        {
            return Err("historical boundary parent not protected");
        }
    }
    if !inspected.is_file() || inspected.file_type().is_symlink() || inspected.nlink() != 1
        || inspected.uid() != 0 || inspected.mode() & 0o022 != 0
        || inspected.len() == 0 || inspected.len() > MAX_BOUNDARY_BYTES
    {
        return Err("historical boundary file not protected");
    }
    let mut file = OpenOptions::new().read(true)
        .custom_flags(LINUX_O_NOFOLLOW | LINUX_O_CLOEXEC).open(path)
        .map_err(|_| "historical boundary open failed")?;
    let identity = |metadata: &fs::Metadata| (
        metadata.dev(), metadata.ino(), metadata.len(), metadata.mtime(), metadata.mtime_nsec(),
        metadata.ctime(), metadata.ctime_nsec(), metadata.uid(), metadata.mode(), metadata.nlink(),
    );
    if identity(&file.metadata().map_err(|_| "historical boundary stat failed")?) != identity(&inspected) {
        return Err("historical boundary replaced during open");
    }
    let mut bytes = Vec::new();
    file.by_ref().take(MAX_BOUNDARY_BYTES + 1).read_to_end(&mut bytes)
        .map_err(|_| "historical boundary read failed")?;
    if bytes.len() as u64 > MAX_BOUNDARY_BYTES
        || identity(&file.metadata().map_err(|_| "historical boundary restat failed")?) != identity(&inspected)
        || identity(&fs::symlink_metadata(path).map_err(|_| "historical boundary vanished")?) != identity(&inspected)
    {
        return Err("historical boundary changed during read");
    }
    validate_boundary_records(&bytes, request_id, request_digest, claimed_at_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic receipts only. No fixture attests an actual deployed boundary.
    fn fixture() -> serde_json::Value {
        let profile = serde_json::json!({
            "cli_version": PINNED_CLI_VERSION, "ephemeral": true,
            "strict_config": true, "ignore_user_config": true, "ignore_rules": true,
            "sandbox": "read-only", "shell_environment_inherit": "none",
            "request_max_retries": 0, "stream_max_retries": 0, "supports_websockets": false,
            "web_search": "disabled", "sentinel_tool_authority": false, "shell_tool": false,
            "code_mode": false, "disabled_features": DISABLED_FEATURES,
        });
        let proof = serde_json::json!({
            "provenance": "retrospective_deployment_verification",
            "request_id": "company-adaptive-fixture", "request_digest": "a".repeat(64),
            "release_git_sha": "b".repeat(40), "release_manifest_sha256": "c".repeat(64),
            "gateway_binary_sha256": "d".repeat(64), "cli_binary_sha256": "e".repeat(64),
            "claimed_at_ms": 150, "valid_from_ms": 100, "valid_until_ms": 200,
            "cli_profile": profile,
        });
        let record = serde_json::json!({
            "request_id": "company-adaptive-fixture", "request_digest": "a".repeat(64),
            "boundary": {
                "release_git_sha": "b".repeat(40), "release_manifest_sha256": "c".repeat(64),
                "gateway_binary_sha256": "d".repeat(64), "cli_binary_sha256": "e".repeat(64),
                "cli_profile_sha256": "f".repeat(64), "boundary_receipt_sha256": "f".repeat(64),
                "valid_from_ms": 100, "valid_until_ms": 200,
            }, "proof": proof,
        });
        let mut file = serde_json::json!({"schema_version": 1, "records": [record]});
        rehash(&mut file["records"][0]);
        file
    }

    fn rehash(record: &mut serde_json::Value) {
        record["boundary"]["cli_profile_sha256"] = serde_json::json!(sentinel_common::sha256_hex(
            &sentinel_common::canonical_json(&record["proof"]["cli_profile"]).unwrap()));
        record["boundary"]["boundary_receipt_sha256"] = serde_json::json!(sentinel_common::sha256_hex(
            &sentinel_common::canonical_json(&record["proof"]).unwrap()));
    }

    fn validate(file: &serde_json::Value) -> Result<Option<sentinel_limbo::LlmHistoricalInferenceBoundaryV1>, &'static str> {
        validate_boundary_records(&serde_json::to_vec(file).unwrap(), "company-adaptive-fixture", &"a".repeat(64), 150)
    }

    #[test]
    fn valid_receipt_is_exact_and_claim_interval_is_half_open() {
        let file = fixture();
        let expected = serde_json::from_value(file["records"][0]["boundary"].clone()).unwrap();
        assert_eq!(validate(&file).unwrap(), Some(expected));
        for claim in [100, 199] {
            let mut changed = file.clone();
            changed["records"][0]["proof"]["claimed_at_ms"] = serde_json::json!(claim);
            rehash(&mut changed["records"][0]);
            let bytes = serde_json::to_vec(&changed).unwrap();
            assert!(validate_boundary_records(&bytes, "company-adaptive-fixture", &"a".repeat(64), claim).unwrap().is_some());
        }
        for claim in [99, 151, 200, u64::MAX] {
            let bytes = serde_json::to_vec(&file).unwrap();
            assert!(validate_boundary_records(&bytes, "company-adaptive-fixture", &"a".repeat(64), claim).is_err());
        }
        for claim in [99, 200] {
            let mut changed = file.clone();
            changed["records"][0]["proof"]["claimed_at_ms"] = serde_json::json!(claim);
            rehash(&mut changed["records"][0]);
            assert!(validate_boundary_records(&serde_json::to_vec(&changed).unwrap(),
                "company-adaptive-fixture", &"a".repeat(64), claim).is_err());
        }
        let bytes = serde_json::to_vec(&file).unwrap();
        assert!(validate_boundary_records(&bytes, "absent-request", &"a".repeat(64), 150).unwrap().is_none());
    }

    #[test]
    fn request_digest_and_proof_profile_hash_mismatch_fail_closed() {
        let file = fixture();
        let bytes = serde_json::to_vec(&file).unwrap();
        assert!(validate_boundary_records(&bytes, "company-adaptive-fixture", &"b".repeat(64), 150).is_err());
        for pointer in ["/request_id", "/request_digest", "/release_git_sha", "/release_manifest_sha256",
            "/gateway_binary_sha256", "/cli_binary_sha256", "/provenance"] {
            let mut changed = file.clone();
            *changed["records"][0]["proof"].pointer_mut(pointer).unwrap() = serde_json::json!("different");
            rehash(&mut changed["records"][0]);
            assert!(validate(&changed).is_err(), "{pointer}");
        }
        for field in ["boundary_receipt_sha256", "cli_profile_sha256", "release_manifest_sha256",
            "gateway_binary_sha256", "cli_binary_sha256", "release_git_sha"] {
            let mut changed = file.clone();
            changed["records"][0]["boundary"][field] = serde_json::json!("a".repeat(if field == "release_git_sha" { 40 } else { 64 }));
            assert!(validate(&changed).is_err(), "{field}");
            changed["records"][0]["boundary"][field] = serde_json::json!("invalid-hash");
            assert!(validate(&changed).is_err(), "{field}");
        }
        for field in ["claimed_at_ms", "valid_from_ms", "valid_until_ms"] {
            let mut changed = file.clone();
            changed["records"][0]["proof"][field] = serde_json::json!(0);
            rehash(&mut changed["records"][0]);
            assert!(validate(&changed).is_err(), "{field}");
        }
        let mut changed = file;
        changed["records"][0]["request_id"] = serde_json::json!("borrowed-request");
        assert!(validate(&changed).is_err());
    }

    #[test]
    fn unsafe_or_missing_profile_fields_fail_even_with_consistent_hashes() {
        let file = fixture();
        for (field, unsafe_value) in [
            ("cli_version", serde_json::json!("latest")), ("ephemeral", serde_json::json!(false)),
            ("strict_config", serde_json::json!(false)), ("ignore_user_config", serde_json::json!(false)),
            ("ignore_rules", serde_json::json!(false)), ("sandbox", serde_json::json!("workspace-write")),
            ("shell_environment_inherit", serde_json::json!("all")), ("request_max_retries", serde_json::json!(1)),
            ("stream_max_retries", serde_json::json!(1)), ("supports_websockets", serde_json::json!(true)),
            ("web_search", serde_json::json!("live")), ("sentinel_tool_authority", serde_json::json!(true)),
            ("shell_tool", serde_json::json!(true)), ("code_mode", serde_json::json!(true)),
        ] {
            let mut changed = file.clone();
            changed["records"][0]["proof"]["cli_profile"][field] = unsafe_value;
            rehash(&mut changed["records"][0]);
            assert!(validate(&changed).is_err(), "{field}");
            changed["records"][0]["proof"]["cli_profile"].as_object_mut().unwrap().remove(field);
            rehash(&mut changed["records"][0]);
            assert!(validate(&changed).is_err(), "missing {field}");
        }
        let mut changed = file;
        changed["records"][0]["proof"]["cli_profile"]["unreviewed_override"] = serde_json::json!(true);
        rehash(&mut changed["records"][0]);
        assert!(validate(&changed).is_err());
    }

    #[test]
    fn every_pinned_feature_is_required_once_without_unknown_replacements() {
        let file = fixture();
        for index in 0..DISABLED_FEATURES.len() {
            let mut changed = file.clone();
            changed["records"][0]["proof"]["cli_profile"]["disabled_features"].as_array_mut().unwrap().remove(index);
            rehash(&mut changed["records"][0]);
            assert!(validate(&changed).is_err(), "missing {}", DISABLED_FEATURES[index]);
            let mut changed = file.clone();
            changed["records"][0]["proof"]["cli_profile"]["disabled_features"][index] = serde_json::json!("unknown_feature");
            rehash(&mut changed["records"][0]);
            assert!(validate(&changed).is_err());
        }
        let mut changed = file.clone();
        changed["records"][0]["proof"]["cli_profile"]["disabled_features"][1] = serde_json::json!(DISABLED_FEATURES[0]);
        rehash(&mut changed["records"][0]);
        assert!(validate(&changed).is_err());
        let mut reordered = file;
        reordered["records"][0]["proof"]["cli_profile"]["disabled_features"].as_array_mut().unwrap().reverse();
        rehash(&mut reordered["records"][0]);
        assert!(validate(&reordered).unwrap().is_some());
    }

    #[test]
    fn duplicated_and_unselected_invalid_records_poison_the_file() {
        let mut file = fixture();
        let record = file["records"][0].clone();
        file["records"].as_array_mut().unwrap().push(record.clone());
        assert!(validate(&file).is_err());
        let mut other = record;
        other["request_id"] = serde_json::json!("other-request");
        other["proof"]["request_id"] = serde_json::json!("other-request");
        other["proof"]["cli_profile"]["ignore_rules"] = serde_json::json!(false);
        rehash(&mut other);
        file["records"][1] = other;
        assert!(validate(&file).is_err());
        file["records"].as_array_mut().unwrap().remove(0);
        assert!(validate(&file).is_err());
    }

    #[test]
    fn byte_record_schema_and_identity_bounds_have_no_defaults() {
        let mut file = fixture();
        let original = file["records"][0].clone();
        let mut records = Vec::new();
        for index in 0..MAX_BOUNDARY_RECORDS {
            let mut record = original.clone();
            let request = if index == 0 { "company-adaptive-fixture".to_owned() } else { format!("request-{index}") };
            record["request_id"] = serde_json::json!(request);
            record["proof"]["request_id"] = record["request_id"].clone();
            rehash(&mut record);
            records.push(record);
        }
        file["records"] = serde_json::json!(records);
        assert!(validate(&file).unwrap().is_some());
        file["records"].as_array_mut().unwrap().push(original);
        assert!(validate(&file).is_err());
        assert!(validate_boundary_records(&vec![b' '; MAX_BOUNDARY_BYTES as usize + 1],
            "company-adaptive-fixture", &"a".repeat(64), 150).is_err());
        assert!(validate_boundary_records(&[], "company-adaptive-fixture", &"a".repeat(64), 150).is_err());
        let mut padded = serde_json::to_vec(&fixture()).unwrap();
        padded.resize(MAX_BOUNDARY_BYTES as usize, b' ');
        assert!(validate_boundary_records(&padded, "company-adaptive-fixture", &"a".repeat(64), 150).unwrap().is_some());
        for schema in [0, 2] {
            let mut changed = fixture();
            changed["schema_version"] = serde_json::json!(schema);
            assert!(validate(&changed).is_err());
        }
        for pointer in ["/proof", "/proof/cli_profile", "/proof/provenance"] {
            let mut changed = fixture();
            *changed["records"][0].pointer_mut(pointer).unwrap() = serde_json::Value::Null;
            assert!(validate(&changed).is_err());
        }
        for field in ["claimed_at_ms", "valid_from_ms", "valid_until_ms"] {
            let mut changed = fixture();
            changed["records"][0]["proof"].as_object_mut().unwrap().remove(field);
            rehash(&mut changed["records"][0]);
            assert!(validate(&changed).is_err(), "missing {field}");
        }
        for request in ["".to_owned(), "a".repeat(129), "request/unsafe".to_owned()] {
            assert!(validate_boundary_records(&serde_json::to_vec(&fixture()).unwrap(), &request, &"a".repeat(64), 150).is_err());
        }
        let mut changed = fixture();
        changed["records"][0]["boundary"]["valid_from_ms"] = serde_json::json!(200);
        assert!(validate(&changed).is_err());
        changed["records"][0]["boundary"]["valid_until_ms"] = serde_json::json!(u64::MAX);
        assert!(validate(&changed).is_err());
    }
}
