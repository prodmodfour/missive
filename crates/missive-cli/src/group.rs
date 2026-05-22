//! Group command implementation.
//!
//! Groups are profile-scoped local control-plane rows that bind registered
//! agent aliases to stable rank names plus routing hints. Collective operations
//! consume this state in later tickets; this module intentionally stops at
//! group and membership CRUD.

use std::collections::BTreeMap;
use std::io::Write;

use clap::{Args, Subcommand};
use missive_core::{
    AgentAlias, GroupName, LoadedConfig, Metadata, MissiveError, RankName, Result,
    parse_routing_policy as parse_core_routing_policy,
};
use missive_store::{GroupMemberRecord, GroupMemberUpsert, GroupRecord, GroupUpsert, Store};
use serde::Serialize;
use serde_json::{Value, json};

use crate::agent::{get_existing_agent, open_agent_registry};
use crate::events::new_cli_event;
use crate::output::{OutputMode, redact_json, redact_text, render_success};

const GROUP_NOTES_MAX_BYTES: usize = 8 * 1024;
const GROUP_NOTES_HELP: &str = "Use concise non-secret group notes.";
const ROUTING_POLICY_HELP: &str = "Use one of: direct, capability-match, tag-match, round-robin, weighted, broadcast, first-success, quorum, fallback.";
const NAMED_IDENTIFIER_MAX_BYTES: usize = 63;
const NAMED_IDENTIFIER_HELP: &str =
    "Use lowercase ASCII letters or digits, with '-', '_' or '.' only in the middle.";

/// Group subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum GroupCommands {
    /// Create a local group definition.
    Create(GroupCreateArgs),
    /// List local groups.
    List,
    /// Show one group and its members.
    Show(GroupNameArgs),
    /// Add or update one registered agent membership.
    Add(GroupAddArgs),
    /// Remove one agent membership from a group.
    Remove(GroupMemberSelectorArgs),
    /// Rename a group while preserving membership.
    Rename(GroupRenameArgs),
    /// Delete a group and its membership rows.
    Delete(GroupNameArgs),
}

impl GroupCommands {
    /// Stable subcommand spelling used in structured output messages.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Create(_) => "create",
            Self::List => "list",
            Self::Show(_) => "show",
            Self::Add(_) => "add",
            Self::Remove(_) => "remove",
            Self::Rename(_) => "rename",
            Self::Delete(_) => "delete",
        }
    }
}

/// Arguments for `missive group create`.
#[derive(Debug, Clone, Args)]
pub struct GroupCreateArgs {
    /// Group name used by later collective/routing commands.
    pub group: String,

    /// Routing policy label to store with the group.
    #[arg(
        long = "routing-policy",
        value_name = "POLICY",
        default_value = "direct"
    )]
    pub routing_policy: String,

    /// Human notes for this group.
    #[arg(long = "notes", value_name = "TEXT")]
    pub notes: Option<String>,

    /// Non-secret metadata entry as KEY=VALUE; VALUE is parsed as JSON when possible.
    #[arg(long = "metadata", value_name = "KEY=VALUE")]
    pub metadata: Vec<String>,
}

/// Arguments for commands that take one group name.
#[derive(Debug, Clone, Args)]
pub struct GroupNameArgs {
    /// Group name.
    pub group: String,
}

/// Arguments for `missive group add`.
#[derive(Debug, Clone, Args)]
pub struct GroupAddArgs {
    /// Group name.
    pub group: String,

    /// Registered agent alias to add as a member.
    pub agent: String,

    /// Rank name unique within the group.
    #[arg(long = "rank", value_name = "RANK")]
    pub rank: String,

    /// Member tag used by future routing/capability policies; repeat for multiple tags.
    #[arg(long = "tag", value_name = "TAG")]
    pub tags: Vec<String>,

    /// Positive finite routing weight for weighted/fallback policies.
    #[arg(long = "weight", value_name = "WEIGHT", default_value_t = 1.0, value_parser = parse_positive_weight_arg)]
    pub weight: f64,

    /// Member routing metadata as KEY=VALUE; VALUE is parsed as JSON when possible.
    #[arg(long = "routing-metadata", value_name = "KEY=VALUE")]
    pub routing_metadata: Vec<String>,
}

/// Arguments for commands that select one group member by agent alias.
#[derive(Debug, Clone, Args)]
pub struct GroupMemberSelectorArgs {
    /// Group name.
    pub group: String,

    /// Member agent alias.
    pub agent: String,
}

/// Arguments for `missive group rename`.
#[derive(Debug, Clone, Args)]
pub struct GroupRenameArgs {
    /// Existing group name.
    pub group: String,

    /// New group name.
    pub new_group: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct GroupView {
    name: String,
    routing_policy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    metadata: Metadata,
    member_count: usize,
    members: Vec<GroupMemberView>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct GroupSummaryView {
    name: String,
    routing_policy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    metadata: Metadata,
    member_count: usize,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct GroupMemberView {
    agent: String,
    rank: String,
    tags: Vec<String>,
    weight: f64,
    routing_metadata: Metadata,
    created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct GroupListOutput {
    profile: String,
    count: usize,
    groups: Vec<GroupSummaryView>,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct GroupShowOutput {
    profile: String,
    group: GroupView,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct GroupActionOutput {
    profile: String,
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_name: Option<String>,
    group: GroupView,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct GroupDeleteOutput {
    profile: String,
    action: String,
    group: GroupView,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct GroupMemberActionOutput {
    profile: String,
    action: String,
    group: GroupView,
    member: GroupMemberView,
    message: String,
}

/// Executes one group subcommand.
pub(crate) fn execute_group_command<W>(
    command: &GroupCommands,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let registry = open_agent_registry(loaded_config, environment)?;

    match command {
        GroupCommands::Create(args) => {
            create_group(args, registry.profile, registry.store, mode, writer)
        }
        GroupCommands::List => list_groups(registry.profile, registry.store, mode, writer),
        GroupCommands::Show(args) => {
            show_group(args, registry.profile, registry.store, mode, writer)
        }
        GroupCommands::Add(args) => {
            add_group_member(args, registry.profile, registry.store, mode, writer)
        }
        GroupCommands::Remove(args) => {
            remove_group_member(args, registry.profile, registry.store, mode, writer)
        }
        GroupCommands::Rename(args) => {
            rename_group(args, registry.profile, registry.store, mode, writer)
        }
        GroupCommands::Delete(args) => {
            delete_group(args, registry.profile, registry.store, mode, writer)
        }
    }
}

fn create_group<W>(
    args: &GroupCreateArgs,
    profile: String,
    store: Store,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let group_name = parse_group_name(&args.group)?;
    ensure_group_absent(&store, &group_name)?;
    let routing_policy = parse_routing_policy(&args.routing_policy)?;
    let notes = parse_notes(args.notes.as_deref())?;
    let metadata = parse_metadata(&args.metadata, "--metadata")?;

    let mut upsert = GroupUpsert::new(group_name.clone());
    upsert.routing_policy = routing_policy;
    upsert.notes = notes;
    upsert.metadata = metadata;

    let record = store.upsert_group(&upsert)?;
    append_group_event(
        &store,
        "missive.group.create",
        Some(record.group_name.clone()),
        None,
        json!({
            "group": record.group_name.as_str(),
            "routing_policy": record.routing_policy.clone(),
            "notes": record.notes.clone(),
            "metadata": record.metadata.clone(),
        }),
    )?;
    let group = GroupView::from_record(&store, &record)?;
    let output = GroupActionOutput {
        profile,
        action: "create".to_owned(),
        previous_name: None,
        message: format!("Created group '{}'", record.group_name.as_str()),
        group,
    };

    render_group_action(writer, mode, "group_create", &output)
}

fn list_groups<W>(profile: String, store: Store, mode: OutputMode, writer: &mut W) -> Result<()>
where
    W: Write,
{
    let groups = store
        .list_groups()?
        .iter()
        .map(|record| GroupSummaryView::from_record(&store, record))
        .collect::<Result<Vec<_>>>()?;
    let output = GroupListOutput {
        profile,
        count: groups.len(),
        message: format!("Listed {} group(s)", groups.len()),
        groups,
    };

    render_group_list(writer, mode, &output)
}

fn show_group<W>(
    args: &GroupNameArgs,
    profile: String,
    store: Store,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let group_name = parse_group_name(&args.group)?;
    let record = get_existing_group(&store, &group_name)?;
    let group = GroupView::from_record(&store, &record)?;
    let output = GroupShowOutput {
        profile,
        message: format!("Showing group '{}'", record.group_name.as_str()),
        group,
    };

    render_group_show(writer, mode, &output)
}

fn add_group_member<W>(
    args: &GroupAddArgs,
    profile: String,
    store: Store,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let group_name = parse_group_name(&args.group)?;
    get_existing_group(&store, &group_name)?;
    let agent_alias = parse_agent_alias(&args.agent)?;
    get_existing_agent(&store, &agent_alias)?;
    let rank_name = parse_rank_name(&args.rank)?;
    ensure_rank_available(&store, &group_name, &rank_name, &agent_alias)?;
    let tags = parse_tags(&args.tags)?;
    let routing_metadata = parse_metadata(&args.routing_metadata, "--routing-metadata")?;

    let mut upsert = GroupMemberUpsert::new(group_name.clone(), agent_alias.clone(), rank_name);
    upsert.tags = tags;
    upsert.weight = args.weight;
    upsert.routing_metadata = routing_metadata;

    let member = store.upsert_group_member(&upsert)?;
    let record = get_existing_group(&store, &group_name)?;
    append_group_event(
        &store,
        "missive.group.member.add",
        Some(group_name.clone()),
        Some(agent_alias.clone()),
        json!({
            "group": group_name.as_str(),
            "agent": agent_alias.as_str(),
            "rank": member.rank_name.as_str(),
            "tags": member.tags.clone(),
            "weight": member.weight,
            "routing_metadata": member.routing_metadata.clone(),
        }),
    )?;
    let member_view = GroupMemberView::from_record(&member);
    let group = GroupView::from_record(&store, &record)?;
    let output = GroupMemberActionOutput {
        profile,
        action: "add".to_owned(),
        message: format!(
            "Added agent '{}' to group '{}' as rank '{}'",
            agent_alias.as_str(),
            group_name.as_str(),
            member.rank_name.as_str()
        ),
        group,
        member: member_view,
    };

    render_group_member_action(writer, mode, "group_add", &output)
}

fn remove_group_member<W>(
    args: &GroupMemberSelectorArgs,
    profile: String,
    store: Store,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let group_name = parse_group_name(&args.group)?;
    get_existing_group(&store, &group_name)?;
    let agent_alias = parse_agent_alias(&args.agent)?;
    let member = get_existing_member(&store, &group_name, &agent_alias)?;
    let member_view = GroupMemberView::from_record(&member);
    if !store.remove_group_member(&group_name, &agent_alias)? {
        return Err(MissiveError::storage(format!(
            "group member {}/{} disappeared before it could be removed",
            group_name.as_str(),
            agent_alias.as_str()
        )));
    }
    let record = get_existing_group(&store, &group_name)?;
    append_group_event(
        &store,
        "missive.group.member.remove",
        Some(group_name.clone()),
        Some(agent_alias.clone()),
        json!({
            "group": group_name.as_str(),
            "agent": agent_alias.as_str(),
            "removed_member": member_view.clone(),
        }),
    )?;
    let group = GroupView::from_record(&store, &record)?;
    let output = GroupMemberActionOutput {
        profile,
        action: "remove".to_owned(),
        message: format!(
            "Removed agent '{}' from group '{}'",
            agent_alias.as_str(),
            group_name.as_str()
        ),
        group,
        member: member_view,
    };

    render_group_member_action(writer, mode, "group_remove", &output)
}

fn rename_group<W>(
    args: &GroupRenameArgs,
    profile: String,
    store: Store,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let old_name = parse_group_name(&args.group)?;
    let new_name = parse_group_name(&args.new_group)?;
    if old_name == new_name {
        return Err(
            MissiveError::validation("group rename requires a different new group name")
                .with_help("Choose a new group name that does not match the current name."),
        );
    }
    get_existing_group(&store, &old_name)?;
    ensure_group_absent(&store, &new_name)?;

    if !store.rename_group(&old_name, &new_name)? {
        return Err(MissiveError::storage(format!(
            "group {:?} disappeared before it could be renamed",
            old_name.as_str()
        )));
    }
    let record = get_existing_group(&store, &new_name)?;
    append_group_event(
        &store,
        "missive.group.rename",
        Some(new_name.clone()),
        None,
        json!({
            "previous_name": old_name.as_str(),
            "group": new_name.as_str(),
        }),
    )?;
    let group = GroupView::from_record(&store, &record)?;
    let output = GroupActionOutput {
        profile,
        action: "rename".to_owned(),
        previous_name: Some(old_name.into_string()),
        message: format!(
            "Renamed group '{}' to '{}'",
            args.group.as_str(),
            new_name.as_str()
        ),
        group,
    };

    render_group_action(writer, mode, "group_rename", &output)
}

fn delete_group<W>(
    args: &GroupNameArgs,
    profile: String,
    store: Store,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let group_name = parse_group_name(&args.group)?;
    let record = get_existing_group(&store, &group_name)?;
    let group = GroupView::from_record(&store, &record)?;
    if !store.delete_group(&group_name)? {
        return Err(MissiveError::storage(format!(
            "group {:?} disappeared before it could be deleted",
            group_name.as_str()
        )));
    }
    append_group_event(
        &store,
        "missive.group.delete",
        None,
        None,
        json!({
            "group": group_name.as_str(),
            "deleted_group": group.clone(),
        }),
    )?;
    let output = GroupDeleteOutput {
        profile,
        action: "delete".to_owned(),
        message: format!("Deleted group '{}'", group_name.as_str()),
        group,
    };

    render_group_delete(writer, mode, &output)
}

fn append_group_event(
    store: &Store,
    event_type: &str,
    group_name: Option<GroupName>,
    agent_alias: Option<AgentAlias>,
    payload: Value,
) -> Result<()> {
    let mut event = new_cli_event(event_type, payload)?;
    event.group_name = group_name;
    event.agent_alias = agent_alias;
    store.append_event(&event)?;
    Ok(())
}

fn parse_group_name(value: &str) -> Result<GroupName> {
    GroupName::new(value.to_owned())
}

fn parse_agent_alias(value: &str) -> Result<AgentAlias> {
    AgentAlias::new(value.to_owned())
}

fn parse_rank_name(value: &str) -> Result<RankName> {
    RankName::new(value.to_owned())
}

fn parse_routing_policy(value: &str) -> Result<String> {
    let policy = parse_core_routing_policy(value).map_err(|error| {
        if error.help().is_some() {
            error
        } else {
            error.with_help(ROUTING_POLICY_HELP)
        }
    })?;
    Ok(policy.as_str().to_owned())
}

fn parse_notes(value: Option<&str>) -> Result<Option<String>> {
    value
        .map(|value| {
            validate_optional_text("--notes", value, GROUP_NOTES_MAX_BYTES, GROUP_NOTES_HELP)
        })
        .transpose()
}

fn parse_tags(values: &[String]) -> Result<Vec<String>> {
    values
        .iter()
        .map(|value| {
            validate_named_cli_identifier("member tag", value)?;
            Ok(value.clone())
        })
        .collect()
}

fn parse_metadata(values: &[String], flag: &str) -> Result<Metadata> {
    let mut metadata = Metadata::new();
    for value in values {
        let (key, raw_value) = split_key_value(flag, value)?;
        let parsed = serde_json::from_str::<Value>(raw_value)
            .unwrap_or_else(|_| Value::String(raw_value.to_owned()));
        if metadata.insert(key.to_owned(), parsed)?.is_some() {
            return Err(
                MissiveError::validation(format!("duplicate {flag} key {key:?}"))
                    .with_help("Pass each metadata key at most once."),
            );
        }
    }
    Ok(metadata)
}

fn split_key_value<'a>(flag: &str, value: &'a str) -> Result<(&'a str, &'a str)> {
    let Some((key, raw_value)) = value.split_once('=') else {
        return Err(MissiveError::validation(format!(
            "{flag} value {value:?} must use KEY=VALUE syntax"
        )));
    };
    if key.is_empty() || raw_value.is_empty() {
        return Err(MissiveError::validation(format!(
            "{flag} value {value:?} must include a non-empty key and value"
        )));
    }
    Ok((key, raw_value))
}

fn validate_optional_text(flag: &str, value: &str, max_bytes: usize, help: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(
            MissiveError::validation(format!("{flag} value cannot be empty")).with_help(help),
        );
    }
    if value.len() > max_bytes {
        return Err(MissiveError::validation(format!(
            "{flag} value is {} bytes, but the maximum is {max_bytes}",
            value.len()
        ))
        .with_help(help));
    }
    if value.chars().any(char::is_control) {
        return Err(MissiveError::validation(format!(
            "{flag} value cannot contain control characters"
        ))
        .with_help(help));
    }
    Ok(value.to_owned())
}

fn validate_named_cli_identifier(kind: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return invalid_named_cli_identifier(kind, "value cannot be empty");
    }
    if value.len() > NAMED_IDENTIFIER_MAX_BYTES {
        return invalid_named_cli_identifier(
            kind,
            format!(
                "value is {} bytes, but the maximum is {NAMED_IDENTIFIER_MAX_BYTES}",
                value.len()
            ),
        );
    }

    let bytes = value.as_bytes();
    if !is_ascii_lower_alphanumeric(bytes[0]) {
        return invalid_named_cli_identifier(
            kind,
            "value must start with a lowercase ASCII letter or digit",
        );
    }
    if !is_ascii_lower_alphanumeric(bytes[bytes.len() - 1]) {
        return invalid_named_cli_identifier(
            kind,
            "value must end with a lowercase ASCII letter or digit",
        );
    }

    for byte in bytes {
        if is_ascii_lower_alphanumeric(*byte) || matches!(*byte, b'-' | b'_' | b'.') {
            continue;
        }
        return invalid_named_cli_identifier(
            kind,
            "value must contain only lowercase ASCII letters, digits, '-', '_' or '.'",
        );
    }

    Ok(())
}

fn invalid_named_cli_identifier(kind: &str, reason: impl Into<String>) -> Result<()> {
    Err(
        MissiveError::validation(format!("invalid {kind}: {}", reason.into()))
            .with_help(NAMED_IDENTIFIER_HELP),
    )
}

const fn is_ascii_lower_alphanumeric(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

fn parse_positive_weight_arg(value: &str) -> std::result::Result<f64, String> {
    let parsed = value
        .parse::<f64>()
        .map_err(|error| format!("expected a positive finite number: {error}"))?;
    if parsed.is_finite() && parsed > 0.0 {
        Ok(parsed)
    } else {
        Err("expected a positive finite number greater than zero".to_owned())
    }
}

fn ensure_group_absent(store: &Store, group_name: &GroupName) -> Result<()> {
    if store.get_group(group_name)?.is_some() {
        return Err(MissiveError::validation(format!(
            "group {:?} already exists",
            group_name.as_str()
        ))
        .with_help("Choose a different group name or delete/rename the existing group."));
    }
    Ok(())
}

fn get_existing_group(store: &Store, group_name: &GroupName) -> Result<GroupRecord> {
    store.get_group(group_name)?.ok_or_else(|| {
        MissiveError::validation(format!("group {:?} does not exist", group_name.as_str()))
            .with_help("Run 'missive group list' to see locally known groups.")
    })
}

fn get_existing_member(
    store: &Store,
    group_name: &GroupName,
    agent_alias: &AgentAlias,
) -> Result<GroupMemberRecord> {
    store
        .list_group_members(group_name)?
        .into_iter()
        .find(|member| &member.agent_alias == agent_alias)
        .ok_or_else(|| {
            MissiveError::validation(format!(
                "agent {:?} is not a member of group {:?}",
                agent_alias.as_str(),
                group_name.as_str()
            ))
            .with_help("Run 'missive group show <group>' to inspect current membership.")
        })
}

fn ensure_rank_available(
    store: &Store,
    group_name: &GroupName,
    rank_name: &RankName,
    agent_alias: &AgentAlias,
) -> Result<()> {
    if let Some(existing) = store
        .list_group_members(group_name)?
        .into_iter()
        .find(|member| &member.rank_name == rank_name && &member.agent_alias != agent_alias)
    {
        return Err(MissiveError::validation(format!(
            "rank {:?} is already used in group {:?} by agent {:?}",
            rank_name.as_str(),
            group_name.as_str(),
            existing.agent_alias.as_str()
        ))
        .with_help("Choose a unique --rank value for each group member."));
    }
    Ok(())
}

impl GroupSummaryView {
    fn from_record(store: &Store, record: &GroupRecord) -> Result<Self> {
        let member_count = store.list_group_members(&record.group_name)?.len();
        Ok(Self {
            name: record.group_name.as_str().to_owned(),
            routing_policy: record.routing_policy.clone(),
            notes: record.notes.clone(),
            metadata: record.metadata.clone(),
            member_count,
            created_at: record.created_at.to_rfc3339(),
            updated_at: record.updated_at.to_rfc3339(),
        })
    }
}

impl GroupView {
    fn from_record(store: &Store, record: &GroupRecord) -> Result<Self> {
        let members = store
            .list_group_members(&record.group_name)?
            .iter()
            .map(GroupMemberView::from_record)
            .collect::<Vec<_>>();
        Ok(Self {
            name: record.group_name.as_str().to_owned(),
            routing_policy: record.routing_policy.clone(),
            notes: record.notes.clone(),
            metadata: record.metadata.clone(),
            member_count: members.len(),
            members,
            created_at: record.created_at.to_rfc3339(),
            updated_at: record.updated_at.to_rfc3339(),
        })
    }
}

impl GroupMemberView {
    fn from_record(record: &GroupMemberRecord) -> Self {
        Self {
            agent: record.agent_alias.as_str().to_owned(),
            rank: record.rank_name.as_str().to_owned(),
            tags: record.tags.clone(),
            weight: record.weight,
            routing_metadata: record.routing_metadata.clone(),
            created_at: record.created_at.to_rfc3339(),
        }
    }
}

fn render_group_list<W>(writer: &mut W, mode: OutputMode, output: &GroupListOutput) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_group_list_human(writer, output),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "group_list", output, &output.message)
        }
    }
}

fn render_group_show<W>(writer: &mut W, mode: OutputMode, output: &GroupShowOutput) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_group_human(writer, &output.group, None),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "group_show", output, &output.message)
        }
    }
}

fn render_group_action<W>(
    writer: &mut W,
    mode: OutputMode,
    kind: &str,
    output: &GroupActionOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_group_human(writer, &output.group, Some(&output.message)),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, kind, output, &output.message)
        }
    }
}

fn render_group_delete<W>(
    writer: &mut W,
    mode: OutputMode,
    output: &GroupDeleteOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => writeln!(writer, "{}", redact_text(&output.message))
            .map_err(|error| MissiveError::io("writing group delete output", error)),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "group_delete", output, &output.message)
        }
    }
}

fn render_group_member_action<W>(
    writer: &mut W,
    mode: OutputMode,
    kind: &str,
    output: &GroupMemberActionOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_group_human(writer, &output.group, Some(&output.message)),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, kind, output, &output.message)
        }
    }
}

fn write_group_list_human<W>(writer: &mut W, output: &GroupListOutput) -> Result<()>
where
    W: Write,
{
    if output.groups.is_empty() {
        return writeln!(
            writer,
            "No groups registered for profile '{}'.",
            redact_text(&output.profile)
        )
        .map_err(|error| MissiveError::io("writing group list output", error));
    }

    writeln!(
        writer,
        "Groups for profile '{}':",
        redact_text(&output.profile)
    )
    .map_err(|error| MissiveError::io("writing group list output", error))?;
    for group in &output.groups {
        writeln!(
            writer,
            "  {}  routing_policy={}  members={}  notes={}",
            redact_text(&group.name),
            redact_text(&group.routing_policy),
            group.member_count,
            group
                .notes
                .as_deref()
                .map(redact_text)
                .unwrap_or_else(|| "-".to_owned())
        )
        .map_err(|error| MissiveError::io("writing group list output", error))?;
    }
    Ok(())
}

fn write_group_human<W>(writer: &mut W, group: &GroupView, message: Option<&str>) -> Result<()>
where
    W: Write,
{
    if let Some(message) = message {
        writeln!(writer, "{}", redact_text(message))
            .map_err(|error| MissiveError::io("writing group output", error))?;
    }
    writeln!(writer, "Group {}", redact_text(&group.name))
        .map_err(|error| MissiveError::io("writing group output", error))?;
    writeln!(
        writer,
        "  routing_policy: {}",
        redact_text(&group.routing_policy)
    )
    .map_err(|error| MissiveError::io("writing group output", error))?;
    writeln!(
        writer,
        "  notes: {}",
        group
            .notes
            .as_deref()
            .map(redact_text)
            .unwrap_or_else(|| "-".to_owned())
    )
    .map_err(|error| MissiveError::io("writing group output", error))?;
    writeln!(writer, "  metadata: {}", metadata_json(&group.metadata)?)
        .map_err(|error| MissiveError::io("writing group output", error))?;
    writeln!(writer, "  members: {}", group.member_count)
        .map_err(|error| MissiveError::io("writing group output", error))?;
    for member in &group.members {
        writeln!(
            writer,
            "    {}  agent={}  weight={}  tags={}  routing_metadata={}",
            redact_text(&member.rank),
            redact_text(&member.agent),
            member.weight,
            if member.tags.is_empty() {
                "-".to_owned()
            } else {
                redact_text(&member.tags.join(","))
            },
            metadata_json(&member.routing_metadata)?
        )
        .map_err(|error| MissiveError::io("writing group output", error))?;
    }
    writeln!(writer, "  created_at: {}", redact_text(&group.created_at))
        .map_err(|error| MissiveError::io("writing group output", error))?;
    writeln!(writer, "  updated_at: {}", redact_text(&group.updated_at))
        .map_err(|error| MissiveError::io("writing group output", error))
}

fn metadata_json(metadata: &Metadata) -> Result<String> {
    let value = serde_json::to_value(metadata).map_err(|error| {
        MissiveError::orchestration("failed to encode metadata for human output").with_source(error)
    })?;
    serde_json::to_string(&redact_json(&value)).map_err(|error| {
        MissiveError::orchestration("failed to render metadata for human output").with_source(error)
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn positive_weight_parser_rejects_invalid_values() {
        assert_eq!(parse_positive_weight_arg("1.5"), Ok(1.5));
        assert!(parse_positive_weight_arg("0").is_err());
        assert!(parse_positive_weight_arg("-1").is_err());
        assert!(parse_positive_weight_arg("NaN").is_err());
    }

    #[test]
    fn metadata_values_parse_as_json_and_reject_duplicates() {
        let metadata = parse_metadata(
            &[
                "mode=planner".to_owned(),
                "weight=2".to_owned(),
                "flags={\"fast\":true}".to_owned(),
            ],
            "--metadata",
        )
        .expect("metadata");

        assert_eq!(metadata.get_str("mode"), Some("planner"));
        assert_eq!(metadata.get("weight"), Some(&json!(2)));
        assert_eq!(metadata.get("flags"), Some(&json!({"fast": true})));
        assert!(parse_metadata(&["a=1".to_owned(), "a=2".to_owned()], "--metadata").is_err());
    }

    #[test]
    fn routing_policy_validation_uses_named_identifier_rules() {
        assert_eq!(parse_routing_policy("direct").expect("policy"), "direct");
        assert!(parse_routing_policy("Bad Policy").is_err());
    }
}
