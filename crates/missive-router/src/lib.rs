#![doc = "Routing and collective-operation planning for missive."]

use std::collections::BTreeSet;

use missive_core::{AgentAlias, Metadata, MissiveError, RankName, Result, RoutingPolicyKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Cargo package name for this crate.
pub const CRATE_NAME: &str = "missive-router";

/// Short description of this crate's target responsibility.
pub const CRATE_PURPOSE: &str = "agent selection, policies, groups, and collectives";

/// Returns metadata for this crate.
#[must_use]
pub const fn crate_info() -> missive_core::CrateInfo {
    missive_core::CrateInfo::new(CRATE_NAME, CRATE_PURPOSE)
}

/// One local route candidate considered by a routing policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteCandidate {
    /// Registered agent alias.
    pub alias: AgentAlias,
    /// Optional group rank name used for deterministic ordering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<RankName>,
    /// Local and Agent Card skill tags from the agent row and/or group membership.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Local metadata labels plus Agent Card skill/capability labels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Input MIME/media modes advertised by the cached Agent Card.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modes: Vec<String>,
    /// Output MIME/media modes advertised by the cached Agent Card.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_modes: Vec<String>,
    /// Whether the cached Agent Card advertises streaming support.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_streaming: Option<bool>,
    /// Whether the cached Agent Card advertises push notification support.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_push_notifications: Option<bool>,
    /// Cache/source status for capability data, for example `cached` or `missing`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_cache_status: Option<String>,
    /// Positive routing weight.
    pub weight: f64,
    /// Non-secret routing metadata copied from local registry/group rows.
    #[serde(default)]
    pub metadata: Metadata,
}

impl RouteCandidate {
    /// Creates a route candidate with default weight `1.0` and no rank.
    pub fn new(alias: AgentAlias) -> Self {
        Self {
            alias,
            rank: None,
            tags: Vec::new(),
            capabilities: Vec::new(),
            input_modes: Vec::new(),
            output_modes: Vec::new(),
            supports_streaming: None,
            supports_push_notifications: None,
            capability_cache_status: None,
            weight: 1.0,
            metadata: Metadata::new(),
        }
    }

    fn validate(&self) -> Result<()> {
        if !(self.weight.is_finite() && self.weight > 0.0) {
            return Err(MissiveError::validation(format!(
                "route candidate {:?} has invalid weight {}; weights must be positive finite numbers",
                self.alias.as_str(),
                self.weight
            )));
        }

        validate_labels("tag", &self.tags)?;
        validate_labels("capability", &self.capabilities)?;
        validate_labels("input mode", &self.input_modes)?;
        validate_labels("output mode", &self.output_modes)
    }
}

/// Input to a dry-run route explanation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutePlanInput {
    /// Policy used to explain the decision.
    pub policy: RoutingPolicyKind,
    /// Ordered candidates considered by the policy.
    pub candidates: Vec<RouteCandidate>,
    /// Optional preferred/primary agent for direct and fallback policies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_agent: Option<AgentAlias>,
    /// Required local tags for tag-match decisions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_tags: Vec<String>,
    /// Required local or Agent Card capability labels for capability-match decisions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<String>,
    /// Required input MIME/media modes for capability-match decisions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_input_modes: Vec<String>,
    /// Required output MIME/media modes for capability-match decisions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_output_modes: Vec<String>,
    /// Require A2A streaming support for capability-match decisions.
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_streaming: bool,
    /// Require A2A push notification support for capability-match decisions.
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_push_notifications: bool,
    /// Deterministic cursor used by round-robin dry-runs.
    #[serde(default)]
    pub round_robin_cursor: u64,
    /// Requested quorum for quorum policy; defaults to majority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quorum: Option<usize>,
}

impl RoutePlanInput {
    fn validate(&self) -> Result<()> {
        if self.candidates.is_empty() {
            return Err(MissiveError::validation(
                "route explanation requires at least one candidate",
            )
            .with_help("Pass --agent at least once or choose a non-empty --group."));
        }

        for candidate in &self.candidates {
            candidate.validate()?;
        }
        validate_labels("required tag", &self.required_tags)?;
        validate_labels("required capability", &self.required_capabilities)?;
        validate_labels("required input mode", &self.required_input_modes)?;
        validate_labels("required output mode", &self.required_output_modes)?;

        if let Some(quorum) = self.quorum {
            validate_quorum(quorum, self.candidates.len())?;
        }

        Ok(())
    }
}

/// One candidate's policy explanation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteDecision {
    /// Registered agent alias.
    pub alias: AgentAlias,
    /// Optional group rank name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<RankName>,
    /// Whether this policy selects the candidate for the dry-run plan.
    pub selected: bool,
    /// Zero-based order among selected candidates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<usize>,
    /// Candidate routing weight.
    pub weight: f64,
    /// Human-readable deterministic decision reason.
    pub reason: String,
    /// Candidate tags that satisfied requested tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_tags: Vec<String>,
    /// Candidate capability labels that satisfied requested capabilities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_capabilities: Vec<String>,
    /// Candidate input modes that satisfied requested input modes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_input_modes: Vec<String>,
    /// Candidate output modes that satisfied requested output modes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_output_modes: Vec<String>,
    /// Missing requirements that kept the candidate from being selected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_requirements: Vec<String>,
}

/// Dry-run route explanation output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutePlan {
    /// Policy that produced this plan.
    pub policy: RoutingPolicyKind,
    /// High-level policy mode, for example `single`, `all`, or `ordered_attempts`.
    pub mode: String,
    /// Stable status string for automation.
    pub status: String,
    /// Candidate count before policy filtering.
    pub total_candidates: usize,
    /// Selected aliases in execution/attempt order.
    pub selected: Vec<AgentAlias>,
    /// Per-candidate explanations in input order.
    pub decisions: Vec<RouteDecision>,
    /// Quorum required by quorum policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_quorum: Option<usize>,
    /// Next cursor a caller may persist after applying round-robin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_round_robin_cursor: Option<u64>,
}

/// Builds a deterministic dry-run route plan for the requested policy.
pub fn explain_route(input: &RoutePlanInput) -> Result<RoutePlan> {
    input.validate()?;

    match input.policy {
        RoutingPolicyKind::Direct => explain_direct(input),
        RoutingPolicyKind::CapabilityMatch => explain_capability_match(input),
        RoutingPolicyKind::TagMatch => explain_tag_match(input),
        RoutingPolicyKind::RoundRobin => explain_round_robin(input),
        RoutingPolicyKind::Weighted => explain_weighted(input),
        RoutingPolicyKind::Broadcast => explain_all(input, "all", "broadcast"),
        RoutingPolicyKind::FirstSuccess => {
            explain_all(input, "ordered_attempts", "first-success-attempt-order")
        }
        RoutingPolicyKind::Quorum => explain_quorum(input),
        RoutingPolicyKind::Fallback => explain_fallback(input),
    }
}

/// Extracts capability labels from non-secret metadata values.
///
/// CLI-side Agent Card extraction augments these local labels with skill ids,
/// skill names, skill tags, input/output modes, streaming support, and push
/// support before calling the router.
#[must_use]
pub fn capabilities_from_metadata(metadata: &Metadata) -> Vec<String> {
    let mut labels = BTreeSet::new();
    for key in ["capability", "capabilities", "skill", "skills"] {
        if let Some(value) = metadata.get(key) {
            collect_labels(value, &mut labels);
        }
    }
    labels.into_iter().collect()
}

fn explain_direct(input: &RoutePlanInput) -> Result<RoutePlan> {
    let selected_index = preferred_index_or_first(input)?;
    let selected = vec![input.candidates[selected_index].alias.clone()];
    let decisions = input
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let is_selected = index == selected_index;
            decision(
                candidate,
                is_selected,
                is_selected.then_some(0),
                if is_selected {
                    if input.preferred_agent.is_some() {
                        "preferred direct target"
                    } else {
                        "first candidate selected by direct policy"
                    }
                } else {
                    "not selected by direct policy"
                },
                Vec::new(),
                Vec::new(),
            )
        })
        .collect();

    Ok(plan(
        input, "single", "selected", selected, decisions, None, None,
    ))
}

fn explain_capability_match(input: &RoutePlanInput) -> Result<RoutePlan> {
    let outcomes: Vec<_> = input
        .candidates
        .iter()
        .map(|candidate| candidate_capability_match(input, candidate))
        .collect();
    let has_requirements = capability_requirements_present(input);
    let mut selected_indices: Vec<usize> = outcomes
        .iter()
        .enumerate()
        .filter_map(|(index, outcome)| outcome.matches.then_some(index))
        .collect();

    if has_requirements {
        selected_indices.sort_by(|left, right| {
            outcomes[*right]
                .score
                .cmp(&outcomes[*left].score)
                .then_with(|| {
                    input.candidates[*right]
                        .weight
                        .total_cmp(&input.candidates[*left].weight)
                })
                .then_with(|| left.cmp(right))
        });
    }

    let selected = selected_indices
        .iter()
        .map(|index| input.candidates[*index].alias.clone())
        .collect::<Vec<_>>();
    let decisions = input
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let outcome = &outcomes[index];
            let decision_order = selected_indices
                .iter()
                .position(|selected_index| *selected_index == index);
            decision_with_details(
                candidate,
                outcome.matches,
                decision_order,
                capability_reason(
                    outcome.matches,
                    has_requirements,
                    &outcome.missing_requirements,
                ),
                DecisionRequirementDetails {
                    matched_tags: outcome.matched_tags.clone(),
                    matched_capabilities: outcome.matched_capabilities.clone(),
                    matched_input_modes: outcome.matched_input_modes.clone(),
                    matched_output_modes: outcome.matched_output_modes.clone(),
                    missing_requirements: outcome.missing_requirements.clone(),
                },
            )
        })
        .collect();

    let status = if selected.is_empty() {
        "no_match"
    } else {
        "selected"
    };
    Ok(plan(
        input, "filter", status, selected, decisions, None, None,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapabilityMatchOutcome {
    matches: bool,
    matched_tags: Vec<String>,
    matched_capabilities: Vec<String>,
    matched_input_modes: Vec<String>,
    matched_output_modes: Vec<String>,
    missing_requirements: Vec<String>,
    score: usize,
}

fn candidate_capability_match(
    input: &RoutePlanInput,
    candidate: &RouteCandidate,
) -> CapabilityMatchOutcome {
    let available_tags = normalized_set(&candidate.tags);
    let available_capabilities = normalized_set(&candidate.capabilities);
    let available_input_modes = normalized_set(&candidate.input_modes);
    let available_output_modes = normalized_set(&candidate.output_modes);

    let matched_tags = intersection_in_required_order(&input.required_tags, &available_tags);
    let matched_capabilities =
        intersection_in_required_order(&input.required_capabilities, &available_capabilities);
    let matched_input_modes =
        intersection_in_required_order(&input.required_input_modes, &available_input_modes);
    let matched_output_modes =
        intersection_in_required_order(&input.required_output_modes, &available_output_modes);

    let mut missing_requirements = Vec::new();
    push_missing_requirements(
        "tag",
        &input.required_tags,
        &available_tags,
        &mut missing_requirements,
    );
    push_missing_requirements(
        "capability",
        &input.required_capabilities,
        &available_capabilities,
        &mut missing_requirements,
    );
    push_missing_requirements(
        "input_mode",
        &input.required_input_modes,
        &available_input_modes,
        &mut missing_requirements,
    );
    push_missing_requirements(
        "output_mode",
        &input.required_output_modes,
        &available_output_modes,
        &mut missing_requirements,
    );

    if input.require_streaming && candidate.supports_streaming != Some(true) {
        missing_requirements.push(match candidate.supports_streaming {
            Some(false) => "streaming:false".to_owned(),
            None => "streaming:unknown".to_owned(),
            Some(true) => unreachable!("handled above"),
        });
    }
    if input.require_push_notifications && candidate.supports_push_notifications != Some(true) {
        missing_requirements.push(match candidate.supports_push_notifications {
            Some(false) => "push_notifications:false".to_owned(),
            None => "push_notifications:unknown".to_owned(),
            Some(true) => unreachable!("handled above"),
        });
    }

    let streaming_score =
        usize::from(input.require_streaming && candidate.supports_streaming == Some(true));
    let push_score = usize::from(
        input.require_push_notifications && candidate.supports_push_notifications == Some(true),
    );
    let score = matched_tags.len()
        + matched_capabilities.len()
        + matched_input_modes.len()
        + matched_output_modes.len()
        + streaming_score
        + push_score;

    CapabilityMatchOutcome {
        matches: missing_requirements.is_empty(),
        matched_tags,
        matched_capabilities,
        matched_input_modes,
        matched_output_modes,
        missing_requirements,
        score,
    }
}

fn capability_requirements_present(input: &RoutePlanInput) -> bool {
    !input.required_tags.is_empty()
        || !input.required_capabilities.is_empty()
        || !input.required_input_modes.is_empty()
        || !input.required_output_modes.is_empty()
        || input.require_streaming
        || input.require_push_notifications
}

fn capability_reason(matches: bool, has_requirements: bool, missing: &[String]) -> &'static str {
    if matches {
        if has_requirements {
            "candidate satisfies capability, tag, mode, streaming, and push requirements; ties use score, weight, then deterministic candidate order"
        } else {
            "no required capabilities, tags, modes, streaming, or push support; candidate remains eligible"
        }
    } else if missing.iter().any(|value| value.ends_with(":unknown")) {
        "candidate is missing required Agent Card capability data; refresh the Agent Card cache and inspect missing_requirements"
    } else {
        "candidate is missing one or more capability requirements; inspect missing_requirements"
    }
}

fn push_missing_requirements(
    kind: &str,
    required: &[String],
    available: &BTreeSet<String>,
    missing: &mut Vec<String>,
) {
    for requirement in required {
        let normalized = normalize_label(requirement);
        if !available.contains(&normalized) {
            missing.push(format!("{kind}:{normalized}"));
        }
    }
}

fn explain_tag_match(input: &RoutePlanInput) -> Result<RoutePlan> {
    let required = normalized_set(&input.required_tags);
    let mut order = 0usize;
    let mut selected = Vec::new();
    let decisions = input
        .candidates
        .iter()
        .map(|candidate| {
            let available = normalized_set(&candidate.tags);
            let matched = intersection_in_required_order(&input.required_tags, &available);
            let matches = required.is_empty() || required.is_subset(&available);
            let decision_order = if matches {
                selected.push(candidate.alias.clone());
                let current = order;
                order += 1;
                Some(current)
            } else {
                None
            };
            decision(
                candidate,
                matches,
                decision_order,
                if matches {
                    if required.is_empty() {
                        "no required tags; candidate remains eligible"
                    } else {
                        "candidate satisfies required tags"
                    }
                } else {
                    "candidate is missing one or more required tags"
                },
                matched,
                Vec::new(),
            )
        })
        .collect();

    let status = if selected.is_empty() {
        "no_match"
    } else {
        "selected"
    };
    Ok(plan(
        input, "filter", status, selected, decisions, None, None,
    ))
}

fn explain_round_robin(input: &RoutePlanInput) -> Result<RoutePlan> {
    let selected_index = (input.round_robin_cursor as usize) % input.candidates.len();
    let selected = vec![input.candidates[selected_index].alias.clone()];
    let decisions = input
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let is_selected = index == selected_index;
            decision(
                candidate,
                is_selected,
                is_selected.then_some(0),
                if is_selected {
                    "selected by round-robin cursor"
                } else {
                    "not selected for this cursor"
                },
                Vec::new(),
                Vec::new(),
            )
        })
        .collect();

    Ok(plan(
        input,
        "single",
        "selected",
        selected,
        decisions,
        None,
        Some(input.round_robin_cursor.saturating_add(1)),
    ))
}

fn explain_weighted(input: &RoutePlanInput) -> Result<RoutePlan> {
    let selected_index = input
        .candidates
        .iter()
        .enumerate()
        .max_by(|(left_index, left), (right_index, right)| {
            left.weight
                .total_cmp(&right.weight)
                .then_with(|| right_index.cmp(left_index))
        })
        .map(|(index, _)| index)
        .expect("validated non-empty candidates");
    let selected_weight = input.candidates[selected_index].weight;
    let selected = vec![input.candidates[selected_index].alias.clone()];
    let decisions = input
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let is_selected = index == selected_index;
            decision(
                candidate,
                is_selected,
                is_selected.then_some(0),
                if is_selected {
                    "highest weight selected; ties keep deterministic candidate order"
                } else if (candidate.weight - selected_weight).abs() < f64::EPSILON {
                    "same weight as selected candidate but later in deterministic order"
                } else {
                    "lower weight than selected candidate"
                },
                Vec::new(),
                Vec::new(),
            )
        })
        .collect();

    Ok(plan(
        input, "single", "selected", selected, decisions, None, None,
    ))
}

fn explain_all(input: &RoutePlanInput, mode: &str, reason: &str) -> Result<RoutePlan> {
    let selected: Vec<_> = input
        .candidates
        .iter()
        .map(|candidate| candidate.alias.clone())
        .collect();
    let decisions = input
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            decision(candidate, true, Some(index), reason, Vec::new(), Vec::new())
        })
        .collect();

    Ok(plan(
        input, mode, "selected", selected, decisions, None, None,
    ))
}

fn explain_quorum(input: &RoutePlanInput) -> Result<RoutePlan> {
    let quorum = input
        .quorum
        .unwrap_or_else(|| (input.candidates.len() / 2) + 1);
    validate_quorum(quorum, input.candidates.len())?;

    let selected: Vec<_> = input
        .candidates
        .iter()
        .map(|candidate| candidate.alias.clone())
        .collect();
    let decisions = input
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            decision(
                candidate,
                true,
                Some(index),
                "candidate participates in quorum attempt set",
                Vec::new(),
                Vec::new(),
            )
        })
        .collect();

    Ok(plan(
        input,
        "quorum",
        "selected",
        selected,
        decisions,
        Some(quorum),
        None,
    ))
}

fn explain_fallback(input: &RoutePlanInput) -> Result<RoutePlan> {
    let primary_index = preferred_index_or_first(input)?;
    let mut selected_indices = vec![primary_index];
    selected_indices.extend(
        input
            .candidates
            .iter()
            .enumerate()
            .filter_map(|(index, _)| (index != primary_index).then_some(index)),
    );
    let selected: Vec<_> = selected_indices
        .iter()
        .map(|index| input.candidates[*index].alias.clone())
        .collect();

    let decisions = input
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let order = selected_indices
                .iter()
                .position(|selected_index| *selected_index == index)
                .expect("all candidates are included");
            decision(
                candidate,
                true,
                Some(order),
                if index == primary_index {
                    "primary fallback target"
                } else {
                    "fallback candidate"
                },
                Vec::new(),
                Vec::new(),
            )
        })
        .collect();

    Ok(plan(
        input,
        "ordered_attempts",
        "selected",
        selected,
        decisions,
        None,
        None,
    ))
}

fn preferred_index_or_first(input: &RoutePlanInput) -> Result<usize> {
    if let Some(preferred) = &input.preferred_agent {
        input
            .candidates
            .iter()
            .position(|candidate| candidate.alias == *preferred)
            .ok_or_else(|| {
                MissiveError::validation(format!(
                    "preferred agent {:?} is not in the route candidate set",
                    preferred.as_str()
                ))
                .with_help("Pass a registered --agent candidate or choose a group containing the preferred agent.")
            })
    } else {
        Ok(0)
    }
}

fn plan(
    input: &RoutePlanInput,
    mode: &str,
    status: &str,
    selected: Vec<AgentAlias>,
    decisions: Vec<RouteDecision>,
    required_quorum: Option<usize>,
    next_round_robin_cursor: Option<u64>,
) -> RoutePlan {
    RoutePlan {
        policy: input.policy,
        mode: mode.to_owned(),
        status: status.to_owned(),
        total_candidates: input.candidates.len(),
        selected,
        decisions,
        required_quorum,
        next_round_robin_cursor,
    }
}

fn decision(
    candidate: &RouteCandidate,
    selected: bool,
    order: Option<usize>,
    reason: &str,
    matched_tags: Vec<String>,
    matched_capabilities: Vec<String>,
) -> RouteDecision {
    decision_with_details(
        candidate,
        selected,
        order,
        reason,
        DecisionRequirementDetails {
            matched_tags,
            matched_capabilities,
            ..DecisionRequirementDetails::default()
        },
    )
}

#[derive(Debug, Clone, Default)]
struct DecisionRequirementDetails {
    matched_tags: Vec<String>,
    matched_capabilities: Vec<String>,
    matched_input_modes: Vec<String>,
    matched_output_modes: Vec<String>,
    missing_requirements: Vec<String>,
}

fn decision_with_details(
    candidate: &RouteCandidate,
    selected: bool,
    order: Option<usize>,
    reason: &str,
    details: DecisionRequirementDetails,
) -> RouteDecision {
    RouteDecision {
        alias: candidate.alias.clone(),
        rank: candidate.rank.clone(),
        selected,
        order,
        weight: candidate.weight,
        reason: reason.to_owned(),
        matched_tags: details.matched_tags,
        matched_capabilities: details.matched_capabilities,
        matched_input_modes: details.matched_input_modes,
        matched_output_modes: details.matched_output_modes,
        missing_requirements: details.missing_requirements,
    }
}

fn validate_quorum(quorum: usize, candidates: usize) -> Result<()> {
    if quorum == 0 || quorum > candidates {
        return Err(MissiveError::validation(format!(
            "quorum must be between 1 and the candidate count ({candidates}); got {quorum}"
        ))
        .with_help("Lower --quorum or add more candidates to the group."));
    }
    Ok(())
}

const fn is_false(value: &bool) -> bool {
    !*value
}

fn validate_labels(kind: &str, labels: &[String]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for label in labels {
        let normalized = normalize_label(label);
        if normalized.is_empty() {
            return Err(MissiveError::validation(format!(
                "{kind} labels cannot be empty"
            )));
        }
        if !seen.insert(normalized) {
            return Err(MissiveError::validation(format!(
                "duplicate {kind} label {:?}",
                label
            )));
        }
    }
    Ok(())
}

fn normalized_set(labels: &[String]) -> BTreeSet<String> {
    labels.iter().map(|label| normalize_label(label)).collect()
}

fn normalize_label(label: &str) -> String {
    label.trim().to_ascii_lowercase()
}

fn intersection_in_required_order(
    required: &[String],
    available: &BTreeSet<String>,
) -> Vec<String> {
    required
        .iter()
        .map(|label| normalize_label(label))
        .filter(|label| available.contains(label))
        .collect()
}

fn collect_labels(value: &Value, labels: &mut BTreeSet<String>) {
    match value {
        Value::String(label) => {
            let label = normalize_label(label);
            if !label.is_empty() {
                labels.insert(label);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_labels(item, labels);
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                if value.as_bool() == Some(true) {
                    let label = normalize_label(key);
                    if !label.is_empty() {
                        labels.insert(label);
                    }
                } else {
                    collect_labels(value, labels);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    fn alias(value: &str) -> AgentAlias {
        AgentAlias::new(value.to_owned()).expect("alias")
    }

    fn candidate(value: &str) -> RouteCandidate {
        RouteCandidate::new(alias(value))
    }

    fn route_input(policy: RoutingPolicyKind, candidates: Vec<RouteCandidate>) -> RoutePlanInput {
        RoutePlanInput {
            policy,
            candidates,
            preferred_agent: None,
            required_tags: Vec::new(),
            required_capabilities: Vec::new(),
            required_input_modes: Vec::new(),
            required_output_modes: Vec::new(),
            require_streaming: false,
            require_push_notifications: false,
            round_robin_cursor: 0,
            quorum: None,
        }
    }

    #[test]
    fn crate_info_describes_router_layer() {
        let info = crate_info();

        assert_eq!(info.name(), CRATE_NAME);
        assert!(info.purpose().contains("collectives"));
    }

    #[test]
    fn direct_selects_preferred_or_first_candidate() {
        let input = RoutePlanInput {
            policy: RoutingPolicyKind::Direct,
            candidates: vec![candidate("alpha"), candidate("beta")],
            preferred_agent: Some(alias("beta")),
            required_tags: Vec::new(),
            required_capabilities: Vec::new(),
            required_input_modes: Vec::new(),
            required_output_modes: Vec::new(),
            require_streaming: false,
            require_push_notifications: false,
            round_robin_cursor: 0,
            quorum: None,
        };

        let plan = explain_route(&input).expect("plan");

        assert_eq!(plan.mode, "single");
        assert_eq!(plan.selected, vec![alias("beta")]);
        assert_eq!(plan.decisions[1].order, Some(0));
    }

    #[test]
    fn capability_match_filters_by_required_labels() {
        let mut alpha = candidate("alpha");
        alpha.capabilities = vec!["summarise".to_owned(), "json".to_owned()];
        let mut beta = candidate("beta");
        beta.capabilities = vec!["translate".to_owned()];
        let input = RoutePlanInput {
            policy: RoutingPolicyKind::CapabilityMatch,
            candidates: vec![alpha, beta],
            preferred_agent: None,
            required_tags: Vec::new(),
            required_capabilities: vec!["summarise".to_owned()],
            required_input_modes: Vec::new(),
            required_output_modes: Vec::new(),
            require_streaming: false,
            require_push_notifications: false,
            round_robin_cursor: 0,
            quorum: None,
        };

        let plan = explain_route(&input).expect("plan");

        assert_eq!(plan.selected, vec![alias("alpha")]);
        assert_eq!(plan.decisions[0].matched_capabilities, ["summarise"]);
        assert!(!plan.decisions[1].selected);
    }

    #[test]
    fn tag_match_filters_by_required_tags() {
        let mut alpha = candidate("alpha");
        alpha.tags = vec!["writer".to_owned(), "local".to_owned()];
        let mut beta = candidate("beta");
        beta.tags = vec!["reviewer".to_owned()];
        let input = RoutePlanInput {
            policy: RoutingPolicyKind::TagMatch,
            candidates: vec![alpha, beta],
            preferred_agent: None,
            required_tags: vec!["writer".to_owned()],
            required_capabilities: Vec::new(),
            required_input_modes: Vec::new(),
            required_output_modes: Vec::new(),
            require_streaming: false,
            require_push_notifications: false,
            round_robin_cursor: 0,
            quorum: None,
        };

        let plan = explain_route(&input).expect("plan");

        assert_eq!(plan.selected, vec![alias("alpha")]);
        assert_eq!(plan.status, "selected");
    }

    #[test]
    fn round_robin_uses_cursor_modulo_and_reports_next_cursor() {
        let input = RoutePlanInput {
            policy: RoutingPolicyKind::RoundRobin,
            candidates: vec![candidate("alpha"), candidate("beta"), candidate("gamma")],
            preferred_agent: None,
            required_tags: Vec::new(),
            required_capabilities: Vec::new(),
            required_input_modes: Vec::new(),
            required_output_modes: Vec::new(),
            require_streaming: false,
            require_push_notifications: false,
            round_robin_cursor: 4,
            quorum: None,
        };

        let plan = explain_route(&input).expect("plan");

        assert_eq!(plan.selected, vec![alias("beta")]);
        assert_eq!(plan.next_round_robin_cursor, Some(5));
    }

    #[test]
    fn weighted_selects_highest_weight_with_input_order_tie_break() {
        let mut alpha = candidate("alpha");
        alpha.weight = 3.0;
        let mut beta = candidate("beta");
        beta.weight = 3.0;
        let mut gamma = candidate("gamma");
        gamma.weight = 1.0;
        let input = RoutePlanInput {
            policy: RoutingPolicyKind::Weighted,
            candidates: vec![alpha, beta, gamma],
            preferred_agent: None,
            required_tags: Vec::new(),
            required_capabilities: Vec::new(),
            required_input_modes: Vec::new(),
            required_output_modes: Vec::new(),
            require_streaming: false,
            require_push_notifications: false,
            round_robin_cursor: 0,
            quorum: None,
        };

        let plan = explain_route(&input).expect("plan");

        assert_eq!(plan.selected, vec![alias("alpha")]);
        assert_eq!(
            plan.decisions[1].reason,
            "same weight as selected candidate but later in deterministic order"
        );
    }

    #[test]
    fn broadcast_selects_all_candidates_in_order() {
        let input = RoutePlanInput {
            policy: RoutingPolicyKind::Broadcast,
            candidates: vec![candidate("alpha"), candidate("beta")],
            preferred_agent: None,
            required_tags: Vec::new(),
            required_capabilities: Vec::new(),
            required_input_modes: Vec::new(),
            required_output_modes: Vec::new(),
            require_streaming: false,
            require_push_notifications: false,
            round_robin_cursor: 0,
            quorum: None,
        };

        let plan = explain_route(&input).expect("plan");

        assert_eq!(plan.mode, "all");
        assert_eq!(plan.selected, vec![alias("alpha"), alias("beta")]);
    }

    #[test]
    fn first_success_returns_ordered_attempt_plan() {
        let input = RoutePlanInput {
            policy: RoutingPolicyKind::FirstSuccess,
            candidates: vec![candidate("alpha"), candidate("beta")],
            preferred_agent: None,
            required_tags: Vec::new(),
            required_capabilities: Vec::new(),
            required_input_modes: Vec::new(),
            required_output_modes: Vec::new(),
            require_streaming: false,
            require_push_notifications: false,
            round_robin_cursor: 0,
            quorum: None,
        };

        let plan = explain_route(&input).expect("plan");

        assert_eq!(plan.mode, "ordered_attempts");
        assert_eq!(plan.decisions[1].order, Some(1));
    }

    #[test]
    fn quorum_defaults_to_majority_and_validates_requested_quorum() {
        let input = RoutePlanInput {
            policy: RoutingPolicyKind::Quorum,
            candidates: vec![candidate("alpha"), candidate("beta"), candidate("gamma")],
            preferred_agent: None,
            required_tags: Vec::new(),
            required_capabilities: Vec::new(),
            required_input_modes: Vec::new(),
            required_output_modes: Vec::new(),
            require_streaming: false,
            require_push_notifications: false,
            round_robin_cursor: 0,
            quorum: None,
        };

        let plan = explain_route(&input).expect("plan");
        assert_eq!(plan.required_quorum, Some(2));

        let bad = RoutePlanInput {
            quorum: Some(4),
            ..input
        };
        assert!(explain_route(&bad).is_err());
    }

    #[test]
    fn fallback_places_preferred_candidate_first_then_remaining_order() {
        let input = RoutePlanInput {
            policy: RoutingPolicyKind::Fallback,
            candidates: vec![candidate("alpha"), candidate("beta"), candidate("gamma")],
            preferred_agent: Some(alias("beta")),
            required_tags: Vec::new(),
            required_capabilities: Vec::new(),
            required_input_modes: Vec::new(),
            required_output_modes: Vec::new(),
            require_streaming: false,
            require_push_notifications: false,
            round_robin_cursor: 0,
            quorum: None,
        };

        let plan = explain_route(&input).expect("plan");

        assert_eq!(
            plan.selected,
            vec![alias("beta"), alias("alpha"), alias("gamma")]
        );
        assert_eq!(plan.decisions[1].order, Some(0));
    }

    #[test]
    fn capability_match_uses_modes_streaming_push_tags_and_weight_tie_break() {
        let mut alpha = candidate("alpha");
        alpha.tags = vec!["research".to_owned()];
        alpha.capabilities = vec!["summarise".to_owned()];
        alpha.input_modes = vec!["text/plain".to_owned()];
        alpha.output_modes = vec!["application/json".to_owned()];
        alpha.supports_streaming = Some(true);
        alpha.supports_push_notifications = Some(true);
        alpha.weight = 1.0;
        let mut beta = alpha.clone();
        beta.alias = alias("beta");
        beta.weight = 3.0;
        let mut gamma = candidate("gamma");
        gamma.tags = vec!["research".to_owned()];
        gamma.capabilities = vec!["summarise".to_owned()];
        gamma.input_modes = vec!["text/plain".to_owned()];
        gamma.output_modes = vec!["text/plain".to_owned()];
        gamma.supports_streaming = Some(true);
        gamma.supports_push_notifications = Some(false);

        let input = RoutePlanInput {
            policy: RoutingPolicyKind::CapabilityMatch,
            candidates: vec![alpha, beta, gamma],
            preferred_agent: None,
            required_tags: vec!["research".to_owned()],
            required_capabilities: vec!["summarise".to_owned()],
            required_input_modes: vec!["text/plain".to_owned()],
            required_output_modes: vec!["application/json".to_owned()],
            require_streaming: true,
            require_push_notifications: true,
            round_robin_cursor: 0,
            quorum: None,
        };

        let plan = explain_route(&input).expect("plan");

        assert_eq!(plan.selected, vec![alias("beta"), alias("alpha")]);
        assert_eq!(plan.decisions[0].order, Some(1));
        assert_eq!(plan.decisions[1].order, Some(0));
        assert_eq!(plan.decisions[0].matched_input_modes, ["text/plain"]);
        assert_eq!(plan.decisions[0].matched_output_modes, ["application/json"]);
        assert_eq!(
            plan.decisions[2].missing_requirements,
            ["output_mode:application/json", "push_notifications:false"]
        );
    }

    #[test]
    fn capability_match_reports_unknown_agent_card_data_actionably() {
        let input = RoutePlanInput {
            policy: RoutingPolicyKind::CapabilityMatch,
            candidates: vec![candidate("alpha")],
            preferred_agent: None,
            required_tags: Vec::new(),
            required_capabilities: Vec::new(),
            required_input_modes: Vec::new(),
            required_output_modes: Vec::new(),
            require_streaming: true,
            require_push_notifications: false,
            round_robin_cursor: 0,
            quorum: None,
        };

        let plan = explain_route(&input).expect("plan");

        assert_eq!(plan.status, "no_match");
        assert!(plan.selected.is_empty());
        assert_eq!(
            plan.decisions[0].missing_requirements,
            ["streaming:unknown"]
        );
        assert!(plan.decisions[0].reason.contains("refresh"));
    }

    #[test]
    fn metadata_capabilities_extract_strings_arrays_and_boolean_maps() {
        let metadata = Metadata::try_from_iter([
            ("capabilities", json!(["summarise", "json"])),
            ("skills", json!({"vote": true, "ignore": false})),
        ])
        .expect("metadata");

        assert_eq!(
            capabilities_from_metadata(&metadata),
            vec!["json".to_owned(), "summarise".to_owned(), "vote".to_owned()]
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(96))]

        #[test]
        fn round_robin_selection_is_cursor_modulo_candidate_count(
            candidate_count in 1usize..16,
            cursor in any::<u64>(),
        ) {
            let candidates = (0..candidate_count)
                .map(|index| candidate(&format!("agent{index}")))
                .collect::<Vec<_>>();
            let mut input = route_input(RoutingPolicyKind::RoundRobin, candidates);
            input.round_robin_cursor = cursor;
            let plan = explain_route(&input).expect("round-robin plan");
            let expected_index = (cursor as usize) % candidate_count;
            let expected_alias = alias(&format!("agent{expected_index}"));

            prop_assert_eq!(plan.selected, vec![expected_alias]);
            prop_assert_eq!(plan.next_round_robin_cursor, Some(cursor.saturating_add(1)));
            prop_assert_eq!(plan.decisions.iter().filter(|decision| decision.selected).count(), 1);
            prop_assert_eq!(plan.decisions[expected_index].order, Some(0));
        }

        #[test]
        fn weighted_selection_chooses_first_highest_weight(
            weights in prop::collection::vec(1u16..=1_000, 1..16),
        ) {
            let candidates = weights
                .iter()
                .enumerate()
                .map(|(index, weight)| {
                    let mut candidate = candidate(&format!("agent{index}"));
                    candidate.weight = f64::from(*weight);
                    candidate
                })
                .collect::<Vec<_>>();
            let input = route_input(RoutingPolicyKind::Weighted, candidates);
            let plan = explain_route(&input).expect("weighted plan");
            let max_weight = weights.iter().max().copied().expect("non-empty generated weights");
            let expected_index = weights
                .iter()
                .position(|weight| *weight == max_weight)
                .expect("max weight has an index");

            prop_assert_eq!(plan.selected, vec![alias(&format!("agent{expected_index}"))]);
            prop_assert!(plan.decisions[expected_index].selected);
            prop_assert_eq!(plan.decisions[expected_index].order, Some(0));
        }

        #[test]
        fn tag_match_selects_exactly_candidates_with_all_required_tags(
            membership in prop::collection::vec(any::<bool>(), 1..16),
        ) {
            let candidates = membership
                .iter()
                .enumerate()
                .map(|(index, has_required_tag)| {
                    let mut candidate = candidate(&format!("agent{index}"));
                    candidate.tags = if *has_required_tag {
                        vec!["required".to_owned(), "shared".to_owned()]
                    } else {
                        vec!["shared".to_owned()]
                    };
                    candidate
                })
                .collect::<Vec<_>>();
            let mut input = route_input(RoutingPolicyKind::TagMatch, candidates);
            input.required_tags = vec!["required".to_owned()];

            let plan = explain_route(&input).expect("tag-match plan");
            let expected = membership
                .iter()
                .enumerate()
                .filter(|(_, selected)| **selected)
                .map(|(index, _)| alias(&format!("agent{index}")))
                .collect::<Vec<_>>();

            let expected_status = if expected.is_empty() { "no_match" } else { "selected" };
            prop_assert_eq!(&plan.selected, &expected);
            prop_assert_eq!(plan.status, expected_status);
            for (decision, expected_selected) in plan.decisions.iter().zip(membership) {
                prop_assert_eq!(decision.selected, expected_selected);
            }
        }
    }
}
