//! Routing policy names shared by configuration, CLI, and router planning.

use std::fmt::{self, Display};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{MissiveError, Result};

/// Canonical built-in routing policy names supported by missive.
pub const SUPPORTED_ROUTING_POLICIES: [&str; 9] = [
    "direct",
    "capability-match",
    "tag-match",
    "round-robin",
    "weighted",
    "broadcast",
    "first-success",
    "quorum",
    "fallback",
];

const ROUTING_POLICY_HELP: &str = "Use one of: direct, capability-match, tag-match, round-robin, weighted, broadcast, first-success, quorum, fallback.";

/// Built-in router policy kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoutingPolicyKind {
    /// Route to one explicitly selected or first available candidate.
    Direct,
    /// Select candidates whose advertised local capabilities satisfy the request.
    CapabilityMatch,
    /// Select candidates whose local tags satisfy the request.
    TagMatch,
    /// Select one candidate by a deterministic round-robin cursor.
    RoundRobin,
    /// Select the highest-weighted candidate with deterministic tie-breaking.
    Weighted,
    /// Select every candidate.
    Broadcast,
    /// Try candidates in order until one succeeds.
    FirstSuccess,
    /// Select enough candidates to satisfy a requested quorum.
    Quorum,
    /// Select a primary candidate plus ordered fallback candidates.
    Fallback,
}

impl RoutingPolicyKind {
    /// Returns the canonical policy name used in config, CLI arguments, and output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::CapabilityMatch => "capability-match",
            Self::TagMatch => "tag-match",
            Self::RoundRobin => "round-robin",
            Self::Weighted => "weighted",
            Self::Broadcast => "broadcast",
            Self::FirstSuccess => "first-success",
            Self::Quorum => "quorum",
            Self::Fallback => "fallback",
        }
    }

    /// Parses a routing policy name, accepting canonical kebab-case names and
    /// snake_case aliases for TOML/automation convenience.
    #[must_use]
    pub fn from_name(value: &str) -> Option<Self> {
        match value.trim() {
            "direct" => Some(Self::Direct),
            "capability-match" | "capability_match" => Some(Self::CapabilityMatch),
            "tag-match" | "tag_match" => Some(Self::TagMatch),
            "round-robin" | "round_robin" => Some(Self::RoundRobin),
            "weighted" => Some(Self::Weighted),
            "broadcast" => Some(Self::Broadcast),
            "first-success" | "first_success" => Some(Self::FirstSuccess),
            "quorum" => Some(Self::Quorum),
            "fallback" => Some(Self::Fallback),
            _ => None,
        }
    }
}

impl Display for RoutingPolicyKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for RoutingPolicyKind {
    type Err = MissiveError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_routing_policy(value)
    }
}

/// Parses a routing policy name as user/CLI input.
pub fn parse_routing_policy(value: &str) -> Result<RoutingPolicyKind> {
    let trimmed = value.trim();
    RoutingPolicyKind::from_name(trimmed).ok_or_else(|| {
        MissiveError::validation(format!("unknown routing policy {trimmed:?}"))
            .with_help(ROUTING_POLICY_HELP)
    })
}

/// Parses a routing policy name in configuration validation context.
pub fn parse_config_routing_policy(field: &str, value: &str) -> Result<RoutingPolicyKind> {
    let trimmed = value.trim();
    RoutingPolicyKind::from_name(trimmed).ok_or_else(|| {
        MissiveError::config(format!("{field} has unknown routing policy {trimmed:?}"))
            .with_help(ROUTING_POLICY_HELP)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCategory;

    #[test]
    fn policy_names_round_trip_through_display_and_parse() {
        for name in SUPPORTED_ROUTING_POLICIES {
            let policy = parse_routing_policy(name).expect("policy should parse");
            assert_eq!(policy.as_str(), name);
            assert_eq!(policy.to_string(), name);
        }
    }

    #[test]
    fn snake_case_aliases_parse_to_canonical_names() {
        assert_eq!(
            parse_routing_policy("capability_match").expect("policy"),
            RoutingPolicyKind::CapabilityMatch
        );
        assert_eq!(
            parse_routing_policy("round_robin")
                .expect("policy")
                .as_str(),
            "round-robin"
        );
    }

    #[test]
    fn invalid_policy_is_actionable() {
        let error = parse_config_routing_policy("routing.default_policy", "least-latency")
            .expect_err("unsupported policy should fail");

        assert_eq!(error.category(), ErrorCategory::Config);
        assert!(error.to_string().contains("routing.default_policy"));
        assert!(error.help().expect("help").contains("direct"));
    }
}
