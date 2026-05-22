//! Capability summary helpers shared by agent, group, and route commands.
//!
//! This module turns cached/fetched A2A Agent Cards plus local registry metadata
//! into stable capability summaries and router candidates. It intentionally keeps
//! secrets out of output: only public Agent Card fields, local tags, and
//! non-secret metadata-derived labels are included.

use std::collections::BTreeSet;

use missive_a2a::{AgentCard, AgentCardExt};
use missive_router::capabilities_from_metadata;
use missive_store::AgentRecord;
use serde::Serialize;

/// Cache status for the Agent Card used to build a capability summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CapabilityCacheView {
    /// Stable cache status such as `cached`, `fetched`, `refreshed`, `not_modified`, or `missing`.
    pub status: String,
    /// Last successful cache fetch timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    /// Cached ETag validator, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// Cached Last-Modified validator, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

impl CapabilityCacheView {
    /// Builds a cache view from one stored agent record and a status label.
    pub(crate) fn from_record(record: &AgentRecord, status: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            fetched_at: record
                .agent_card_fetched_at
                .map(|timestamp| timestamp.to_rfc3339()),
            etag: record.agent_card_etag.clone(),
            last_modified: record.agent_card_last_modified.clone(),
        }
    }
}

/// One public Agent Card skill reduced to fields useful for routing and summaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SkillCapabilityView {
    /// A2A skill id.
    pub id: String,
    /// Human-readable skill name.
    pub name: String,
    /// Skill tags from the Agent Card.
    pub tags: Vec<String>,
    /// Input modes accepted by this skill.
    pub input_modes: Vec<String>,
    /// Output modes emitted by this skill.
    pub output_modes: Vec<String>,
}

/// Capability summary for one registered agent.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AgentCapabilitySummary {
    /// Registered missive alias.
    pub alias: String,
    /// Data source used for public Agent Card fields.
    pub source: String,
    /// Cache information for the Agent Card source.
    pub cache: CapabilityCacheView,
    /// Local registry tags only.
    pub local_tags: Vec<String>,
    /// Local tags plus Agent Card skill tags.
    pub tags: Vec<String>,
    /// Capability labels used by router capability-match.
    pub capability_labels: Vec<String>,
    /// Aggregated input modes from Agent Card defaults and skills.
    pub input_modes: Vec<String>,
    /// Aggregated output modes from Agent Card defaults and skills.
    pub output_modes: Vec<String>,
    /// Whether streaming support is known and advertised.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_streaming: Option<bool>,
    /// Whether push notification support is known and advertised.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_push_notifications: Option<bool>,
    /// Public A2A skills from the Agent Card.
    pub skills: Vec<SkillCapabilityView>,
}

/// Builds a capability summary from a parsed Agent Card.
#[must_use]
pub(crate) fn summarize_agent_capabilities(
    record: &AgentRecord,
    card: Option<&AgentCard>,
    cache: CapabilityCacheView,
) -> AgentCapabilitySummary {
    let mut tags = string_set(record.tags.iter().cloned());
    let mut labels = string_set(capabilities_from_metadata(&record.metadata));
    let mut input_modes = BTreeSet::new();
    let mut output_modes = BTreeSet::new();
    let mut skills = Vec::new();
    let mut supports_streaming = None;
    let mut supports_push_notifications = None;
    let source;

    if let Some(card) = card {
        source = "agent_card".to_owned();
        let summary = card.summary();
        supports_streaming = Some(summary.capabilities.streaming.unwrap_or(false));
        supports_push_notifications =
            Some(summary.capabilities.push_notifications.unwrap_or(false));
        if supports_streaming == Some(true) {
            labels.insert("streaming".to_owned());
        }
        if supports_push_notifications == Some(true) {
            labels.insert("push".to_owned());
            labels.insert("push-notifications".to_owned());
        }

        extend_normalized(&mut input_modes, summary.default_input_modes);
        extend_normalized(&mut output_modes, summary.default_output_modes);

        for skill in summary.skills {
            let skill_id = normalize_token(&skill.id);
            let skill_name = normalize_token(&skill.name);
            if !skill_id.is_empty() {
                labels.insert(skill_id.clone());
            }
            if !skill_name.is_empty() {
                labels.insert(skill_name.clone());
            }

            let skill_tags = string_set(skill.tags);
            for tag in &skill_tags {
                tags.insert(tag.clone());
                labels.insert(tag.clone());
            }

            let skill_input_modes = string_set(skill.input_modes.unwrap_or_default());
            let skill_output_modes = string_set(skill.output_modes.unwrap_or_default());
            for mode in &skill_input_modes {
                input_modes.insert(mode.clone());
            }
            for mode in &skill_output_modes {
                output_modes.insert(mode.clone());
            }

            skills.push(SkillCapabilityView {
                id: skill_id,
                name: skill_name,
                tags: skill_tags.into_iter().collect(),
                input_modes: skill_input_modes.into_iter().collect(),
                output_modes: skill_output_modes.into_iter().collect(),
            });
        }
    } else {
        source = if labels.is_empty() {
            "missing".to_owned()
        } else {
            "local_metadata".to_owned()
        };
    }

    AgentCapabilitySummary {
        alias: record.alias.as_str().to_owned(),
        source,
        cache,
        local_tags: string_set(record.tags.iter().cloned())
            .into_iter()
            .collect(),
        tags: tags.into_iter().collect(),
        capability_labels: labels.into_iter().collect(),
        input_modes: input_modes.into_iter().collect(),
        output_modes: output_modes.into_iter().collect(),
        supports_streaming,
        supports_push_notifications,
        skills,
    }
}

/// Returns a stable, normalized set of non-empty labels.
#[must_use]
pub(crate) fn string_set(values: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    values
        .into_iter()
        .map(|value| normalize_token(&value))
        .filter(|value| !value.is_empty())
        .collect()
}

fn extend_normalized(set: &mut BTreeSet<String>, values: impl IntoIterator<Item = String>) {
    for value in values {
        let normalized = normalize_token(&value);
        if !normalized.is_empty() {
            set.insert(normalized);
        }
    }
}

fn normalize_token(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}
