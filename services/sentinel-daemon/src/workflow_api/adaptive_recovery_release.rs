//! Current installed repair evidence is distinct from historical model evidence.
use super::*;
use sentinel_workflow::{
    adaptive_leadership_admission_repair_history_digest,
    AdaptiveLeadershipAdmissionRepairEvidenceV1, AdaptiveLeadershipAdmissionRepairSourceV1,
    AdaptiveRecoveryReleaseV1,
};
use std::os::fd::AsRawFd;

const REPAIR_FILE: &str = "/etc/sentinel/adaptive-recovery-release.json";
const MANIFEST_FILE: &str = "/opt/sentinel/release-manifest.json";
const GATEWAY_FILE: &str = "/opt/sentinel/bin/cortex-gateway";
const DAEMON_FILE: &str = "/opt/sentinel/bin/sentinel-daemon";
const ADMISSION_REPAIR_DIRECTORY: &str = "/etc/sentinel/adaptive-admission-repair";
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProtectedRepairEvidenceV1 {
    pub path: PathBuf,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdmissionRepairAttestationV1 {
    pub schema_version: u16,
    pub purpose: String,
    pub release: AdaptiveRecoveryReleaseV1,
    pub daemon_binary_digest: String,
    pub failed_release: AdaptiveRecoveryReleaseV1,
    pub failed_daemon_binary_digest: String,
    pub source_digest: String,
    pub inventory_digest: String,
    pub gate_evidence: ProtectedRepairEvidenceV1,
    pub admission_ordering: ProtectedRepairEvidenceV1,
    pub failure_evidence: ProtectedRepairEvidenceV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdmissionOrderingEvidenceV1 {
    pub schema_version: u16,
    pub purpose: String,
    pub failed_release: AdaptiveRecoveryReleaseV1,
    pub daemon_binary_digest: String,
    pub valid_from_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub ordering: String,
    pub fencing: String,
    pub source_evidence: ProtectedRepairEvidenceV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdmissionFailureEvidenceV1 {
    pub schema_version: u16,
    pub purpose: String,
    pub source_digest: String,
    pub inventory_digest: String,
    pub receipts: Vec<AdmissionNoIoReceiptV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdmissionNoIoReceiptV1 {
    pub review_id: Uuid,
    pub request_id: String,
    pub authority_digest: String,
    pub context_digest: String,
    pub retired_call_digest: String,
    pub retired_at_unix_ms: u64,
    pub proof: AdmissionNoIoProofV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum AdmissionNoIoProofV1 {
    FencedUndispatchedRetirement {
        historical_deployment: ProtectedRepairEvidenceV1,
        retirement_evidence: ProtectedRepairEvidenceV1,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoricalAdmissionDeploymentV1 {
    pub schema_version: u16,
    pub purpose: String,
    pub release: AdaptiveRecoveryReleaseV1,
    pub daemon_binary_digest: String,
    pub valid_from_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub manifest: ProtectedRepairEvidenceV1,
    pub gateway_serving_identity: ProtectedRepairEvidenceV1,
    pub daemon_serving_identity: ProtectedRepairEvidenceV1,
}

// A safe export of verified store facts, not a claimed provider receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetiredAdmissionEvidenceV1 {
    pub schema_version: u16,
    pub purpose: String,
    pub review_id: Uuid,
    pub request_id: String,
    pub authority_digest: String,
    pub context_digest: String,
    pub retired_call_digest: String,
    pub issued_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    pub retired_at_unix_ms: u64,
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn json_digest(value: &impl Serialize) -> Result<String, &'static str> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(value).map_err(|_| "admission repair evidence encoding failed")?,
        )
    ))
}

pub(crate) fn retired_admission_evidence(
    call: &sentinel_workflow::AdaptiveLeadershipReviewCallV1,
) -> Result<RetiredAdmissionEvidenceV1, &'static str> {
    let authority = super::model_execution::ProviderExecutionAuthority::AdaptiveLeadershipReview(
        Box::new(super::adaptive_leadership_review::LeadershipAuthority::from_call(call)),
    );
    Ok(RetiredAdmissionEvidenceV1 {
        schema_version: 1,
        purpose: "verified-store-undispatched-retirement".into(),
        review_id: call.grant.review_id,
        request_id: call.request_id(),
        authority_digest: json_digest(&authority)?,
        context_digest: call
            .context_digest()
            .map_err(|_| "admission call context invalid")?,
        retired_call_digest: json_digest(call)?,
        issued_at_unix_ms: call.grant_issued_at_unix_ms,
        expires_at_unix_ms: call.grant.expires_at_unix_ms,
        retired_at_unix_ms: call
            .retired_at_unix_ms
            .ok_or("admission call not retired")?,
    })
}

fn referenced_evidence(reference: &ProtectedRepairEvidenceV1) -> Result<Vec<u8>, &'static str> {
    if !is_digest(&reference.digest) {
        return Err("admission repair evidence digest invalid");
    }
    let bytes = evidence_bytes(&reference.path)?;
    if format!("{:x}", Sha256::digest(&bytes)) != reference.digest {
        return Err("admission repair evidence changed");
    }
    Ok(bytes)
}

fn require_manifest_binary(
    manifest: &InstalledManifest,
    path: &str,
    source: &str,
    digest: &str,
) -> Result<(), &'static str> {
    let rows: Vec<_> = manifest
        .artifacts
        .iter()
        .filter(|row| row.path == path || row.source == source)
        .collect();
    if rows.len() != 1
        || rows[0].path != path
        || rows[0].source != source
        || rows[0].kind != "binary"
        || rows[0].sha256 != digest
    {
        return Err("admission repair binary artifact mismatch");
    }
    Ok(())
}

pub(crate) fn validate_admission_repair(
    repair: &[u8],
    manifest: &[u8],
    actual_gateway: &str,
    actual_daemon: &str,
    source: &AdaptiveLeadershipAdmissionRepairSourceV1,
    loader: impl Fn(&ProtectedRepairEvidenceV1) -> Result<Vec<u8>, &'static str>,
) -> Result<(AdaptiveLeadershipAdmissionRepairEvidenceV1, Vec<Uuid>), &'static str> {
    let read = |reference: &ProtectedRepairEvidenceV1| {
        if !reference.path.is_absolute() || !is_digest(&reference.digest) {
            return Err("admission repair reference invalid");
        }
        let bytes = loader(reference)?;
        if bytes.is_empty()
            || bytes.len() as u64 > MAX_EVIDENCE_BYTES
            || format!("{:x}", Sha256::digest(&bytes)) != reference.digest
        {
            return Err("admission repair referenced bytes changed");
        }
        Ok(bytes)
    };
    let proof: AdmissionRepairAttestationV1 =
        serde_json::from_slice(repair).map_err(|_| "admission repair attestation shape invalid")?;
    proof
        .release
        .validate()
        .map_err(|_| "admission repair release invalid")?;
    proof
        .failed_release
        .validate()
        .map_err(|_| "admission failed release invalid")?;
    let source_digest = source
        .canonical_digest()
        .map_err(|_| "admission source digest invalid")?;
    let inventory_digest = adaptive_leadership_admission_repair_history_digest(&source.calls)
        .map_err(|_| "admission inventory invalid")?;
    if proof.schema_version != 1
        || proof.purpose != "adaptive-leadership-admission-repair"
        || proof.source_digest != source_digest
        || proof.inventory_digest != inventory_digest
        || proof.release.source_git_sha == proof.failed_release.source_git_sha
        || proof.release.gateway_binary_digest == proof.failed_release.gateway_binary_digest
        || proof.daemon_binary_digest == proof.failed_daemon_binary_digest
        || !is_digest(&proof.daemon_binary_digest)
        || !is_digest(&proof.failed_daemon_binary_digest)
        || proof.release.gateway_binary_digest != actual_gateway
        || proof.daemon_binary_digest != actual_daemon
    {
        return Err("admission repair attestation binding invalid");
    }
    let installed: InstalledManifest =
        serde_json::from_slice(manifest).map_err(|_| "admission repair manifest invalid")?;
    if installed.version != "1.0"
        || installed.created_at.is_empty()
        || installed.artifacts.is_empty()
        || installed.artifacts.len() > 4096
        || installed.git_sha != proof.release.source_git_sha
        || format!("{:x}", Sha256::digest(manifest)) != proof.release.release_manifest_digest
    {
        return Err("admission repair installed release mismatch");
    }
    require_manifest_binary(
        &installed,
        GATEWAY_FILE,
        "cmd/cortex-gateway/cortex-gateway",
        actual_gateway,
    )?;
    require_manifest_binary(
        &installed,
        DAEMON_FILE,
        "target/release/sentinel-daemon",
        actual_daemon,
    )?;
    // Root-protected evidence files are immutable by digest, not client assertions.
    let gate: serde_json::Value = serde_json::from_slice(&read(&proof.gate_evidence)?)
        .map_err(|_| "admission repair gate evidence invalid")?;
    if gate.get("purpose").and_then(|value| value.as_str()) != Some("exact-release-gate-evidence")
        || gate.get("merged_commit").and_then(|value| value.as_str())
            != Some(proof.release.source_git_sha.as_str())
        || gate
            .get("merged_tree_equals_reviewed_tree")
            .and_then(|value| value.as_bool())
            != Some(true)
        || gate.pointer("/ci/status").and_then(|value| value.as_str()) != Some("completed")
        || gate
            .pointer("/ci/conclusion")
            .and_then(|value| value.as_str())
            != Some("success")
        || gate
            .pointer("/release/result")
            .and_then(|value| value.as_str())
            != Some("PASS")
        || gate
            .pointer("/release/gateway_sha256")
            .and_then(|value| value.as_str())
            != Some(actual_gateway)
        || gate
            .pointer("/release/daemon_sha256")
            .and_then(|value| value.as_str())
            != Some(actual_daemon)
    {
        return Err("admission repair gate binding invalid");
    }
    let ordering: AdmissionOrderingEvidenceV1 =
        serde_json::from_slice(&read(&proof.admission_ordering)?)
            .map_err(|_| "admission ordering evidence invalid")?;
    if ordering.schema_version != 1
        || ordering.purpose != "verified-leadership-admission-ordering"
        || ordering.failed_release != proof.failed_release
        || ordering.daemon_binary_digest != proof.failed_daemon_binary_digest
        || ordering.ordering != "durable-leadership-dispatch-before-provider-io"
        || ordering.fencing != "no-provider-io-after-rejected-or-retired-dispatch"
        || ordering.valid_from_unix_ms == 0
        || ordering.valid_from_unix_ms >= ordering.valid_until_unix_ms
    {
        return Err("admission ordering release binding invalid");
    }
    read(&ordering.source_evidence)?;
    let failures: AdmissionFailureEvidenceV1 =
        serde_json::from_slice(&read(&proof.failure_evidence)?)
            .map_err(|_| "admission failure evidence invalid")?;
    if failures.schema_version != 1
        || failures.purpose != "verified-retired-leadership-no-io"
        || failures.source_digest != source_digest
        || failures.inventory_digest != inventory_digest
        || failures.receipts.is_empty()
        || failures.receipts.len() > source.qualifying_review_ids.len()
    {
        return Err("admission failure inventory mismatch");
    }
    let mut seen = BTreeSet::new();
    for receipt in &failures.receipts {
        if !seen.insert(receipt.review_id)
            || !source.qualifying_review_ids.contains(&receipt.review_id)
        {
            return Err("admission no-I/O identity invalid");
        }
        let call = source
            .calls
            .iter()
            .find(|call| call.grant.review_id == receipt.review_id)
            .ok_or("admission no-I/O call missing")?;
        let authority =
            super::model_execution::ProviderExecutionAuthority::AdaptiveLeadershipReview(Box::new(
                super::adaptive_leadership_review::LeadershipAuthority::from_call(call),
            ));
        if call.grant.schema_version != 3
            || call.grant.recovery_epoch.is_some()
            || call.context.source_session != source.session
            || call.context.source_project != source.project
            || call.dispatch.is_some()
            || call.decision.is_some()
            || call.continuation.is_some()
            || call.model_response_digest.is_some()
            || call.resolution_event_id.is_some()
            || call.retired_at_unix_ms != Some(receipt.retired_at_unix_ms)
            || receipt.retired_at_unix_ms < call.grant.expires_at_unix_ms
            || call.grant_issued_at_unix_ms < ordering.valid_from_unix_ms
            || receipt.retired_at_unix_ms > ordering.valid_until_unix_ms
            || receipt.request_id != call.request_id()
            || receipt.context_digest
                != call
                    .context_digest()
                    .map_err(|_| "admission call context invalid")?
            || receipt.authority_digest != json_digest(&authority)?
            || receipt.retired_call_digest != json_digest(call)?
        {
            return Err("admission sealed no-I/O binding invalid");
        }
        match &receipt.proof {
            AdmissionNoIoProofV1::FencedUndispatchedRetirement {
                historical_deployment,
                retirement_evidence,
            } => {
                let deployed: HistoricalAdmissionDeploymentV1 =
                    serde_json::from_slice(&read(historical_deployment)?)
                        .map_err(|_| "admission historical deployment evidence invalid")?;
                if deployed.schema_version != 1
                    || deployed.purpose != "verified-historical-serving-release"
                    || deployed.release != proof.failed_release
                    || deployed.daemon_binary_digest != proof.failed_daemon_binary_digest
                    || deployed.valid_from_unix_ms == 0
                    || deployed.valid_from_unix_ms > call.grant_issued_at_unix_ms
                    || deployed.valid_until_unix_ms < receipt.retired_at_unix_ms
                    || deployed.valid_from_unix_ms >= deployed.valid_until_unix_ms
                {
                    return Err("admission historical serving release unproven");
                }
                let manifest = read(&deployed.manifest)?;
                let historical: InstalledManifest = serde_json::from_slice(&manifest)
                    .map_err(|_| "admission historical manifest invalid")?;
                if historical.version != "1.0"
                    || historical.created_at.is_empty()
                    || historical.artifacts.is_empty()
                    || historical.artifacts.len() > 4096
                    || historical.git_sha != deployed.release.source_git_sha
                    || deployed.manifest.digest != deployed.release.release_manifest_digest
                {
                    return Err("admission historical manifest binding invalid");
                }
                require_manifest_binary(
                    &historical,
                    GATEWAY_FILE,
                    "cmd/cortex-gateway/cortex-gateway",
                    &deployed.release.gateway_binary_digest,
                )?;
                require_manifest_binary(
                    &historical,
                    DAEMON_FILE,
                    "target/release/sentinel-daemon",
                    &deployed.daemon_binary_digest,
                )?;
                read(&deployed.gateway_serving_identity)?;
                read(&deployed.daemon_serving_identity)?;
                let retired: RetiredAdmissionEvidenceV1 =
                    serde_json::from_slice(&read(retirement_evidence)?)
                        .map_err(|_| "admission retirement evidence invalid")?;
                if retired != retired_admission_evidence(call)? {
                    return Err("admission fenced retirement changed");
                }
            }
        }
    }
    let attested_review_ids: Vec<_> = seen.into_iter().collect();
    Ok((
        AdaptiveLeadershipAdmissionRepairEvidenceV1 {
            source_digest,
            disposition_digest: proof.failure_evidence.digest,
            repair_digest: format!("{:x}", Sha256::digest(repair)),
            release: proof.release,
            failed_release: proof.failed_release,
            attested_review_ids: attested_review_ids.clone(),
        },
        attested_review_ids,
    ))
}

/// File-only verifier: safe inside the WorkflowStore issuance transaction.
/// It never acquires EventStore or reconstructs a provider effect from absence.
pub(crate) fn verified_current_admission_repair(
    source: &AdaptiveLeadershipAdmissionRepairSourceV1,
) -> Result<AdaptiveLeadershipAdmissionRepairEvidenceV1, &'static str> {
    verified_current_admission_repair_with_reviews(source).map(|(evidence, _)| evidence)
}

pub(crate) fn verified_current_admission_repair_with_reviews(
    source: &AdaptiveLeadershipAdmissionRepairSourceV1,
) -> Result<(AdaptiveLeadershipAdmissionRepairEvidenceV1, Vec<Uuid>), &'static str> {
    #[cfg(test)]
    if let Some(value) =
        TEST_ADMISSION_REPAIRS.with(|stack| stack.borrow().last().map(|(_, value)| value.clone()))
    {
        return validate_admission_repair(
            &value.repair,
            &value.manifest,
            &value.gateway,
            &value.daemon,
            source,
            |reference| {
                value
                    .documents
                    .get(&reference.path)
                    .cloned()
                    .ok_or("test admission evidence missing")
            },
        );
    }
    let path = Path::new(ADMISSION_REPAIR_DIRECTORY)
        .join(format!("{}.json", source.session.grant.session_id));
    let repair = evidence_bytes(&path)?;
    let manifest = evidence_bytes(Path::new(MANIFEST_FILE))?;
    let gateway = gateway_digest()?;
    let daemon = serving_binary_digest(Path::new(DAEMON_FILE), "sentinel-daemon.service")?;
    let value = validate_admission_repair(
        &repair,
        &manifest,
        &gateway,
        &daemon,
        source,
        referenced_evidence,
    )?;
    if evidence_bytes(&path)? != repair
        || evidence_bytes(Path::new(MANIFEST_FILE))? != manifest
        || gateway_digest()? != gateway
        || serving_binary_digest(Path::new(DAEMON_FILE), "sentinel-daemon.service")? != daemon
    {
        return Err("admission repair serving evidence changed");
    }
    Ok(value)
}

#[cfg(test)]
#[derive(Clone)]
struct TestAdmissionRepairEvidence {
    repair: Vec<u8>,
    manifest: Vec<u8>,
    gateway: String,
    daemon: String,
    documents: HashMap<PathBuf, Vec<u8>>,
}

#[cfg(test)]
std::thread_local! {
    static TEST_ADMISSION_REPAIRS: std::cell::RefCell<Vec<(u64, TestAdmissionRepairEvidence)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// File substitution only; full source, proof and grant validation remain active.
#[cfg(test)]
pub(crate) struct TestAdmissionRepairGuard {
    id: u64,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(test)]
impl TestAdmissionRepairGuard {
    pub(crate) fn fixture(source: &AdaptiveLeadershipAdmissionRepairSourceV1) -> Self {
        let gateway = "b".repeat(64);
        let daemon = "c".repeat(64);
        let make_manifest = |sha: &str, gateway: &str, daemon: &str| {
            serde_json::to_vec(&serde_json::json!({
            "version":"1.0", "created_at":"fixture-only", "git_sha":sha,
            "artifacts":[
                {"source":"cmd/cortex-gateway/cortex-gateway", "path":GATEWAY_FILE,"type":"binary","sha256":gateway},
                {"source":"target/release/sentinel-daemon", "path":DAEMON_FILE,"type":"binary","sha256":daemon},
            ],
        })).unwrap()
        };
        let manifest = make_manifest(&"a".repeat(40), &gateway, &daemon);
        let historical_manifest = make_manifest(&"d".repeat(40), &"e".repeat(64), &"f".repeat(64));
        let release = AdaptiveRecoveryReleaseV1 {
            schema_version: 1,
            source_git_sha: "a".repeat(40),
            release_manifest_digest: format!("{:x}", Sha256::digest(&manifest)),
            gateway_binary_digest: gateway.clone(),
        };
        let failed_release = AdaptiveRecoveryReleaseV1 {
            schema_version: 1,
            source_git_sha: "d".repeat(40),
            release_manifest_digest: format!("{:x}", Sha256::digest(&historical_manifest)),
            gateway_binary_digest: "e".repeat(64),
        };
        let mut documents = HashMap::new();
        let mut add = |name: String, bytes: Vec<u8>| {
            let path = PathBuf::from(format!("/fixture-admission/{name}"));
            let digest = format!("{:x}", Sha256::digest(&bytes));
            documents.insert(path.clone(), bytes);
            ProtectedRepairEvidenceV1 { path, digest }
        };
        let source_digest = source.canonical_digest().unwrap();
        let inventory_digest =
            adaptive_leadership_admission_repair_history_digest(&source.calls).unwrap();
        let source_evidence = add(
            "ordering-source".into(),
            b"fixture-only reviewed ordering source".to_vec(),
        );
        let historical_manifest = add("historical-manifest".into(), historical_manifest);
        let gateway_serving_identity = add(
            "historical-gateway".into(),
            b"fixture-only archived gateway serving identity".to_vec(),
        );
        let daemon_serving_identity = add(
            "historical-daemon".into(),
            b"fixture-only archived daemon serving identity".to_vec(),
        );
        let until = source
            .calls
            .iter()
            .map(|call| call.updated_at_unix_ms)
            .max()
            .unwrap()
            + 1;
        let historical_deployment = add(
            "historical-deployment".into(),
            serde_json::to_vec(&HistoricalAdmissionDeploymentV1 {
                schema_version: 1,
                purpose: "verified-historical-serving-release".into(),
                release: failed_release.clone(),
                daemon_binary_digest: "f".repeat(64),
                valid_from_unix_ms: 1,
                valid_until_unix_ms: until,
                manifest: historical_manifest,
                gateway_serving_identity,
                daemon_serving_identity,
            })
            .unwrap(),
        );
        let admission_ordering = add(
            "ordering".into(),
            serde_json::to_vec(&AdmissionOrderingEvidenceV1 {
                schema_version: 1,
                purpose: "verified-leadership-admission-ordering".into(),
                failed_release: failed_release.clone(),
                daemon_binary_digest: "f".repeat(64),
                valid_from_unix_ms: 1,
                valid_until_unix_ms: until,
                ordering: "durable-leadership-dispatch-before-provider-io".into(),
                fencing: "no-provider-io-after-rejected-or-retired-dispatch".into(),
                source_evidence,
            })
            .unwrap(),
        );
        // One proven candidate is sufficient; every other call remains in the inventory.
        let id = source.qualifying_review_ids[0];
        let call = source
            .calls
            .iter()
            .find(|call| call.grant.review_id == id)
            .unwrap();
        let retired = retired_admission_evidence(call).unwrap();
        let retirement_evidence = add("retirement".into(), serde_json::to_vec(&retired).unwrap());
        let failure_evidence = add(
            "failures".into(),
            serde_json::to_vec(&AdmissionFailureEvidenceV1 {
                schema_version: 1,
                purpose: "verified-retired-leadership-no-io".into(),
                source_digest: source_digest.clone(),
                inventory_digest: inventory_digest.clone(),
                receipts: vec![AdmissionNoIoReceiptV1 {
                    review_id: id,
                    request_id: retired.request_id,
                    authority_digest: retired.authority_digest,
                    context_digest: retired.context_digest,
                    retired_call_digest: retired.retired_call_digest,
                    retired_at_unix_ms: retired.retired_at_unix_ms,
                    proof: AdmissionNoIoProofV1::FencedUndispatchedRetirement {
                        historical_deployment,
                        retirement_evidence,
                    },
                }],
            })
            .unwrap(),
        );
        let gate_evidence = add("gate".into(), serde_json::to_vec(&serde_json::json!({
            "purpose":"exact-release-gate-evidence", "merged_commit":release.source_git_sha,
            "merged_tree_equals_reviewed_tree":true, "ci":{"status":"completed","conclusion":"success"},
            "release":{"result":"PASS", "gateway_sha256":gateway, "daemon_sha256":daemon},
        })).unwrap());
        let repair = serde_json::to_vec(&AdmissionRepairAttestationV1 {
            schema_version: 1,
            purpose: "adaptive-leadership-admission-repair".into(),
            release,
            daemon_binary_digest: daemon.clone(),
            failed_release,
            failed_daemon_binary_digest: "f".repeat(64),
            source_digest,
            inventory_digest,
            gate_evidence,
            admission_ordering,
            failure_evidence,
        })
        .unwrap();
        validate_admission_repair(&repair, &manifest, &gateway, &daemon, source, |reference| {
            documents
                .get(&reference.path)
                .cloned()
                .ok_or("fixture reference missing")
        })
        .unwrap();
        let id = TEST_REPAIR_ID.with(|next| {
            let id = next.get().checked_add(1).unwrap();
            next.set(id);
            id
        });
        TEST_ADMISSION_REPAIRS.with(|stack| {
            stack.borrow_mut().push((
                id,
                TestAdmissionRepairEvidence {
                    repair,
                    manifest,
                    gateway,
                    daemon,
                    documents,
                },
            ))
        });
        Self {
            id,
            _thread_bound: std::marker::PhantomData,
        }
    }

    pub(crate) fn reject(&self) {
        TEST_ADMISSION_REPAIRS.with(|stack| {
            stack
                .borrow_mut()
                .iter_mut()
                .find(|(id, _)| *id == self.id)
                .unwrap()
                .1
                .repair
                .clear();
        });
    }

    pub(crate) fn replace_serving_binaries(&self) {
        TEST_ADMISSION_REPAIRS.with(|stack| {
            let mut stack = stack.borrow_mut();
            let value = &mut stack.iter_mut().find(|(id, _)| *id == self.id).unwrap().1;
            value.gateway = "1".repeat(64);
            value.daemon = "2".repeat(64);
        });
    }
}

#[cfg(test)]
impl Drop for TestAdmissionRepairGuard {
    fn drop(&mut self) {
        TEST_ADMISSION_REPAIRS.with(|stack| stack.borrow_mut().retain(|(id, _)| *id != self.id));
    }
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
    serving_binary_digest(Path::new(GATEWAY_FILE), "sentinel-gateway.service")
}

fn serving_binary_digest(path: &Path, service: &str) -> Result<String, &'static str> {
    let (mut file, before) = protected_file(path, MAX_GATEWAY_BYTES)?;
    let pid = serving_service_pid(service)?;
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
    if serving_service_pid(service)? != pid {
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

fn serving_service_pid(service: &str) -> Result<u32, &'static str> {
    let output = std::process::Command::new("/usr/bin/timeout")
        .args([
            "--signal=KILL",
            "3s",
            "/usr/bin/systemctl",
            "show",
            service,
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

#[cfg(not(test))]
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
#[derive(Clone)]
struct TestRepairEvidence {
    repair: Vec<u8>,
    manifest: Vec<u8>,
    gateway_digest: String,
}

#[cfg(test)]
std::thread_local! {
    static TEST_REPAIRS: std::cell::RefCell<Vec<(u64, TestRepairEvidence)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static TEST_REPAIR_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test evidence substitution only, not evidence of an installed or serving gateway.
#[cfg(test)]
pub(super) struct TestRepairGuard {
    id: u64,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(test)]
impl TestRepairGuard {
    pub(super) fn fixture() -> Result<Self, &'static str> {
        let (repair, manifest, gateway) = tests::fixture();
        Self::install(
            serde_json::to_vec(&repair).map_err(|_| "test repair encoding failed")?,
            manifest,
            gateway,
        )
    }

    pub(super) fn install(
        repair: Vec<u8>,
        manifest: Vec<u8>,
        gateway_digest: String,
    ) -> Result<Self, &'static str> {
        validate_evidence(&repair, &manifest, &gateway_digest)?;
        let id = TEST_REPAIR_ID.with(|next| {
            let id = next
                .get()
                .checked_add(1)
                .expect("test repair token exhausted");
            next.set(id);
            id
        });
        TEST_REPAIRS.with(|stack| {
            stack.borrow_mut().push((
                id,
                TestRepairEvidence {
                    repair,
                    manifest,
                    gateway_digest,
                },
            ));
        });
        Ok(Self {
            id,
            _thread_bound: std::marker::PhantomData,
        })
    }
}

#[cfg(test)]
impl Drop for TestRepairGuard {
    fn drop(&mut self) {
        // Remove this token rather than restoring a snapshot: out-of-order drops are safe.
        TEST_REPAIRS.with(|stack| stack.borrow_mut().retain(|(id, _)| *id != self.id));
    }
}

#[cfg(test)]
pub(super) fn verified_current_repair() -> Result<(AdaptiveRecoveryReleaseV1, String), &'static str>
{
    if let Some(evidence) =
        TEST_REPAIRS.with(|stack| stack.borrow().last().map(|(_, value)| value.clone()))
    {
        return validate_evidence(
            &evidence.repair,
            &evidence.manifest,
            &evidence.gateway_digest,
        );
    }
    // Without a guard, tests retain the same protected-file and serving-process checks.
    let repair = evidence_bytes(Path::new(REPAIR_FILE))?;
    let manifest = evidence_bytes(Path::new(MANIFEST_FILE))?;
    let actual_gateway = gateway_digest()?;
    let result = validate_evidence(&repair, &manifest, &actual_gateway)?;
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

    pub(super) fn fixture() -> (serde_json::Value, Vec<u8>, String) {
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
    fn repair_guard_validates_restores_and_is_thread_local() {
        let (repair, manifest, gateway) = fixture();
        let bytes = serde_json::to_vec(&repair).unwrap();
        let expected = validate_evidence(&bytes, &manifest, &gateway).unwrap();
        assert!(TestRepairGuard::install(bytes.clone(), manifest.clone(), "f".repeat(64)).is_err());
        assert!(TEST_REPAIRS.with(|stack| stack.borrow().is_empty()));
        let outer =
            TestRepairGuard::install(bytes.clone(), manifest.clone(), gateway.clone()).unwrap();
        let mut nested_repair = repair.clone();
        nested_repair["gate_evidence_digest"] = serde_json::json!("e".repeat(64));
        let nested_bytes = serde_json::to_vec(&nested_repair).unwrap();
        let nested_expected = validate_evidence(&nested_bytes, &manifest, &gateway).unwrap();
        {
            let _inner =
                TestRepairGuard::install(nested_bytes.clone(), manifest.clone(), gateway.clone())
                    .unwrap();
            assert_eq!(verified_current_repair().unwrap(), nested_expected);
        }
        assert_eq!(verified_current_repair().unwrap(), expected);
        std::thread::spawn(|| {
            assert!(TEST_REPAIRS.with(|stack| stack.borrow().is_empty()));
        })
        .join()
        .unwrap();
        let inner =
            TestRepairGuard::install(nested_bytes, manifest.clone(), gateway.clone()).unwrap();
        drop(outer);
        assert_eq!(verified_current_repair().unwrap(), nested_expected);
        drop(inner);
        assert!(TEST_REPAIRS.with(|stack| stack.borrow().is_empty()));
        let panic = std::panic::catch_unwind(|| {
            let _guard = TestRepairGuard::install(bytes, manifest, gateway).unwrap();
            panic!("fixture unwind");
        });
        assert!(panic.is_err());
        assert!(TEST_REPAIRS.with(|stack| stack.borrow().is_empty()));
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
