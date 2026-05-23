//! Deterministic metadata maps for messages, tasks, events, and gateway state.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{MissiveError, Result};

/// Metadata key used to record the A2A protocol version applied to a request.
pub const METADATA_A2A_PROTOCOL_VERSION: &str = "a2a.protocol_version";

/// Metadata key used to record requested A2A extensions.
pub const METADATA_A2A_EXTENSIONS: &str = "a2a.extensions";

/// Metadata key used to record extra non-auth A2A service parameters.
pub const METADATA_A2A_SERVICE_PARAMETERS: &str = "a2a.service_parameters";

const METADATA_KEY_MAX_BYTES: usize = 128;
const METADATA_KEY_HELP: &str =
    "Use a short non-empty metadata key without whitespace or control characters.";

/// A JSON metadata map with deterministic key ordering.
///
/// Metadata values are intentionally represented as arbitrary JSON so protocol,
/// gateway, routing, and store layers can attach structured A2A-compatible
/// context without widening core types for every future field.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Metadata(BTreeMap<String, Value>);

impl Metadata {
    /// Creates an empty metadata map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a metadata map from an iterator of key/value pairs.
    pub fn try_from_iter<K, V, I>(pairs: I) -> Result<Self>
    where
        K: Into<String>,
        V: Into<Value>,
        I: IntoIterator<Item = (K, V)>,
    {
        let mut metadata = Self::new();
        for (key, value) in pairs {
            metadata.insert(key, value)?;
        }
        Ok(metadata)
    }

    /// Returns the number of metadata entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether the metadata map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns whether the metadata map contains `key`.
    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// Inserts or replaces a metadata value after validating the key.
    pub fn insert(
        &mut self,
        key: impl Into<String>,
        value: impl Into<Value>,
    ) -> Result<Option<Value>> {
        let key = key.into();
        validate_metadata_key(&key)?;
        Ok(self.0.insert(key, value.into()))
    }

    /// Inserts a string metadata value after validating the key.
    pub fn insert_str(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Option<Value>> {
        self.insert(key, Value::String(value.into()))
    }

    /// Returns a metadata value by key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    /// Returns a metadata value as a string when it is encoded as a JSON string.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    /// Removes a metadata value by key.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.0.remove(key)
    }

    /// Iterates over metadata entries in deterministic key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(key, value)| (key.as_str(), value))
    }

    /// Merges another metadata map into this one, replacing duplicate keys.
    pub fn merge(&mut self, other: Self) {
        self.0.extend(other.0);
    }

    /// Returns the underlying ordered map.
    #[must_use]
    pub fn into_inner(self) -> BTreeMap<String, Value> {
        self.0
    }
}

impl<'de> Deserialize<'de> for Metadata {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let map = BTreeMap::<String, Value>::deserialize(deserializer)?;
        for key in map.keys() {
            validate_metadata_key(key).map_err(serde::de::Error::custom)?;
        }
        Ok(Self(map))
    }
}

fn validate_metadata_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return invalid_metadata_key(key, "key cannot be empty");
    }

    if key.len() > METADATA_KEY_MAX_BYTES {
        return invalid_metadata_key(
            key,
            format!(
                "key is {} bytes, but the maximum is {METADATA_KEY_MAX_BYTES}",
                key.len()
            ),
        );
    }

    if key.chars().any(char::is_whitespace) {
        return invalid_metadata_key(key, "key cannot contain whitespace");
    }

    if key.chars().any(char::is_control) {
        return invalid_metadata_key(key, "key cannot contain control characters");
    }

    Ok(())
}

fn invalid_metadata_key(key: &str, reason: impl Into<String>) -> Result<()> {
    Err(
        MissiveError::validation(format!("invalid metadata key {key:?}: {}", reason.into()))
            .with_help(METADATA_KEY_HELP),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use proptest::prelude::*;
    use serde_json::{Value, json};

    use super::*;
    use crate::ErrorCategory;

    #[test]
    fn metadata_helpers_insert_read_merge_and_remove_values() {
        let mut metadata = Metadata::new();

        metadata
            .insert_str("a2a.version", "0.3")
            .expect("insert string");
        metadata.insert("retry_count", 2).expect("insert number");

        assert_eq!(metadata.len(), 2);
        assert_eq!(metadata.get_str("a2a.version"), Some("0.3"));
        assert_eq!(metadata.get("retry_count"), Some(&json!(2)));
        assert!(metadata.contains_key("retry_count"));

        let other = Metadata::try_from_iter([("retry_count", json!(3)), ("source", json!("test"))])
            .expect("metadata should build from pairs");
        metadata.merge(other);

        assert_eq!(metadata.get("retry_count"), Some(&json!(3)));
        assert_eq!(metadata.remove("source"), Some(json!("test")));
        assert!(!metadata.contains_key("source"));
    }

    #[test]
    fn metadata_serializes_as_deterministic_json_object() {
        let metadata = Metadata::try_from_iter([("b", json!(2)), ("a", json!(1))])
            .expect("metadata should build");

        assert_eq!(
            serde_json::to_string(&metadata).expect("serialize metadata"),
            r#"{"a":1,"b":2}"#
        );
    }

    #[test]
    fn metadata_deserialize_rejects_invalid_keys() {
        let error = serde_json::from_value::<Metadata>(json!({"bad key": true}))
            .expect_err("metadata key should be invalid");

        assert!(error.to_string().contains("invalid metadata key"));
        assert!(error.to_string().contains("whitespace"));
    }

    #[test]
    fn metadata_insert_rejects_invalid_keys_with_diagnostics() {
        let mut metadata = Metadata::new();
        let error = metadata
            .insert("", json!(true))
            .expect_err("empty key should be invalid");

        assert_eq!(error.category(), ErrorCategory::Validation);
        assert!(error.to_string().contains("invalid metadata key"));
        assert_eq!(error.help(), Some(METADATA_KEY_HELP));
    }

    fn metadata_key() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9_.-]{0,20}"
    }

    fn simple_json_value() -> impl Strategy<Value = Value> {
        prop_oneof![
            any::<bool>().prop_map(Value::Bool),
            (-1_000_i64..=1_000).prop_map(|value| json!(value)),
            "[A-Za-z0-9_.:-]{0,24}".prop_map(Value::String),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(96))]

        #[test]
        fn metadata_merge_is_right_biased_and_deterministically_ordered(
            left_pairs in prop::collection::btree_map(metadata_key(), simple_json_value(), 0..12),
            right_pairs in prop::collection::btree_map(metadata_key(), simple_json_value(), 0..12),
        ) {
            let mut expected: BTreeMap<String, Value> = left_pairs.clone();
            for (key, value) in right_pairs.clone() {
                expected.insert(key, value);
            }

            let mut merged = Metadata::try_from_iter(left_pairs).expect("generated metadata keys are valid");
            let right = Metadata::try_from_iter(right_pairs).expect("generated metadata keys are valid");
            merged.merge(right);

            let iterated_keys = merged
                .iter()
                .map(|(key, _)| key.to_owned())
                .collect::<Vec<_>>();
            let mut sorted_keys = iterated_keys.clone();
            sorted_keys.sort();

            prop_assert_eq!(iterated_keys, sorted_keys);
            prop_assert_eq!(merged.into_inner(), expected);
        }
    }
}
