//! Gather collective command implementation.
//!
//! `missive gather` reads local group membership and the latest locally known
//! task for each member in one shared context, then emits deterministic
//! rank-ordered output and optional artifact exports. It intentionally stays
//! local-only: use `missive barrier` or `missive task get --remote` first when
//! remote task state should be refreshed before gathering.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::{ArgAction, Args};
use missive_a2a::protocol::{Message, Task};
use missive_core::{ContextId, GroupName, LoadedConfig, MissiveError, Result, TaskId};
use missive_store::{
    ArtifactRecord, ContextUpsert, GroupMemberRecord, GroupRecord, MessageDirection, MessageRecord,
    Store, TaskRecord,
};
use serde::Serialize;
use serde_json::json;

use crate::agent::open_agent_registry;
use crate::artifact::{
    ArtifactSavedView, ArtifactSummaryView, first_artifact_text_from_records, safe_file_name,
    write_artifact_to_path,
};
use crate::events::new_cli_event;
use crate::output::{OutputMode, redact_text, render_success};

/// Arguments for `missive gather`.
#[derive(Debug, Clone, Args)]
pub struct GatherArgs {
    /// Local group name whose member outputs should be gathered.
    pub group: String,

    /// A2A context id shared by the member tasks.
    #[arg(long = "context", value_name = "CONTEXT_ID", required = true)]
    pub context: String,

    /// Export gathered artifacts into this directory using safe deterministic filenames.
    #[arg(short = 'o', long = "output-dir", value_name = "DIR")]
    pub output_dir: Option<PathBuf>,

    /// Overwrite existing artifact export files in --output-dir.
    #[arg(long = "force", action = ArgAction::SetTrue)]
    pub force: bool,
}

#[derive(Debug)]
struct GatherOperation {
    operation_id: String,
    group: GroupRecord,
    members: Vec<GroupMemberRecord>,
    context_id: ContextId,
    output_dir: Option<PathBuf>,
    force: bool,
    started_at: Instant,
}

#[derive(Debug, Clone)]
struct GatherMemberState {
    member: GroupMemberRecord,
    task: Option<TaskRecord>,
    messages: Vec<MessageRecord>,
    artifacts: Vec<ArtifactRecord>,
    exported_artifacts: Vec<ArtifactSavedView>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct GatherMessageView {
    message_id: String,
    direction: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    ordinal: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    protocol_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct GatherTaskView {
    task_id: String,
    state: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    protocol_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_message_id: Option<String>,
    created_at: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct GatherMemberView {
    agent: String,
    rank: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<GatherTaskView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    message_count: usize,
    messages: Vec<GatherMessageView>,
    artifact_count: usize,
    artifacts: Vec<ArtifactSummaryView>,
    exported_artifact_count: usize,
    exported_artifacts: Vec<ArtifactSavedView>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct GatherOutput {
    profile: String,
    operation_id: String,
    group: String,
    context_id: String,
    status: String,
    member_count: usize,
    gathered_count: usize,
    missing_count: usize,
    empty_count: usize,
    message_count: usize,
    artifact_count: usize,
    exported_artifact_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_dir: Option<String>,
    elapsed_ms: u128,
    members: Vec<GatherMemberView>,
    message: String,
}

#[derive(Debug, Clone)]
struct ExportPlan {
    member_index: usize,
    artifact_index: usize,
    path: PathBuf,
}

/// Executes `missive gather`.
pub(crate) fn execute_gather_command<W>(
    args: &GatherArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let span = tracing::debug_span!(
        target: "missive_cli",
        "collective.operation",
        collective = "gather",
        group = %args.group,
        context_id = %args.context,
        output_dir = %args.output_dir.as_ref().map(|path| path.display().to_string()).unwrap_or_else(|| "-".to_owned()),
    );
    let _span_guard = span.enter();
    tracing::debug!(
        target: "missive_cli",
        collective = "gather",
        group = %args.group,
        context_id = %args.context,
        "collective operation started"
    );
    let mut registry = open_agent_registry(loaded_config, environment)?;
    let operation = prepare_gather_operation(args, &mut registry.store)?;
    append_gather_started_event(&registry.store, &operation)?;

    let mut members = collect_gather_members(&registry.store, &operation)?;
    export_gather_artifacts(&mut members, &operation)?;

    let output = finalize_gather_output(registry.profile.clone(), &operation, members);
    append_gather_member_events(&registry.store, &output)?;
    append_gather_completed_event(&registry.store, &output)?;
    tracing::debug!(
        target: "missive_cli",
        collective = "gather",
        operation_id = %output.operation_id,
        status = %output.status,
        member_count = output.member_count,
        gathered_count = output.gathered_count,
        missing_count = output.missing_count,
        artifact_count = output.artifact_count,
        "collective operation completed"
    );
    render_gather_success(writer, mode, &output)
}

fn prepare_gather_operation(args: &GatherArgs, store: &mut Store) -> Result<GatherOperation> {
    let group_name = GroupName::new(args.group.clone())?;
    let context_id = ContextId::new(args.context.clone())?;
    let group = store.get_group(&group_name)?.ok_or_else(|| {
        MissiveError::validation(format!("group {:?} does not exist", group_name.as_str()))
            .with_help("Run 'missive group list' to see locally known groups.")
    })?;
    let members = store.list_group_members(&group.group_name)?;
    if members.is_empty() {
        return Err(MissiveError::validation(format!(
            "group {:?} has no members for gather",
            group.group_name.as_str()
        ))
        .with_help("Add members with 'missive group add <group> <agent> --rank <rank>' before running gather."));
    }
    if store.get_context(&context_id)?.is_none() {
        let mut context = ContextUpsert::new(context_id.clone());
        context.summary = Some(format!(
            "Gather collective context for group '{}'",
            group.group_name.as_str()
        ));
        context
            .metadata
            .insert_str("missive.collective", "gather")?;
        store.upsert_context(&context)?;
    }

    Ok(GatherOperation {
        operation_id: format!("gather/{}", missive_a2a::protocol::new_message_id()),
        group,
        members,
        context_id,
        output_dir: args.output_dir.clone(),
        force: args.force,
        started_at: Instant::now(),
    })
}

fn collect_gather_members(
    store: &Store,
    operation: &GatherOperation,
) -> Result<Vec<GatherMemberState>> {
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
            Ok(GatherMemberState {
                member,
                task,
                messages,
                artifacts,
                exported_artifacts: Vec::new(),
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

fn export_gather_artifacts(
    members: &mut [GatherMemberState],
    operation: &GatherOperation,
) -> Result<()> {
    let Some(output_dir) = operation.output_dir.as_ref() else {
        return Ok(());
    };
    fs::create_dir_all(output_dir).map_err(|error| {
        MissiveError::io(
            format!(
                "creating gather artifact export directory {}",
                output_dir.display()
            ),
            error,
        )
    })?;

    let plans = plan_gather_artifact_exports(members, output_dir)?;
    if !operation.force
        && let Some(existing) = plans.iter().find(|plan| plan.path.exists())
    {
        return Err(MissiveError::validation(format!(
            "refusing to overwrite existing gather artifact output path {}",
            existing.path.display()
        ))
        .with_help("Choose another --output-dir or pass --force to overwrite existing exports."));
    }

    let mut saved_by_member = vec![Vec::new(); members.len()];
    for plan in plans {
        let saved = write_artifact_to_path(
            &members[plan.member_index].artifacts[plan.artifact_index],
            &plan.path,
            operation.force,
        )?;
        saved_by_member[plan.member_index].push(saved);
    }
    for (member, saved) in members.iter_mut().zip(saved_by_member) {
        member.exported_artifacts = saved;
    }
    Ok(())
}

fn plan_gather_artifact_exports(
    members: &[GatherMemberState],
    output_dir: &Path,
) -> Result<Vec<ExportPlan>> {
    let mut used_names = BTreeSet::new();
    let mut plans = Vec::new();
    for (member_index, member) in members.iter().enumerate() {
        for (artifact_index, artifact) in member.artifacts.iter().enumerate() {
            let filename = unique_member_artifact_file_name(member, artifact, &mut used_names)?;
            plans.push(ExportPlan {
                member_index,
                artifact_index,
                path: output_dir.join(filename),
            });
        }
    }
    Ok(plans)
}

fn unique_member_artifact_file_name(
    member: &GatherMemberState,
    artifact: &ArtifactRecord,
    used_names: &mut BTreeSet<String>,
) -> Result<String> {
    let prefix = sanitize_path_component(&format!(
        "{}-{}",
        member.member.rank_name.as_str(),
        member.member.agent_alias.as_str()
    ));
    let base = safe_file_name(artifact)?;
    unique_file_name(format!("{prefix}-{base}"), used_names)
}

fn unique_file_name(base: String, used_names: &mut BTreeSet<String>) -> Result<String> {
    if used_names.insert(base.clone()) {
        return Ok(base);
    }

    let path = Path::new(&base);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("artifact");
    let extension = path.extension().and_then(|value| value.to_str());
    for suffix in 2_u64.. {
        let candidate = if let Some(extension) = extension {
            format!("{stem}-{suffix}.{extension}")
        } else {
            format!("{stem}-{suffix}")
        };
        if used_names.insert(candidate.clone()) {
            return Ok(candidate);
        }
    }
    unreachable!("unbounded suffix iterator always returns")
}

fn sanitize_path_component(value: &str) -> String {
    let mut sanitized = value
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => character,
            _ => '_',
        })
        .collect::<String>()
        .trim_matches('.')
        .to_owned();
    if sanitized.is_empty() {
        sanitized = "member".to_owned();
    }
    sanitized
}

fn finalize_gather_output(
    profile: String,
    operation: &GatherOperation,
    members: Vec<GatherMemberState>,
) -> GatherOutput {
    let elapsed_ms = operation.started_at.elapsed().as_millis();
    let member_views = members
        .iter()
        .map(GatherMemberView::from_state)
        .collect::<Vec<_>>();
    let gathered_count = member_views
        .iter()
        .filter(|member| member.status == "gathered")
        .count();
    let missing_count = member_views
        .iter()
        .filter(|member| member.status == "missing_task")
        .count();
    let empty_count = member_views
        .iter()
        .filter(|member| member.status == "empty_output")
        .count();
    let message_count = member_views
        .iter()
        .map(|member| member.message_count)
        .sum::<usize>();
    let artifact_count = member_views
        .iter()
        .map(|member| member.artifact_count)
        .sum::<usize>();
    let exported_artifact_count = member_views
        .iter()
        .map(|member| member.exported_artifact_count)
        .sum::<usize>();
    let status = if gathered_count == member_views.len() {
        "succeeded"
    } else if gathered_count > 0 {
        "partial"
    } else {
        "missing"
    }
    .to_owned();
    let message = match status.as_str() {
        "succeeded" => format!(
            "Gathered outputs from all {} member task(s) in context '{}'",
            member_views.len(),
            operation.context_id.as_str()
        ),
        "partial" => format!(
            "Gathered outputs from {}/{} member task(s) in context '{}'",
            gathered_count,
            member_views.len(),
            operation.context_id.as_str()
        ),
        _ => format!(
            "No gatherable member task outputs were found in context '{}'",
            operation.context_id.as_str()
        ),
    };

    GatherOutput {
        profile,
        operation_id: operation.operation_id.clone(),
        group: operation.group.group_name.as_str().to_owned(),
        context_id: operation.context_id.as_str().to_owned(),
        status,
        member_count: member_views.len(),
        gathered_count,
        missing_count,
        empty_count,
        message_count,
        artifact_count,
        exported_artifact_count,
        output_dir: operation
            .output_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        elapsed_ms,
        members: member_views,
        message,
    }
}

impl GatherMemberView {
    fn from_state(state: &GatherMemberState) -> Self {
        let text = latest_member_text(state);
        let messages = state
            .messages
            .iter()
            .map(GatherMessageView::from_record)
            .collect::<Vec<_>>();
        let artifacts = state
            .artifacts
            .iter()
            .map(ArtifactSummaryView::from_record)
            .collect::<Vec<_>>();
        let task = state.task.as_ref().map(GatherTaskView::from_record);
        let status = if state.task.is_none() {
            "missing_task"
        } else if text.is_some() || !messages.is_empty() || !artifacts.is_empty() {
            "gathered"
        } else {
            "empty_output"
        }
        .to_owned();
        Self {
            agent: state.member.agent_alias.as_str().to_owned(),
            rank: state.member.rank_name.as_str().to_owned(),
            status,
            task,
            text,
            message_count: messages.len(),
            messages,
            artifact_count: artifacts.len(),
            artifacts,
            exported_artifact_count: state.exported_artifacts.len(),
            exported_artifacts: state.exported_artifacts.clone(),
        }
    }
}

impl GatherTaskView {
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
            created_at: record.created_at.to_rfc3339(),
            updated_at: record.updated_at.to_rfc3339(),
            completed_at: record.completed_at.map(|timestamp| timestamp.to_rfc3339()),
        }
    }
}

impl GatherMessageView {
    fn from_record(record: &MessageRecord) -> Self {
        Self {
            message_id: record.message_id.as_str().to_owned(),
            direction: record.direction.as_str().to_owned(),
            role: record.role.map(|role| role.as_str().to_owned()),
            ordinal: record.ordinal,
            protocol_message_id: record.protocol_message_id.clone(),
            text: message_text(record),
            created_at: record.created_at.to_rfc3339(),
        }
    }
}

fn latest_member_text(state: &GatherMemberState) -> Option<String> {
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

fn append_gather_started_event(store: &Store, operation: &GatherOperation) -> Result<()> {
    let mut event = new_cli_event(
        "missive.gather.started",
        json!({
            "operation_id": operation.operation_id,
            "group": operation.group.group_name.as_str(),
            "context_id": operation.context_id.as_str(),
            "member_count": operation.members.len(),
            "output_dir": operation.output_dir.as_ref().map(|path| path.display().to_string()),
            "force": operation.force,
        }),
    )?;
    event.group_name = Some(operation.group.group_name.clone());
    event.context_id = Some(operation.context_id.clone());
    store.append_event(&event)?;
    Ok(())
}

fn append_gather_member_events(store: &Store, output: &GatherOutput) -> Result<()> {
    let group_name = GroupName::new(output.group.clone())?;
    let context_id = ContextId::new(output.context_id.clone())?;
    for member in &output.members {
        let event_type = match member.status.as_str() {
            "gathered" => "missive.gather.member.gathered",
            "missing_task" => "missive.gather.member.missing",
            _ => "missive.gather.member.empty",
        };
        let mut event = new_cli_event(
            event_type,
            json!({
                "operation_id": output.operation_id,
                "group": output.group,
                "context_id": output.context_id,
                "agent": member.agent,
                "rank": member.rank,
                "status": member.status,
                "task_id": member.task.as_ref().map(|task| task.task_id.as_str()),
                "state": member.task.as_ref().map(|task| task.state.as_str()),
                "message_count": member.message_count,
                "artifact_count": member.artifact_count,
                "exported_artifact_count": member.exported_artifact_count,
            }),
        )?;
        event.group_name = Some(group_name.clone());
        event.context_id = Some(context_id.clone());
        if let Ok(agent) = missive_core::AgentAlias::new(member.agent.clone()) {
            event.agent_alias = Some(agent);
        }
        if let Some(task) = &member.task {
            event.task_id = Some(TaskId::new(task.task_id.clone())?);
        }
        store.append_event(&event)?;
    }
    Ok(())
}

fn append_gather_completed_event(store: &Store, output: &GatherOutput) -> Result<()> {
    let mut event = new_cli_event(
        "missive.gather.completed",
        json!({
            "operation_id": output.operation_id,
            "group": output.group,
            "context_id": output.context_id,
            "status": output.status,
            "member_count": output.member_count,
            "gathered_count": output.gathered_count,
            "missing_count": output.missing_count,
            "empty_count": output.empty_count,
            "message_count": output.message_count,
            "artifact_count": output.artifact_count,
            "exported_artifact_count": output.exported_artifact_count,
            "output_dir": output.output_dir,
        }),
    )?;
    event.group_name = Some(GroupName::new(output.group.clone())?);
    event.context_id = Some(ContextId::new(output.context_id.clone())?);
    store.append_event(&event)?;
    Ok(())
}

fn render_gather_success<W>(writer: &mut W, mode: OutputMode, output: &GatherOutput) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_gather_markdown(writer, output),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "gather_result", output, &output.message)
        }
    }
}

fn write_gather_markdown<W>(writer: &mut W, output: &GatherOutput) -> Result<()>
where
    W: Write,
{
    writeln!(writer, "# Gather `{}`", redact_text(&output.group))
        .map_err(|error| MissiveError::io("writing gather output", error))?;
    writeln!(writer).map_err(|error| MissiveError::io("writing gather output", error))?;
    writeln!(writer, "- context: `{}`", redact_text(&output.context_id))
        .map_err(|error| MissiveError::io("writing gather output", error))?;
    writeln!(writer, "- status: `{}`", redact_text(&output.status))
        .map_err(|error| MissiveError::io("writing gather output", error))?;
    writeln!(
        writer,
        "- gathered: {}/{}",
        output.gathered_count, output.member_count
    )
    .map_err(|error| MissiveError::io("writing gather output", error))?;
    if let Some(output_dir) = &output.output_dir {
        writeln!(writer, "- artifact export: `{}`", redact_text(output_dir))
            .map_err(|error| MissiveError::io("writing gather output", error))?;
    }
    writeln!(writer).map_err(|error| MissiveError::io("writing gather output", error))?;

    for member in &output.members {
        writeln!(
            writer,
            "## {} (`{}`)",
            redact_text(&member.rank),
            redact_text(&member.agent)
        )
        .map_err(|error| MissiveError::io("writing gather output", error))?;
        writeln!(writer).map_err(|error| MissiveError::io("writing gather output", error))?;
        writeln!(writer, "- status: `{}`", redact_text(&member.status))
            .map_err(|error| MissiveError::io("writing gather output", error))?;
        if let Some(task) = &member.task {
            writeln!(writer, "- task: `{}`", redact_text(&task.task_id))
                .map_err(|error| MissiveError::io("writing gather output", error))?;
            writeln!(writer, "- state: `{}`", redact_text(&task.state))
                .map_err(|error| MissiveError::io("writing gather output", error))?;
        } else {
            writeln!(writer, "- task: _missing_")
                .map_err(|error| MissiveError::io("writing gather output", error))?;
        }
        writeln!(writer, "- artifacts: {}", member.artifact_count)
            .map_err(|error| MissiveError::io("writing gather output", error))?;
        if let Some(text) = &member.text {
            writeln!(writer).map_err(|error| MissiveError::io("writing gather output", error))?;
            writeln!(writer, "> {}", redact_text(text).replace('\n', "\n> "))
                .map_err(|error| MissiveError::io("writing gather output", error))?;
        }
        if !member.exported_artifacts.is_empty() {
            writeln!(writer).map_err(|error| MissiveError::io("writing gather output", error))?;
            writeln!(writer, "Exported artifacts:")
                .map_err(|error| MissiveError::io("writing gather output", error))?;
            for artifact in &member.exported_artifacts {
                writeln!(
                    writer,
                    "- `{}` -> `{}` ({} bytes)",
                    redact_text(&artifact.artifact_id),
                    redact_text(&artifact.path),
                    artifact.bytes_written
                )
                .map_err(|error| MissiveError::io("writing gather output", error))?;
            }
        }
        writeln!(writer).map_err(|error| MissiveError::io("writing gather output", error))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_component_sanitizer_removes_separators() {
        assert_eq!(sanitize_path_component("rank/0:alpha"), "rank_0_alpha");
        assert_eq!(sanitize_path_component("..."), "member");
    }

    #[test]
    fn unique_file_names_insert_suffix_before_extension() {
        let mut used = BTreeSet::new();
        assert_eq!(
            unique_file_name("rank-alpha-answer.txt".to_owned(), &mut used).expect("name"),
            "rank-alpha-answer.txt"
        );
        assert_eq!(
            unique_file_name("rank-alpha-answer.txt".to_owned(), &mut used).expect("name"),
            "rank-alpha-answer-2.txt"
        );
    }
}
