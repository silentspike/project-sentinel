use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Write;

use crate::{WorkflowError, WorkflowErrorCode};

pub(crate) fn canonical_sha256<T: Serialize>(
    domain: &'static str,
    value: &T,
) -> Result<String, WorkflowError> {
    let mut writer = DigestWriter::new(domain);
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        WorkflowError::new(
            WorkflowErrorCode::InvalidInput,
            false,
            "canonical workflow serialization failed",
        )
    })?;
    Ok(writer.finish())
}

const DIGEST_BUFFER_BYTES: usize = 8192;

struct DigestWriter {
    hasher: Sha256,
    buffer: [u8; DIGEST_BUFFER_BYTES],
    used: usize,
}

impl DigestWriter {
    fn new(domain: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(domain.as_bytes());
        hasher.update([0]);
        Self {
            hasher,
            buffer: [0; DIGEST_BUFFER_BYTES],
            used: 0,
        }
    }

    fn append(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            if self.used == 0 && bytes.len() >= DIGEST_BUFFER_BYTES {
                self.hasher.update(bytes);
                return;
            }
            let count = bytes.len().min(DIGEST_BUFFER_BYTES - self.used);
            self.buffer[self.used..self.used + count].copy_from_slice(&bytes[..count]);
            self.used += count;
            bytes = &bytes[count..];
            if self.used == DIGEST_BUFFER_BYTES {
                self.hasher.update(self.buffer.as_slice());
                self.used = 0;
            }
        }
    }

    fn finish(mut self) -> String {
        self.hasher.update(&self.buffer[..self.used]);
        hex_sha256(&self.hasher.finalize())
    }
}

impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.append(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.hasher.update(&self.buffer[..self.used]);
        self.used = 0;
        Ok(())
    }
}

struct DecimalByte {
    bytes: [u8; 4],
    len: usize,
}

const fn decimal_bytes() -> [DecimalByte; 256] {
    let mut values = [const {
        DecimalByte {
            bytes: [0; 4],
            len: 0,
        }
    }; 256];
    let mut index = 0;
    while index < values.len() {
        let value = index as u8;
        values[index].bytes[0] = b',';
        if value >= 100 {
            values[index].bytes[1] = b'0' + value / 100;
            values[index].bytes[2] = b'0' + (value / 10) % 10;
            values[index].bytes[3] = b'0' + value % 10;
            values[index].len = 4;
        } else if value >= 10 {
            values[index].bytes[1] = b'0' + value / 10;
            values[index].bytes[2] = b'0' + value % 10;
            values[index].len = 3;
        } else {
            values[index].bytes[1] = b'0' + value;
            values[index].len = 2;
        }
        index += 1;
    }
    values
}

const DECIMAL_BYTES: [DecimalByte; 256] = decimal_bytes();

// Persisted byte digests hash the JSON numeric array, not the raw bytes.
// Preserve that contract without allocating or formatting the expanded array.
pub(crate) fn canonical_bytes_sha256(domain: &'static str, bytes: &[u8]) -> String {
    let mut writer = DigestWriter::new(domain);
    writer.append(b"[");
    if let Some((first, rest)) = bytes.split_first() {
        let decimal = &DECIMAL_BYTES[usize::from(*first)];
        writer.append(&decimal.bytes[1..decimal.len]);
        for byte in rest {
            let decimal = &DECIMAL_BYTES[usize::from(*byte)];
            writer.append(&decimal.bytes[..decimal.len]);
        }
    }
    writer.append(b"]");
    writer.finish()
}

pub(crate) fn serialized_json_size<T: Serialize>(value: &T) -> Result<usize, serde_json::Error> {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("serialized workflow size overflow"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.0)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_digest<T: Serialize>(domain: &'static str, value: &T) -> String {
        let mut hasher = Sha256::new();
        hasher.update(domain.as_bytes());
        hasher.update([0]);
        hasher.update(serde_json::to_vec(value).unwrap());
        hex_sha256(&hasher.finalize())
    }

    #[test]
    fn streamed_canonical_digest_preserves_legacy_json_and_domain() {
        let value = serde_json::json!({
            "control": "\u{0}\n\r\t\\\"",
            "unicode": "\u{e4}\u{1f642}",
            "numbers": [0, -42, u64::MAX, 1.25, 1e-30],
            "nested": {"null": null, "bool": true, "empty": []},
            "large": "x".repeat(DIGEST_BUFFER_BYTES * 3 + 1),
        });
        for domain in ["test.workflow.first.v1", "test.workflow.second.v1"] {
            assert_eq!(
                canonical_sha256(domain, &value).unwrap(),
                legacy_digest(domain, &value)
            );
        }
        assert_eq!(
            serialized_json_size(&value).unwrap(),
            serde_json::to_vec(&value).unwrap().len()
        );
        assert_ne!(
            canonical_sha256("first", &value).unwrap(),
            canonical_sha256("second", &value).unwrap()
        );
    }

    #[test]
    fn streamed_byte_digest_preserves_every_decimal_value_and_buffer_boundary() {
        let all: Vec<u8> = (0..=u8::MAX).collect();
        let domains = [
            "sentinel.workflow.company-entity-row.v1",
            "sentinel.workflow.company-event-payload.v1",
            "sentinel.workflow.company-operation-response.v1",
            "sentinel.workflow.company-projection-row.v1",
        ];
        for domain in domains {
            for len in [
                0, 1, 2, 9, 10, 99, 100, 255, 256, 2047, 2048, 2049, 8191, 8192, 8193, 65537,
            ] {
                let bytes: Vec<u8> = all.iter().copied().cycle().take(len).collect();
                assert_eq!(
                    canonical_bytes_sha256(domain, &bytes),
                    legacy_digest(domain, &bytes)
                );
                assert_eq!(
                    canonical_bytes_sha256(domain, &bytes),
                    canonical_sha256(domain, &bytes).unwrap()
                );
                assert_eq!(
                    serialized_json_size(&bytes).unwrap(),
                    serde_json::to_vec(&bytes).unwrap().len()
                );
            }
        }
        for byte in all {
            assert_eq!(
                canonical_bytes_sha256("single", &[byte]),
                legacy_digest("single", &[byte])
            );
        }
    }

    #[test]
    fn digest_writer_handles_fragmentation_flush_and_empty_writes() {
        let bytes = vec![b'a'; DIGEST_BUFFER_BYTES * 4 + 17];
        let mut expected = Sha256::new();
        expected.update(b"writer\0");
        expected.update(&bytes);
        for size in [1, 3, 8191, 8192, 8193, bytes.len()] {
            let mut writer = DigestWriter::new("writer");
            writer.write_all(&[]).unwrap();
            for chunk in bytes.chunks(size) {
                writer.write_all(chunk).unwrap();
                writer.flush().unwrap();
            }
            assert_eq!(writer.finish(), hex_sha256(&expected.clone().finalize()));
        }
    }

    #[test]
    fn streamed_canonical_digest_rejects_serialization_failure() {
        struct Invalid;
        impl Serialize for Invalid {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("invalid test value"))
            }
        }
        assert!(canonical_sha256("invalid", &Invalid).is_err());
        assert!(serialized_json_size(&Invalid).is_err());
    }
}
