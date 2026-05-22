//! File-drop adapter framing and atomic directory handoff helpers.
//!
//! The file-drop adapter is a local filesystem boundary for simple automation
//! where another process can write one request file into an inbox and read the
//! corresponding result file from an outbox. Producers should write to a
//! temporary filename and atomically rename to `*.json` when the request is
//! complete; the adapter only claims ready `*.json` files.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use missive_core::{
    AgentAlias, ContextId, ErrorReport, EventId, MessageId, Metadata, MissiveError, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::stdio::{
    STDIO_FRAME_SCHEMA_VERSION, StdioCommand, StdioFrameSource, StdioInputFrame,
    StdioMessageFields, StdioOutputFrame, StdioStreamCommand, StdioTaskCancelCommand,
    StdioTaskGetCommand, StdioTaskListCommand, StdioTaskWaitCommand,
};
use crate::{
    Adapter, AdapterAcknowledgement, AdapterContext, AdapterDefinition, AdapterEvent,
    AdapterExternalIdentity, AdapterIdentity, AdapterInboundMessage, AdapterInboundPayload,
    AdapterLifecycleEvent, AdapterLifecycleState, AdapterOutboundUpdate,
};

/// Built-in adapter kind for local file-drop directories.
pub const FILE_DROP_ADAPTER_KIND: &str = "file-drop";

/// Stable schema marker for file-drop request and result files.
pub const FILE_DROP_FRAME_SCHEMA_VERSION: &str = "missive.file_drop.v1";

/// Output kind used when a ready file is processed successfully.
pub const FILE_DROP_OUTPUT_KIND_RESULT: &str = "file_drop_result";

/// Output kind used when a ready file cannot be parsed or processed.
pub const FILE_DROP_OUTPUT_KIND_ERROR: &str = "file_drop_error";

const DEFAULT_SOURCE_ID: &str = "file-drop";
const DEFAULT_RESUME_NAME: &str = "default";
const MAX_FILE_DROP_ID_BYTES: usize = 128;
const MAX_SHORT_FIELD_BYTES: usize = 4096;
const UNIQUE_ATTEMPTS: usize = 10_000;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Source/session hints attached to one file-drop request file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDropSource {
    /// Stable source id for session continuity. Defaults to `file-drop`.
    #[serde(default = "default_source_id")]
    pub source_id: String,
    /// Optional human display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Gateway session resume name. Defaults to `default`.
    #[serde(default = "default_resume_name")]
    pub resume_name: String,
    /// Optional profile hint for a gateway session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

impl Default for FileDropSource {
    fn default() -> Self {
        Self {
            source_id: default_source_id(),
            display_name: None,
            resume_name: default_resume_name(),
            profile: None,
        }
    }
}

impl From<FileDropSource> for StdioFrameSource {
    fn from(value: FileDropSource) -> Self {
        Self {
            source_id: value.source_id,
            display_name: value.display_name,
            resume_name: value.resume_name,
            profile: value.profile,
        }
    }
}

/// Shared options for file-drop `job_start_*` request files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDropJobOptions {
    /// Maximum attempts before the gateway marks the job failed.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// Wait for the queued job to reach a terminal state and include the final row.
    #[serde(default)]
    pub attach: bool,
    /// Timeout for `attach`, for example `30s`, `2m`, or `1h`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attach_timeout: Option<String>,
    /// Request remote A2A task cancellation when this job is cancelled and a task id is known.
    #[serde(default)]
    pub cancel_remote_on_cancel: bool,
}

impl Default for FileDropJobOptions {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            attach: false,
            attach_timeout: None,
            cancel_remote_on_cancel: false,
        }
    }
}

/// Arguments for a file-drop background send request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDropJobStartSendCommand {
    /// Message fields to enqueue.
    #[serde(flatten)]
    pub message: StdioMessageFields,
    /// Background job options.
    #[serde(default)]
    pub options: FileDropJobOptions,
}

/// Arguments for a file-drop background stream request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDropJobStartStreamCommand {
    /// Stream command fields to enqueue.
    #[serde(flatten)]
    pub stream: StdioStreamCommand,
    /// Background job options.
    #[serde(default)]
    pub options: FileDropJobOptions,
}

/// Arguments for a file-drop background task wait request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDropJobStartWaitCommand {
    /// Task wait fields to enqueue.
    #[serde(flatten)]
    pub wait: StdioTaskWaitCommand,
    /// Background job options.
    #[serde(default)]
    pub options: FileDropJobOptions,
}

/// Arguments for a file-drop background local reduce request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDropJobStartReduceCommand {
    /// Local group to reduce.
    pub group: String,
    /// Shared A2A context id whose local group outputs should be reduced.
    pub context: String,
    /// Local deterministic reduce strategy.
    #[serde(default = "default_reduce_strategy")]
    pub strategy: String,
    /// Background job options.
    #[serde(default)]
    pub options: FileDropJobOptions,
}

/// Arguments for a file-drop background job list request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileDropJobListCommand {
    /// Filter by job kind: send, stream, wait, or reduce.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Filter by job state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Filter by agent alias.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Filter by context id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Limit the number of rows rendered after filtering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// Arguments for a file-drop background job show request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDropJobShowCommand {
    /// Gateway job id.
    pub job_id: String,
}

/// Arguments for a file-drop background job cancel request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDropJobCancelCommand {
    /// Gateway job id.
    pub job_id: String,
    /// Also request remote A2A CancelTask when the job has or records a task id.
    #[serde(default)]
    pub remote: bool,
}

/// Commands accepted by file-drop request files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum FileDropCommand {
    /// Run a foreground non-streaming send command.
    Send(StdioMessageFields),
    /// Run a foreground streaming send command.
    Stream(StdioStreamCommand),
    /// Run foreground task get.
    TaskGet(StdioTaskGetCommand),
    /// Run foreground task list.
    TaskList(StdioTaskListCommand),
    /// Run foreground task wait.
    TaskWait(StdioTaskWaitCommand),
    /// Run foreground task cancel.
    TaskCancel(StdioTaskCancelCommand),
    /// Enqueue a gateway-managed background send job.
    JobStartSend(FileDropJobStartSendCommand),
    /// Enqueue a gateway-managed background streaming job.
    JobStartStream(FileDropJobStartStreamCommand),
    /// Enqueue a gateway-managed background task wait job.
    JobStartWait(FileDropJobStartWaitCommand),
    /// Enqueue a gateway-managed background local reduce job.
    JobStartReduce(FileDropJobStartReduceCommand),
    /// List gateway-managed background jobs.
    JobList(FileDropJobListCommand),
    /// Show one gateway-managed background job.
    JobShow(FileDropJobShowCommand),
    /// Cancel one gateway-managed background job.
    JobCancel(FileDropJobCancelCommand),
}

impl FileDropCommand {
    /// Stable command label.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Send(_) => "send",
            Self::Stream(_) => "stream",
            Self::TaskGet(_) => "task_get",
            Self::TaskList(_) => "task_list",
            Self::TaskWait(_) => "task_wait",
            Self::TaskCancel(_) => "task_cancel",
            Self::JobStartSend(_) => "job_start_send",
            Self::JobStartStream(_) => "job_start_stream",
            Self::JobStartWait(_) => "job_start_wait",
            Self::JobStartReduce(_) => "job_start_reduce",
            Self::JobList(_) => "job_list",
            Self::JobShow(_) => "job_show",
            Self::JobCancel(_) => "job_cancel",
        }
    }

    /// Converts foreground stdio-compatible commands into the shared stdio command model.
    #[must_use]
    pub fn to_stdio_command(&self) -> Option<StdioCommand> {
        match self {
            Self::Send(command) => Some(StdioCommand::Send(command.clone())),
            Self::Stream(command) => Some(StdioCommand::Stream(command.clone())),
            Self::TaskGet(command) => Some(StdioCommand::TaskGet(command.clone())),
            Self::TaskList(command) => Some(StdioCommand::TaskList(command.clone())),
            Self::TaskWait(command) => Some(StdioCommand::TaskWait(command.clone())),
            Self::TaskCancel(command) => Some(StdioCommand::TaskCancel(command.clone())),
            Self::JobStartSend(_)
            | Self::JobStartStream(_)
            | Self::JobStartWait(_)
            | Self::JobStartReduce(_)
            | Self::JobList(_)
            | Self::JobShow(_)
            | Self::JobCancel(_) => None,
        }
    }

    /// Agent targeted by this command when present.
    #[must_use]
    pub fn target_agent(&self) -> Option<&str> {
        match self {
            Self::Send(command) => Some(&command.agent),
            Self::Stream(command) => Some(&command.message.agent),
            Self::TaskGet(command) => command.agent.as_deref(),
            Self::TaskList(command) => command.agent.as_deref(),
            Self::TaskWait(command) => command.agent.as_deref(),
            Self::TaskCancel(command) => command.agent.as_deref(),
            Self::JobStartSend(command) => Some(&command.message.agent),
            Self::JobStartStream(command) => Some(&command.stream.message.agent),
            Self::JobStartWait(command) => command.wait.agent.as_deref(),
            Self::JobList(command) => command.agent.as_deref(),
            Self::JobStartReduce(_) | Self::JobShow(_) | Self::JobCancel(_) => None,
        }
    }

    /// Context id referenced by this command when present.
    #[must_use]
    pub fn context_id(&self) -> Option<&str> {
        match self {
            Self::Send(command) => command.context.as_deref(),
            Self::Stream(command) => command.message.context.as_deref(),
            Self::TaskList(command) => command.context.as_deref(),
            Self::JobStartSend(command) => command.message.context.as_deref(),
            Self::JobStartStream(command) => command.stream.message.context.as_deref(),
            Self::JobStartReduce(command) => Some(&command.context),
            Self::JobList(command) => command.context.as_deref(),
            Self::TaskGet(_)
            | Self::TaskWait(_)
            | Self::TaskCancel(_)
            | Self::JobStartWait(_)
            | Self::JobShow(_)
            | Self::JobCancel(_) => None,
        }
    }
}

/// One file-drop request file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDropInputFile {
    /// Stable file-drop schema marker.
    #[serde(default = "default_file_drop_schema_version")]
    pub schema_version: String,
    /// Request/correlation id supplied by the producer.
    pub id: String,
    /// Source/session hints.
    #[serde(default)]
    pub source: FileDropSource,
    /// Command to run or enqueue.
    #[serde(flatten)]
    pub command: FileDropCommand,
    /// Non-secret frame metadata.
    #[serde(default)]
    pub metadata: Metadata,
}

impl FileDropInputFile {
    /// Parses and validates one JSON request file.
    pub fn from_json_str(input: &str) -> Result<Self> {
        let file = serde_json::from_str::<Self>(input).map_err(|error| {
            MissiveError::validation("failed to parse file-drop request as JSON")
                .with_source(error)
                .with_help("Write one complete JSON object, then atomically rename it to *.json.")
        })?;
        file.validate()?;
        Ok(file)
    }

    /// Parses and validates one request from a JSON value.
    pub fn from_value(value: Value) -> Result<Self> {
        let file = serde_json::from_value::<Self>(value).map_err(|error| {
            MissiveError::validation("failed to parse file-drop request")
                .with_source(error)
                .with_help("Use schema_version, id, command, and command-specific fields.")
        })?;
        file.validate()?;
        Ok(file)
    }

    /// Validates the schema marker, id, source, and command fields.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != FILE_DROP_FRAME_SCHEMA_VERSION {
            return Err(MissiveError::validation(format!(
                "unsupported file-drop schema_version {:?}",
                self.schema_version
            ))
            .with_help(format!(
                "Use schema_version {FILE_DROP_FRAME_SCHEMA_VERSION:?}."
            )));
        }
        validate_file_drop_id(&self.id)?;
        AdapterExternalIdentity::new(self.source.source_id.clone())?;
        if self.source.resume_name.trim().is_empty() {
            return Err(MissiveError::validation(
                "file-drop source.resume_name cannot be empty",
            ));
        }
        if let Some(command) = self.command.to_stdio_command() {
            self.to_stdio_frame_with_command(command)?.validate()?;
        } else {
            self.validate_file_drop_command()?;
        }
        Ok(())
    }

    /// Converts this request into a stdio frame for foreground send/stream/task execution.
    pub fn to_stdio_frame(&self) -> Result<StdioInputFrame> {
        let command = self.command.to_stdio_command().ok_or_else(|| {
            MissiveError::validation(format!(
                "file-drop command {:?} cannot be executed as a foreground stdio command",
                self.command.as_str()
            ))
        })?;
        self.to_stdio_frame_with_command(command)
    }

    fn to_stdio_frame_with_command(&self, command: StdioCommand) -> Result<StdioInputFrame> {
        Ok(StdioInputFrame {
            schema_version: STDIO_FRAME_SCHEMA_VERSION.to_owned(),
            id: self.id.clone(),
            source: self.source.clone().into(),
            command,
            metadata: self.metadata.clone(),
        })
    }

    fn validate_file_drop_command(&self) -> Result<()> {
        match &self.command {
            FileDropCommand::JobStartSend(command) => {
                self.to_stdio_frame_with_command(StdioCommand::Send(command.message.clone()))?
                    .validate()?;
                validate_job_options(&command.options)
            }
            FileDropCommand::JobStartStream(command) => {
                self.to_stdio_frame_with_command(StdioCommand::Stream(command.stream.clone()))?
                    .validate()?;
                validate_job_options(&command.options)
            }
            FileDropCommand::JobStartWait(command) => {
                self.to_stdio_frame_with_command(StdioCommand::TaskWait(command.wait.clone()))?
                    .validate()?;
                validate_job_options(&command.options)
            }
            FileDropCommand::JobStartReduce(command) => {
                missive_core::GroupName::new(command.group.clone())?;
                ContextId::new(command.context.clone())?;
                validate_short("strategy", &command.strategy)?;
                validate_job_options(&command.options)
            }
            FileDropCommand::JobList(command) => {
                validate_optional_short("kind", command.kind.as_deref())?;
                validate_optional_short("state", command.state.as_deref())?;
                if let Some(agent) = &command.agent {
                    AgentAlias::new(agent.clone())?;
                }
                if let Some(context) = &command.context {
                    ContextId::new(context.clone())?;
                }
                if command.limit.is_some_and(|limit| limit == 0) {
                    return Err(MissiveError::validation(
                        "file-drop job_list limit must be greater than zero",
                    ));
                }
                Ok(())
            }
            FileDropCommand::JobShow(command) => validate_short("job_id", &command.job_id),
            FileDropCommand::JobCancel(command) => validate_short("job_id", &command.job_id),
            FileDropCommand::Send(_)
            | FileDropCommand::Stream(_)
            | FileDropCommand::TaskGet(_)
            | FileDropCommand::TaskList(_)
            | FileDropCommand::TaskWait(_)
            | FileDropCommand::TaskCancel(_) => {
                unreachable!("stdio commands are validated earlier")
            }
        }
    }
}

/// One file-drop result file written to the outbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDropOutputFile {
    /// Stable file-drop schema marker.
    pub schema_version: String,
    /// Request/correlation id when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Whether the file was parsed and handled successfully.
    pub ok: bool,
    /// Stable output kind.
    pub kind: String,
    /// Original ready filename from the inbox.
    pub input_file: String,
    /// Final archived input path relative to the configured processed/error directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_file: Option<String>,
    /// Wrapped command output frames when successful.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<StdioOutputFrame>,
    /// Structured error when `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorReport>,
}

impl FileDropOutputFile {
    /// Creates one successful result file.
    #[must_use]
    pub fn success(
        id: impl Into<String>,
        input_file: impl Into<String>,
        archived_file: impl Into<String>,
        outputs: Vec<StdioOutputFrame>,
    ) -> Self {
        Self {
            schema_version: FILE_DROP_FRAME_SCHEMA_VERSION.to_owned(),
            id: Some(id.into()),
            ok: true,
            kind: FILE_DROP_OUTPUT_KIND_RESULT.to_owned(),
            input_file: input_file.into(),
            archived_file: Some(archived_file.into()),
            outputs,
            error: None,
        }
    }

    /// Creates one error result file.
    #[must_use]
    pub fn error(
        id: Option<String>,
        input_file: impl Into<String>,
        archived_file: impl Into<String>,
        error: &MissiveError,
    ) -> Self {
        Self {
            schema_version: FILE_DROP_FRAME_SCHEMA_VERSION.to_owned(),
            id,
            ok: false,
            kind: FILE_DROP_OUTPUT_KIND_ERROR.to_owned(),
            input_file: input_file.into(),
            archived_file: Some(archived_file.into()),
            outputs: Vec::new(),
            error: Some(error.to_report()),
        }
    }
}

/// Directory set used by a file-drop adapter instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDropPaths {
    /// Directory watched for ready request files.
    pub inbox: PathBuf,
    /// Directory receiving result files.
    pub outbox: PathBuf,
    /// Directory receiving successfully processed input files.
    pub processed: PathBuf,
    /// Directory receiving invalid or failed input files.
    pub error: PathBuf,
}

impl FileDropPaths {
    /// Creates a file-drop path set.
    #[must_use]
    pub fn new(
        inbox: impl Into<PathBuf>,
        outbox: impl Into<PathBuf>,
        processed: impl Into<PathBuf>,
        error: impl Into<PathBuf>,
    ) -> Self {
        Self {
            inbox: inbox.into(),
            outbox: outbox.into(),
            processed: processed.into(),
            error: error.into(),
        }
    }

    /// Builds path settings from adapter definition metadata when all required keys are present.
    pub fn from_definition_settings(definition: &AdapterDefinition) -> Result<Option<Self>> {
        let Some(inbox) = definition.settings.get_str("inbox") else {
            return Ok(None);
        };
        let outbox = definition.settings.get_str("outbox").ok_or_else(|| {
            MissiveError::config(format!(
                "file-drop adapter {:?} settings include inbox but not outbox",
                definition.name
            ))
            .with_help("Set non-secret settings inbox, outbox, processed, and error paths.")
        })?;
        let processed = definition.settings.get_str("processed").ok_or_else(|| {
            MissiveError::config(format!(
                "file-drop adapter {:?} settings include inbox but not processed",
                definition.name
            ))
        })?;
        let error = definition.settings.get_str("error").ok_or_else(|| {
            MissiveError::config(format!(
                "file-drop adapter {:?} settings include inbox but not error",
                definition.name
            ))
        })?;
        Ok(Some(Self::new(inbox, outbox, processed, error)))
    }

    /// Ensures all file-drop directories exist.
    pub fn ensure_directories(&self) -> Result<()> {
        for path in [&self.inbox, &self.outbox, &self.processed, &self.error] {
            fs::create_dir_all(path).map_err(|error| {
                MissiveError::io(
                    format!("creating file-drop directory {}", path.display()),
                    error,
                )
            })?;
        }
        Ok(())
    }

    /// Lists ready `*.json` files in deterministic filename order.
    pub fn ready_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.inbox).map_err(|error| {
            MissiveError::io(
                format!("reading file-drop inbox {}", self.inbox.display()),
                error,
            )
        })? {
            let entry = entry.map_err(|error| {
                MissiveError::io(
                    format!("reading file-drop inbox entry in {}", self.inbox.display()),
                    error,
                )
            })?;
            let file_type = entry.file_type().map_err(|error| {
                MissiveError::io(
                    format!("reading file type for {}", entry.path().display()),
                    error,
                )
            })?;
            if !file_type.is_file() {
                continue;
            }
            if is_ready_file_name(&entry.file_name()) {
                files.push(entry.path());
            }
        }
        files.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
        Ok(files)
    }

    /// Atomically claims one ready inbox file by moving it to the processed directory under a hidden processing name.
    pub fn claim_ready_file(&self, source: &Path) -> Result<FileDropClaim> {
        let file_name = safe_file_name(source)?;
        if !is_ready_file_name(OsStr::new(&file_name)) {
            return Err(MissiveError::validation(format!(
                "file-drop input file {file_name:?} is not a ready *.json file"
            ))
            .with_help("Write to a temporary name first, then atomically rename to *.json."));
        }
        self.ensure_directories()?;
        let processing_name = format!(".{file_name}.processing-{}", next_unique_suffix());
        let claim_path = unique_path(&self.processed, &processing_name)?;
        fs::rename(source, &claim_path).map_err(|error| {
            MissiveError::io(
                format!(
                    "claiming file-drop input {} as {}",
                    source.display(),
                    claim_path.display()
                ),
                error,
            )
        })?;
        Ok(FileDropClaim {
            original_name: file_name,
            claim_path,
        })
    }

    /// Moves a claimed input into the processed directory with its original filename.
    pub fn complete_success(&self, claim: &FileDropClaim) -> Result<PathBuf> {
        self.complete_claim(claim, &self.processed)
    }

    /// Moves a claimed input into the error directory with its original filename.
    pub fn complete_error(&self, claim: &FileDropClaim) -> Result<PathBuf> {
        self.complete_claim(claim, &self.error)
    }

    fn complete_claim(&self, claim: &FileDropClaim, directory: &Path) -> Result<PathBuf> {
        let destination = unique_path(directory, &claim.original_name)?;
        fs::rename(&claim.claim_path, &destination).map_err(|error| {
            MissiveError::io(
                format!(
                    "archiving file-drop input {} to {}",
                    claim.claim_path.display(),
                    destination.display()
                ),
                error,
            )
        })?;
        Ok(destination)
    }

    /// Returns the outbox path for a result or error file corresponding to a claimed input.
    pub fn output_path_for_claim(&self, claim: &FileDropClaim, ok: bool) -> Result<PathBuf> {
        let stem = Path::new(&claim.original_name)
            .file_stem()
            .and_then(OsStr::to_str)
            .ok_or_else(|| {
                MissiveError::validation(format!(
                    "file-drop input file {:?} does not have a usable stem",
                    claim.original_name
                ))
            })?;
        let suffix = if ok { "result" } else { "error" };
        unique_path(&self.outbox, &format!("{stem}.{suffix}.json"))
    }

    /// Writes one JSON result file through a temporary file plus atomic rename.
    pub fn write_output_atomic(
        &self,
        destination: &Path,
        output: &FileDropOutputFile,
    ) -> Result<()> {
        self.ensure_directories()?;
        let temp = temp_output_path(destination)?;
        let bytes = serde_json::to_vec_pretty(output).map_err(|error| {
            MissiveError::orchestration("failed to encode file-drop output JSON").with_source(error)
        })?;
        fs::write(&temp, bytes).map_err(|error| {
            MissiveError::io(
                format!("writing temporary file-drop output {}", temp.display()),
                error,
            )
        })?;
        fs::rename(&temp, destination).map_err(|error| {
            let _ = fs::remove_file(&temp);
            MissiveError::io(
                format!(
                    "publishing file-drop output {} as {}",
                    temp.display(),
                    destination.display()
                ),
                error,
            )
        })
    }
}

/// A claimed file-drop input file that is no longer in the inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDropClaim {
    /// Original inbox filename.
    pub original_name: String,
    /// Hidden processing path currently holding the input.
    pub claim_path: PathBuf,
}

/// Minimal file-drop adapter implementation for the shared adapter registry.
#[derive(Debug, Clone)]
pub struct FileDropAdapter {
    definition: AdapterDefinition,
    paths: Option<FileDropPaths>,
    started: bool,
    stopped: bool,
    delivered_updates: Vec<AdapterOutboundUpdate>,
    acknowledgements: Vec<AdapterAcknowledgement>,
}

impl FileDropAdapter {
    /// Creates a file-drop adapter instance for one definition.
    pub fn new(definition: AdapterDefinition) -> Result<Self> {
        if definition.kind != FILE_DROP_ADAPTER_KIND {
            return Err(MissiveError::config(format!(
                "file-drop adapter cannot be created for adapter kind {:?}",
                definition.kind
            )));
        }
        let paths = FileDropPaths::from_definition_settings(&definition)?;
        Ok(Self {
            definition,
            paths,
            started: false,
            stopped: false,
            delivered_updates: Vec::new(),
            acknowledgements: Vec::new(),
        })
    }

    /// Returns configured path settings when provided through adapter metadata.
    #[must_use]
    pub const fn paths(&self) -> Option<&FileDropPaths> {
        self.paths.as_ref()
    }

    /// Maps one validated request file into a generic adapter inbound message.
    pub fn inbound_message_from_file(
        &self,
        file: &FileDropInputFile,
    ) -> Result<AdapterInboundMessage> {
        let external =
            AdapterExternalIdentity::new(file.source.source_id.clone()).map(|identity| {
                if let Some(display_name) = &file.source.display_name {
                    identity.with_display_name(display_name.clone())
                } else {
                    identity
                }
            })?;
        let identity = self.map_identity(external)?;
        let mut session = crate::AdapterSession::new(file.source.resume_name.clone())?;
        session.profile = file
            .source
            .profile
            .clone()
            .or_else(|| self.definition.session_profile.clone());
        session.target_agent = file
            .command
            .target_agent()
            .map(|agent| AgentAlias::new(agent.to_owned()))
            .transpose()?;
        session.context_id = file
            .command
            .context_id()
            .map(|context| ContextId::new(context.to_owned()))
            .transpose()?;
        let payload = inbound_payload_for_command(&file.command)?;
        let mut message = AdapterInboundMessage::new(
            self.definition.name.clone(),
            MessageId::new(format!("msg/file-drop/{}", file.id))?,
            identity,
            session,
            payload,
        )?;
        message.metadata = file.metadata.clone();
        message
            .metadata
            .insert_str("missive.file_drop.command", file.command.as_str())?;
        message
            .metadata
            .insert_str("missive.file_drop.frame_id", file.id.clone())?;
        Ok(message)
    }

    /// Emits one request file as an adapter inbound-message event through a running context.
    pub fn emit_file(&self, context: &AdapterContext, file: &FileDropInputFile) -> Result<()> {
        context.emit(AdapterEvent::inbound_message(
            self.inbound_message_from_file(file)?,
        ))
    }

    /// Delivered updates recorded by this in-process adapter instance.
    #[must_use]
    pub fn delivered_updates(&self) -> &[AdapterOutboundUpdate] {
        &self.delivered_updates
    }

    /// Acknowledgements recorded by this in-process adapter instance.
    #[must_use]
    pub fn acknowledgements(&self) -> &[AdapterAcknowledgement] {
        &self.acknowledgements
    }
}

impl Adapter for FileDropAdapter {
    fn definition(&self) -> &AdapterDefinition {
        &self.definition
    }

    fn start(&mut self, context: AdapterContext) -> Result<()> {
        if let Some(paths) = &self.paths {
            paths.ensure_directories()?;
        }
        context.emit(AdapterEvent::lifecycle(AdapterLifecycleEvent::new(
            context.definition(),
            AdapterLifecycleState::Running,
            "file-drop adapter ready to process inbox request files",
        )?))?;
        self.started = true;
        self.stopped = false;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.stopped = true;
        self.started = false;
        Ok(())
    }

    fn map_identity(&self, external: AdapterExternalIdentity) -> Result<AdapterIdentity> {
        AdapterIdentity::new(
            self.definition.name.clone(),
            FILE_DROP_ADAPTER_KIND,
            external.provider_user_id,
        )
        .map(|identity| {
            if let Some(display_name) = external.display_name {
                identity.with_display_name(display_name)
            } else {
                identity
            }
        })
    }

    fn deliver_update(&mut self, update: AdapterOutboundUpdate) -> Result<()> {
        if update.adapter_name != self.definition.name {
            return Err(MissiveError::validation(format!(
                "file-drop adapter {:?} cannot deliver update for adapter {:?}",
                self.definition.name, update.adapter_name
            )));
        }
        self.delivered_updates.push(update);
        Ok(())
    }

    fn acknowledge(&mut self, acknowledgement: AdapterAcknowledgement) -> Result<()> {
        if acknowledgement.adapter_name != self.definition.name {
            return Err(MissiveError::validation(format!(
                "file-drop adapter {:?} cannot acknowledge message for adapter {:?}",
                self.definition.name, acknowledgement.adapter_name
            )));
        }
        self.acknowledgements.push(acknowledgement);
        Ok(())
    }
}

/// Registers the built-in file-drop adapter factory in an adapter registry.
pub fn register_file_drop_adapter(registry: &mut crate::AdapterRegistry) -> Result<()> {
    registry.register_fn(FILE_DROP_ADAPTER_KIND, |definition| {
        Ok(Box::new(FileDropAdapter::new(definition)?))
    })
}

/// Returns whether an inbox filename is ready for processing.
#[must_use]
pub fn is_ready_file_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if name.starts_with('.') || name.ends_with('~') {
        return false;
    }
    if name.contains(".processing") {
        return false;
    }
    Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension == "json")
}

fn inbound_payload_for_command(command: &FileDropCommand) -> Result<AdapterInboundPayload> {
    match command {
        FileDropCommand::Send(message) => command_payload_from_message(command.as_str(), message),
        FileDropCommand::Stream(stream) => {
            command_payload_from_message(command.as_str(), &stream.message)
        }
        FileDropCommand::TaskGet(_)
        | FileDropCommand::TaskList(_)
        | FileDropCommand::TaskWait(_)
        | FileDropCommand::TaskCancel(_)
        | FileDropCommand::JobStartSend(_)
        | FileDropCommand::JobStartStream(_)
        | FileDropCommand::JobStartWait(_)
        | FileDropCommand::JobStartReduce(_)
        | FileDropCommand::JobList(_)
        | FileDropCommand::JobShow(_)
        | FileDropCommand::JobCancel(_) => Ok(AdapterInboundPayload::json(json!({
            "command": command.as_str(),
            "args": command,
        }))),
    }
}

fn command_payload_from_message(
    command_name: &str,
    message: &StdioMessageFields,
) -> Result<AdapterInboundPayload> {
    if let Some(text) = &message.message
        && message.text_parts.is_empty()
        && message.json_parts.is_empty()
        && message.files.is_empty()
        && message.file_bytes.is_empty()
    {
        return Ok(AdapterInboundPayload::text(text.clone()));
    }
    Ok(AdapterInboundPayload::json(json!({
        "command": command_name,
        "agent": message.agent,
        "message": message.message,
        "text_parts": message.text_parts,
        "json_parts": message.json_parts,
        "files": message.files,
        "file_bytes": message.file_bytes,
        "mime": message.mime,
        "context": message.context,
        "task": message.task,
        "accepted_output_modes": message.accepted_output_modes,
        "metadata": message.metadata,
    })))
}

fn validate_job_options(options: &FileDropJobOptions) -> Result<()> {
    if options.max_attempts == 0 {
        return Err(MissiveError::validation(
            "file-drop job option max_attempts must be greater than zero",
        ));
    }
    validate_optional_short("attach_timeout", options.attach_timeout.as_deref())
}

fn validate_file_drop_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.chars().any(char::is_whitespace)
        || value.chars().any(char::is_control)
    {
        return Err(MissiveError::validation(
            "file-drop id must be non-empty and cannot contain whitespace or control characters",
        ));
    }
    if value.len() > MAX_FILE_DROP_ID_BYTES {
        return Err(MissiveError::validation(format!(
            "file-drop id cannot exceed {MAX_FILE_DROP_ID_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_optional_short(field: &str, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        validate_short(field, value)?;
    }
    Ok(())
}

fn validate_short(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(MissiveError::validation(format!(
            "file-drop field {field} cannot be empty"
        )));
    }
    if value.len() > MAX_SHORT_FIELD_BYTES || value.chars().any(char::is_control) {
        return Err(MissiveError::validation(format!(
            "file-drop field {field} contains an invalid or too-long value"
        )));
    }
    Ok(())
}

fn safe_file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            MissiveError::validation(format!(
                "file-drop path {} does not have a safe UTF-8 filename",
                path.display()
            ))
        })
}

fn unique_path(directory: &Path, desired_name: &str) -> Result<PathBuf> {
    fs::create_dir_all(directory).map_err(|error| {
        MissiveError::io(
            format!("creating file-drop directory {}", directory.display()),
            error,
        )
    })?;
    let candidate = directory.join(desired_name);
    if !candidate.exists() {
        return Ok(candidate);
    }

    let desired = Path::new(desired_name);
    let stem = desired
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or(desired_name);
    let extension = desired.extension().and_then(OsStr::to_str);
    for index in 1..=UNIQUE_ATTEMPTS {
        let name = match extension {
            Some(extension) => format!("{stem}-{index}.{extension}"),
            None => format!("{stem}-{index}"),
        };
        let candidate = directory.join(name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(MissiveError::io(
        format!(
            "choosing unique file-drop path for {} in {}",
            desired_name,
            directory.display()
        ),
        std::io::Error::new(std::io::ErrorKind::AlreadyExists, "too many collisions"),
    ))
}

fn temp_output_path(destination: &Path) -> Result<PathBuf> {
    let parent = destination.parent().ok_or_else(|| {
        MissiveError::validation(format!(
            "file-drop output path {} has no parent directory",
            destination.display()
        ))
    })?;
    let file_name = safe_file_name(destination)?;
    unique_path(
        parent,
        &format!(".{file_name}.tmp-{}", next_unique_suffix()),
    )
}

fn next_unique_suffix() -> String {
    let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{sequence}", std::process::id())
}

fn default_source_id() -> String {
    DEFAULT_SOURCE_ID.to_owned()
}

fn default_resume_name() -> String {
    DEFAULT_RESUME_NAME.to_owned()
}

fn default_file_drop_schema_version() -> String {
    FILE_DROP_FRAME_SCHEMA_VERSION.to_owned()
}

const fn default_max_attempts() -> u32 {
    1
}

fn default_reduce_strategy() -> String {
    "summarise".to_owned()
}

/// Returns a best-effort process-unique event id for file-drop lifecycle frames.
pub fn new_file_drop_event_id() -> Result<EventId> {
    EventId::new(format!("evt/file-drop/{}", next_unique_suffix()))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::{AdapterEventSink, AdapterRegistry};

    #[derive(Debug, Default)]
    struct RecordingSink {
        events: Mutex<Vec<AdapterEvent>>,
    }

    impl AdapterEventSink for RecordingSink {
        fn emit(&self, event: AdapterEvent) -> Result<()> {
            self.events.lock().expect("event mutex").push(event);
            Ok(())
        }
    }

    impl RecordingSink {
        fn events(&self) -> Vec<AdapterEvent> {
            self.events.lock().expect("event mutex").clone()
        }
    }

    #[test]
    fn valid_file_drop_send_maps_to_inbound_message() -> Result<()> {
        let request = FileDropInputFile::from_json_str(
            r#"{
                "schema_version":"missive.file_drop.v1",
                "id":"drop-1",
                "source":{"source_id":"agent-42","display_name":"Agent 42","resume_name":"work"},
                "command":"send",
                "agent":"echo",
                "message":"hello from file"
            }"#,
        )?;
        let definition = AdapterDefinition::new("drop", FILE_DROP_ADAPTER_KIND)?;
        let adapter = FileDropAdapter::new(definition)?;
        let message = adapter.inbound_message_from_file(&request)?;

        assert_eq!(message.adapter_name, "drop");
        assert_eq!(message.identity.source_id, "agent-42");
        assert_eq!(
            message
                .session
                .target_agent
                .as_ref()
                .map(AgentAlias::as_str),
            Some("echo")
        );
        assert_eq!(
            message.metadata.get_str("missive.file_drop.command"),
            Some("send")
        );
        assert_eq!(
            message.payload,
            AdapterInboundPayload::text("hello from file")
        );
        Ok(())
    }

    #[test]
    fn job_start_send_file_validates_and_preserves_options() -> Result<()> {
        let request = FileDropInputFile::from_value(json!({
            "schema_version": "missive.file_drop.v1",
            "id": "job-1",
            "command": "job_start_send",
            "agent": "echo",
            "message": "run in background",
            "options": {"max_attempts": 2, "cancel_remote_on_cancel": true}
        }))?;

        match request.command {
            FileDropCommand::JobStartSend(command) => {
                assert_eq!(command.message.agent, "echo");
                assert_eq!(command.options.max_attempts, 2);
                assert!(command.options.cancel_remote_on_cancel);
            }
            other => panic!("unexpected command {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn path_handoff_ignores_tmp_and_archives_ready_files() -> Result<()> {
        let temp = tempdir().expect("tempdir");
        let paths = FileDropPaths::new(
            temp.path().join("inbox"),
            temp.path().join("outbox"),
            temp.path().join("processed"),
            temp.path().join("error"),
        );
        paths.ensure_directories()?;
        fs::write(paths.inbox.join("partial.tmp"), "not ready").expect("partial");
        fs::write(paths.inbox.join("req.json"), "{}").expect("ready");

        let ready = paths.ready_files()?;
        assert_eq!(ready, vec![paths.inbox.join("req.json")]);
        let claim = paths.claim_ready_file(&ready[0])?;
        assert!(!paths.inbox.join("req.json").exists());
        assert!(claim.claim_path.exists());
        let archived = paths.complete_success(&claim)?;

        assert_eq!(
            archived.file_name().and_then(OsStr::to_str),
            Some("req.json")
        );
        assert!(archived.exists());
        assert!(paths.inbox.join("partial.tmp").exists());
        Ok(())
    }

    #[test]
    fn registry_can_create_file_drop_adapter_and_emit_file() -> Result<()> {
        let mut registry = AdapterRegistry::new();
        register_file_drop_adapter(&mut registry)?;
        let definition = AdapterDefinition::new("drop", FILE_DROP_ADAPTER_KIND)?;
        let mut adapter = registry.create(&definition)?;
        let sink = Arc::new(RecordingSink::default());
        let context = AdapterContext::new(definition.clone(), sink.clone());
        adapter.start(context.clone())?;

        let request = FileDropInputFile::from_value(json!({
            "id": "drop-list",
            "command": "task_list",
            "source": {"source_id": "runner"}
        }))?;
        let drop_adapter = FileDropAdapter::new(definition)?;
        drop_adapter.emit_file(&context, &request)?;

        let events = sink.events();
        assert!(matches!(events[0], AdapterEvent::Lifecycle(_)));
        assert!(matches!(events[1], AdapterEvent::InboundMessage(_)));
        Ok(())
    }
}
