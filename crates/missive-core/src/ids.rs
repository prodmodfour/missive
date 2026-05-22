//! Strongly typed string identifiers used across missive domain objects.

use std::fmt::{self, Display};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{MissiveError, Result};

const NAMED_IDENTIFIER_MAX_BYTES: usize = 63;
const TRANSPORT_NAME_MAX_BYTES: usize = 64;
const OPAQUE_IDENTIFIER_MAX_BYTES: usize = 256;
const NAMED_IDENTIFIER_HELP: &str =
    "Use lowercase ASCII letters or digits, with '-', '_' or '.' only in the middle.";
const TRANSPORT_NAME_HELP: &str =
    "Use lowercase ASCII letters or digits, with '+', '-', '_' or '.' only in the middle.";
const OPAQUE_IDENTIFIER_HELP: &str =
    "Use a non-empty A2A identifier without whitespace or control characters.";

macro_rules! string_identifier {
    ($name:ident, $kind:literal, $validator:path) => {
        #[doc = concat!("Strongly typed ", $kind, " wrapper.")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Creates a validated ", $kind, ".")]
            pub fn new(value: impl Into<String>) -> Result<Self> {
                let value = value.into();
                $validator($kind, &value)?;
                Ok(Self(value))
            }

            #[doc = concat!("Returns this ", $kind, " as a string slice.")]
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[doc = concat!("Consumes this ", $kind, " and returns the owned string.")]
            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = MissiveError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = MissiveError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = MissiveError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

string_identifier!(AgentAlias, "agent alias", validate_named_identifier);
string_identifier!(GroupName, "group name", validate_named_identifier);
string_identifier!(RankName, "rank name", validate_named_identifier);
string_identifier!(TransportName, "transport name", validate_transport_name);
string_identifier!(ContextId, "context id", validate_opaque_identifier);
string_identifier!(TaskId, "task id", validate_opaque_identifier);
string_identifier!(MessageId, "message id", validate_opaque_identifier);
string_identifier!(EventId, "event id", validate_opaque_identifier);

fn validate_named_identifier(kind: &'static str, value: &str) -> Result<()> {
    validate_identifier_shape(
        kind,
        value,
        NAMED_IDENTIFIER_MAX_BYTES,
        "lowercase ASCII letters, digits, '-', '_' or '.'",
        NAMED_IDENTIFIER_HELP,
        is_named_separator,
    )
}

fn validate_transport_name(kind: &'static str, value: &str) -> Result<()> {
    validate_identifier_shape(
        kind,
        value,
        TRANSPORT_NAME_MAX_BYTES,
        "lowercase ASCII letters, digits, '+', '-', '_' or '.'",
        TRANSPORT_NAME_HELP,
        is_transport_separator,
    )
}

fn validate_identifier_shape(
    kind: &'static str,
    value: &str,
    max_bytes: usize,
    allowed_description: &'static str,
    help: &'static str,
    is_separator: fn(u8) -> bool,
) -> Result<()> {
    if value.is_empty() {
        return invalid_identifier(kind, value, "value cannot be empty", help);
    }

    if value.len() > max_bytes {
        return invalid_identifier(
            kind,
            value,
            format!(
                "value is {} bytes, but the maximum is {max_bytes}",
                value.len()
            ),
            help,
        );
    }

    let bytes = value.as_bytes();
    if !is_ascii_lower_alphanumeric(bytes[0]) {
        return invalid_identifier(
            kind,
            value,
            "value must start with a lowercase ASCII letter or digit",
            help,
        );
    }

    if !is_ascii_lower_alphanumeric(bytes[bytes.len() - 1]) {
        return invalid_identifier(
            kind,
            value,
            "value must end with a lowercase ASCII letter or digit",
            help,
        );
    }

    for byte in bytes {
        if is_ascii_lower_alphanumeric(*byte) || is_separator(*byte) {
            continue;
        }

        return invalid_identifier(
            kind,
            value,
            format!("value must contain only {allowed_description}"),
            help,
        );
    }

    Ok(())
}

fn validate_opaque_identifier(kind: &'static str, value: &str) -> Result<()> {
    if value.is_empty() {
        return invalid_identifier(kind, value, "value cannot be empty", OPAQUE_IDENTIFIER_HELP);
    }

    if value.len() > OPAQUE_IDENTIFIER_MAX_BYTES {
        return invalid_identifier(
            kind,
            value,
            format!(
                "value is {} bytes, but the maximum is {OPAQUE_IDENTIFIER_MAX_BYTES}",
                value.len()
            ),
            OPAQUE_IDENTIFIER_HELP,
        );
    }

    if value.chars().any(char::is_whitespace) {
        return invalid_identifier(
            kind,
            value,
            "value cannot contain whitespace",
            OPAQUE_IDENTIFIER_HELP,
        );
    }

    if value.chars().any(char::is_control) {
        return invalid_identifier(
            kind,
            value,
            "value cannot contain control characters",
            OPAQUE_IDENTIFIER_HELP,
        );
    }

    Ok(())
}

fn invalid_identifier(
    kind: &'static str,
    value: &str,
    reason: impl Into<String>,
    help: &'static str,
) -> Result<()> {
    Err(
        MissiveError::validation(format!("invalid {kind} {value:?}: {}", reason.into()))
            .with_help(help),
    )
}

const fn is_ascii_lower_alphanumeric(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

const fn is_named_separator(byte: u8) -> bool {
    matches!(byte, b'-' | b'_' | b'.')
}

const fn is_transport_separator(byte: u8) -> bool {
    matches!(byte, b'+' | b'-' | b'_' | b'.')
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use proptest::prelude::*;
    use serde::de::DeserializeOwned;

    use super::*;
    use crate::ErrorCategory;

    fn assert_identifier_round_trip<T>(value: &str)
    where
        T: Clone
            + Debug
            + Display
            + DeserializeOwned
            + Eq
            + FromStr<Err = MissiveError>
            + Serialize,
    {
        let parsed = value.parse::<T>().expect("identifier should parse");

        assert_eq!(parsed.to_string(), value);
        assert_eq!(serde_json::to_value(&parsed).expect("serialize"), value);
        assert_eq!(
            serde_json::from_value::<T>(serde_json::json!(value)).expect("deserialize"),
            parsed
        );
    }

    #[test]
    fn all_identifier_types_parse_display_and_serde() {
        assert_identifier_round_trip::<AgentAlias>("planner-1");
        assert_identifier_round_trip::<GroupName>("research_team");
        assert_identifier_round_trip::<RankName>("rank.0");
        assert_identifier_round_trip::<TransportName>("http+json");
        assert_identifier_round_trip::<ContextId>("ctx_018f5f7a-01");
        assert_identifier_round_trip::<TaskId>("task:server:42");
        assert_identifier_round_trip::<MessageId>("msg_01HX8Z");
        assert_identifier_round_trip::<EventId>("evt/local/0001");
    }

    #[test]
    fn invalid_alias_has_clear_diagnostics() {
        let error = AgentAlias::from_str("Bad Alias").expect_err("alias should be invalid");

        assert_eq!(error.category(), ErrorCategory::Validation);
        assert!(error.to_string().contains("invalid agent alias"));
        assert!(error.to_string().contains("lowercase ASCII"));
        assert_eq!(
            error.help(),
            Some("Use lowercase ASCII letters or digits, with '-', '_' or '.' only in the middle.")
        );
    }

    #[test]
    fn invalid_group_name_has_clear_diagnostics() {
        let error = GroupName::from_str("team-").expect_err("group name should be invalid");

        assert_eq!(error.category(), ErrorCategory::Validation);
        assert!(error.to_string().contains("invalid group name"));
        assert!(
            error
                .to_string()
                .contains("end with a lowercase ASCII letter or digit")
        );
    }

    #[test]
    fn transport_names_allow_a2a_binding_separators() {
        assert_identifier_round_trip::<TransportName>("json-rpc");
        assert_identifier_round_trip::<TransportName>("grpc");
        assert_identifier_round_trip::<TransportName>("http+json");
    }

    #[test]
    fn opaque_ids_reject_whitespace() {
        let error = TaskId::from_str("task 1").expect_err("task id should be invalid");

        assert!(error.to_string().contains("cannot contain whitespace"));
    }

    fn valid_named_identifier() -> impl Strategy<Value = String> {
        "[a-z0-9]([a-z0-9_.-]{0,61}[a-z0-9])?"
    }

    fn invalid_named_identifier() -> impl Strategy<Value = String> {
        prop_oneof![
            Just(String::new()),
            "[A-Z][a-z0-9]{0,8}".prop_map(|value| value),
            " [a-z0-9]{1,8}".prop_map(|value| value),
            "[a-z0-9]{1,8}-".prop_map(|value| value),
            "[a-z]{64,80}".prop_map(|value| value),
        ]
    }

    fn valid_opaque_identifier() -> impl Strategy<Value = String> {
        "[A-Za-z0-9][A-Za-z0-9:._~/-]{0,63}"
    }

    proptest! {
        #[test]
        fn named_identifiers_round_trip(value in valid_named_identifier()) {
            assert_identifier_round_trip::<AgentAlias>(&value);
            assert_identifier_round_trip::<GroupName>(&value);
            assert_identifier_round_trip::<RankName>(&value);
        }

        #[test]
        fn invalid_named_identifiers_are_rejected(value in invalid_named_identifier()) {
            prop_assert!(AgentAlias::from_str(&value).is_err());
            prop_assert!(GroupName::from_str(&value).is_err());
        }

        #[test]
        fn opaque_identifiers_round_trip(value in valid_opaque_identifier()) {
            assert_identifier_round_trip::<ContextId>(&value);
            assert_identifier_round_trip::<TaskId>(&value);
            assert_identifier_round_trip::<MessageId>(&value);
            assert_identifier_round_trip::<EventId>(&value);
        }
    }
}
