//! Content checksums used as resume identity.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Lowercase hex SHA-256 digest of workflow, input, or request content.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    /// Returns the lowercase hex digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(
        &self,
        formatter: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for ContentHash {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// SHA-256 of raw bytes, encoded as lowercase hex.
#[must_use]
pub fn hash_bytes(input: &[u8]) -> ContentHash {
    let digest = Sha256::digest(input);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    ContentHash(encoded)
}

/// SHA-256 of a UTF-8 string.
#[must_use]
pub fn hash_str(input: &str) -> ContentHash {
    hash_bytes(input.as_bytes())
}

/// SHA-256 of canonical JSON (object keys sorted recursively).
///
/// # Errors
///
/// Returns a JSON error if serialization fails.
pub fn hash_json(value: &Value) -> Result<ContentHash, serde_json::Error> {
    let canonical = canonicalize(value);
    let bytes = serde_json::to_vec(&canonical)?;
    Ok(hash_bytes(&bytes))
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys = map.keys().cloned().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut ordered = Map::new();
            for key in keys {
                if let Some(child) = map.get(&key) {
                    ordered.insert(key, canonicalize(child));
                }
            }
            Value::Object(ordered)
        },
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{hash_json, hash_str};

    #[test]
    fn encodes_sha256_as_lowercase_hex() {
        assert_eq!(
            hash_str("abc").as_str(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn json_hash_is_key_order_independent() -> Result<(), serde_json::Error> {
        let left = json!({ "b": 1, "a": { "y": 2, "x": 3 } });
        let right = json!({ "a": { "x": 3, "y": 2 }, "b": 1 });
        assert_eq!(hash_json(&left)?, hash_json(&right)?);
        Ok(())
    }
}
