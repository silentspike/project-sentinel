//! Current installed repair evidence is distinct from historical model evidence.
use super::*;
use sentinel_workflow::AdaptiveRecoveryReleaseV1;
use std::os::fd::AsRawFd;

const REPAIR_FILE: &str = "/etc/sentinel/adaptive-recovery-release.json";
const MANIFEST_FILE: &str = "/opt/sentinel/release-manifest.json";
const GATEWAY_FILE: &str = "/opt/sentinel/bin/cortex-gateway";
const MAX_EVIDENCE_BYTES: u64 = 256 * 1024;
const MAX_GATEWAY_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepairEvidenceV1 {
    schema_version: u16,
    purpose: String,
    release: AdaptiveRecoveryReleaseV1,
    gate_evidence_digest: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledManifest {
    version: String,
    created_at: String,
    git_sha: String,
    artifacts: Vec<InstalledArtifact>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledArtifact {
    source: String,
    path: String,
    #[serde(rename = "type")]
    kind: String,
    sha256: String,
}

type FileIdentity = (u64, u64, u64, i64, i64, i64, i64, u32, u32, u64);

fn identity(metadata: &fs::Metadata) -> FileIdentity {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
        metadata.uid(),
        metadata.mode(),
        metadata.nlink(),
    )
}

fn protected_file(path: &Path, maximum: u64) -> Result<(fs::File, FileIdentity), &'static str> {
    if !path.is_absolute() {
        return Err("recovery evidence path invalid");
    }
    for parent in path.ancestors().skip(1) {
        let metadata =
            fs::symlink_metadata(parent).map_err(|_| "recovery evidence parent unavailable")?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
        {
            return Err("recovery evidence parent not protected");
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| "recovery evidence unavailable")?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.nlink() != 1
        || metadata.mode() & 0o022 != 0
        || metadata.len() == 0
        || metadata.len() > maximum
    {
        return Err("recovery evidence file not protected");
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(LINUX_O_NOFOLLOW | LINUX_O_CLOEXEC)
        .open(path)
        .map_err(|_| "recovery evidence open failed")?;
    let inspected = identity(&metadata);
    if identity(
        &file
            .metadata()
            .map_err(|_| "recovery evidence stat failed")?,
    ) != inspected
    {
        return Err("recovery evidence replaced during open");
    }
    Ok((file, inspected))
}

fn unchanged(path: &Path, file: &fs::File, expected: FileIdentity) -> Result<(), &'static str> {
    if identity(
        &file
            .metadata()
            .map_err(|_| "recovery evidence stat failed")?,
    ) != expected
        || identity(&fs::symlink_metadata(path).map_err(|_| "recovery evidence vanished")?)
            != expected
    {
        return Err("recovery evidence changed during read");
    }
    Ok(())
}

fn evidence_bytes(path: &Path) -> Result<Vec<u8>, &'static str> {
    let (mut file, before) = protected_file(path, MAX_EVIDENCE_BYTES)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_EVIDENCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "recovery evidence read failed")?;
    if bytes.len() as u64 > MAX_EVIDENCE_BYTES {
        return Err("recovery evidence oversized");
    }
    unchanged(path, &file, before)?;
    Ok(bytes)
}

fn gateway_digest() -> Result<String, &'static str> {
    let path = Path::new(GATEWAY_FILE);
    let (mut file, before) = protected_file(path, MAX_GATEWAY_BYTES)?;
    let pid = serving_gateway_pid()?;
    let process = fs::File::open(format!("/proc/{pid}"))
        .map_err(|_| "recovery serving process unavailable")?;
    let executable_path = format!("/proc/self/fd/{}/exe", process.as_raw_fd());
    // This kernel symlink is resolved relative to the pinned process directory.
    let executable = OpenOptions::new()
        .read(true)
        .custom_flags(LINUX_O_CLOEXEC)
        .open(&executable_path)
        .map_err(|_| "recovery serving executable unavailable")?;
    let running = executable
        .metadata()
        .map_err(|_| "recovery serving executable stat failed")?;
    validate_serving_identity(before, identity(&running))?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "recovery gateway read failed")?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .ok_or("recovery gateway oversized")?;
        if size > MAX_GATEWAY_BYTES {
            return Err("recovery gateway oversized");
        }
        digest.update(&buffer[..read]);
    }
    unchanged(path, &file, before)?;
    validate_serving_identity(
        before,
        identity(
            &executable
                .metadata()
                .map_err(|_| "recovery serving executable stat failed")?,
        ),
    )?;
    validate_serving_identity(
        before,
        identity(
            &fs::metadata(&executable_path).map_err(|_| "recovery serving executable changed")?,
        ),
    )?;
    if serving_gateway_pid()? != pid {
        return Err("recovery serving process changed");
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn parse_serving_pid(bytes: &[u8]) -> Result<u32, &'static str> {
    if bytes.len() > 16 {
        return Err("recovery serving PID invalid");
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "recovery serving PID invalid")?;
    let number = text.strip_suffix('\n').unwrap_or(text);
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("recovery serving PID invalid");
    }
    number
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or("recovery serving PID invalid")
}

fn serving_gateway_pid() -> Result<u32, &'static str> {
    let output = std::process::Command::new("/usr/bin/timeout")
        .args([
            "--signal=KILL",
            "3s",
            "/usr/bin/systemctl",
            "show",
            "sentinel-gateway.service",
            "--property=MainPID",
            "--value",
        ])
        .output()
        .map_err(|_| "recovery serving identity unavailable")?;
    if !output.status.success() {
        return Err("recovery serving identity unavailable");
    }
    parse_serving_pid(&output.stdout)
}

fn validate_serving_identity(
    installed: FileIdentity,
    running: FileIdentity,
) -> Result<(), &'static str> {
    if installed != running {
        return Err("recovery serving executable identity mismatch");
    }
    Ok(())
}

fn validate_evidence(
    repair: &[u8],
    manifest: &[u8],
    actual_gateway: &str,
) -> Result<(AdaptiveRecoveryReleaseV1, String), &'static str> {
    let proof: RepairEvidenceV1 =
        serde_json::from_slice(repair).map_err(|_| "recovery repair shape invalid")?;
    proof
        .release
        .validate()
        .map_err(|_| "recovery repair release invalid")?;
    if proof.schema_version != 1
        || proof.purpose != "adaptive-leadership-schema-repair"
        || proof.gate_evidence_digest.len() != 64
        || !proof
            .gate_evidence_digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("recovery repair binding invalid");
    }
    let installed: InstalledManifest =
        serde_json::from_slice(manifest).map_err(|_| "recovery installed manifest invalid")?;
    if installed.version != "1.0"
        || installed.created_at.is_empty()
        || installed.artifacts.is_empty()
        || installed.artifacts.len() > 4096
        || installed.git_sha != proof.release.source_git_sha
        || format!("{:x}", Sha256::digest(manifest)) != proof.release.release_manifest_digest
        || actual_gateway != proof.release.gateway_binary_digest
    {
        return Err("recovery installed release mismatch");
    }
    let matches: Vec<_> = installed
        .artifacts
        .iter()
        .filter(|row| row.source == "cmd/cortex-gateway/cortex-gateway" || row.path == GATEWAY_FILE)
        .collect();
    if matches.len() != 1
        || matches[0].source != "cmd/cortex-gateway/cortex-gateway"
        || matches[0].path != GATEWAY_FILE
        || matches[0].kind != "binary"
        || matches[0].sha256 != actual_gateway
    {
        return Err("recovery gateway artifact mismatch");
    }
    Ok((proof.release, format!("{:x}", Sha256::digest(repair))))
}

pub(super) fn verified_current_repair() -> Result<(AdaptiveRecoveryReleaseV1, String), &'static str>
{
    let repair = evidence_bytes(Path::new(REPAIR_FILE))?;
    let manifest = evidence_bytes(Path::new(MANIFEST_FILE))?;
    let actual_gateway = gateway_digest()?;
    let result = validate_evidence(&repair, &manifest, &actual_gateway)?;
    // Recheck the small root-owned inputs after hashing the executable.
    if evidence_bytes(Path::new(REPAIR_FILE))? != repair
        || evidence_bytes(Path::new(MANIFEST_FILE))? != manifest
    {
        return Err("recovery installed evidence changed");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (serde_json::Value, Vec<u8>, String) {
        let gateway = "c".repeat(64);
        let manifest = serde_json::to_vec(&serde_json::json!({
            "version":"1.0", "created_at":"2026-09-30", "git_sha":"a".repeat(40),
            "artifacts":[{"source":"cmd/cortex-gateway/cortex-gateway",
                "path":GATEWAY_FILE,"type":"binary","sha256":gateway}]
        }))
        .unwrap();
        let repair = serde_json::json!({
            "schema_version":1, "purpose":"adaptive-leadership-schema-repair",
            "release":{"schema_version":1,"source_git_sha":"a".repeat(40),
                "release_manifest_digest":format!("{:x}",Sha256::digest(&manifest)),
                "gateway_binary_digest":gateway},
            "gate_evidence_digest":"d".repeat(64)
        });
        (repair, manifest, gateway)
    }

    #[test]
    fn installed_repair_binds_manifest_bytes_and_actual_gateway() {
        let (repair, manifest, gateway) = fixture();
        let bytes = serde_json::to_vec(&repair).unwrap();
        let (release, digest) = validate_evidence(&bytes, &manifest, &gateway).unwrap();
        assert_eq!(release.gateway_binary_digest, gateway);
        assert_eq!(digest, format!("{:x}", Sha256::digest(&bytes)));
        let mut changed_manifest = manifest.clone();
        changed_manifest.push(b' ');
        assert!(validate_evidence(&bytes, &changed_manifest, &gateway).is_err());
        assert!(validate_evidence(&bytes, &manifest, &"f".repeat(64)).is_err());
    }

    #[test]
    fn installed_repair_rejects_invented_or_malformed_attestations() {
        let (repair, manifest, gateway) = fixture();
        for pointer in [
            "/purpose",
            "/schema_version",
            "/gate_evidence_digest",
            "/release/source_git_sha",
            "/release/release_manifest_digest",
            "/release/gateway_binary_digest",
        ] {
            let mut changed = repair.clone();
            *changed.pointer_mut(pointer).unwrap() = serde_json::json!("invalid");
            assert!(
                validate_evidence(&serde_json::to_vec(&changed).unwrap(), &manifest, &gateway)
                    .is_err(),
                "{pointer}"
            );
        }
        let mut changed = repair;
        changed["invented_authority"] = serde_json::json!(true);
        assert!(
            validate_evidence(&serde_json::to_vec(&changed).unwrap(), &manifest, &gateway).is_err()
        );
    }

    #[test]
    fn installed_repair_rejects_duplicate_and_foreign_gateway_entries() {
        let (repair, manifest, gateway) = fixture();
        let installed: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        for delta in ["duplicate", "source", "path", "type", "sha256"] {
            let mut changed = installed.clone();
            if delta == "duplicate" {
                let row = changed["artifacts"][0].clone();
                changed["artifacts"].as_array_mut().unwrap().push(row);
            } else {
                changed["artifacts"][0][delta] = serde_json::json!("foreign");
            }
            let changed_manifest = serde_json::to_vec(&changed).unwrap();
            let mut proof = repair.clone();
            proof["release"]["release_manifest_digest"] =
                serde_json::json!(format!("{:x}", Sha256::digest(&changed_manifest)));
            assert!(
                validate_evidence(
                    &serde_json::to_vec(&proof).unwrap(),
                    &changed_manifest,
                    &gateway
                )
                .is_err(),
                "{delta}"
            );
        }
    }

    #[test]
    fn installed_repair_requires_evidence_and_canonical_field_shapes() {
        let (mut repair, manifest, gateway) = fixture();
        repair["gate_evidence_digest"] = serde_json::json!("D".repeat(64));
        assert!(
            validate_evidence(&serde_json::to_vec(&repair).unwrap(), &manifest, &gateway).is_err()
        );
        assert!(validate_evidence(b"{}", &manifest, &gateway).is_err());
        assert!(validate_evidence(b"[]", &manifest, &gateway).is_err());
        assert!(protected_file(Path::new("relative-manifest"), 1024).is_err());
    }

    #[test]
    fn installed_repair_requires_a_positive_bounded_systemd_pid() {
        assert_eq!(parse_serving_pid(b"1234\n"), Ok(1234));
        assert_eq!(parse_serving_pid(b"1234"), Ok(1234));
        for bytes in [
            b"0\n".as_slice(),
            b"",
            b"-1",
            b"12\n13\n",
            b"123 \n",
            b"4294967296\n",
            b"12345678901234567",
            &[0xff],
        ] {
            assert!(parse_serving_pid(bytes).is_err());
        }
    }

    #[test]
    fn installed_repair_rejects_a_different_running_executable_even_with_identical_bytes() {
        let executable = fs::File::open("/proc/self/exe").unwrap();
        let original = identity(&executable.metadata().unwrap());
        assert!(validate_serving_identity(original, original).is_ok());
        let mut replaced = original;
        replaced.1 = replaced.1.wrapping_add(1);
        assert!(validate_serving_identity(original, replaced).is_err());
        replaced = original;
        replaced.0 = replaced.0.wrapping_add(1);
        assert!(validate_serving_identity(original, replaced).is_err());
        replaced = original;
        replaced.2 = replaced.2.wrapping_add(1);
        assert!(validate_serving_identity(original, replaced).is_err());
    }
}
