use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use super::error::DeliveryError;

/// A lowercase SHA-256 digest over canonical JSON bytes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentDigest(String);

impl ContentDigest {
    pub fn zero() -> Self {
        Self("0".repeat(64))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, DeliveryError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DeliveryError::InvalidDigest(value));
        }
        Ok(Self(value))
    }

    /// Hashes a value in an explicit record and schema domain.
    ///
    /// Delivery wire and persisted records must use this method rather than a
    /// bare JSON hash. The domain prefix prevents the same JSON shape from
    /// being replayed as another record type or schema version.
    pub fn of_domain<T: Serialize>(
        record_type: &str,
        schema_version: u16,
        value: &T,
    ) -> Result<Self, DeliveryError> {
        validate_domain(record_type)?;
        let bytes = Self::canonical_bytes(value)?;
        Self::of_bytes_domain(record_type, schema_version, &bytes)
    }

    pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, DeliveryError> {
        let value = serde_json::to_value(value)?;
        serde_json::to_vec(&canonicalize(value)).map_err(DeliveryError::from)
    }

    pub fn of_bytes_domain(
        record_type: &str,
        schema_version: u16,
        bytes: &[u8],
    ) -> Result<Self, DeliveryError> {
        validate_domain(record_type)?;
        let mut hasher = Sha256::new();
        hasher.update(b"sentinel.delivery.digest\0");
        hasher.update(schema_version.to_be_bytes());
        hasher.update((record_type.len() as u32).to_be_bytes());
        hasher.update(record_type.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
        let hash = hasher.finalize();
        Ok(Self(lower_hex(&hash)))
    }

    /// Non-wire helper retained for opaque test values only.
    pub fn of<T: Serialize>(value: &T) -> Result<Self, DeliveryError> {
        Self::of_domain("opaque-test-value", 1, value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn validate_domain(record_type: &str) -> Result<(), DeliveryError> {
    if record_type.is_empty()
        || record_type.len() > 96
        || !record_type
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(DeliveryError::Validation(
            "digest record type is not canonical".to_string(),
        ));
    }
    Ok(())
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for ContentDigest {
    type Error = DeliveryError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<ContentDigest> for String {
    fn from(value: ContentDigest) -> Self {
        value.0
    }
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        Value::Object(values) => {
            let mut entries: Vec<_> = values.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut canonical = Map::new();
            for (key, value) in entries {
                canonical.insert(key, canonicalize(value));
            }
            Value::Object(canonical)
        }
        scalar => scalar,
    }
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ContentDigest;

    #[test]
    fn object_order_does_not_change_digest() {
        let left = json!({"b": 2, "a": {"d": 4, "c": 3}});
        let right = json!({"a": {"c": 3, "d": 4}, "b": 2});
        assert_eq!(
            ContentDigest::of(&left).unwrap(),
            ContentDigest::of(&right).unwrap()
        );
    }

    #[test]
    fn domain_and_schema_change_the_golden_digest() {
        let value = json!({"a": 1, "b": ["x", "y"]});
        let first = ContentDigest::of_domain("qa-plan", 1, &value).unwrap();
        assert_eq!(
            first.as_str(),
            "fa68b8a7096f0867a5223eff0cae25339bc50f5f110ea78584e43897989956a5"
        );
        assert_ne!(
            first,
            ContentDigest::of_domain("qa-plan", 2, &value).unwrap()
        );
        assert_ne!(
            first,
            ContentDigest::of_domain("qa-run", 1, &value).unwrap()
        );
    }

    #[test]
    fn rejects_non_canonical_digest_text() {
        assert!(ContentDigest::parse("AA").is_err());
        assert!(ContentDigest::parse("z".repeat(64)).is_err());
    }
}
