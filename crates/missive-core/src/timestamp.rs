//! RFC3339 timestamp primitive used by missive records and envelopes.

use std::fmt::{self, Display};
use std::str::FromStr;
use std::time::SystemTime;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{MissiveError, Result};

/// UTC timestamp rendered and parsed as canonical RFC3339.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MissiveTimestamp(DateTime<Utc>);

impl MissiveTimestamp {
    /// Returns the current UTC timestamp.
    #[must_use]
    pub fn now_utc() -> Self {
        Self(DateTime::<Utc>::from(SystemTime::now()))
    }

    /// Creates a timestamp from a chrono UTC date-time.
    #[must_use]
    pub fn from_datetime(value: DateTime<Utc>) -> Self {
        Self(value)
    }

    /// Creates a timestamp from a Unix timestamp in seconds.
    pub fn from_unix_timestamp(seconds: i64) -> Result<Self> {
        DateTime::<Utc>::from_timestamp(seconds, 0)
            .map(Self)
            .ok_or_else(|| {
                MissiveError::validation(format!(
                    "invalid unix timestamp {seconds}: value is outside supported range"
                ))
                .with_help("Use a Unix timestamp supported by chrono.")
            })
    }

    /// Returns the inner chrono UTC date-time value.
    #[must_use]
    pub const fn as_datetime(self) -> DateTime<Utc> {
        self.0
    }

    /// Returns seconds since the Unix epoch.
    #[must_use]
    pub const fn unix_timestamp(self) -> i64 {
        self.0.timestamp()
    }

    /// Formats this timestamp as canonical RFC3339 in UTC.
    #[must_use]
    pub fn to_rfc3339(self) -> String {
        self.0.to_rfc3339_opts(SecondsFormat::Secs, true)
    }
}

impl Display for MissiveTimestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_rfc3339())
    }
}

impl FromStr for MissiveTimestamp {
    type Err = MissiveError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        DateTime::parse_from_rfc3339(value)
            .map(|timestamp| Self(timestamp.with_timezone(&Utc)))
            .map_err(|error| {
                MissiveError::validation(format!(
                    "invalid timestamp {value:?}: expected an RFC3339 timestamp"
                ))
                .with_source(error)
                .with_help("Use an RFC3339 timestamp such as 2025-01-02T03:04:05Z.")
            })
    }
}

impl Serialize for MissiveTimestamp {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_rfc3339())
    }
}

impl<'de> Deserialize<'de> for MissiveTimestamp {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ErrorCategory;

    #[test]
    fn timestamp_display_parse_and_serde_round_trip() {
        let timestamp = MissiveTimestamp::from_unix_timestamp(1_735_787_045)
            .expect("timestamp should be valid");

        assert_eq!(timestamp.to_string(), "2025-01-02T03:04:05Z");
        assert_eq!(
            "2025-01-02T03:04:05Z"
                .parse::<MissiveTimestamp>()
                .expect("parse timestamp"),
            timestamp
        );
        assert_eq!(
            serde_json::to_value(timestamp).expect("serialize"),
            json!("2025-01-02T03:04:05Z")
        );
        assert_eq!(
            serde_json::from_value::<MissiveTimestamp>(json!("2025-01-02T03:04:05Z"))
                .expect("deserialize"),
            timestamp
        );
    }

    #[test]
    fn timestamp_canonicalizes_offsets_to_utc() {
        let timestamp = "2025-01-02T04:04:05+01:00"
            .parse::<MissiveTimestamp>()
            .expect("parse offset timestamp");

        assert_eq!(timestamp.to_string(), "2025-01-02T03:04:05Z");
    }

    #[test]
    fn timestamp_rejects_non_rfc3339_input() {
        let error = "2025-01-02 03:04:05"
            .parse::<MissiveTimestamp>()
            .expect_err("timestamp should be invalid");

        assert_eq!(error.category(), ErrorCategory::Validation);
        assert!(error.to_string().contains("invalid timestamp"));
        assert!(
            error
                .to_report()
                .sources
                .iter()
                .any(|source| !source.is_empty())
        );
    }
}
