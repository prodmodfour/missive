//! Reduce collective command implementation.
//!
//! `missive reduce` is the final basic collective: it reads the same local
//! rank-ordered member outputs that `missive gather` exposes, then turns them
//! into one reduced result with provenance. Reduction can be performed locally
//! with deterministic strategies, by sending a generated prompt to a registered
//! reducer agent, or by piping that prompt to a user-supplied local command.

use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;

use clap::{Args, ValueEnum};
use missive_a2a::{
    NegotiatedInterface, SendMessageClient, SendMessageOutcome, ServiceParameters,
    protocol::{Message, SendMessageResponse, Task},
};
use missive_core::{
    AgentAlias, ContextId, GroupName, LoadedConfig, Metadata, MissiveError, Result, TaskId,
};
use missive_store::{
    AgentRecord, ArtifactRecord, ContextUpsert, GroupMemberRecord, GroupRecord, MessageDirection,
    MessageInsert, MessageRecord, MessageRole, Store, TaskRecord,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::agent::{get_existing_agent, open_agent_registry};
use crate::artifact::{ArtifactSummaryView, first_artifact_text_from_records};
use crate::auth::auth_headers_for_agent;
use crate::events::new_cli_event;
use crate::output::{OutputMode, redact_text, render_success};
use crate::send::{
    PersistedSend, SendArgs, message_part_limit_bytes, new_local_message_id, persist_send,
    prepare_send_request, resolve_send_interface_with_store, store_task_state,
};
use crate::{GlobalArgs, service_parameters_from_config_and_globals};

/// Arguments for `missive reduce`.
#[derive(Debug, Clone, Args)]
pub struct ReduceArgs {
    /// Local group name whose gathered member outputs should be reduced.
    pub group: String,

    /// A2A context id shared by the member tasks to reduce.
    #[arg(long = "context", value_name = "CONTEXT_ID", required = true)]
    pub context: String,

    /// Reduction strategy to apply or describe in the generated prompt.
    #[arg(long = "strategy", value_name = "STRATEGY", default_value_t = ReduceStrategy::Summarise, value_enum)]
    pub strategy: ReduceStrategy,

    /// Registered agent alias that should perform the final reduction.
    #[arg(long = "reducer-agent", value_name = "ALIAS")]
    pub reducer_agent: Option<String>,

    /// Local command pipeline to receive the generated prompt on stdin and return the reduced output on stdout.
    #[arg(long = "command", value_name = "COMMAND")]
    pub command: Option<String>,

    /// Template or custom prompt. Supports {{group}}, {{context_id}}, {{strategy}}, {{input_count}}, {{inputs}}, and {{default_reduction}}.
    #[arg(long = "template", value_name = "TEXT")]
    pub template: Option<String>,

    /// Accepted reducer-agent response MIME/output mode; repeatable.
    #[arg(long = "accepted-output-mode", value_name = "MIME")]
    pub accepted_output_modes: Vec<String>,

    /// Add non-secret metadata to reducer-agent requests and the local reduced-output message.
    #[arg(long = "metadata", value_name = "KEY=VALUE")]
    pub metadata: Vec<String>,
}

/// Supported reduce strategies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReduceStrategy {
    /// Produce a concise source-attributed summary.
    #[value(alias = "summarize")]
    Summarise,
    /// Count identical member answers and report the deterministic winner.
    Vote,
    /// Concatenate member outputs in rank order.
    Merge,
    /// Rank member outputs using a deterministic local heuristic.
    Rank,
    /// Use the provided --template as a custom prompt/output template.
    Custom,
}

impl ReduceStrategy {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Summarise => "summarise",
            Self::Vote => "vote",
            Self::Merge => "merge",
            Self::Rank => "rank",
            Self::Custom => "custom",
        }
    }
}

impl std::fmt::Display for ReduceStrategy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug)]
struct ReduceOperation {
    operation_id: String,
    group: GroupRecord,
    members: Vec<GroupMemberRecord>,
    context_id: ContextId,
    started_at: Instant,
}

#[derive(Debug, Clone)]
struct ReduceMemberState {
    member: GroupMemberRecord,
    task: Option<TaskRecord>,
    messages: Vec<MessageRecord>,
    artifacts: Vec<ArtifactRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ReduceTaskView {
    task_id: String,
    state: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    protocol_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_message_id: Option<String>,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ReduceSourceMessageView {
    message_id: String,
    direction: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    ordinal: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    protocol_message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ReduceSourceArtifactView {
    artifact_id: String,
    task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime_type: Option<String>,
    kind: String,
    version: u64,
    part_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ReduceInputView {
    index: usize,
    agent: String,
    rank: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<ReduceTaskView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    message_count: usize,
    messages: Vec<ReduceSourceMessageView>,
    artifact_count: usize,
    artifacts: Vec<ReduceSourceArtifactView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ReduceInterfaceView {
    binding: String,
    protocol_binding: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
    protocol_version: String,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ReduceAgentView {
    agent: String,
    selected_interface: ReduceInterfaceView,
    request_message_id: String,
    response_shape: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ReduceCommandView {
    command: String,
    exit_code: Option<i32>,
    stdout_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ReduceReducerView {
    method: String,
    strategy: String,
    prompt_bytes: u64,
    template_applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<ReduceAgentView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<ReduceCommandView>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ReducePersistenceView {
    reduced_message_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ReduceOutput {
    profile: String,
    operation_id: String,
    group: String,
    context_id: String,
    strategy: String,
    status: String,
    member_count: usize,
    gathered_count: usize,
    missing_count: usize,
    empty_count: usize,
    input_count: usize,
    text_input_count: usize,
    elapsed_ms: u128,
    reducer: ReduceReducerView,
    reduced_text: String,
    provenance: Vec<ReduceInputView>,
    persistence: ReducePersistenceView,
    message: String,
}

#[derive(Debug, Clone)]
struct ReductionResult {
    method: String,
    prompt: String,
    template_applied: bool,
    reduced_text: String,
    agent: Option<ReduceAgentView>,
    command: Option<ReduceCommandView>,
}

struct ReduceRuntime<'a> {
    globals: &'a GlobalArgs,
    environment: &'a BTreeMap<String, String>,
    store: &'a mut Store,
    service_parameters: &'a ServiceParameters,
    loaded_config: &'a LoadedConfig,
}

/// Executes `missive reduce`.
pub(crate) fn execute_reduce_command<W>(
    args: &ReduceArgs,
    globals: &GlobalArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    validate_reduce_args(args)?;
    let service_parameters = service_parameters_from_config_and_globals(loaded_config, globals)?;
    let mut registry = open_agent_registry(loaded_config, environment)?;
    let operation = prepare_reduce_operation(args, &mut registry.store)?;
    let member_states = collect_reduce_members(&registry.store, &operation)?;
    let provenance = provenance_from_states(&member_states);
    ensure_reduce_has_inputs(&provenance)?;
    append_reduce_started_event(&registry.store, args, &operation, &provenance)?;

    let reduction = {
        let mut runtime = ReduceRuntime {
            globals,
            environment,
            store: &mut registry.store,
            service_parameters: &service_parameters,
            loaded_config,
        };
        match run_reducer(args, &operation, &provenance, &mut runtime) {
            Ok(reduction) => reduction,
            Err(error) => {
                append_reduce_failed_event(runtime.store, &operation, &error)?;
                return Err(error);
            }
        }
    };

    let reduced_message = persist_reduced_output(
        &registry.store,
        args,
        &operation,
        &provenance,
        &reduction,
        &service_parameters,
    )?;
    let output = finalize_reduce_output(
        registry.profile.clone(),
        args,
        &operation,
        provenance,
        reduction,
        &reduced_message,
    );
    append_reduce_input_events(&registry.store, &output)?;
    append_reduce_completed_event(&registry.store, &output)?;
    render_reduce_success(writer, mode, &output)
}

fn validate_reduce_args(args: &ReduceArgs) -> Result<()> {
    if args.reducer_agent.is_some() && args.command.is_some() {
        return Err(MissiveError::validation(
            "missive reduce accepts either --reducer-agent or --command, not both",
        )
        .with_help("Omit both flags for a deterministic local reduction."));
    }
    if args.strategy == ReduceStrategy::Custom && args.template.is_none() {
        return Err(MissiveError::validation(
            "missive reduce --strategy custom requires --template",
        )
        .with_help("Provide a prompt template containing {{inputs}} or use a built-in strategy."));
    }
    Ok(())
}

fn prepare_reduce_operation(args: &ReduceArgs, store: &mut Store) -> Result<ReduceOperation> {
    let group_name = GroupName::new(args.group.clone())?;
    let context_id = ContextId::new(args.context.clone())?;
    let group = store.get_group(&group_name)?.ok_or_else(|| {
        MissiveError::validation(format!("group {:?} does not exist", group_name.as_str()))
            .with_help("Run 'missive group list' to see locally known groups.")
    })?;
    let members = store.list_group_members(&group.group_name)?;
    if members.is_empty() {
        return Err(MissiveError::validation(format!(
            "group {:?} has no members for reduce",
            group.group_name.as_str()
        ))
        .with_help("Add members with 'missive group add <group> <agent> --rank <rank>' before running reduce."));
    }
    ensure_reduce_context(store, &context_id, &group)?;
    Ok(ReduceOperation {
        operation_id: format!("reduce/{}", missive_a2a::protocol::new_message_id()),
        group,
        members,
        context_id,
        started_at: Instant::now(),
    })
}

fn ensure_reduce_context(store: &Store, context_id: &ContextId, group: &GroupRecord) -> Result<()> {
    if store.get_context(context_id)?.is_some() {
        return Ok(());
    }
    let mut context = ContextUpsert::new(context_id.clone());
    context.summary = Some(format!(
        "Reduce collective context for group '{}'",
        group.group_name.as_str()
    ));
    context
        .metadata
        .insert_str("missive.collective", "reduce")?;
    context
        .metadata
        .insert_str("missive.group", group.group_name.as_str())?;
    store.upsert_context(&context)?;
    Ok(())
}

fn collect_reduce_members(
    store: &Store,
    operation: &ReduceOperation,
) -> Result<Vec<ReduceMemberState>> {
    operation
        .members
        .iter()
        .cloned()
        .map(|member| {
            let task = latest_member_task(store, &member, &operation.context_id)?;
            let (messages, artifacts) = if let Some(task) = &task {
                (
                    output_messages_for_task(store, &member, &operation.context_id, &task.task_id)?,
                    store.list_artifacts_for_task(&task.task_id)?,
                )
            } else {
                (Vec::new(), Vec::new())
            };
            Ok(ReduceMemberState {
                member,
                task,
                messages,
                artifacts,
            })
        })
        .collect()
}

fn latest_member_task(
    store: &Store,
    member: &GroupMemberRecord,
    context_id: &ContextId,
) -> Result<Option<TaskRecord>> {
    Ok(store
        .list_tasks()?
        .into_iter()
        .filter(|record| record.agent_alias == member.agent_alias)
        .filter(|record| record.context_id.as_ref() == Some(context_id))
        .max_by(|left, right| {
            left.updated_at
                .cmp(&right.updated_at)
                .then_with(|| left.task_id.as_str().cmp(right.task_id.as_str()))
        }))
}

fn output_messages_for_task(
    store: &Store,
    member: &GroupMemberRecord,
    context_id: &ContextId,
    task_id: &TaskId,
) -> Result<Vec<MessageRecord>> {
    Ok(store
        .list_messages()?
        .into_iter()
        .filter(|record| record.agent_alias.as_ref() == Some(&member.agent_alias))
        .filter(|record| record.context_id.as_ref() == Some(context_id))
        .filter(|record| record.task_id.as_ref() == Some(task_id))
        .filter(|record| !matches!(record.direction, MessageDirection::Request))
        .collect())
}

fn provenance_from_states(states: &[ReduceMemberState]) -> Vec<ReduceInputView> {
    states
        .iter()
        .enumerate()
        .map(|(index, state)| ReduceInputView::from_state(index, state))
        .collect()
}

fn ensure_reduce_has_inputs(provenance: &[ReduceInputView]) -> Result<()> {
    if provenance.iter().any(|input| input.status == "gathered") {
        return Ok(());
    }
    Err(MissiveError::validation(
        "missive reduce found no gathered member outputs in the selected context",
    )
    .with_help("Run 'missive barrier' and 'missive gather' first, or refresh tasks with 'missive task get --remote'."))
}

fn run_reducer(
    args: &ReduceArgs,
    operation: &ReduceOperation,
    provenance: &[ReduceInputView],
    runtime: &mut ReduceRuntime<'_>,
) -> Result<ReductionResult> {
    let default_reduction = local_strategy_output(args.strategy, operation, provenance)?;
    let prompt = reduction_prompt(args, operation, provenance, &default_reduction)?;
    let template_applied = args.template.is_some();

    if let Some(agent) = &args.reducer_agent {
        return run_agent_reducer(agent, args, operation, prompt, template_applied, runtime);
    }

    if let Some(command) = &args.command {
        return run_command_reducer(command, prompt, template_applied);
    }

    let reduced_text = if template_applied {
        prompt.clone()
    } else {
        default_reduction
    };
    Ok(ReductionResult {
        method: "local".to_owned(),
        prompt,
        template_applied,
        reduced_text,
        agent: None,
        command: None,
    })
}

fn run_agent_reducer(
    agent_alias: &str,
    args: &ReduceArgs,
    operation: &ReduceOperation,
    prompt: String,
    template_applied: bool,
    runtime: &mut ReduceRuntime<'_>,
) -> Result<ReductionResult> {
    let alias = AgentAlias::new(agent_alias.to_owned())?;
    let agent = get_existing_agent(runtime.store, &alias)?;
    let auth_headers =
        auth_headers_for_agent(runtime.store, &agent, runtime.globals, runtime.environment)?;
    let (agent, selected_interface) = resolve_send_interface_with_store(
        runtime.store,
        agent,
        runtime.service_parameters,
        &auth_headers,
    )?;
    let prepared = prepare_reducer_send(
        args,
        runtime.loaded_config,
        operation,
        runtime.service_parameters,
        &prompt,
    )?;
    let client = SendMessageClient::new()?;
    let outcome = client.send_message(
        &selected_interface,
        &prepared.request,
        runtime.service_parameters,
        &auth_headers,
    )?;
    let persisted = persist_send(
        runtime.store,
        &agent,
        &prepared,
        &outcome,
        runtime.service_parameters,
    )?;
    let reduced_text =
        reducer_response_text(&outcome).unwrap_or_else(|| fallback_agent_text(&outcome));
    let agent_view = ReduceAgentView::from_parts(&agent, &selected_interface, &outcome, &persisted);

    Ok(ReductionResult {
        method: "agent".to_owned(),
        prompt,
        template_applied,
        reduced_text,
        agent: Some(agent_view),
        command: None,
    })
}

fn prepare_reducer_send(
    args: &ReduceArgs,
    loaded_config: &LoadedConfig,
    operation: &ReduceOperation,
    service_parameters: &ServiceParameters,
    prompt: &str,
) -> Result<crate::send::PreparedSend> {
    let mut metadata = args.metadata.clone();
    metadata.push("missive.collective=reduce".to_owned());
    metadata.push(format!(
        "missive.reduce.operation_id={}",
        operation.operation_id
    ));
    metadata.push(format!(
        "missive.reduce.strategy={}",
        args.strategy.as_str()
    ));
    metadata.push(format!(
        "missive.group={}",
        operation.group.group_name.as_str()
    ));
    let send_args = SendArgs {
        agent: args.reducer_agent.clone().unwrap_or_default(),
        message: Some(prompt.to_owned()),
        stdin: false,
        files: Vec::new(),
        file_bytes: Vec::new(),
        json_parts: Vec::new(),
        mime: vec!["text/plain".to_owned()],
        parts: Vec::new(),
        metadata,
        context: Some(operation.context_id.as_str().to_owned()),
        task: None,
        accepted_output_modes: args.accepted_output_modes.clone(),
    };
    let max_request_bytes = message_part_limit_bytes(loaded_config)?;
    prepare_send_request(
        &send_args,
        service_parameters,
        max_request_bytes,
        &mut std::io::empty(),
    )
}

fn run_command_reducer(
    command: &str,
    prompt: String,
    template_applied: bool,
) -> Result<ReductionResult> {
    if command.trim().is_empty() {
        return Err(MissiveError::validation("--command cannot be empty"));
    }
    let mut child = shell_command(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| MissiveError::io(format!("starting reduce command {command:?}"), error))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .map_err(|error| MissiveError::io("writing reduce prompt to command stdin", error))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| MissiveError::io("waiting for reduce command", error))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(MissiveError::orchestration(format!(
            "reduce command pipeline exited with status {}",
            output.status
        ))
        .with_help(format!(
            "Inspect the command locally. Redacted stderr: {}",
            redact_text(stderr.trim())
        )));
    }
    let reduced_text = String::from_utf8(output.stdout).map_err(|error| {
        MissiveError::orchestration("reduce command stdout was not valid UTF-8")
            .with_source(error)
            .with_help("Ensure reducer command pipelines write UTF-8 text to stdout.")
    })?;
    let reduced_text = reduced_text.trim_end().to_owned();
    if reduced_text.is_empty() {
        return Err(
            MissiveError::orchestration("reduce command pipeline produced empty stdout").with_help(
                "Reducer command pipelines must write the final reduced output to stdout.",
            ),
        );
    }
    let stdout_bytes = usize_to_u64(reduced_text.len(), "reduce command stdout")?;
    Ok(ReductionResult {
        method: "command".to_owned(),
        prompt,
        template_applied,
        reduced_text,
        agent: None,
        command: Some(ReduceCommandView {
            command: command.to_owned(),
            exit_code: output.status.code(),
            stdout_bytes,
        }),
    })
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(command);
    shell
}

fn local_strategy_output(
    strategy: ReduceStrategy,
    operation: &ReduceOperation,
    provenance: &[ReduceInputView],
) -> Result<String> {
    match strategy {
        ReduceStrategy::Summarise => Ok(local_summary(operation, provenance)),
        ReduceStrategy::Vote => local_vote(provenance),
        ReduceStrategy::Merge => Ok(local_merge(operation, provenance)),
        ReduceStrategy::Rank => Ok(local_rank(operation, provenance)),
        ReduceStrategy::Custom => Ok(format_inputs_markdown(provenance)),
    }
}

fn reduction_prompt(
    args: &ReduceArgs,
    operation: &ReduceOperation,
    provenance: &[ReduceInputView],
    default_reduction: &str,
) -> Result<String> {
    if let Some(template) = &args.template {
        return render_template(
            template,
            operation,
            args.strategy,
            provenance,
            default_reduction,
        );
    }
    let instruction = match args.strategy {
        ReduceStrategy::Summarise => {
            "Summarise the member outputs into one concise answer. Preserve source references."
        }
        ReduceStrategy::Vote => {
            "Identify the majority answer, report the tally, and preserve source references."
        }
        ReduceStrategy::Merge => {
            "Merge the member outputs into one coherent answer without dropping source references."
        }
        ReduceStrategy::Rank => {
            "Rank the member outputs by usefulness and explain the ranking with source references."
        }
        ReduceStrategy::Custom => unreachable!("custom requires --template"),
    };
    Ok(format!(
        "You are reducing outputs for missive group '{group}' in context '{context}'.\n\
Strategy: {strategy}.\n\
Instruction: {instruction}\n\n\
Inputs:\n{inputs}\n\n\
Deterministic local baseline:\n{default_reduction}\n",
        group = operation.group.group_name.as_str(),
        context = operation.context_id.as_str(),
        strategy = args.strategy.as_str(),
        inputs = format_inputs_markdown(provenance),
    ))
}

fn render_template(
    template: &str,
    operation: &ReduceOperation,
    strategy: ReduceStrategy,
    provenance: &[ReduceInputView],
    default_reduction: &str,
) -> Result<String> {
    let mut rendered = template.to_owned();
    let replacements = [
        ("{{group}}", operation.group.group_name.as_str().to_owned()),
        ("{{context_id}}", operation.context_id.as_str().to_owned()),
        ("{{strategy}}", strategy.as_str().to_owned()),
        (
            "{{input_count}}",
            provenance
                .iter()
                .filter(|input| input.status == "gathered")
                .count()
                .to_string(),
        ),
        ("{{inputs}}", format_inputs_markdown(provenance)),
        ("{{default_reduction}}", default_reduction.to_owned()),
    ];
    for (needle, value) in replacements {
        rendered = rendered.replace(needle, &value);
    }
    if rendered.trim().is_empty() {
        return Err(MissiveError::validation(
            "rendered reduce template cannot be empty",
        ));
    }
    Ok(rendered)
}

fn local_summary(operation: &ReduceOperation, provenance: &[ReduceInputView]) -> String {
    let mut lines = vec![format!(
        "Summary for group '{}' in context '{}':",
        operation.group.group_name.as_str(),
        operation.context_id.as_str()
    )];
    for input in gathered_inputs(provenance) {
        lines.push(format!(
            "- {} ({}, task {}): {}",
            input.rank,
            input.agent,
            input
                .task
                .as_ref()
                .map(|task| task.task_id.as_str())
                .unwrap_or("missing"),
            input_text_or_reference(input)
        ));
    }
    lines.join("\n")
}

fn local_merge(operation: &ReduceOperation, provenance: &[ReduceInputView]) -> String {
    let mut sections = vec![format!(
        "Merged output for group '{}' in context '{}'.",
        operation.group.group_name.as_str(),
        operation.context_id.as_str()
    )];
    for input in gathered_inputs(provenance) {
        sections.push(format!(
            "## {} ({})\n{}",
            input.rank,
            input.agent,
            input_text_or_reference(input)
        ));
    }
    sections.join("\n\n")
}

fn local_vote(provenance: &[ReduceInputView]) -> Result<String> {
    let mut tallies: BTreeMap<String, (usize, String, Vec<String>)> = BTreeMap::new();
    for input in gathered_inputs(provenance) {
        let Some(text) = input.text.as_ref() else {
            continue;
        };
        let normalized = normalize_vote_text(text);
        let entry = tallies
            .entry(normalized)
            .or_insert_with(|| (0, text.clone(), Vec::new()));
        entry.0 = entry.0.saturating_add(1);
        entry.2.push(format!("{} ({})", input.rank, input.agent));
    }
    if tallies.is_empty() {
        return Err(
            MissiveError::validation("vote reduction requires at least one text input").with_help(
                "Use a different strategy when gathered outputs only contain artifacts.",
            ),
        );
    }
    let winner = tallies
        .iter()
        .max_by(|left, right| left.1.0.cmp(&right.1.0).then_with(|| right.0.cmp(left.0)))
        .expect("non-empty tallies");
    let total = tallies.values().map(|(count, _, _)| *count).sum::<usize>();
    let mut lines = vec![format!(
        "Vote result: {:?} received {} of {} vote(s).",
        winner.1.1, winner.1.0, total
    )];
    lines.push("Tallies:".to_owned());
    for (_normalized, (count, original, sources)) in tallies {
        lines.push(format!(
            "- {:?}: {} vote(s) from {}",
            original,
            count,
            sources.join(", ")
        ));
    }
    Ok(lines.join("\n"))
}

fn local_rank(operation: &ReduceOperation, provenance: &[ReduceInputView]) -> String {
    let mut ranked = gathered_inputs(provenance).collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        input_rank_score(right)
            .cmp(&input_rank_score(left))
            .then_with(|| left.rank.cmp(&right.rank))
            .then_with(|| left.agent.cmp(&right.agent))
    });
    let mut lines = vec![format!(
        "Ranked outputs for group '{}' in context '{}':",
        operation.group.group_name.as_str(),
        operation.context_id.as_str()
    )];
    for (index, input) in ranked.iter().enumerate() {
        lines.push(format!(
            "{}. {} ({}) — score {} — {}",
            index.saturating_add(1),
            input.rank,
            input.agent,
            input_rank_score(input),
            input_text_or_reference(input)
        ));
    }
    lines.join("\n")
}

fn gathered_inputs(provenance: &[ReduceInputView]) -> impl Iterator<Item = &ReduceInputView> {
    provenance.iter().filter(|input| input.status == "gathered")
}

fn input_text_or_reference(input: &ReduceInputView) -> String {
    input.text.clone().unwrap_or_else(|| {
        let artifact_ids = input
            .artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if artifact_ids.is_empty() {
            "[no text output]".to_owned()
        } else {
            format!("[artifact output: {artifact_ids}]")
        }
    })
}

fn normalize_vote_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn input_rank_score(input: &ReduceInputView) -> usize {
    input.text.as_ref().map_or(0, |text| text.chars().count())
        + input.artifact_count.saturating_mul(25)
        + input.message_count.saturating_mul(10)
}

fn format_inputs_markdown(provenance: &[ReduceInputView]) -> String {
    let mut lines = Vec::new();
    for input in provenance {
        lines.push(format!(
            "### {} ({})\n- status: {}\n- task: {}\n- messages: {}\n- artifacts: {}",
            input.rank,
            input.agent,
            input.status,
            input
                .task
                .as_ref()
                .map(|task| task.task_id.as_str())
                .unwrap_or("missing"),
            input.message_count,
            input.artifact_count
        ));
        if let Some(text) = &input.text {
            lines.push(format!("\n{}", text));
        } else if !input.artifacts.is_empty() {
            let artifacts = input
                .artifacts
                .iter()
                .map(|artifact| artifact.artifact_id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("\n[artifact references: {artifacts}]"));
        }
    }
    lines.join("\n\n")
}

impl ReduceInputView {
    fn from_state(index: usize, state: &ReduceMemberState) -> Self {
        let text = latest_member_text(state);
        let messages = state
            .messages
            .iter()
            .map(ReduceSourceMessageView::from_record)
            .collect::<Vec<_>>();
        let artifacts = state
            .artifacts
            .iter()
            .map(ReduceSourceArtifactView::from_record)
            .collect::<Vec<_>>();
        let task = state.task.as_ref().map(ReduceTaskView::from_record);
        let status = if state.task.is_none() {
            "missing_task"
        } else if text.is_some() || !messages.is_empty() || !artifacts.is_empty() {
            "gathered"
        } else {
            "empty_output"
        }
        .to_owned();
        Self {
            index,
            agent: state.member.agent_alias.as_str().to_owned(),
            rank: state.member.rank_name.as_str().to_owned(),
            status,
            task,
            text,
            message_count: messages.len(),
            messages,
            artifact_count: artifacts.len(),
            artifacts,
        }
    }
}

impl ReduceTaskView {
    fn from_record(record: &TaskRecord) -> Self {
        Self {
            task_id: record.task_id.as_str().to_owned(),
            state: record.state.as_str().to_owned(),
            source: record.source.as_str().to_owned(),
            protocol_version: record.protocol_version.clone(),
            last_message_id: record
                .last_message_id
                .as_ref()
                .map(|message_id| message_id.as_str().to_owned()),
            updated_at: record.updated_at.to_rfc3339(),
        }
    }
}

impl ReduceSourceMessageView {
    fn from_record(record: &MessageRecord) -> Self {
        Self {
            message_id: record.message_id.as_str().to_owned(),
            direction: record.direction.as_str().to_owned(),
            role: record.role.map(|role| role.as_str().to_owned()),
            ordinal: record.ordinal,
            protocol_message_id: record.protocol_message_id.clone(),
        }
    }
}

impl ReduceSourceArtifactView {
    fn from_record(record: &ArtifactRecord) -> Self {
        let summary = ArtifactSummaryView::from_record(record);
        Self {
            artifact_id: summary.artifact_id,
            task_id: summary.task_id,
            name: summary.name,
            mime_type: summary.mime_type,
            kind: summary.kind,
            version: summary.version,
            part_count: summary.part_count,
        }
    }
}

impl ReduceAgentView {
    fn from_parts(
        agent: &AgentRecord,
        interface: &NegotiatedInterface,
        outcome: &SendMessageOutcome,
        persisted: &PersistedSend,
    ) -> Self {
        let summary = response_summary(outcome);
        Self {
            agent: agent.alias.as_str().to_owned(),
            selected_interface: ReduceInterfaceView::from(interface),
            request_message_id: persisted.request_message.message_id.as_str().to_owned(),
            response_shape: summary.shape,
            response_message_id: persisted
                .response_message
                .as_ref()
                .map(|message| message.message_id.as_str().to_owned())
                .or(summary.response_message_id),
            task_id: persisted
                .task_id
                .as_ref()
                .map(|task_id| task_id.as_str().to_owned())
                .or(summary.task_id),
            context_id: persisted
                .context_id
                .as_ref()
                .map(|context_id| context_id.as_str().to_owned())
                .or(summary.context_id),
            state: summary.state,
        }
    }
}

impl From<&NegotiatedInterface> for ReduceInterfaceView {
    fn from(interface: &NegotiatedInterface) -> Self {
        Self {
            binding: interface.binding.clone(),
            protocol_binding: interface.protocol_binding.clone(),
            url: interface.url.clone(),
            tenant: interface.tenant.clone(),
            protocol_version: interface.protocol_version.clone(),
            source: interface.source.as_str().to_owned(),
        }
    }
}

#[derive(Debug)]
struct ResponseSummary {
    shape: String,
    response_message_id: Option<String>,
    task_id: Option<String>,
    context_id: Option<String>,
    state: Option<String>,
}

fn response_summary(outcome: &SendMessageOutcome) -> ResponseSummary {
    match &outcome.response {
        SendMessageResponse::Message(message) => ResponseSummary {
            shape: "message".to_owned(),
            response_message_id: Some(message.message_id.clone()),
            task_id: message.task_id.clone(),
            context_id: message.context_id.clone(),
            state: None,
        },
        SendMessageResponse::Task(task) => ResponseSummary {
            shape: "task".to_owned(),
            response_message_id: task
                .status
                .message
                .as_ref()
                .map(|message| message.message_id.clone()),
            task_id: Some(task.id.clone()),
            context_id: Some(task.context_id.clone()),
            state: Some(store_task_state(&task.status.state).as_str().to_owned()),
        },
    }
}

fn reducer_response_text(outcome: &SendMessageOutcome) -> Option<String> {
    match &outcome.response {
        SendMessageResponse::Message(message) => message.text().map(ToOwned::to_owned),
        SendMessageResponse::Task(task) => task
            .status
            .message
            .as_ref()
            .and_then(Message::text)
            .map(ToOwned::to_owned),
    }
}

fn fallback_agent_text(outcome: &SendMessageOutcome) -> String {
    match &outcome.response {
        SendMessageResponse::Message(message) => format!(
            "Reducer agent returned message {}",
            message.message_id.as_str()
        ),
        SendMessageResponse::Task(task) => format!(
            "Reducer agent returned task {} in state {}",
            task.id,
            store_task_state(&task.status.state).as_str()
        ),
    }
}

fn latest_member_text(state: &ReduceMemberState) -> Option<String> {
    state
        .messages
        .iter()
        .rev()
        .find_map(message_text)
        .or_else(|| state.task.as_ref().and_then(task_status_text))
        .or_else(|| first_artifact_text_from_records(&state.artifacts))
}

fn message_text(record: &MessageRecord) -> Option<String> {
    serde_json::from_value::<Message>(record.content_json.clone())
        .ok()
        .and_then(|message| Message::text(&message).map(ToOwned::to_owned))
}

fn task_status_text(record: &TaskRecord) -> Option<String> {
    let task = serde_json::from_value::<Task>(record.remote_task_json.as_ref()?.clone()).ok()?;
    task.status
        .message
        .as_ref()
        .and_then(Message::text)
        .map(ToOwned::to_owned)
}

fn persist_reduced_output(
    store: &Store,
    args: &ReduceArgs,
    operation: &ReduceOperation,
    provenance: &[ReduceInputView],
    reduction: &ReductionResult,
    service_parameters: &ServiceParameters,
) -> Result<MessageRecord> {
    let message_id = new_local_message_id();
    let mut metadata = parse_reduce_metadata(&args.metadata)?;
    metadata.merge(service_parameters.to_metadata()?);
    metadata.insert_str("missive.collective", "reduce")?;
    metadata.insert_str(
        "missive.reduce.operation_id",
        operation.operation_id.clone(),
    )?;
    metadata.insert_str("missive.reduce.strategy", args.strategy.as_str())?;
    metadata.insert_str("missive.reduce.method", reduction.method.clone())?;
    metadata.insert_str("missive.group", operation.group.group_name.as_str())?;
    metadata.insert("missive.reduce.provenance", json!(provenance))?;

    let mut insert = MessageInsert::new(
        message_id,
        MessageDirection::Local,
        json!({
            "kind": "missive.reduce.output",
            "operation_id": operation.operation_id,
            "group": operation.group.group_name.as_str(),
            "context_id": operation.context_id.as_str(),
            "strategy": args.strategy.as_str(),
            "method": reduction.method,
            "text": reduction.reduced_text,
            "provenance": provenance,
        }),
    );
    insert.context_id = Some(operation.context_id.clone());
    insert.role = Some(MessageRole::System);
    insert.metadata = metadata;
    store.insert_message(&insert)
}

fn parse_reduce_metadata(values: &[String]) -> Result<Metadata> {
    let mut metadata = Metadata::new();
    for value in values {
        let Some((key, raw_value)) = value.split_once('=') else {
            return Err(MissiveError::validation(format!(
                "--metadata value {value:?} must use KEY=VALUE syntax"
            )));
        };
        if key.is_empty() || raw_value.is_empty() {
            return Err(MissiveError::validation(format!(
                "--metadata value {value:?} must include a non-empty key and value"
            )));
        }
        let parsed = serde_json::from_str::<Value>(raw_value)
            .unwrap_or_else(|_| Value::String(raw_value.to_owned()));
        metadata.insert(key.to_owned(), parsed)?;
    }
    Ok(metadata)
}

fn finalize_reduce_output(
    profile: String,
    args: &ReduceArgs,
    operation: &ReduceOperation,
    provenance: Vec<ReduceInputView>,
    reduction: ReductionResult,
    reduced_message: &MessageRecord,
) -> ReduceOutput {
    let gathered_count = provenance
        .iter()
        .filter(|input| input.status == "gathered")
        .count();
    let missing_count = provenance
        .iter()
        .filter(|input| input.status == "missing_task")
        .count();
    let empty_count = provenance
        .iter()
        .filter(|input| input.status == "empty_output")
        .count();
    let text_input_count = provenance
        .iter()
        .filter(|input| input.text.is_some())
        .count();
    let status = if gathered_count == provenance.len() {
        "succeeded"
    } else {
        "partial"
    }
    .to_owned();
    let method = reduction.method.clone();
    let reducer = ReduceReducerView {
        method,
        strategy: args.strategy.as_str().to_owned(),
        prompt_bytes: reduction.prompt.len() as u64,
        template_applied: reduction.template_applied,
        agent: reduction.agent,
        command: reduction.command,
    };
    let message = format!(
        "Reduced {} gathered input(s) from group '{}' with {} strategy",
        gathered_count,
        operation.group.group_name.as_str(),
        args.strategy.as_str()
    );
    ReduceOutput {
        profile,
        operation_id: operation.operation_id.clone(),
        group: operation.group.group_name.as_str().to_owned(),
        context_id: operation.context_id.as_str().to_owned(),
        strategy: args.strategy.as_str().to_owned(),
        status,
        member_count: provenance.len(),
        gathered_count,
        missing_count,
        empty_count,
        input_count: gathered_count,
        text_input_count,
        elapsed_ms: operation.started_at.elapsed().as_millis(),
        reducer,
        reduced_text: reduction.reduced_text,
        provenance,
        persistence: ReducePersistenceView {
            reduced_message_id: reduced_message.message_id.as_str().to_owned(),
        },
        message,
    }
}

fn append_reduce_started_event(
    store: &Store,
    args: &ReduceArgs,
    operation: &ReduceOperation,
    provenance: &[ReduceInputView],
) -> Result<()> {
    let mut event = new_cli_event(
        "missive.reduce.started",
        json!({
            "operation_id": operation.operation_id,
            "group": operation.group.group_name.as_str(),
            "context_id": operation.context_id.as_str(),
            "strategy": args.strategy.as_str(),
            "method": reducer_method_name(args),
            "member_count": operation.members.len(),
            "gathered_count": provenance.iter().filter(|input| input.status == "gathered").count(),
            "template_applied": args.template.is_some(),
        }),
    )?;
    event.group_name = Some(operation.group.group_name.clone());
    event.context_id = Some(operation.context_id.clone());
    store.append_event(&event)?;
    Ok(())
}

fn append_reduce_input_events(store: &Store, output: &ReduceOutput) -> Result<()> {
    let group_name = GroupName::new(output.group.clone())?;
    let context_id = ContextId::new(output.context_id.clone())?;
    for input in &output.provenance {
        let event_type = match input.status.as_str() {
            "gathered" => "missive.reduce.input.gathered",
            "missing_task" => "missive.reduce.input.missing",
            _ => "missive.reduce.input.empty",
        };
        let mut event = new_cli_event(
            event_type,
            json!({
                "operation_id": output.operation_id,
                "group": output.group,
                "context_id": output.context_id,
                "agent": input.agent,
                "rank": input.rank,
                "status": input.status,
                "task_id": input.task.as_ref().map(|task| task.task_id.as_str()),
                "message_count": input.message_count,
                "artifact_count": input.artifact_count,
            }),
        )?;
        event.group_name = Some(group_name.clone());
        event.context_id = Some(context_id.clone());
        event.agent_alias = Some(AgentAlias::new(input.agent.clone())?);
        event.task_id = input
            .task
            .as_ref()
            .map(|task| TaskId::new(task.task_id.clone()))
            .transpose()?;
        store.append_event(&event)?;
    }
    Ok(())
}

fn append_reduce_completed_event(store: &Store, output: &ReduceOutput) -> Result<()> {
    let mut event = new_cli_event(
        "missive.reduce.completed",
        json!({
            "operation_id": output.operation_id,
            "group": output.group,
            "context_id": output.context_id,
            "strategy": output.strategy,
            "method": output.reducer.method,
            "status": output.status,
            "member_count": output.member_count,
            "gathered_count": output.gathered_count,
            "missing_count": output.missing_count,
            "empty_count": output.empty_count,
            "text_input_count": output.text_input_count,
            "reduced_message_id": output.persistence.reduced_message_id,
            "reduced_text": output.reduced_text,
            "provenance": output.provenance,
        }),
    )?;
    event.group_name = Some(GroupName::new(output.group.clone())?);
    event.context_id = Some(ContextId::new(output.context_id.clone())?);
    event.task_id = output
        .reducer
        .agent
        .as_ref()
        .and_then(|agent| agent.task_id.as_ref())
        .map(|task_id| TaskId::new(task_id.clone()))
        .transpose()?;
    store.append_event(&event)?;
    Ok(())
}

fn append_reduce_failed_event(
    store: &Store,
    operation: &ReduceOperation,
    error: &MissiveError,
) -> Result<()> {
    let mut event = new_cli_event(
        "missive.reduce.failed",
        json!({
            "operation_id": operation.operation_id,
            "group": operation.group.group_name.as_str(),
            "context_id": operation.context_id.as_str(),
            "error": error.to_report(),
        }),
    )?;
    event.group_name = Some(operation.group.group_name.clone());
    event.context_id = Some(operation.context_id.clone());
    store.append_event(&event)?;
    Ok(())
}

fn reducer_method_name(args: &ReduceArgs) -> &'static str {
    if args.reducer_agent.is_some() {
        "agent"
    } else if args.command.is_some() {
        "command"
    } else {
        "local"
    }
}

fn render_reduce_success<W>(writer: &mut W, mode: OutputMode, output: &ReduceOutput) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_reduce_markdown(writer, output),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "reduce_result", output, &output.message)
        }
    }
}

fn write_reduce_markdown<W>(writer: &mut W, output: &ReduceOutput) -> Result<()>
where
    W: Write,
{
    writeln!(writer, "# Reduce `{}`", redact_text(&output.group))
        .map_err(|error| MissiveError::io("writing reduce output", error))?;
    writeln!(writer).map_err(|error| MissiveError::io("writing reduce output", error))?;
    writeln!(writer, "- context: `{}`", redact_text(&output.context_id))
        .map_err(|error| MissiveError::io("writing reduce output", error))?;
    writeln!(writer, "- strategy: `{}`", redact_text(&output.strategy))
        .map_err(|error| MissiveError::io("writing reduce output", error))?;
    writeln!(
        writer,
        "- reducer: `{}`",
        redact_text(&output.reducer.method)
    )
    .map_err(|error| MissiveError::io("writing reduce output", error))?;
    writeln!(
        writer,
        "- gathered inputs: {}/{}",
        output.gathered_count, output.member_count
    )
    .map_err(|error| MissiveError::io("writing reduce output", error))?;
    writeln!(writer).map_err(|error| MissiveError::io("writing reduce output", error))?;
    writeln!(writer, "## Reduced output")
        .map_err(|error| MissiveError::io("writing reduce output", error))?;
    writeln!(writer).map_err(|error| MissiveError::io("writing reduce output", error))?;
    writeln!(
        writer,
        "{}",
        redact_text(&output.reduced_text)
            .lines()
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
    .map_err(|error| MissiveError::io("writing reduce output", error))?;
    writeln!(writer).map_err(|error| MissiveError::io("writing reduce output", error))?;
    writeln!(writer, "## Provenance")
        .map_err(|error| MissiveError::io("writing reduce output", error))?;
    for input in &output.provenance {
        writeln!(
            writer,
            "- `{}` (`{}`): `{}` task `{}` messages {} artifacts {}",
            redact_text(&input.rank),
            redact_text(&input.agent),
            redact_text(&input.status),
            input
                .task
                .as_ref()
                .map(|task| redact_text(&task.task_id))
                .unwrap_or_else(|| "missing".to_owned()),
            input.message_count,
            input.artifact_count
        )
        .map_err(|error| MissiveError::io("writing reduce output", error))?;
    }
    Ok(())
}

fn usize_to_u64(value: usize, source: &str) -> Result<u64> {
    u64::try_from(value).map_err(|error| {
        MissiveError::validation(format!("{source} byte count does not fit into u64"))
            .with_source(error)
    })
}

#[cfg(test)]
mod tests {
    use missive_core::{AgentAlias, RankName};
    use missive_store::{TaskSource, TaskState, TaskUpsert};

    use super::*;

    fn input(rank: &str, agent: &str, text: Option<&str>) -> ReduceInputView {
        let group = GroupName::new("team".to_owned()).expect("group");
        let member = GroupMemberRecord {
            group_name: group,
            agent_alias: AgentAlias::new(agent.to_owned()).expect("agent"),
            rank_name: RankName::new(rank.to_owned()).expect("rank"),
            tags: Vec::new(),
            weight: 1.0,
            routing_metadata: Metadata::new(),
            created_at: missive_core::MissiveTimestamp::now_utc(),
        };
        let task_id = TaskId::new(format!("task-{agent}")).expect("task id");
        let mut task = TaskUpsert::new(task_id, member.agent_alias.clone(), TaskState::Completed);
        task.source = TaskSource::Remote;
        let task = TaskRecord {
            task_id: task.task_id,
            agent_alias: task.agent_alias,
            context_id: None,
            state: task.state,
            source: task.source,
            protocol_version: None,
            remote_task_json: None,
            last_message_id: None,
            metadata: Metadata::new(),
            created_at: missive_core::MissiveTimestamp::now_utc(),
            updated_at: missive_core::MissiveTimestamp::now_utc(),
            completed_at: None,
        };
        ReduceInputView {
            index: 0,
            agent: member.agent_alias.as_str().to_owned(),
            rank: member.rank_name.as_str().to_owned(),
            status: "gathered".to_owned(),
            task: Some(ReduceTaskView::from_record(&task)),
            text: text.map(ToOwned::to_owned),
            message_count: usize::from(text.is_some()),
            messages: Vec::new(),
            artifact_count: 0,
            artifacts: Vec::new(),
        }
    }

    fn operation() -> ReduceOperation {
        ReduceOperation {
            operation_id: "reduce/test".to_owned(),
            group: GroupRecord {
                group_name: GroupName::new("team".to_owned()).expect("group"),
                routing_policy: "broadcast".to_owned(),
                notes: None,
                metadata: Metadata::new(),
                created_at: missive_core::MissiveTimestamp::now_utc(),
                updated_at: missive_core::MissiveTimestamp::now_utc(),
            },
            members: Vec::new(),
            context_id: ContextId::new("ctx-test".to_owned()).expect("context"),
            started_at: Instant::now(),
        }
    }

    #[test]
    fn vote_strategy_tallies_identical_text_deterministically() {
        let provenance = vec![
            input("rank-0", "alpha", Some("Yes")),
            input("rank-1", "beta", Some(" yes ")),
            input("rank-2", "gamma", Some("No")),
        ];

        let output = local_strategy_output(ReduceStrategy::Vote, &operation(), &provenance)
            .expect("vote output");

        assert!(output.contains("received 2 of 3 vote"));
        assert!(output.contains("rank-0"));
        assert!(output.contains("rank-1"));
    }

    #[test]
    fn template_replaces_reduce_placeholders() {
        let provenance = vec![input("rank-0", "alpha", Some("alpha answer"))];
        let rendered = render_template(
            "{{group}} {{context_id}} {{strategy}} {{input_count}}\n{{inputs}}\n{{default_reduction}}",
            &operation(),
            ReduceStrategy::Summarise,
            &provenance,
            "baseline",
        )
        .expect("template");

        assert!(rendered.contains("team ctx-test summarise 1"));
        assert!(rendered.contains("alpha answer"));
        assert!(rendered.contains("baseline"));
    }
}
