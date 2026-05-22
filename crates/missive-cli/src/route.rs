//! Dry-run route explanation command implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use crate::agent::{
    AgentCardLoadMode, get_existing_agent, load_agent_card_for_capabilities, open_agent_registry,
};
use crate::capabilities::{string_set, summarize_agent_capabilities};
use crate::output::{OutputMode, redact_text, render_success};
use crate::{GlobalArgs, service_parameters_from_config_and_globals};
use clap::{Args, Subcommand};
use missive_core::{
    AgentAlias, GroupName, LoadedConfig, MissiveError, Result, RoutingPolicyKind,
    parse_routing_policy,
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

    /// Required local or Agent Card skill tag for tag-match/capability-match explanations.
    #[arg(long = "tag", value_name = "TAG")]
    pub required_tags: Vec<String>,

    /// Required local or Agent Card capability label for capability-match explanations; repeat for multiple labels.
    #[arg(long = "capability", value_name = "CAPABILITY")]
    pub required_capabilities: Vec<String>,

    /// Required Agent Card input MIME/media mode for capability-match explanations.
    #[arg(long = "input-mode", value_name = "MODE")]
    pub required_input_modes: Vec<String>,

    /// Required Agent Card output MIME/media mode for capability-match explanations.
    #[arg(long = "output-mode", value_name = "MODE")]
    pub required_output_modes: Vec<String>,

    /// Require Agent Card streaming support for capability-match explanations.
    #[arg(long = "streaming", action = clap::ArgAction::SetTrue)]
    pub require_streaming: bool,

    /// Require Agent Card push notification support for capability-match explanations.
    #[arg(long = "push-notifications", visible_alias = "push", action = clap::ArgAction::SetTrue)]
    pub require_push_notifications: bool,

    /// Revalidate/fetch Agent Cards before extracting capability data.
    #[arg(long = "refresh-capabilities", action = clap::ArgAction::SetTrue)]
    pub refresh_capabilities: bool,

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
    globals: &GlobalArgs,
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
            explain_route_command(args, globals, loaded_config, environment, mode, writer)
        }
    }
}

fn explain_route_command<W>(
    args: &RouteExplainArgs,
    globals: &GlobalArgs,
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
    let service_parameters = service_parameters_from_config_and_globals(loaded_config, globals)?;
    let (source, stored_policy, candidates) = route_candidates(
        args,
        &registry.store,
        globals,
        environment,
        &service_parameters,
    )?;
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
        required_input_modes: args.required_input_modes.clone(),
        required_output_modes: args.required_output_modes.clone(),
        require_streaming: args.require_streaming,
        require_push_notifications: args.require_push_notifications,
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
    let message = if plan.status == "no_match" && plan.policy == RoutingPolicyKind::CapabilityMatch
    {
        "No route candidates matched the requested capabilities; inspect missing_requirements or rerun with --refresh-capabilities".to_owned()
    } else {
        format!(
            "Route explain selected {selected_text} using {} policy",
            policy.as_str()
        )
    };
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
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: &missive_a2a::ServiceParameters,
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
            candidates.push(candidate_from_group_member(
                store,
                &agent,
                member,
                args.refresh_capabilities,
                globals,
                environment,
                service_parameters,
            )?);
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
            candidates.push(candidate_from_agent(
                store,
                &agent,
                args.refresh_capabilities,
                globals,
                environment,
                service_parameters,
            )?);
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

fn candidate_from_agent(
    store: &Store,
    agent: &AgentRecord,
    refresh_capabilities: bool,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: &missive_a2a::ServiceParameters,
) -> Result<RouteCandidate> {
    let summary = candidate_capability_summary(
        store,
        agent,
        refresh_capabilities,
        globals,
        environment,
        service_parameters,
    )?;
    let mut candidate = RouteCandidate::new(agent.alias.clone());
    candidate.tags = summary.tags.clone();
    candidate.capabilities = summary.capability_labels.clone();
    candidate.input_modes = summary.input_modes.clone();
    candidate.output_modes = summary.output_modes.clone();
    candidate.supports_streaming = summary.supports_streaming;
    candidate.supports_push_notifications = summary.supports_push_notifications;
    candidate.capability_cache_status = Some(summary.cache.status);
    candidate.metadata = agent.metadata.clone();
    Ok(candidate)
}

fn candidate_from_group_member(
    store: &Store,
    agent: &AgentRecord,
    member: &GroupMemberRecord,
    refresh_capabilities: bool,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: &missive_a2a::ServiceParameters,
) -> Result<RouteCandidate> {
    let summary = candidate_capability_summary(
        store,
        agent,
        refresh_capabilities,
        globals,
        environment,
        service_parameters,
    )?;
    let mut metadata = agent.metadata.clone();
    metadata.merge(member.routing_metadata.clone());

    let mut tags = string_set(summary.tags);
    tags.extend(string_set(member.tags.iter().cloned()));
    let mut capabilities = string_set(summary.capability_labels);
    capabilities.extend(string_set(capabilities_from_metadata(
        &member.routing_metadata,
    )));

    Ok(RouteCandidate {
        alias: member.agent_alias.clone(),
        rank: Some(member.rank_name.clone()),
        tags: tags.into_iter().collect(),
        capabilities: capabilities.into_iter().collect(),
        input_modes: summary.input_modes,
        output_modes: summary.output_modes,
        supports_streaming: summary.supports_streaming,
        supports_push_notifications: summary.supports_push_notifications,
        capability_cache_status: Some(summary.cache.status),
        weight: member.weight,
        metadata,
    })
}

fn candidate_capability_summary(
    store: &Store,
    agent: &AgentRecord,
    refresh_capabilities: bool,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: &missive_a2a::ServiceParameters,
) -> Result<crate::capabilities::AgentCapabilitySummary> {
    let load_mode = if refresh_capabilities {
        AgentCardLoadMode::Refresh
    } else {
        AgentCardLoadMode::CacheOnly
    };
    let loaded = load_agent_card_for_capabilities(
        store,
        agent,
        load_mode,
        globals,
        environment,
        service_parameters,
    )?;
    Ok(summarize_agent_capabilities(
        &loaded.record,
        loaded.card.as_ref(),
        loaded.cache,
    ))
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
    .map_err(|error| MissiveError::io("writing route explain output", error))?;
    if !decision.matched_tags.is_empty()
        || !decision.matched_capabilities.is_empty()
        || !decision.matched_input_modes.is_empty()
        || !decision.matched_output_modes.is_empty()
    {
        writeln!(
            writer,
            "      matched: tags={} capabilities={} input_modes={} output_modes={}",
            join_or_dash(&decision.matched_tags),
            join_or_dash(&decision.matched_capabilities),
            join_or_dash(&decision.matched_input_modes),
            join_or_dash(&decision.matched_output_modes)
        )
        .map_err(|error| MissiveError::io("writing route explain output", error))?;
    }
    if !decision.missing_requirements.is_empty() {
        writeln!(
            writer,
            "      missing_requirements: {}",
            join_or_dash(&decision.missing_requirements)
        )
        .map_err(|error| MissiveError::io("writing route explain output", error))?;
    }
    Ok(())
}

fn join_or_dash(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_owned()
    } else {
        redact_text(&values.join(","))
    }
}
