use super::*;

#[derive(Clone)]
struct Fixture {
    missing_repair: bool,
    repair: Vec<u8>,
    manifest: Vec<u8>,
    gateway: String,
    daemon: String,
    gate: Vec<u8>,
}

fn fixture() -> Fixture {
    let gateway = "b".repeat(64);
    let daemon = "c".repeat(64);
    let manifest = serde_json::to_vec(&serde_json::json!({
        "version":"1.0","created_at":"synthetic-test-only","git_sha":"a".repeat(40),
        "artifacts":[
            {"source":"cmd/cortex-gateway/cortex-gateway","path":GATEWAY_FILE,
             "type":"binary","sha256":gateway},
            {"source":"target/release/sentinel-daemon","path":DAEMON_FILE,
             "type":"binary","sha256":daemon}
        ]
    }))
    .unwrap();
    let gate = serde_json::to_vec(&serde_json::json!({
        "purpose":"exact-release-gate-evidence","merged_commit":"a".repeat(40),
        "merged_tree_equals_reviewed_tree":true,
        "ci":{"status":"completed","conclusion":"success"},
        "release":{"result":"PASS","gateway_sha256":gateway,"daemon_sha256":daemon}
    }))
    .unwrap();
    let proof = FundedLocalAdoptionRepairV1 {
        schema_version: 1,
        purpose: "adaptive-funded-local-adoption-repair".into(),
        release: AdaptiveRecoveryReleaseV1 {
            schema_version: 1,
            source_git_sha: "a".repeat(40),
            release_manifest_digest: format!("{:x}", Sha256::digest(&manifest)),
            gateway_binary_digest: gateway.clone(),
        },
        daemon_binary_digest: daemon.clone(),
        repair_contract: "funded-review-history-cut-v1".into(),
        gate_evidence: ProtectedRepairEvidenceV1 {
            path: PathBuf::from("/etc/sentinel/test-funded-local-adoption-gates.json"),
            digest: format!("{:x}", Sha256::digest(&gate)),
        },
    };
    Fixture {
        missing_repair: false,
        repair: serde_json::to_vec(&proof).unwrap(),
        manifest,
        gateway,
        daemon,
        gate,
    }
}

fn validate(f: &Fixture) -> Result<(AdaptiveRecoveryReleaseV1, String), &'static str> {
    if f.missing_repair {
        return Err("funded local adoption repair unavailable");
    }
    validate_funded_local_adoption_repair(&f.repair, &f.manifest, &f.gateway, &f.daemon, |_| {
        Ok(f.gate.clone())
    })
}

std::thread_local! {
    static REPAIRS: std::cell::RefCell<Vec<(u64, Fixture)>> = const {
        std::cell::RefCell::new(Vec::new())
    };
    static NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

// Synthetic protected-file substitution only, never installed-release evidence.
pub(crate) struct TestFundedLocalAdoptionRepairGuard {
    id: u64,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl TestFundedLocalAdoptionRepairGuard {
    pub(crate) fn remove_proof(&self) {
        REPAIRS.with(|repairs| {
            repairs
                .borrow_mut()
                .iter_mut()
                .find(|(id, _)| *id == self.id)
                .unwrap()
                .1
                .missing_repair = true;
        });
    }

    pub(crate) fn rotate_proof(&self) {
        REPAIRS.with(|repairs| {
            let mut repairs = repairs.borrow_mut();
            let value = &mut repairs.iter_mut().find(|(id, _)| *id == self.id).unwrap().1;
            let previous = validate(value).unwrap();
            let mut proof: FundedLocalAdoptionRepairV1 =
                serde_json::from_slice(&value.repair).unwrap();
            proof.gate_evidence.path =
                PathBuf::from("/etc/sentinel/test-funded-local-adoption-rotated-gates.json");
            value.repair = serde_json::to_vec(&proof).unwrap();
            let current = validate(value).unwrap();
            assert_eq!(current.0, previous.0);
            assert_ne!(current.1, previous.1);
        });
    }

    pub(crate) fn fixture() -> Result<Self, &'static str> {
        let value = fixture();
        validate(&value)?;
        let id = NEXT_ID.with(|next| {
            let id = next
                .get()
                .checked_add(1)
                .expect("funded repair test token exhausted");
            next.set(id);
            id
        });
        REPAIRS.with(|repairs| repairs.borrow_mut().push((id, value)));
        Ok(Self {
            id,
            _thread_bound: std::marker::PhantomData,
        })
    }
}

impl Drop for TestFundedLocalAdoptionRepairGuard {
    fn drop(&mut self) {
        REPAIRS.with(|repairs| repairs.borrow_mut().retain(|(id, _)| *id != self.id));
    }
}

pub(super) fn current_verified() -> Option<Result<(AdaptiveRecoveryReleaseV1, String), &'static str>>
{
    REPAIRS.with(|repairs| repairs.borrow().last().map(|(_, value)| validate(value)))
}

#[test]
fn funded_local_repair_requires_typed_exact_installed_release_and_contract() {
    let original = fixture();
    let (release, digest) = validate(&original).unwrap();
    assert_eq!(release.gateway_binary_digest, original.gateway);
    assert_eq!(digest, format!("{:x}", Sha256::digest(&original.repair)));
    for pointer in [
        "/schema_version",
        "/purpose",
        "/repair_contract",
        "/daemon_binary_digest",
        "/release/source_git_sha",
        "/release/release_manifest_digest",
        "/release/gateway_binary_digest",
        "/gate_evidence/digest",
        "/gate_evidence/path",
    ] {
        let mut f = original.clone();
        let mut value: serde_json::Value = serde_json::from_slice(&f.repair).unwrap();
        *value.pointer_mut(pointer).unwrap() = serde_json::json!("invalid");
        f.repair = serde_json::to_vec(&value).unwrap();
        assert!(validate(&f).is_err(), "{pointer}");
    }
    let mut f = original;
    let mut value: serde_json::Value = serde_json::from_slice(&f.repair).unwrap();
    value["no_io"] = serde_json::json!({"kind":"never_reserved"});
    f.repair = serde_json::to_vec(&value).unwrap();
    assert!(
        validate(&f).is_err(),
        "repair must not accept an invented NoIO claim"
    );
}

#[test]
fn funded_local_repair_rejects_redigested_cancelled_ci_tree_and_artifact_substitution() {
    for (pointer, replacement) in [
        ("/purpose", serde_json::json!("other")),
        ("/merged_commit", serde_json::json!("d".repeat(40))),
        (
            "/merged_tree_equals_reviewed_tree",
            serde_json::json!(false),
        ),
        ("/ci/status", serde_json::json!("in_progress")),
        ("/ci/conclusion", serde_json::json!("cancelled")),
        ("/release/result", serde_json::json!("FAIL")),
        ("/release/gateway_sha256", serde_json::json!("d".repeat(64))),
        ("/release/daemon_sha256", serde_json::json!("d".repeat(64))),
    ] {
        let mut f = fixture();
        let mut gate: serde_json::Value = serde_json::from_slice(&f.gate).unwrap();
        *gate.pointer_mut(pointer).unwrap() = replacement;
        f.gate = serde_json::to_vec(&gate).unwrap();
        let mut proof: FundedLocalAdoptionRepairV1 = serde_json::from_slice(&f.repair).unwrap();
        proof.gate_evidence.digest = format!("{:x}", Sha256::digest(&f.gate));
        f.repair = serde_json::to_vec(&proof).unwrap();
        assert!(validate(&f).is_err(), "{pointer}");
    }
}

#[test]
fn funded_local_repair_requires_both_unique_manifest_binaries_and_gate_bytes() {
    for index in [0, 1] {
        for field in ["source", "path", "type", "sha256", "duplicate"] {
            let mut f = fixture();
            let mut manifest: serde_json::Value = serde_json::from_slice(&f.manifest).unwrap();
            if field == "duplicate" {
                let row = manifest["artifacts"][index].clone();
                manifest["artifacts"].as_array_mut().unwrap().push(row);
            } else {
                manifest["artifacts"][index][field] = serde_json::json!("foreign");
            }
            f.manifest = serde_json::to_vec(&manifest).unwrap();
            let mut proof: FundedLocalAdoptionRepairV1 = serde_json::from_slice(&f.repair).unwrap();
            proof.release.release_manifest_digest = format!("{:x}", Sha256::digest(&f.manifest));
            f.repair = serde_json::to_vec(&proof).unwrap();
            assert!(validate(&f).is_err(), "artifact={index} field={field}");
        }
    }
    let mut f = fixture();
    f.gate.push(b' ');
    assert!(validate(&f).is_err());
    assert!(validate_funded_local_adoption_repair(
        &f.repair,
        &f.manifest,
        &f.gateway,
        &f.daemon,
        |_| Err("protected gate missing"),
    )
    .is_err());
    assert!(protected_file(Path::new("relative-funded-repair.json"), MAX_EVIDENCE_BYTES).is_err());
}

#[test]
fn funded_local_repair_guard_revalidates_is_thread_local_and_restores_after_unwind() {
    assert!(current_verified().is_none());
    let outer = TestFundedLocalAdoptionRepairGuard::fixture().unwrap();
    let expected = verified_current_funded_local_adoption_repair().unwrap();
    let inner = TestFundedLocalAdoptionRepairGuard::fixture().unwrap();
    drop(outer);
    assert_eq!(
        verified_current_funded_local_adoption_repair().unwrap(),
        expected
    );
    std::thread::spawn(|| assert!(current_verified().is_none()))
        .join()
        .unwrap();
    REPAIRS.with(|repairs| repairs.borrow_mut().last_mut().unwrap().1.daemon = "d".repeat(64));
    assert!(verified_current_funded_local_adoption_repair().is_err());
    drop(inner);
    assert!(current_verified().is_none());
    assert!(std::panic::catch_unwind(|| {
        let _guard = TestFundedLocalAdoptionRepairGuard::fixture().unwrap();
        panic!("funded repair fixture unwind");
    })
    .is_err());
    assert!(current_verified().is_none());
}
