//! Dry-run route explanation command implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use crate::agent::{get_existing_agent, open_agent_registry};
use crate::output::{OutputMode, redact_text, render_success};
use clap::{Args, Subcommand};
use missive_core::{
    AgentAlias, GroupName, LoadedConfig, MissiveError, Result, parse_routing_policy,
};
use missive_router::{
    RouteCandidate, RouteDecision, RoutePlan, RoutePlanInput, capabilities_from_metadata,
    explain_route,
};
use missive_store::{AgentRecord, GroupMemberRecord, Store};
use serde::Serialize;

/// Route subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum RouteCommands {
    /// Explain a dry-run routing decision without sending a message.
    Explain(RouteExplainArgs),
}

impl RouteCommands {
    /// Stable subcommand spelling used in structured output.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Explain(_) => "explain",
        }
    }
}

/// Arguments for `missive route explain`.
#[derive(Debug, Clone, Args)]
pub struct RouteExplainArgs {
    /// Use this stored group as the route candidate set.
    #[arg(long = "group", value_name = "GROUP")]
    pub group: Option<String>,

    /// Use one registered agent as a route candidate; repeat for multiple candidates.
    #[arg(long = "agent", value_name = "ALIAS")]
    pub agents: Vec<String>,

    /// Override the group/config routing policy for this dry run.
    #[arg(long = "policy", value_name = "POLICY")]
    pub policy: Option<String>,

    /// Preferred primary/direct agent. The alias must be in the candidate set.
    #[arg(long = "preferred-agent", value_name = "ALIAS")]
    pub preferred_agent: Option<String>,

    /// Required local tag for tag-match explanations; repeat for multiple tags.
    #[arg(long = "tag", value_name = "TAG")]
    pub required_tags: Vec<String>,

    /// Required local capability label for capability-match explanations; repeat for multiple labels.
    #[arg(long = "capability", value_name = "CAPABILITY")]
    pub required_capabilities: Vec<String>,

    /// Deterministic cursor for round-robin dry runs.
    #[arg(long = "cursor", value_name = "N", default_value_t = 0)]
    pub cursor: u64,

    /// Required successful member count for quorum dry runs.
    #[arg(long = "quorum", value_name = "N")]
    pub quorum: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct RouteExplainOutput {
    profile: String,
    source: RouteSourceView,
    policy_source: String,
    plan: RoutePlan,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RouteSourceView {
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    candidates: usize,
}

/// Executes one route subcommand.
pub(crate) fn execute_route_command<W>(
    command: &RouteCommands,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    match command {
        RouteCommands::Explain(args) => {
            explain_route_command(args, loaded_config, environment, mode, writer)
        }
    }
}

fn explain_route_command<W>(
    args: &RouteExplainArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    validate_candidate_source(args)?;
    let registry = open_agent_registry(loaded_config, environment)?;
    let (source, stored_policy, candidates) = route_candidates(args, &registry.store)?;
    let (policy, policy_source) = resolve_policy(args, loaded_config, stored_policy.as_deref())?;
    let preferred_agent = args
        .preferred_agent
        .as_deref()
        .map(parse_agent_alias)
        .transpose()?;
    let input = RoutePlanInput {
        policy,
        candidates,
        preferred_agent,
        required_tags: args.required_tags.clone(),
        required_capabilities: args.required_capabilities.clone(),
        round_robin_cursor: args.cursor,
        quorum: args.quorum,
    };
    let plan = explain_route(&input)?;
    let selected_text = if plan.selected.is_empty() {
        "no candidates".to_owned()
    } else {
        plan.selected
            .iter()
            .map(|alias| alias.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let message = format!(
        "Route explain selected {selected_text} using {} policy",
        policy.as_str()
    );
    let output = RouteExplainOutput {
        profile: registry.profile,
        source,
        policy_source,
        plan,
        message,
    };

    render_route_explain(writer, mode, &output)
}

fn validate_candidate_source(args: &RouteExplainArgs) -> Result<()> {
    match (args.group.is_some(), args.agents.is_empty()) {
        (true, true) | (false, false) => Ok(()),
        (true, false) => Err(MissiveError::validation(
            "route explain accepts either --group or one or more --agent candidates, not both",
        )
        .with_help("Use a stored group, or pass explicit --agent flags for an ad-hoc dry run.")),
        (false, true) => Err(MissiveError::validation(
            "route explain requires --group or at least one --agent candidate",
        )
        .with_help("Use 'missive group list' or 'missive agent list' to choose candidates.")),
    }
}

fn route_candidates(
    args: &RouteExplainArgs,
    store: &Store,
) -> Result<(RouteSourceView, Option<String>, Vec<RouteCandidate>)> {
    if let Some(group) = &args.group {
        let group_name = GroupName::new(group.clone())?;
        let group = store.get_group(&group_name)?.ok_or_else(|| {
            MissiveError::validation(format!("group {:?} does not exist", group_name.as_str()))
                .with_help("Run 'missive group list' to see available groups.")
        })?;
        let members = store.list_group_members(&group.group_name)?;
        let mut candidates = Vec::with_capacity(members.len());
        for member in &members {
            let agent = get_existing_agent(store, &member.agent_alias)?;
            candidates.push(candidate_from_group_member(&agent, member));
        }
        let source = RouteSourceView {
            kind: "group".to_owned(),
            group: Some(group.group_name.as_str().to_owned()),
            candidates: candidates.len(),
        };
        Ok((source, Some(group.routing_policy), candidates))
    } else {
        let mut seen = BTreeSet::new();
        let mut candidates = Vec::with_capacity(args.agents.len());
        for alias in &args.agents {
            let alias = parse_agent_alias(alias)?;
            if !seen.insert(alias.clone()) {
                return Err(MissiveError::validation(format!(
                    "duplicate route candidate {:?}",
                    alias.as_str()
                ))
                .with_help("Pass each --agent candidate at most once."));
            }
            let agent = get_existing_agent(store, &alias)?;
            candidates.push(candidate_from_agent(&agent));
        }
        let source = RouteSourceView {
            kind: "agents".to_owned(),
            group: None,
            candidates: candidates.len(),
        };
        Ok((source, None, candidates))
    }
}

fn resolve_policy(
    args: &RouteExplainArgs,
    loaded_config: &LoadedConfig,
    stored_policy: Option<&str>,
) -> Result<(missive_core::RoutingPolicyKind, String)> {
    if let Some(policy) = &args.policy {
        return Ok((parse_routing_policy(policy)?, "cli".to_owned()));
    }
    if let Some(policy) = stored_policy {
        return Ok((parse_routing_policy(policy)?, "group".to_owned()));
    }
    let routing = loaded_config.routing_config()?;
    Ok((
        parse_routing_policy(&routing.default_policy)?,
        "config".to_owned(),
    ))
}

fn candidate_from_agent(agent: &AgentRecord) -> RouteCandidate {
    let mut candidate = RouteCandidate::new(agent.alias.clone());
    candidate.tags = stable_unique_strings(agent.tags.iter().cloned());
    candidate.capabilities = capabilities_from_metadata(&agent.metadata);
    candidate.metadata = agent.metadata.clone();
    candidate
}

fn candidate_from_group_member(agent: &AgentRecord, member: &GroupMemberRecord) -> RouteCandidate {
    let mut metadata = agent.metadata.clone();
    metadata.merge(member.routing_metadata.clone());

    let mut capabilities = capabilities_from_metadata(&agent.metadata);
    capabilities.extend(capabilities_from_metadata(&member.routing_metadata));

    RouteCandidate {
        alias: member.agent_alias.clone(),
        rank: Some(member.rank_name.clone()),
        tags: stable_unique_strings(agent.tags.iter().chain(member.tags.iter()).cloned()),
        capabilities: stable_unique_strings(capabilities),
        weight: member.weight,
        metadata,
    }
}

fn stable_unique_strings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn parse_agent_alias(value: &str) -> Result<AgentAlias> {
    AgentAlias::new(value.to_owned())
}

fn render_route_explain<W>(
    writer: &mut W,
    mode: OutputMode,
    output: &RouteExplainOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_route_explain_human(writer, output),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "route_explain", output, &output.message)
        }
    }
}

fn write_route_explain_human<W>(writer: &mut W, output: &RouteExplainOutput) -> Result<()>
where
    W: Write,
{
    writeln!(
        writer,
        "Route explain for profile '{}':",
        redact_text(&output.profile)
    )
    .map_err(|error| MissiveError::io("writing route explain output", error))?;
    let source = if let Some(group) = &output.source.group {
        format!("group {group}")
    } else {
        "explicit agents".to_owned()
    };
    writeln!(
        writer,
        "  source: {} ({} candidates)",
        redact_text(&source),
        output.source.candidates
    )
    .map_err(|error| MissiveError::io("writing route explain output", error))?;
    writeln!(
        writer,
        "  policy: {} ({})",
        output.plan.policy.as_str(),
        redact_text(&output.policy_source)
    )
    .map_err(|error| MissiveError::io("writing route explain output", error))?;
    writeln!(writer, "  mode: {}", redact_text(&output.plan.mode))
        .map_err(|error| MissiveError::io("writing route explain output", error))?;
    writeln!(writer, "  status: {}", redact_text(&output.plan.status))
        .map_err(|error| MissiveError::io("writing route explain output", error))?;
    if let Some(quorum) = output.plan.required_quorum {
        writeln!(writer, "  required_quorum: {quorum}")
            .map_err(|error| MissiveError::io("writing route explain output", error))?;
    }
    if let Some(cursor) = output.plan.next_round_robin_cursor {
        writeln!(writer, "  next_round_robin_cursor: {cursor}")
            .map_err(|error| MissiveError::io("writing route explain output", error))?;
    }
    writeln!(
        writer,
        "  selected: {}",
        if output.plan.selected.is_empty() {
            "-".to_owned()
        } else {
            redact_text(
                &output
                    .plan
                    .selected
                    .iter()
                    .map(|alias| alias.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        }
    )
    .map_err(|error| MissiveError::io("writing route explain output", error))?;
    writeln!(writer, "  decisions:")
        .map_err(|error| MissiveError::io("writing route explain output", error))?;
    for decision in &output.plan.decisions {
        write_decision_human(writer, decision)?;
    }

    Ok(())
}

fn write_decision_human<W>(writer: &mut W, decision: &RouteDecision) -> Result<()>
where
    W: Write,
{
    let rank = decision
        .rank
        .as_ref()
        .map(|rank| rank.as_str())
        .unwrap_or("-");
    let order = decision
        .order
        .map(|order| order.to_string())
        .unwrap_or_else(|| "-".to_owned());
    writeln!(
        writer,
        "    {}  rank={}  selected={}  order={}  weight={}  reason={}",
        redact_text(decision.alias.as_str()),
        redact_text(rank),
        decision.selected,
        order,
        decision.weight,
        redact_text(&decision.reason)
    )
    .map_err(|error| MissiveError::io("writing route explain output", error))
}
