use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{WorkflowError, WorkflowErrorCode};

pub(crate) fn canonical_sha256<T: Serialize>(
    domain: &'static str,
    value: &T,
) -> Result<String, WorkflowError> {
    let payload = serde_json::to_vec(value).map_err(|_| {
        WorkflowError::new(
            WorkflowErrorCode::InvalidInput,
            false,
            "canonical workflow serialization failed",
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(payload);
    let digest = hasher.finalize();
    Ok(hex_sha256(&digest))
}

pub(crate) fn derive_principal_authority_digest(
    principal_generation: u64,
    credential_digest: &[u8; 32],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"sentinel.workflow.principal-authority.v1\0");
    hasher.update(principal_generation.to_be_bytes());
    hasher.update(credential_digest);
    let digest = hasher.finalize();
    hex_sha256(&digest)
}

pub(crate) fn validate_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

pub(crate) fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn hex_sha256(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}
