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

    pub fn of<T: Serialize>(value: &T) -> Result<Self, DeliveryError> {
        let value = serde_json::to_value(value)?;
        let bytes = serde_json::to_vec(&canonicalize(value))?;
        let hash = Sha256::digest(bytes);
        Ok(Self(lower_hex(&hash)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
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
    fn rejects_non_canonical_digest_text() {
        assert!(ContentDigest::parse("AA").is_err());
        assert!(ContentDigest::parse("z".repeat(64)).is_err());
    }
}
