//! A2A task push notification config command implementation.
//!
//! The push command configures remote A2A task push notification callbacks,
//! persists redacted local records of the configured endpoints, and records
//! redacted event-journal entries for create/get/list/delete operations.

use std::collections::BTreeMap;
use std::io::Write;

use clap::{ArgAction, Args, Subcommand};
use missive_a2a::{
    NegotiatedInterface, PushConfigClient, ServiceParameters,
    protocol::{
        AuthenticationInfo, DeleteTaskPushNotificationConfigRequest,
        GetTaskPushNotificationConfigRequest, ListTaskPushNotificationConfigsRequest,
        TaskPushNotificationConfig,
    },
};
use missive_core::{AgentAlias, LoadedConfig, Metadata, MissiveError, Result, TaskId};
use missive_store::{
    AgentRecord, PushConfigId, PushConfigRecord, PushConfigUpsert, StoreTransaction, TaskSource,
    TaskState, TaskUpsert,
};
use serde::Serialize;
use serde_json::{Value, json};
use url::Url;

use crate::agent::{AgentRegistry, get_existing_agent, open_agent_registry};
use crate::auth::{auth_headers_for_agent, required_env_secret, validate_env_var_name};
use crate::events::new_cli_event;
use crate::output::{OutputMode, REDACTED, redact_json, redact_text, render_success};
use crate::send::resolve_send_interface;
use crate::{GlobalArgs, service_parameters_from_config_and_globals};

/// Push notification config subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum PushCommands {
    /// Create or replace one remote task push notification config.
    Create(PushCreateArgs),
    /// Fetch one remote task push notification config.
    Get(PushGetArgs),
    /// List remote task push notification configs for one task.
    List(PushListArgs),
    /// Delete one remote task push notification config.
    Delete(PushDeleteArgs),
}

impl PushCommands {
    /// Stable command spelling used in structured output messages.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Create(_) => "create",
            Self::Get(_) => "get",
            Self::List(_) => "list",
            Self::Delete(_) => "delete",
        }
    }
}

/// Arguments for `missive push create`.
#[derive(Debug, Clone, Args)]
pub struct PushCreateArgs {
    /// Registered agent alias that owns the remote task.
    pub agent: String,
    /// A2A task id to configure.
    pub task_id: String,
    /// Callback URL that the remote agent should call.
    pub url: String,

    /// Optional remote push config id.
    #[arg(long = "config-id", value_name = "ID")]
    pub config_id: Option<String>,

    /// Authentication scheme the remote agent should use for callback delivery.
    #[arg(long = "auth-scheme", value_name = "SCHEME")]
    pub auth_scheme: Option<String>,

    /// Environment variable containing callback credentials for --auth-scheme.
    #[arg(long = "auth-credentials-env", value_name = "ENV")]
    pub auth_credentials_env: Option<String>,

    /// Non-secret local metadata entry as KEY=VALUE; VALUE is parsed as JSON when possible.
    #[arg(long = "metadata", value_name = "KEY=VALUE", action = ArgAction::Append)]
    pub metadata: Vec<String>,
}

/// Arguments for `missive push get`.
#[derive(Debug, Clone, Args)]
pub struct PushGetArgs {
    /// Registered agent alias that owns the remote task.
    pub agent: String,
    /// A2A task id.
    pub task_id: String,
    /// Remote push config id.
    pub config_id: String,
}

/// Arguments for `missive push list`.
#[derive(Debug, Clone, Args)]
pub struct PushListArgs {
    /// Registered agent alias that owns the remote task.
    pub agent: String,
    /// A2A task id.
    pub task_id: String,

    /// Remote list page size.
    #[arg(long = "page-size", value_name = "N")]
    pub page_size: Option<i32>,

    /// Remote list page token.
    #[arg(long = "page-token", value_name = "TOKEN")]
    pub page_token: Option<String>,
}

/// Arguments for `missive push delete`.
#[derive(Debug, Clone, Args)]
pub struct PushDeleteArgs {
    /// Registered agent alias that owns the remote task.
    pub agent: String,
    /// A2A task id.
    pub task_id: String,
    /// Remote push config id.
    pub config_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PushInterfaceView {
    binding: String,
    protocol_binding: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
    protocol_version: String,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct PushConfigView {
    config_id: String,
    agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    callback_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    authentication: Option<Value>,
    metadata: Metadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_config: Option<Value>,
    created_at: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct PushConfigOutput {
    profile: String,
    selected_interface: PushInterfaceView,
    push_config: PushConfigView,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct PushListOutput {
    profile: String,
    selected_interface: PushInterfaceView,
    agent: String,
    task_id: String,
    count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_page_token: Option<String>,
    push_configs: Vec<PushConfigView>,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct PushDeleteOutput {
    profile: String,
    selected_interface: PushInterfaceView,
    agent: String,
    task_id: String,
    config_id: String,
    deleted: bool,
    local_record_deleted: bool,
    response: Value,
    message: String,
}

struct PushPersistence<'a> {
    agent: &'a AgentRecord,
    task_id: &'a TaskId,
    config: &'a TaskPushNotificationConfig,
    metadata: &'a Metadata,
    event_type: &'a str,
    raw_json: &'a Value,
    service_parameters: &'a ServiceParameters,
}

struct RemotePushContext {
    agent: AgentRecord,
    selected_interface: NegotiatedInterface,
    service_parameters: ServiceParameters,
}

/// Executes one push subcommand.
pub(crate) fn execute_push_command<W>(
    command: &PushCommands,
    globals: &GlobalArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let service_parameters = service_parameters_from_config_and_globals(loaded_config, globals)?;
    let mut registry = open_agent_registry(loaded_config, environment)?;

    match command {
        PushCommands::Create(args) => create_push_config(
            args,
            &mut registry,
            globals,
            environment,
            service_parameters,
            mode,
            writer,
        ),
        PushCommands::Get(args) => get_push_config(
            args,
            &mut registry,
            globals,
            environment,
            service_parameters,
            mode,
            writer,
        ),
        PushCommands::List(args) => list_push_configs(
            args,
            &mut registry,
            globals,
            environment,
            service_parameters,
            mode,
            writer,
        ),
        PushCommands::Delete(args) => delete_push_config(
            args,
            &mut registry,
            globals,
            environment,
            service_parameters,
            mode,
            writer,
        ),
    }
}

fn create_push_config<W>(
    args: &PushCreateArgs,
    registry: &mut AgentRegistry,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: ServiceParameters,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let task_id = TaskId::new(args.task_id.clone())?;
    validate_callback_url(&args.url)?;
    let metadata = parse_metadata(&args.metadata)?;
    let config_id = args
        .config_id
        .as_deref()
        .map(|value| PushConfigId::new(value.to_owned()))
        .transpose()?;
    let callback_auth = callback_authentication(args, environment)?;
    let context = resolve_remote_context(
        registry,
        &args.agent,
        globals,
        environment,
        service_parameters,
    )?;
    let request = TaskPushNotificationConfig {
        url: args.url.clone(),
        id: config_id
            .as_ref()
            .map(|push_config_id| push_config_id.as_str().to_owned()),
        task_id: task_id.as_str().to_owned(),
        token: None,
        authentication: callback_auth,
        tenant: context.selected_interface.tenant.clone(),
    };

    let outcome = PushConfigClient::new()?.create_config(
        &context.selected_interface,
        &request,
        &context.service_parameters,
        &auth_headers_for_agent(&registry.store, &context.agent, globals, environment)?,
    )?;
    let record = persist_remote_push_config(
        registry,
        PushPersistence {
            agent: &context.agent,
            task_id: &task_id,
            config: &outcome.config,
            metadata: &metadata,
            event_type: "a2a.push.create",
            raw_json: &outcome.raw_json,
            service_parameters: &context.service_parameters,
        },
    )?;
    let view = PushConfigView::from_record(&record);
    let output = PushConfigOutput {
        profile: registry.profile.clone(),
        selected_interface: PushInterfaceView::from(&context.selected_interface),
        message: format!(
            "Created push config '{}' for task '{}' on agent '{}'",
            view.config_id,
            task_id.as_str(),
            context.agent.alias.as_str()
        ),
        push_config: view,
    };

    render_push_config(writer, mode, "push_create", &output)
}

fn get_push_config<W>(
    args: &PushGetArgs,
    registry: &mut AgentRegistry,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: ServiceParameters,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let task_id = TaskId::new(args.task_id.clone())?;
    let config_id = PushConfigId::new(args.config_id.clone())?;
    let context = resolve_remote_context(
        registry,
        &args.agent,
        globals,
        environment,
        service_parameters,
    )?;
    let request = GetTaskPushNotificationConfigRequest {
        task_id: task_id.as_str().to_owned(),
        id: config_id.as_str().to_owned(),
        tenant: context.selected_interface.tenant.clone(),
    };
    let outcome = PushConfigClient::new()?.get_config(
        &context.selected_interface,
        &request,
        &context.service_parameters,
        &auth_headers_for_agent(&registry.store, &context.agent, globals, environment)?,
    )?;
    let metadata = Metadata::new();
    let record = persist_remote_push_config(
        registry,
        PushPersistence {
            agent: &context.agent,
            task_id: &task_id,
            config: &outcome.config,
            metadata: &metadata,
            event_type: "a2a.push.get",
            raw_json: &outcome.raw_json,
            service_parameters: &context.service_parameters,
        },
    )?;
    let view = PushConfigView::from_record(&record);
    let output = PushConfigOutput {
        profile: registry.profile.clone(),
        selected_interface: PushInterfaceView::from(&context.selected_interface),
        message: format!(
            "Fetched push config '{}' for task '{}' on agent '{}'",
            config_id.as_str(),
            task_id.as_str(),
            context.agent.alias.as_str()
        ),
        push_config: view,
    };

    render_push_config(writer, mode, "push_get", &output)
}

fn list_push_configs<W>(
    args: &PushListArgs,
    registry: &mut AgentRegistry,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: ServiceParameters,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    validate_positive_i32("--page-size", args.page_size)?;
    let task_id = TaskId::new(args.task_id.clone())?;
    let context = resolve_remote_context(
        registry,
        &args.agent,
        globals,
        environment,
        service_parameters,
    )?;
    let request = ListTaskPushNotificationConfigsRequest {
        task_id: task_id.as_str().to_owned(),
        page_size: args.page_size,
        page_token: args.page_token.clone(),
        tenant: context.selected_interface.tenant.clone(),
    };
    let outcome = PushConfigClient::new()?.list_configs(
        &context.selected_interface,
        &request,
        &context.service_parameters,
        &auth_headers_for_agent(&registry.store, &context.agent, globals, environment)?,
    )?;
    let records = registry.store.transaction(|transaction| {
        ensure_task_placeholder(
            transaction,
            &context.agent,
            &task_id,
            &context.service_parameters,
        )?;
        let mut records = Vec::with_capacity(outcome.response.configs.len());
        for config in &outcome.response.configs {
            let record = upsert_push_config_record(
                transaction,
                &context.agent,
                &task_id,
                config,
                &Metadata::new(),
                &context.service_parameters,
            )?;
            records.push(record);
        }
        append_push_event(
            transaction,
            "a2a.push.list",
            &context.agent,
            &task_id,
            None,
            json!({
                "agent": context.agent.alias.as_str(),
                "task_id": task_id.as_str(),
                "count": records.len(),
                "response": outcome.raw_json,
            }),
            &context.service_parameters,
        )?;
        Ok(records)
    })?;
    let push_configs = records
        .iter()
        .map(PushConfigView::from_record)
        .collect::<Vec<_>>();
    let output = PushListOutput {
        profile: registry.profile.clone(),
        selected_interface: PushInterfaceView::from(&context.selected_interface),
        agent: context.agent.alias.as_str().to_owned(),
        task_id: task_id.as_str().to_owned(),
        count: push_configs.len(),
        next_page_token: outcome.response.next_page_token,
        message: format!(
            "Listed {} push config(s) for task '{}' on agent '{}'",
            push_configs.len(),
            task_id.as_str(),
            context.agent.alias.as_str()
        ),
        push_configs,
    };

    render_push_list(writer, mode, &output)
}

fn delete_push_config<W>(
    args: &PushDeleteArgs,
    registry: &mut AgentRegistry,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: ServiceParameters,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let task_id = TaskId::new(args.task_id.clone())?;
    let config_id = PushConfigId::new(args.config_id.clone())?;
    let context = resolve_remote_context(
        registry,
        &args.agent,
        globals,
        environment,
        service_parameters,
    )?;
    let request = DeleteTaskPushNotificationConfigRequest {
        task_id: task_id.as_str().to_owned(),
        id: config_id.as_str().to_owned(),
        tenant: context.selected_interface.tenant.clone(),
    };
    let outcome = PushConfigClient::new()?.delete_config(
        &context.selected_interface,
        &request,
        &context.service_parameters,
        &auth_headers_for_agent(&registry.store, &context.agent, globals, environment)?,
    )?;
    let local_record_deleted = registry.store.transaction(|transaction| {
        ensure_task_placeholder(
            transaction,
            &context.agent,
            &task_id,
            &context.service_parameters,
        )?;
        let deleted = transaction.delete_push_config(&config_id)?;
        append_push_event(
            transaction,
            "a2a.push.delete",
            &context.agent,
            &task_id,
            Some(&config_id),
            json!({
                "agent": context.agent.alias.as_str(),
                "task_id": task_id.as_str(),
                "config_id": config_id.as_str(),
                "response": outcome.raw_json,
            }),
            &context.service_parameters,
        )?;
        Ok(deleted)
    })?;
    let output = PushDeleteOutput {
        profile: registry.profile.clone(),
        selected_interface: PushInterfaceView::from(&context.selected_interface),
        agent: context.agent.alias.as_str().to_owned(),
        task_id: task_id.as_str().to_owned(),
        config_id: config_id.as_str().to_owned(),
        deleted: delete_response_reports_success(&outcome.raw_json),
        local_record_deleted,
        response: redact_json(&outcome.raw_json),
        message: format!(
            "Deleted push config '{}' for task '{}' on agent '{}'",
            config_id.as_str(),
            task_id.as_str(),
            context.agent.alias.as_str()
        ),
    };

    render_push_delete(writer, mode, &output)
}

fn resolve_remote_context(
    registry: &mut AgentRegistry,
    agent: &str,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: ServiceParameters,
) -> Result<RemotePushContext> {
    let alias = AgentAlias::new(agent.to_owned())?;
    let agent = get_existing_agent(&registry.store, &alias)?;
    let auth_headers = auth_headers_for_agent(&registry.store, &agent, globals, environment)?;
    let (agent, selected_interface) =
        resolve_send_interface(registry, agent, &service_parameters, &auth_headers)?;
    Ok(RemotePushContext {
        agent,
        selected_interface,
        service_parameters,
    })
}

fn callback_authentication(
    args: &PushCreateArgs,
    environment: &BTreeMap<String, String>,
) -> Result<Option<AuthenticationInfo>> {
    let scheme = args
        .auth_scheme
        .as_deref()
        .map(validate_auth_scheme)
        .transpose()?;
    let credentials = args
        .auth_credentials_env
        .as_deref()
        .map(|env_name| {
            validate_env_var_name("--auth-credentials-env", env_name)?;
            required_env_secret("--auth-credentials-env", env_name, environment)
        })
        .transpose()?;

    match (scheme, credentials) {
        (None, None) => Ok(None),
        (Some(scheme), credentials) => Ok(Some(AuthenticationInfo {
            scheme,
            credentials,
        })),
        (None, Some(_)) => Err(MissiveError::validation(
            "--auth-credentials-env requires --auth-scheme",
        )
        .with_help(
            "Pass both flags so the remote agent knows how to authenticate to the callback URL.",
        )),
    }
}

fn persist_remote_push_config(
    registry: &mut AgentRegistry,
    input: PushPersistence<'_>,
) -> Result<PushConfigRecord> {
    registry.store.transaction(|transaction| {
        ensure_task_placeholder(
            transaction,
            input.agent,
            input.task_id,
            input.service_parameters,
        )?;
        let record = upsert_push_config_record(
            transaction,
            input.agent,
            input.task_id,
            input.config,
            input.metadata,
            input.service_parameters,
        )?;
        append_push_event(
            transaction,
            input.event_type,
            input.agent,
            input.task_id,
            Some(&record.push_config_id),
            json!({
                "agent": input.agent.alias.as_str(),
                "task_id": input.task_id.as_str(),
                "config_id": record.push_config_id.as_str(),
                "config": input.raw_json,
            }),
            input.service_parameters,
        )?;
        Ok(record)
    })
}

fn ensure_task_placeholder(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    task_id: &TaskId,
    service_parameters: &ServiceParameters,
) -> Result<()> {
    if transaction.get_task(task_id)?.is_some() {
        return Ok(());
    }
    let mut task = TaskUpsert::new(task_id.clone(), agent.alias.clone(), TaskState::Unknown);
    task.source = TaskSource::Remote;
    task.record_a2a_protocol_version(service_parameters.protocol_version.clone())?;
    transaction.upsert_task(&task)?;
    Ok(())
}

fn upsert_push_config_record(
    transaction: &StoreTransaction<'_>,
    agent: &AgentRecord,
    task_id: &TaskId,
    config: &TaskPushNotificationConfig,
    metadata: &Metadata,
    service_parameters: &ServiceParameters,
) -> Result<PushConfigRecord> {
    let push_config_id = push_config_id_from_remote(config, task_id)?;
    let raw_config = serde_json::to_value(config).map_err(|error| {
        MissiveError::protocol("encoding A2A push config for local persistence").with_source(error)
    })?;
    let mut combined_metadata = transaction
        .get_push_config(&push_config_id)?
        .map_or_else(Metadata::new, |record| record.metadata);
    combined_metadata.merge(metadata.clone());
    service_parameters.record_metadata(&mut combined_metadata)?;
    let mut input = PushConfigUpsert::new(push_config_id, agent.alias.clone(), config.url.clone());
    input.task_id = Some(task_id.clone());
    input.remote_config_json = Some(redact_json(&raw_config));
    input.metadata = combined_metadata;
    transaction.upsert_push_config(&input)
}

fn append_push_event(
    transaction: &StoreTransaction<'_>,
    event_type: &str,
    agent: &AgentRecord,
    task_id: &TaskId,
    push_config_id: Option<&PushConfigId>,
    payload: Value,
    service_parameters: &ServiceParameters,
) -> Result<()> {
    let mut event = new_cli_event(event_type, payload)?;
    event.agent_alias = Some(agent.alias.clone());
    event.task_id = Some(task_id.clone());
    event.record_a2a_protocol_version(service_parameters.protocol_version.clone())?;
    if let Some(push_config_id) = push_config_id {
        event
            .metadata
            .insert_str("missive.push_config_id", push_config_id.as_str())?;
    }
    transaction.append_event(&event)?;
    Ok(())
}

fn push_config_id_from_remote(
    config: &TaskPushNotificationConfig,
    task_id: &TaskId,
) -> Result<PushConfigId> {
    if let Some(id) = config.id.as_deref().filter(|id| !id.trim().is_empty()) {
        return PushConfigId::new(id.to_owned());
    }

    let candidate = format!("{}/{}", task_id.as_str(), config.url);
    PushConfigId::new(candidate)
        .or_else(|_| PushConfigId::new(format!("push/{}", missive_a2a::protocol::new_message_id())))
}

impl PushConfigView {
    fn from_record(record: &PushConfigRecord) -> Self {
        let authentication = record
            .remote_config_json
            .as_ref()
            .and_then(|value| value.get("authentication"))
            .cloned()
            .map(|value| redact_json(&value));
        Self {
            config_id: record.push_config_id.as_str().to_owned(),
            agent: record.agent_alias.as_str().to_owned(),
            task_id: record
                .task_id
                .as_ref()
                .map(|task_id| task_id.as_str().to_owned()),
            callback_url: record.callback_url.clone(),
            authentication,
            metadata: record.metadata.clone(),
            remote_config: record.remote_config_json.as_ref().map(redact_json),
            created_at: record.created_at.to_rfc3339(),
            updated_at: record.updated_at.to_rfc3339(),
            deleted_at: record.deleted_at.map(|timestamp| timestamp.to_rfc3339()),
        }
    }
}

impl From<&NegotiatedInterface> for PushInterfaceView {
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

fn render_push_config<W>(
    writer: &mut W,
    mode: OutputMode,
    kind: &str,
    output: &PushConfigOutput,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => {
            writeln!(writer, "{}", redact_text(&output.message))
                .map_err(|error| MissiveError::io("writing push config output", error))?;
            write_push_config_human(writer, &output.push_config)
        }
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, kind, output, &output.message)
        }
    }
}

fn render_push_list<W>(writer: &mut W, mode: OutputMode, output: &PushListOutput) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_push_list_human(writer, output),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "push_list", output, &output.message)
        }
    }
}

fn render_push_delete<W>(writer: &mut W, mode: OutputMode, output: &PushDeleteOutput) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => writeln!(writer, "{}", redact_text(&output.message))
            .map_err(|error| MissiveError::io("writing push delete output", error)),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "push_delete", output, &output.message)
        }
    }
}

fn write_push_list_human<W>(writer: &mut W, output: &PushListOutput) -> Result<()>
where
    W: Write,
{
    if output.push_configs.is_empty() {
        return writeln!(
            writer,
            "No push configs found for task '{}' on agent '{}'.",
            redact_text(&output.task_id),
            redact_text(&output.agent)
        )
        .map_err(|error| MissiveError::io("writing push list output", error));
    }

    writeln!(
        writer,
        "Push configs for task '{}' on agent '{}':",
        redact_text(&output.task_id),
        redact_text(&output.agent)
    )
    .map_err(|error| MissiveError::io("writing push list output", error))?;
    for config in &output.push_configs {
        write!(
            writer,
            "  {}  url={}  updated={}",
            redact_text(&config.config_id),
            redact_text(&config.callback_url),
            redact_text(&config.updated_at)
        )
        .map_err(|error| MissiveError::io("writing push list output", error))?;
        if config.authentication.is_some() {
            write!(writer, "  auth={REDACTED}")
                .map_err(|error| MissiveError::io("writing push list output", error))?;
        }
        writeln!(writer).map_err(|error| MissiveError::io("writing push list output", error))?;
    }
    Ok(())
}

fn write_push_config_human<W>(writer: &mut W, config: &PushConfigView) -> Result<()>
where
    W: Write,
{
    writeln!(
        writer,
        "Push config '{}' for task {} on agent {}",
        redact_text(&config.config_id),
        config
            .task_id
            .as_deref()
            .map(redact_text)
            .unwrap_or_else(|| "-".to_owned()),
        redact_text(&config.agent)
    )
    .map_err(|error| MissiveError::io("writing push config output", error))?;
    writeln!(writer, "  url: {}", redact_text(&config.callback_url))
        .map_err(|error| MissiveError::io("writing push config output", error))?;
    if config.authentication.is_some() {
        writeln!(writer, "  authentication: {REDACTED}")
            .map_err(|error| MissiveError::io("writing push config output", error))?;
    }
    Ok(())
}

fn parse_metadata(values: &[String]) -> Result<Metadata> {
    let mut metadata = Metadata::new();
    for value in values {
        let (key, raw_value) = split_key_value("--metadata", value)?;
        let parsed_value =
            serde_json::from_str(raw_value).unwrap_or_else(|_| Value::String(raw_value.to_owned()));
        if metadata.insert(key.to_owned(), parsed_value)?.is_some() {
            return Err(
                MissiveError::validation(format!("duplicate metadata key {key:?}"))
                    .with_help("Pass each --metadata key at most once."),
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

fn validate_callback_url(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(MissiveError::validation(
            "push callback URL cannot be empty",
        ));
    }
    if value.chars().any(char::is_whitespace) || value.chars().any(char::is_control) {
        return Err(MissiveError::validation(
            "push callback URL must be an HTTP(S) URL without whitespace or control characters",
        )
        .with_help("Use an absolute callback URL such as https://example.test/a2a/push."));
    }

    let parsed = Url::parse(value).map_err(|error| {
        MissiveError::validation("push callback URL must be a valid absolute HTTP(S) URL")
            .with_source(error)
            .with_help("Use an absolute callback URL such as https://example.test/a2a/push.")
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(MissiveError::validation(
            "push callback URL must use http or https and include a host",
        )
        .with_help("Use an absolute callback URL such as https://example.test/a2a/push."));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(MissiveError::validation(
            "push callback URL must not include embedded credentials",
        )
        .with_help("Use --auth-scheme and --auth-credentials-env for callback authentication."));
    }
    Ok(())
}

fn validate_auth_scheme(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(MissiveError::validation("--auth-scheme cannot be empty"));
    }
    if value.len() > 64
        || value
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
    {
        return Err(MissiveError::validation(
            "--auth-scheme must be a short ASCII token such as Bearer or Basic",
        ));
    }
    Ok(value.to_owned())
}

fn validate_positive_i32(flag: &str, value: Option<i32>) -> Result<()> {
    if value.is_some_and(|value| value <= 0) {
        return Err(MissiveError::validation(format!(
            "{flag} must be a positive integer"
        )));
    }
    Ok(())
}

fn delete_response_reports_success(value: &Value) -> bool {
    value
        .get("deleted")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}
