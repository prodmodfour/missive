//! Local adapter command entry points.
//!
//! The `missive adapter stdio` command exposes the first concrete adapter as a
//! subprocess-friendly JSON/NDJSON control loop. It reuses the existing send,
//! stream, and task command implementations instead of inventing separate
//! protocol behavior.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use missive_adapters::{
    STDIO_OUTPUT_KIND_COMMAND_OUTPUT, StdioCommand, StdioFraming, StdioInputFrame,
    StdioMessageFields, StdioOutputFrame, StdioRunMode, StdioTaskCancelCommand,
    StdioTaskGetCommand, StdioTaskListCommand, StdioTaskWaitCommand, read_single_frame,
    write_output_frame,
};
use missive_core::{LoadedConfig, Metadata, MissiveError, Result};
use missive_store::{TaskSource, TaskState};
use serde_json::Value;

use crate::GlobalArgs;
use crate::output::OutputMode;
use crate::send::SendArgs;
use crate::stream::StreamArgs;
use crate::task::{TaskCancelArgs, TaskCommands, TaskGetArgs, TaskListArgs, TaskWaitArgs};

/// Adapter subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum AdapterCommands {
    /// Run the stdin/stdout JSON/NDJSON adapter.
    Stdio(StdioArgs),
}

impl AdapterCommands {
    /// Stable subcommand spelling used in structured output messages.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Stdio(_) => "stdio",
        }
    }
}

/// Arguments for `missive adapter stdio`.
#[derive(Debug, Clone, Args)]
pub struct StdioArgs {
    /// Run one JSON request or read NDJSON frames until EOF.
    #[arg(long = "mode", value_enum, default_value_t = CliStdioRunMode::SingleShot)]
    pub mode: CliStdioRunMode,

    /// Frame stdin/stdout as JSON or NDJSON. Defaults to JSON for single-shot and NDJSON for long-running.
    #[arg(long = "framing", value_enum)]
    pub framing: Option<CliStdioFraming>,
}

/// clap-facing stdio run mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliStdioRunMode {
    /// Read one request frame and write that request's response frame(s).
    SingleShot,
    /// Read newline-delimited request frames until EOF and write newline-delimited response frames.
    LongRunning,
}

impl From<CliStdioRunMode> for StdioRunMode {
    fn from(value: CliStdioRunMode) -> Self {
        match value {
            CliStdioRunMode::SingleShot => Self::SingleShot,
            CliStdioRunMode::LongRunning => Self::LongRunning,
        }
    }
}

/// clap-facing stdio framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliStdioFraming {
    /// One JSON object.
    Json,
    /// One JSON object per line.
    Ndjson,
}

impl From<CliStdioFraming> for StdioFraming {
    fn from(value: CliStdioFraming) -> Self {
        match value {
            CliStdioFraming::Json => Self::Json,
            CliStdioFraming::Ndjson => Self::Ndjson,
        }
    }
}

/// Executes one adapter command.
pub(crate) fn execute_adapter_command<R, W>(
    command: &AdapterCommands,
    globals: &GlobalArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    input: &mut R,
    writer: &mut W,
) -> Result<()>
where
    R: Read,
    W: Write,
{
    match command {
        AdapterCommands::Stdio(args) => {
            run_stdio_adapter(args, globals, loaded_config, environment, input, writer)
        }
    }
}

fn run_stdio_adapter<R, W>(
    args: &StdioArgs,
    globals: &GlobalArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    input: &mut R,
    writer: &mut W,
) -> Result<()>
where
    R: Read,
    W: Write,
{
    let run_mode: StdioRunMode = args.mode.into();
    let framing = args.framing.map(Into::into).unwrap_or(match run_mode {
        StdioRunMode::SingleShot => StdioFraming::Json,
        StdioRunMode::LongRunning => StdioFraming::Ndjson,
    });
    if matches!(run_mode, StdioRunMode::LongRunning) && framing != StdioFraming::Ndjson {
        return Err(MissiveError::validation(
            "missive adapter stdio --mode long-running requires --framing ndjson",
        )
        .with_help("Long-running mode must preserve one input and output frame per line."));
    }

    match (run_mode, framing) {
        (StdioRunMode::SingleShot, StdioFraming::Json) => {
            let frame_result = read_single_frame(input);
            handle_stdio_frame_result(
                1,
                frame_result,
                globals,
                loaded_config,
                environment,
                run_mode,
                framing,
                writer,
            )
        }
        (StdioRunMode::SingleShot, StdioFraming::Ndjson) => {
            let mut reader = BufReader::new(input);
            let mut line = String::new();
            let bytes = reader
                .read_line(&mut line)
                .map_err(|error| MissiveError::io("reading one stdio NDJSON frame", error))?;
            let frame_result = if bytes == 0 || line.trim().is_empty() {
                Err(MissiveError::validation(
                    "stdin/stdout adapter expected one NDJSON frame on stdin",
                ))
            } else {
                StdioInputFrame::from_json_str(&line)
            };
            handle_stdio_frame_result(
                1,
                frame_result,
                globals,
                loaded_config,
                environment,
                run_mode,
                framing,
                writer,
            )
        }
        (StdioRunMode::LongRunning, StdioFraming::Ndjson) => {
            let mut reader = BufReader::new(input);
            for (line_index, line) in (&mut reader).lines().enumerate() {
                let line =
                    line.map_err(|error| MissiveError::io("reading stdio NDJSON frame", error))?;
                if line.trim().is_empty() {
                    continue;
                }
                handle_stdio_frame_result(
                    line_index + 1,
                    StdioInputFrame::from_json_str(&line),
                    globals,
                    loaded_config,
                    environment,
                    run_mode,
                    framing,
                    writer,
                )?;
                writer
                    .flush()
                    .map_err(|error| MissiveError::io("flushing stdio NDJSON frame", error))?;
            }
            Ok(())
        }
        (StdioRunMode::LongRunning, StdioFraming::Json) => unreachable!("validated above"),
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_stdio_frame_result<W>(
    line_number: usize,
    frame_result: std::result::Result<StdioInputFrame, MissiveError>,
    globals: &GlobalArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    run_mode: StdioRunMode,
    framing: StdioFraming,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    match frame_result {
        Ok(frame) => {
            let frames =
                execute_stdio_frame(&frame, globals, loaded_config, environment, run_mode)?;
            for frame in frames {
                write_output_frame(writer, framing, &frame)?;
            }
            Ok(())
        }
        Err(error) => {
            let mut frame = StdioOutputFrame::error(None, 0, &error);
            frame.data = Some(serde_json::json!({"line": line_number}));
            write_output_frame(writer, framing, &frame)
        }
    }
}

fn execute_stdio_frame(
    frame: &StdioInputFrame,
    globals: &GlobalArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    run_mode: StdioRunMode,
) -> Result<Vec<StdioOutputFrame>> {
    let inner_mode = match (&frame.command, run_mode) {
        (StdioCommand::Stream(_), StdioRunMode::LongRunning) => OutputMode::Ndjson,
        _ => OutputMode::Json,
    };
    let mut command_output = Vec::new();
    let command_result = match &frame.command {
        StdioCommand::Send(fields) => {
            let args = send_args_from_stdio(fields);
            let mut command_input = std::io::empty();
            crate::send::execute_send_command(
                &args,
                globals,
                loaded_config,
                environment,
                inner_mode,
                &mut command_input,
                &mut command_output,
            )
        }
        StdioCommand::Stream(command) => {
            let args = stream_args_from_stdio(command);
            let mut command_input = std::io::empty();
            crate::stream::execute_stream_command(
                &args,
                globals,
                loaded_config,
                environment,
                inner_mode,
                &mut command_input,
                &mut command_output,
            )
        }
        StdioCommand::TaskGet(command) => {
            let command = TaskCommands::Get(task_get_args_from_stdio(command)?);
            crate::task::execute_task_command(
                &command,
                globals,
                loaded_config,
                environment,
                inner_mode,
                &mut command_output,
            )
        }
        StdioCommand::TaskList(command) => {
            let command = TaskCommands::List(task_list_args_from_stdio(command)?);
            crate::task::execute_task_command(
                &command,
                globals,
                loaded_config,
                environment,
                inner_mode,
                &mut command_output,
            )
        }
        StdioCommand::TaskWait(command) => {
            let command = TaskCommands::Wait(task_wait_args_from_stdio(command));
            crate::task::execute_task_command(
                &command,
                globals,
                loaded_config,
                environment,
                inner_mode,
                &mut command_output,
            )
        }
        StdioCommand::TaskCancel(command) => {
            let command = TaskCommands::Cancel(task_cancel_args_from_stdio(command));
            crate::task::execute_task_command(
                &command,
                globals,
                loaded_config,
                environment,
                inner_mode,
                &mut command_output,
            )
        }
    };

    let mut frames = wrap_command_output(&frame.id, &command_output, inner_mode)?;
    if let Err(error) = command_result {
        let sequence = u64::try_from(frames.len()).map_err(|convert_error| {
            MissiveError::orchestration("stdio output sequence overflowed")
                .with_source(convert_error)
        })?;
        frames.push(StdioOutputFrame::error(
            Some(frame.id.clone()),
            sequence,
            &error,
        ));
    }
    Ok(frames)
}

fn wrap_command_output(
    frame_id: &str,
    command_output: &[u8],
    inner_mode: OutputMode,
) -> Result<Vec<StdioOutputFrame>> {
    if command_output.is_empty() {
        return Ok(Vec::new());
    }
    let output = std::str::from_utf8(command_output).map_err(|error| {
        MissiveError::orchestration("command output was not valid UTF-8").with_source(error)
    })?;
    match inner_mode {
        OutputMode::Json => {
            let value = serde_json::from_str::<Value>(output).map_err(|error| {
                MissiveError::orchestration(
                    "command JSON output could not be parsed for stdio framing",
                )
                .with_source(error)
            })?;
            Ok(vec![StdioOutputFrame::success(
                Some(frame_id.to_owned()),
                STDIO_OUTPUT_KIND_COMMAND_OUTPUT,
                0,
                value,
            )?])
        }
        OutputMode::Ndjson => output
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .map(|(index, line)| {
                let value = serde_json::from_str::<Value>(line).map_err(|error| {
                    MissiveError::orchestration(
                        "command NDJSON output could not be parsed for stdio framing",
                    )
                    .with_source(error)
                })?;
                let sequence = u64::try_from(index).map_err(|error| {
                    MissiveError::orchestration("stdio output sequence overflowed")
                        .with_source(error)
                })?;
                StdioOutputFrame::success(
                    Some(frame_id.to_owned()),
                    STDIO_OUTPUT_KIND_COMMAND_OUTPUT,
                    sequence,
                    value,
                )
            })
            .collect(),
        OutputMode::Human | OutputMode::Quiet => Ok(Vec::new()),
    }
}

fn send_args_from_stdio(fields: &StdioMessageFields) -> SendArgs {
    SendArgs {
        agent: fields.agent.clone(),
        message: fields.message.clone(),
        stdin: false,
        files: fields.files.iter().map(PathBuf::from).collect(),
        file_bytes: fields.file_bytes.iter().map(PathBuf::from).collect(),
        json_parts: fields.json_parts.iter().map(Value::to_string).collect(),
        mime: fields.mime.clone(),
        parts: fields
            .text_parts
            .iter()
            .map(|part| format!("text={part}"))
            .collect(),
        metadata: metadata_to_cli_args(&fields.metadata),
        context: fields.context.clone(),
        task: fields.task.clone(),
        accepted_output_modes: fields.accepted_output_modes.clone(),
    }
}

fn stream_args_from_stdio(command: &missive_adapters::StdioStreamCommand) -> StreamArgs {
    let send = send_args_from_stdio(&command.message);
    StreamArgs {
        agent: send.agent,
        message: send.message,
        stdin: false,
        files: send.files,
        file_bytes: send.file_bytes,
        json_parts: send.json_parts,
        mime: send.mime,
        parts: send.parts,
        metadata: send.metadata,
        context: send.context,
        task: send.task,
        accepted_output_modes: send.accepted_output_modes,
        force: command.force,
    }
}

fn task_get_args_from_stdio(command: &StdioTaskGetCommand) -> Result<TaskGetArgs> {
    Ok(TaskGetArgs {
        task_id: command.task_id.clone(),
        agent: command.agent.clone(),
        remote: command.remote,
        source: command
            .source
            .as_deref()
            .map(parse_task_source)
            .transpose()?,
        history_length: command.history_length,
    })
}

fn task_list_args_from_stdio(command: &StdioTaskListCommand) -> Result<TaskListArgs> {
    Ok(TaskListArgs {
        agent: command.agent.clone(),
        context: command.context.clone(),
        state: command.state.as_deref().map(parse_task_state).transpose()?,
        updated_after: command.updated_after.clone(),
        source: command
            .source
            .as_deref()
            .map(parse_task_source)
            .transpose()?,
        remote: command.remote,
        page_size: command.page_size,
        page_token: command.page_token.clone(),
        history_length: command.history_length,
        include_artifacts: command.include_artifacts,
    })
}

fn task_wait_args_from_stdio(command: &StdioTaskWaitCommand) -> TaskWaitArgs {
    TaskWaitArgs {
        task_id: command.task_id.clone(),
        agent: command.agent.clone(),
        local: command.local,
        interval: command.interval.clone(),
        history_length: command.history_length,
    }
}

fn task_cancel_args_from_stdio(command: &StdioTaskCancelCommand) -> TaskCancelArgs {
    TaskCancelArgs {
        task_id: command.task_id.clone(),
        agent: command.agent.clone(),
    }
}

fn metadata_to_cli_args(metadata: &Metadata) -> Vec<String> {
    metadata
        .iter()
        .map(|(key, value)| format!("{key}={}", value))
        .collect()
}

fn parse_task_state(value: &str) -> Result<TaskState> {
    let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
    normalized.parse::<TaskState>().map_err(|error| {
        MissiveError::validation(format!(
            "stdio task state {value:?} is invalid; expected submitted, working, input_required, completed, failed, cancelled, or unknown"
        ))
        .with_source(error)
    })
}

fn parse_task_source(value: &str) -> Result<TaskSource> {
    let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
    normalized.parse::<TaskSource>().map_err(|error| {
        MissiveError::validation(format!(
            "stdio task source {value:?} is invalid; expected remote, local, or gateway"
        ))
        .with_source(error)
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use missive_core::ConfigDiscovery;
    use missive_core::{AgentAlias, TaskId};
    use missive_store::{AgentUpsert, StatePathResolver, Store, TaskUpsert};
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    fn temp_loaded_config() -> (tempfile::TempDir, LoadedConfig, BTreeMap<String, String>) {
        let temp = tempdir().expect("tempdir");
        let env = BTreeMap::from([(
            "MISSIVE_HOME".to_owned(),
            temp.path().join("home").to_string_lossy().into_owned(),
        )]);
        let loaded = ConfigDiscovery::new()
            .with_env(env.iter().map(|(key, value)| (key.clone(), value.clone())))
            .load()
            .expect("config");
        (temp, loaded, env)
    }

    fn seed_local_task(loaded: &LoadedConfig, env: &BTreeMap<String, String>) {
        let paths = StatePathResolver::new()
            .with_env(env.iter().map(|(key, value)| (key.clone(), value.clone())))
            .resolve_loaded(loaded)
            .expect("paths");
        paths.ensure_directories().expect("dirs");
        let store = Store::open(paths.database_path()).expect("store");
        let alias = AgentAlias::new("echo").expect("agent");
        store
            .upsert_agent(&AgentUpsert::new(alias.clone(), "http://127.0.0.1:1"))
            .expect("agent");
        let task = TaskUpsert::new(
            TaskId::new("task-1").expect("task id"),
            alias,
            TaskState::Completed,
        );
        store.upsert_task(&task).expect("task");
    }

    #[test]
    fn single_shot_valid_task_list_frame_emits_wrapped_json_output() -> Result<()> {
        let (_temp, loaded, env) = temp_loaded_config();
        seed_local_task(&loaded, &env);
        let input = json!({
            "schema_version":"missive.stdio.v1",
            "id":"req-list",
            "command":"task_list"
        })
        .to_string();
        let mut input = input.as_bytes();
        let mut output = Vec::new();

        run_stdio_adapter(
            &StdioArgs {
                mode: CliStdioRunMode::SingleShot,
                framing: Some(CliStdioFraming::Json),
            },
            &GlobalArgs::default(),
            &loaded,
            &env,
            &mut input,
            &mut output,
        )?;

        let value = serde_json::from_slice::<Value>(&output).expect("stdio JSON output");
        assert_eq!(value["schema_version"], "missive.stdio.v1");
        assert_eq!(value["id"], "req-list");
        assert_eq!(value["ok"], true);
        assert_eq!(value["data"]["kind"], "task_list");
        assert_eq!(value["data"]["data"]["count"], 1);
        Ok(())
    }

    #[test]
    fn long_running_invalid_frame_emits_error_and_continues() -> Result<()> {
        let (_temp, loaded, env) = temp_loaded_config();
        seed_local_task(&loaded, &env);
        let input = concat!(
            "not-json\n",
            "{\"schema_version\":\"missive.stdio.v1\",\"id\":\"req-list\",\"command\":\"task_list\"}\n"
        );
        let mut input = input.as_bytes();
        let mut output = Vec::new();

        run_stdio_adapter(
            &StdioArgs {
                mode: CliStdioRunMode::LongRunning,
                framing: None,
            },
            &GlobalArgs::default(),
            &loaded,
            &env,
            &mut input,
            &mut output,
        )?;

        let output = String::from_utf8(output).expect("UTF-8");
        let lines = output.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        let first = serde_json::from_str::<Value>(lines[0]).expect("line 1");
        let second = serde_json::from_str::<Value>(lines[1]).expect("line 2");
        assert_eq!(first["ok"], false);
        assert_eq!(first["kind"], "stdio_error");
        assert_eq!(second["id"], "req-list");
        assert_eq!(second["data"]["kind"], "task_list");
        Ok(())
    }
}
