#![doc = "Command-line skeleton for missive."]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand};
use missive_a2a::ServiceParameters;
use missive_core::{ConfigDiscovery, LoadedConfig, MissiveError, MissiveExitCode, Result};
use tracing::field;

pub mod adapter;
pub mod agent;
pub(crate) mod artifact;
pub(crate) mod auth;
pub(crate) mod barrier;
pub(crate) mod bcast;
pub(crate) mod capabilities;
pub mod context;
pub mod events;
pub mod gateway;
pub(crate) mod gather;
pub mod group;
pub mod job;
pub mod logs;
pub mod output;
pub mod push;
pub(crate) mod reduce;
pub(crate) mod route;
pub mod send;
pub mod stream;
pub mod task;
pub mod webhook;

pub use output::{
    CommandStatus, ConfigLoadStatus, OUTPUT_SCHEMA_VERSION, OutputMode, REDACTED, redact_header,
    redact_headers, redact_json, redact_text, render_error, render_success,
};

/// Cargo package name for this crate.
pub const CRATE_NAME: &str = "missive-cli";

/// Installed command name for the CLI binary.
pub const BINARY_NAME: &str = missive_core::PROJECT_NAME;

/// Short description of this crate's target responsibility.
pub const CRATE_PURPOSE: &str = "command parsing, output rendering, and exit codes";

const REQUIRED_SUBCOMMANDS: [&str; 21] = [
    "adapter",
    "agent",
    "send",
    "stream",
    "task",
    "context",
    "group",
    "route",
    "bcast",
    "barrier",
    "gather",
    "reduce",
    "gateway",
    "webhook",
    "push",
    "job",
    "doctor",
    "logs",
    "events",
    "completion",
    "manpage",
];

/// Parsed `missive` command line.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "missive",
    bin_name = "missive",
    version,
    about = "Manage A2A-native agent communication from the terminal.",
    long_about = "missive is a local control-plane CLI for A2A-native agent communication. The command tree and global flags are stable, and operational behavior is being implemented ticket by ticket.",
    disable_help_subcommand = true,
    propagate_version = true,
    term_width = 100
)]
pub struct Cli {
    /// Flags accepted by every command.
    #[command(flatten)]
    pub globals: GlobalArgs,

    /// Command to run. With no command, missive prints the top-level help page.
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Global flags shared by all current and future `missive` commands.
#[derive(Debug, Clone, Default, Args)]
pub struct GlobalArgs {
    /// Emit a single JSON document for command output when the command supports it.
    #[arg(long, global = true, action = ArgAction::SetTrue, help_heading = "Global options")]
    pub json: bool,

    /// Emit newline-delimited JSON events when the command supports streaming output.
    #[arg(long, global = true, action = ArgAction::SetTrue, help_heading = "Global options")]
    pub ndjson: bool,

    /// Suppress non-error output.
    #[arg(short = 'q', long, global = true, action = ArgAction::SetTrue, help_heading = "Global options")]
    pub quiet: bool,

    /// Disable colored terminal output and diagnostics.
    #[arg(long, global = true, action = ArgAction::SetTrue, help_heading = "Global options")]
    pub no_color: bool,

    /// Read configuration from this file path.
    #[arg(
        long,
        value_name = "PATH",
        global = true,
        help_heading = "Global options"
    )]
    pub config: Option<PathBuf>,

    /// Select a named configuration profile.
    #[arg(
        long,
        value_name = "PROFILE",
        global = true,
        help_heading = "Global options"
    )]
    pub profile: Option<String>,

    /// Set an overall command timeout such as 30s, 2m, or 1h.
    #[arg(
        long,
        value_name = "DURATION",
        global = true,
        help_heading = "Global options"
    )]
    pub timeout: Option<String>,

    /// Override the A2A protocol version sent as the A2A-Version service parameter.
    #[arg(
        long = "protocol-version",
        value_name = "VERSION",
        global = true,
        help_heading = "Global options"
    )]
    pub protocol_version: Option<String>,

    /// Request an A2A extension through the A2A-Extensions service parameter; repeatable.
    #[arg(
        long = "a2a-extension",
        value_name = "EXTENSION",
        global = true,
        action = ArgAction::Append,
        help_heading = "Global options"
    )]
    pub a2a_extensions: Vec<String>,

    /// Add an arbitrary A2A service parameter as NAME=VALUE; repeatable.
    #[arg(
        long = "service-param",
        value_name = "NAME=VALUE",
        global = true,
        action = ArgAction::Append,
        help_heading = "Global options"
    )]
    pub service_params: Vec<String>,

    /// Read a bearer token from this environment variable and send it as Authorization.
    #[arg(
        long = "bearer-token-env",
        value_name = "ENV",
        global = true,
        help_heading = "Global options"
    )]
    pub bearer_token_env: Option<String>,

    /// Add an outbound HTTP header as Name:Value; repeatable and never persisted.
    #[arg(
        long = "header",
        value_name = "NAME:VALUE",
        global = true,
        action = ArgAction::Append,
        help_heading = "Global options"
    )]
    pub headers: Vec<String>,

    /// Enable trace-oriented diagnostics for this invocation.
    #[arg(long, global = true, action = ArgAction::SetTrue, help_heading = "Global options")]
    pub trace: bool,

    /// Increase human diagnostic verbosity; repeat for more detail.
    #[arg(short = 'v', long, global = true, action = ArgAction::Count, help_heading = "Global options")]
    pub verbose: u8,
}

/// Top-level command groups and leaf commands exposed by the CLI skeleton.
#[derive(Debug, Clone, Subcommand)]
pub enum Commands {
    /// Run local adapters for subprocess and gateway integration.
    #[command(
        long_about = "Run concrete local adapters that translate external or local frames into missive communication commands. The stdio adapter reads JSON/NDJSON request frames from stdin, while the file-drop adapter polls an inbox directory for complete request files and writes result files to an outbox for filesystem-based automation."
    )]
    Adapter {
        /// Adapter operation to run. With no operation, missive emits a parsed command status.
        #[command(subcommand)]
        command: Option<adapter::AdapterCommands>,
    },

    /// Manage configured A2A agents and cached Agent Cards.
    #[command(
        long_about = "Manage configured A2A agent aliases in the local SQLite registry, inspect public A2A Agent Cards, refresh the local Agent Card cache, and summarize public capabilities for selection."
    )]
    Agent {
        /// Agent registry operation to run. With no operation, missive emits a parsed command status.
        #[command(subcommand)]
        command: Option<agent::AgentCommands>,
    },

    /// Send one message to an A2A agent.
    #[command(
        long_about = "Send one non-streaming A2A message to a registered agent, persist the request/response linkage, and print the direct Message or created Task summary."
    )]
    Send(send::SendArgs),

    /// Stream message updates from an A2A agent.
    #[command(
        long_about = "Start an A2A SendStreamingMessage exchange, render status/artifact updates as they arrive, and persist streaming events locally."
    )]
    Stream(stream::StreamArgs),

    /// Inspect, list, wait for, or cancel A2A tasks.
    #[command(
        long_about = "Inspect local A2A task state, refresh/list tasks from a remote agent, poll task state transitions, and request remote task cancellation."
    )]
    Task {
        /// Task operation to run. With no operation, missive emits a parsed command status.
        #[command(subcommand)]
        command: Option<task::TaskCommands>,
    },

    /// Manage conversation contexts and session continuity.
    #[command(
        long_about = "Create, list, show, fork, close, and export A2A contexts used for conversation and task continuity. Context ids are persisted in the local store and can be given human-friendly names."
    )]
    Context {
        /// Context operation to run. With no operation, missive emits a parsed command status.
        #[command(subcommand)]
        command: Option<context::ContextCommands>,
    },

    /// Manage groups of agents for collective operations.
    #[command(
        long_about = "Create, list, show, rename, delete, and summarize capabilities for profile-scoped groups, and add/remove registered agent members with rank names, tags, weights, and routing metadata for collective operations."
    )]
    Group {
        /// Group operation to run. With no operation, missive emits a parsed command status.
        #[command(subcommand)]
        command: Option<group::GroupCommands>,
    },

    /// Explain dry-run routing decisions for agents or groups.
    #[command(
        long_about = "Explain which registered agents a routing policy would select from an explicit candidate set or a stored group. This is a dry-run planning command: it reads local registry/group metadata and cached Agent Cards by default, and fetches Agent Cards only when --refresh-capabilities is requested."
    )]
    Route {
        /// Route operation to run. With no operation, missive emits a parsed command status.
        #[command(subcommand)]
        command: Option<route::RouteCommands>,
    },

    /// Broadcast one message to every member of a local group.
    #[command(
        long_about = "Send the same A2A SendMessage content to every registered member of a local group, create or reuse one shared context id, persist per-member message/task rows, and record broadcast collective events."
    )]
    Bcast(bcast::BcastArgs),

    /// Wait for group member tasks to reach terminal or requested states.
    #[command(
        long_about = "Wait for tasks associated with every registered member of a local group and one shared A2A context to reach terminal states or explicitly requested states, optionally consuming a previous bcast JSON result for task ids."
    )]
    Barrier(barrier::BarrierArgs),

    /// Gather latest local outputs and artifacts from group member tasks.
    #[command(
        long_about = "Collect the latest locally known task output and persisted artifacts for every registered member of a local group in one shared A2A context. Human output is markdown; --json and --ndjson provide machine-readable summaries, and --output-dir safely exports artifacts."
    )]
    Gather(gather::GatherArgs),

    /// Reduce gathered group outputs into one source-attributed result.
    #[command(
        long_about = "Reduce rank-ordered group member outputs from one shared A2A context into one final result. Reduction can run locally with deterministic summarise, vote, merge, rank, or custom-template strategies, call a registered reducer agent, or pipe a generated prompt through a local command."
    )]
    Reduce(reduce::ReduceArgs),

    /// Run and manage the local missive gateway daemon.
    #[command(
        long_about = "Run and manage the local missive gateway daemon for subscriptions, webhooks, adapters, background jobs, and optional OS service installation. The current daemon supervises the event bus, store access, health/status endpoints, the opt-in HTTP inbound adapter, and A2A task subscription/resume worker while service commands generate or call Linux systemd and macOS launchd supervision where supported."
    )]
    Gateway {
        /// Gateway operation to run. With no operation, missive emits a parsed command status.
        #[command(subcommand)]
        command: Option<gateway::GatewayCommands>,
    },

    /// Receive A2A push notification callbacks locally.
    #[command(
        long_about = "Receive A2A push notification callbacks locally over HTTP, validate A2A StreamResponse payloads, persist redacted callback events, optionally print NDJSON as callbacks arrive, and expose /healthz for local readiness checks. HTTPS/TLS should terminate in a trusted local tunnel or reverse proxy before forwarding to this listener."
    )]
    Webhook {
        /// Webhook operation to run. With no operation, missive emits a parsed command status.
        #[command(subcommand)]
        command: Option<webhook::WebhookCommands>,
    },

    /// Manage A2A push notification configurations.
    #[command(
        long_about = "Create, inspect, list, and delete A2A push notification configurations for remote tasks. Push commands call the selected A2A interface and persist redacted local records of configured callback endpoints."
    )]
    Push {
        /// Push notification config operation to run. With no operation, missive emits a parsed command status.
        #[command(subcommand)]
        command: Option<push::PushCommands>,
    },

    /// Enqueue, inspect, and cancel gateway-managed background communication jobs.
    #[command(
        long_about = "Enqueue send, stream, wait, and local reduce operations for the gateway daemon, list/show durable job rows, attach to a queued job until it completes, and cancel local or remote task-backed jobs."
    )]
    Job {
        /// Job operation to run. With no operation, missive emits a parsed command status.
        #[command(subcommand)]
        command: Option<job::JobCommands>,
    },

    /// Diagnose local configuration, storage, gateway, and endpoint health.
    #[command(
        long_about = "Diagnose local configuration, storage, gateway status, tool availability, and A2A endpoint reachability. Diagnostic checks are implemented by a later observability ticket."
    )]
    Doctor,

    /// Inspect local missive logs.
    #[command(
        long_about = "Inspect local missive diagnostic sources for the selected profile. Foreground command logs are written to stderr; this command inventories service-manager and event-journal sources and reads bounded records from profile-local log files when available."
    )]
    Logs(logs::LogsArgs),

    /// Inspect, tail, replay, or export the local event journal.
    #[command(
        long_about = "Inspect, tail, replay, or export the local event journal in human, JSON, or NDJSON forms. Event records are stored in the selected profile's SQLite database."
    )]
    Events {
        /// Event journal operation to run. With no operation, missive emits a parsed command status.
        #[command(subcommand)]
        command: Option<events::EventsCommands>,
    },

    /// Generate shell completion scripts.
    #[command(
        long_about = "Generate shell completion scripts for supported shells. Completion generation is implemented by a later CLI polish ticket."
    )]
    Completion,

    /// Generate manual pages.
    #[command(
        long_about = "Generate manual pages for the missive CLI. Manpage generation is implemented by a later CLI polish ticket."
    )]
    Manpage,
}

impl Commands {
    /// Returns the stable CLI spelling for this command.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Adapter { .. } => "adapter",
            Self::Agent { .. } => "agent",
            Self::Send(_) => "send",
            Self::Stream(_) => "stream",
            Self::Task { .. } => "task",
            Self::Context { .. } => "context",
            Self::Group { .. } => "group",
            Self::Route { .. } => "route",
            Self::Bcast(_) => "bcast",
            Self::Barrier(_) => "barrier",
            Self::Gather(_) => "gather",
            Self::Reduce(_) => "reduce",
            Self::Gateway { .. } => "gateway",
            Self::Webhook { .. } => "webhook",
            Self::Push { .. } => "push",
            Self::Job { .. } => "job",
            Self::Doctor => "doctor",
            Self::Logs(_) => "logs",
            Self::Events { .. } => "events",
            Self::Completion => "completion",
            Self::Manpage => "manpage",
        }
    }
}

/// Returns metadata for this crate.
#[must_use]
pub const fn crate_info() -> missive_core::CrateInfo {
    missive_core::CrateInfo::new(CRATE_NAME, CRATE_PURPOSE)
}

/// Returns the target workspace crate layout.
#[must_use]
pub fn workspace_crates() -> [missive_core::CrateInfo; 8] {
    [
        crate_info(),
        missive_core::crate_info(),
        missive_a2a::crate_info(),
        missive_store::crate_info(),
        missive_router::crate_info(),
        missive_gateway::crate_info(),
        missive_adapters::crate_info(),
        missive_observe::crate_info(),
    ]
}

/// Returns the required top-level subcommands for tests and documentation checks.
#[must_use]
pub const fn required_subcommands() -> [&'static str; 21] {
    REQUIRED_SUBCOMMANDS
}

/// Executes an already parsed CLI skeleton command.
pub fn execute<W>(cli: &Cli, writer: &mut W) -> Result<()>
where
    W: Write,
{
    let environment = process_environment();
    let current_dir = std::env::current_dir()
        .map_err(|error| MissiveError::io("reading current directory", error))?;

    let mut input = io::stdin().lock();
    execute_with_environment_and_input(cli, &environment, &current_dir, &mut input, writer)
}

/// Executes an already parsed CLI command with deterministic environment inputs.
pub fn execute_with_environment<W>(
    cli: &Cli,
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let mut input = io::empty();
    execute_with_environment_and_input(cli, environment, current_dir, &mut input, writer)
}

/// Executes an already parsed CLI command with deterministic environment,
/// current-directory, and standard-input inputs.
pub fn execute_with_environment_and_input<R, W>(
    cli: &Cli,
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
    input: &mut R,
    writer: &mut W,
) -> Result<()>
where
    R: Read,
    W: Write,
{
    let command_name = cli.command.as_ref().map(Commands::name).unwrap_or("help");
    let command_span = tracing::debug_span!(
        target: "missive_cli",
        "cli.command",
        command = %command_name,
        selected_profile = field::Empty,
        output_mode = field::Empty,
    );
    let _command_span_guard = command_span.enter();
    tracing::debug!(
        target: "missive_cli",
        command = %command_name,
        "CLI command started"
    );

    let loaded_config = load_config(&cli.globals, environment, current_dir)?;
    command_span.record("selected_profile", loaded_config.selected_profile.as_str());
    let mode = OutputMode::from_globals_and_config(&cli.globals, loaded_config.output_format()?)?;
    let mode_label = output_mode_label(mode);
    command_span.record("output_mode", mode_label);
    tracing::debug!(
        target: "missive_cli",
        command = %command_name,
        selected_profile = %loaded_config.selected_profile,
        output_mode = %mode_label,
        "CLI command configured"
    );

    let result = match &cli.command {
        Some(Commands::Adapter {
            command: Some(adapter_command),
        }) => adapter::execute_adapter_command(
            adapter_command,
            &cli.globals,
            &loaded_config,
            environment,
            input,
            writer,
        ),
        Some(Commands::Agent {
            command: Some(agent_command),
        }) => agent::execute_agent_command(
            agent_command,
            &cli.globals,
            &loaded_config,
            environment,
            mode,
            writer,
        ),
        Some(Commands::Send(args)) => send::execute_send_command(
            args,
            &cli.globals,
            &loaded_config,
            environment,
            mode,
            input,
            writer,
        ),
        Some(Commands::Stream(args)) => stream::execute_stream_command(
            args,
            &cli.globals,
            &loaded_config,
            environment,
            mode,
            input,
            writer,
        ),
        Some(Commands::Task {
            command: Some(task_command),
        }) => task::execute_task_command(
            task_command,
            &cli.globals,
            &loaded_config,
            environment,
            mode,
            writer,
        ),
        Some(Commands::Context {
            command: Some(context_command),
        }) => context::execute_context_command(
            context_command,
            &loaded_config,
            environment,
            mode,
            writer,
        ),
        Some(Commands::Events {
            command: Some(events_command),
        }) => events::execute_events_command(
            events_command,
            &cli.globals,
            &loaded_config,
            environment,
            mode,
            writer,
        ),
        Some(Commands::Group {
            command: Some(group_command),
        }) => group::execute_group_command(
            group_command,
            &cli.globals,
            &loaded_config,
            environment,
            mode,
            writer,
        ),
        Some(Commands::Route {
            command: Some(route_command),
        }) => route::execute_route_command(
            route_command,
            &cli.globals,
            &loaded_config,
            environment,
            mode,
            writer,
        ),
        Some(Commands::Bcast(args)) => bcast::execute_bcast_command(
            args,
            &cli.globals,
            &loaded_config,
            environment,
            mode,
            input,
            writer,
        ),
        Some(Commands::Barrier(args)) => barrier::execute_barrier_command(
            args,
            &cli.globals,
            &loaded_config,
            environment,
            mode,
            input,
            writer,
        ),
        Some(Commands::Gather(args)) => {
            gather::execute_gather_command(args, &loaded_config, environment, mode, writer)
        }
        Some(Commands::Reduce(args)) => reduce::execute_reduce_command(
            args,
            &cli.globals,
            &loaded_config,
            environment,
            mode,
            writer,
        ),
        Some(Commands::Gateway {
            command: Some(gateway_command),
        }) => gateway::execute_gateway_command(
            gateway_command,
            &cli.globals,
            &loaded_config,
            environment,
            mode,
            writer,
        ),
        Some(Commands::Push {
            command: Some(push_command),
        }) => push::execute_push_command(
            push_command,
            &cli.globals,
            &loaded_config,
            environment,
            mode,
            writer,
        ),
        Some(Commands::Webhook {
            command: Some(webhook_command),
        }) => webhook::execute_webhook_command(
            webhook_command,
            &cli.globals,
            &loaded_config,
            environment,
            mode,
            writer,
        ),
        Some(Commands::Job {
            command: Some(job_command),
        }) => job::execute_job_command(
            job_command,
            &cli.globals,
            &loaded_config,
            environment,
            mode,
            input,
            writer,
        ),
        Some(Commands::Logs(args)) => {
            logs::execute_logs_command(args, &loaded_config, environment, mode, writer)
        }
        Some(command) => {
            let status = CommandStatus::parsed(command.name()).with_config(&loaded_config);
            render_success(writer, mode, "command_status", &status, &status.message)
        }
        None if matches!(mode, OutputMode::Human) => {
            let mut command = Cli::command();
            command
                .write_long_help(writer)
                .map_err(|error| MissiveError::io("writing help output", error))?;
            writeln!(writer).map_err(|error| MissiveError::io("writing help output", error))
        }
        None => {
            let status = CommandStatus::root_help_available().with_config(&loaded_config);
            render_success(writer, mode, "command_status", &status, &status.message)
        }
    };

    match &result {
        Ok(()) => tracing::debug!(
            target: "missive_cli",
            command = %command_name,
            selected_profile = %loaded_config.selected_profile,
            output_mode = %mode_label,
            "CLI command completed"
        ),
        Err(error) => tracing::debug!(
            target: "missive_cli",
            command = %command_name,
            selected_profile = %loaded_config.selected_profile,
            output_mode = %mode_label,
            exit_code = error.exit_code().as_i32(),
            error_code = error.code(),
            error_category = ?error.category(),
            "CLI command failed"
        ),
    }

    result
}

fn load_config(
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
) -> Result<LoadedConfig> {
    ConfigDiscovery::new()
        .with_current_dir(current_dir.to_path_buf())
        .with_env(
            environment
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        )
        .with_explicit_path(globals.config.clone())
        .with_selected_profile(globals.profile.clone())
        .load()
}

pub(crate) fn service_parameters_from_config_and_globals(
    loaded_config: &LoadedConfig,
    globals: &GlobalArgs,
) -> Result<ServiceParameters> {
    let protocol = loaded_config.protocol_config()?;
    let mut protocol_version = protocol.protocol_version;
    if let Some(override_version) = &globals.protocol_version {
        protocol_version = override_version.clone();
    }

    let mut extensions = protocol.extensions;
    extensions.extend(globals.a2a_extensions.iter().cloned());

    let mut extra = protocol.service_parameters;
    for value in &globals.service_params {
        let (name, parameter_value) = split_global_key_value("--service-param", value)?;
        extra.insert(name.to_owned(), parameter_value.to_owned());
    }

    ServiceParameters::new(protocol_version, extensions, extra)
}

fn split_global_key_value<'a>(flag: &str, value: &'a str) -> Result<(&'a str, &'a str)> {
    let Some((key, raw_value)) = value.split_once('=') else {
        return Err(MissiveError::validation(format!(
            "{flag} value {value:?} must use NAME=VALUE syntax"
        )));
    };
    if key.is_empty() || raw_value.is_empty() {
        return Err(MissiveError::validation(format!(
            "{flag} value {value:?} must include a non-empty name and value"
        )));
    }
    Ok((key, raw_value))
}

fn process_environment() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

const fn output_mode_label(mode: OutputMode) -> &'static str {
    match mode {
        OutputMode::Human => "human",
        OutputMode::Json => "json",
        OutputMode::Ndjson => "ndjson",
        OutputMode::Quiet => "quiet",
    }
}

/// Builds the observability configuration from parsed global diagnostics flags.
pub fn diagnostics_config_from_globals(
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
) -> Result<missive_observe::ObserveConfig> {
    missive_observe::ObserveConfig::from_environment(
        environment,
        globals.trace,
        globals.verbose,
        globals.no_color,
    )
}

fn execute_with_diagnostics_and_environment<R, W>(
    cli: &Cli,
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
    input: &mut R,
    stdout: &mut W,
) -> Result<()>
where
    R: Read,
    W: Write,
{
    let diagnostics = diagnostics_config_from_globals(&cli.globals, environment)?;
    let bootstrap = diagnostics.clone();
    missive_observe::with_observer(diagnostics, || {
        missive_observe::emit_bootstrap_diagnostic(&bootstrap);
        execute_with_environment_and_input(cli, environment, current_dir, input, stdout)
    })?
}

/// Runs the CLI using process arguments and standard streams.
#[must_use]
pub fn run() -> i32 {
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    run_from_with_input(std::env::args_os(), &mut stdin, &mut stdout, &mut stderr)
}

/// Runs the CLI from an arbitrary argument iterator and writer pair.
///
/// This keeps clap exits testable without letting parse errors terminate the test process.
#[must_use]
pub fn run_from<I, T, W, E>(args: I, stdout: &mut W, stderr: &mut E) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
    W: Write,
    E: Write,
{
    let mut input = io::empty();
    run_from_with_input(args, &mut input, stdout, stderr)
}

fn run_from_with_input<I, T, R, W, E>(args: I, input: &mut R, stdout: &mut W, stderr: &mut E) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
    R: Read,
    W: Write,
    E: Write,
{
    match Cli::try_parse_from(args) {
        Ok(cli) => {
            let environment = process_environment();
            match std::env::current_dir()
                .map_err(|error| MissiveError::io("reading current directory", error))
                .and_then(|current_dir| {
                    execute_with_diagnostics_and_environment(
                        &cli,
                        &environment,
                        &current_dir,
                        input,
                        stdout,
                    )
                }) {
                Ok(()) => MissiveExitCode::Success.as_i32(),
                Err(error) => render_execution_error(stderr, &cli.globals, &error),
            }
        }
        Err(error) => render_clap_error(error, stdout, stderr),
    }
}

/// Runs the CLI from arguments with deterministic environment and current-directory inputs.
#[must_use]
pub fn run_from_with_environment<I, T, W, E>(
    args: I,
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
    stdout: &mut W,
    stderr: &mut E,
) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
    W: Write,
    E: Write,
{
    let mut input = io::empty();
    run_from_with_environment_and_input(args, environment, current_dir, &mut input, stdout, stderr)
}

/// Runs the CLI from arguments with deterministic environment, current-directory,
/// and standard-input inputs.
#[must_use]
pub fn run_from_with_environment_and_input<I, T, R, W, E>(
    args: I,
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
    input: &mut R,
    stdout: &mut W,
    stderr: &mut E,
) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
    R: Read,
    W: Write,
    E: Write,
{
    match Cli::try_parse_from(args) {
        Ok(cli) => match execute_with_diagnostics_and_environment(
            &cli,
            environment,
            current_dir,
            input,
            stdout,
        ) {
            Ok(()) => MissiveExitCode::Success.as_i32(),
            Err(error) => render_execution_error(stderr, &cli.globals, &error),
        },
        Err(error) => render_clap_error(error, stdout, stderr),
    }
}

fn render_execution_error<W>(stderr: &mut W, globals: &GlobalArgs, error: &MissiveError) -> i32
where
    W: Write,
{
    let exit_code = error.exit_code().as_i32();
    if render_error(stderr, globals, error).is_err() {
        MissiveExitCode::Io.as_i32()
    } else {
        exit_code
    }
}

fn render_clap_error<W, E>(error: clap::Error, stdout: &mut W, stderr: &mut E) -> i32
where
    W: Write,
    E: Write,
{
    let exit_code = error.exit_code();
    let rendered = error.to_string();
    let write_result = if error.use_stderr() {
        stderr.write_all(rendered.as_bytes())
    } else {
        stdout.write_all(rendered.as_bytes())
    };

    if write_result.is_err() {
        MissiveExitCode::Io.as_i32()
    } else {
        exit_code
    }
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    #[test]
    fn binary_name_matches_project_name() {
        assert_eq!(BINARY_NAME, "missive");
    }

    #[test]
    fn workspace_crates_cover_target_layout() {
        let names: Vec<_> = workspace_crates().iter().map(|info| info.name()).collect();

        assert_eq!(
            names,
            [
                "missive-cli",
                "missive-core",
                "missive-a2a",
                "missive-store",
                "missive-router",
                "missive-gateway",
                "missive-adapters",
                "missive-observe",
            ]
        );
    }

    #[test]
    fn clap_command_tree_contains_required_subcommands() {
        let command = Cli::command();
        command.clone().debug_assert();
        let names: Vec<_> = command
            .get_subcommands()
            .map(clap::Command::get_name)
            .collect();

        assert_eq!(names, required_subcommands());
    }

    #[test]
    fn global_flags_parse_after_subcommands() {
        let cli = Cli::try_parse_from([
            "missive",
            "agent",
            "--json",
            "--ndjson",
            "--quiet",
            "--no-color",
            "--config",
            "./missive.toml",
            "--profile",
            "dev",
            "--timeout",
            "30s",
            "--protocol-version",
            "1.0",
            "--a2a-extension",
            "urn:example:ext",
            "--service-param",
            "A2A-Trace=trace-1",
            "--bearer-token-env",
            "MISSIVE_EXAMPLE_TOKEN",
            "--header",
            "X-Request-Id:trace-1",
            "--trace",
            "--verbose",
            "--verbose",
        ])
        .expect("global flags should parse at subcommand scope");

        assert!(matches!(
            cli.command,
            Some(Commands::Agent { command: None })
        ));
        assert!(cli.globals.json);
        assert!(cli.globals.ndjson);
        assert!(cli.globals.quiet);
        assert!(cli.globals.no_color);
        assert_eq!(
            cli.globals.config.as_deref(),
            Some(std::path::Path::new("./missive.toml"))
        );
        assert_eq!(cli.globals.profile.as_deref(), Some("dev"));
        assert_eq!(cli.globals.timeout.as_deref(), Some("30s"));
        assert_eq!(cli.globals.protocol_version.as_deref(), Some("1.0"));
        assert_eq!(cli.globals.a2a_extensions, ["urn:example:ext"]);
        assert_eq!(cli.globals.service_params, ["A2A-Trace=trace-1"]);
        assert_eq!(
            cli.globals.bearer_token_env.as_deref(),
            Some("MISSIVE_EXAMPLE_TOKEN")
        );
        assert_eq!(cli.globals.headers, ["X-Request-Id:trace-1"]);
        assert!(cli.globals.trace);
        assert_eq!(cli.globals.verbose, 2);
    }

    #[test]
    fn diagnostics_config_respects_env_filter_and_global_flags() {
        let globals = GlobalArgs::default();
        let config = diagnostics_config_from_globals(&globals, &BTreeMap::new())
            .expect("default diagnostics config");
        assert_eq!(config.filter, missive_observe::DEFAULT_FILTER);
        assert_eq!(config.format, missive_observe::LogFormat::Human);

        let verbose = GlobalArgs {
            verbose: 2,
            ..GlobalArgs::default()
        };
        let config = diagnostics_config_from_globals(&verbose, &BTreeMap::new())
            .expect("verbose diagnostics config");
        assert_eq!(config.filter, "debug");

        let trace = GlobalArgs {
            trace: true,
            ..GlobalArgs::default()
        };
        let config = diagnostics_config_from_globals(&trace, &BTreeMap::new())
            .expect("trace diagnostics config");
        assert_eq!(config.filter, "trace");

        let environment = BTreeMap::from([
            ("RUST_LOG".to_owned(), "missive_cli=info".to_owned()),
            ("MISSIVE_LOG_FORMAT".to_owned(), "json".to_owned()),
        ]);
        let config =
            diagnostics_config_from_globals(&trace, &environment).expect("env diagnostics config");
        assert_eq!(config.filter, "missive_cli=info");
        assert_eq!(config.format, missive_observe::LogFormat::Json);
    }

    #[test]
    fn service_parameters_merge_config_with_cli_overrides() {
        let config = missive_core::MissiveConfig::from_toml_str(
            r#"
schema_version = "missive.config.v1"
default_profile = "default"

[profiles.default]

[protocol]
protocol_version = "1.0"
extensions = ["urn:example:config"]

[protocol.service_parameters]
A2A-Tenant = "tenant-a"
"#,
        )
        .expect("config");
        let loaded = LoadedConfig {
            config,
            source: missive_core::ConfigSource {
                kind: missive_core::ConfigSourceKind::BuiltInDefault,
                path: None,
            },
            selected_profile: "default".to_owned(),
        };
        let globals = GlobalArgs {
            protocol_version: Some("2.0".to_owned()),
            a2a_extensions: vec!["urn:example:cli".to_owned()],
            service_params: vec!["A2A-Trace=trace-1".to_owned()],
            ..GlobalArgs::default()
        };

        let parameters = service_parameters_from_config_and_globals(&loaded, &globals)
            .expect("service parameters");

        assert_eq!(parameters.protocol_version, "2.0");
        assert_eq!(
            parameters.extensions,
            vec![
                "urn:example:config".to_owned(),
                "urn:example:cli".to_owned()
            ]
        );
        assert_eq!(
            parameters.extra.get("A2A-Tenant").map(String::as_str),
            Some("tenant-a")
        );
        assert_eq!(
            parameters.extra.get("A2A-Trace").map(String::as_str),
            Some("trace-1")
        );
    }

    #[test]
    fn every_required_subcommand_has_help_page() {
        for name in required_subcommands() {
            let error = Cli::try_parse_from(["missive", name, "--help"])
                .expect_err("help should be reported through clap's display-help error");

            assert_eq!(error.kind(), ErrorKind::DisplayHelp);
            let help = error.to_string();
            assert!(help.contains(&format!("Usage: missive {name}")));
            assert!(help.contains("Global options"));
        }
    }

    #[test]
    fn no_command_prints_top_level_help() {
        let cli = Cli {
            globals: GlobalArgs::default(),
            command: None,
        };
        let mut output = Vec::new();

        execute_with_environment(&cli, &BTreeMap::new(), Path::new("."), &mut output)
            .expect("help output should write");
        let output = String::from_utf8(output).expect("help should be UTF-8");

        assert!(output.contains("Usage: missive"));
        assert!(output.contains("Commands:"));
    }

    #[test]
    fn quiet_command_suppresses_skeleton_status() {
        let cli = Cli {
            globals: GlobalArgs {
                quiet: true,
                ..GlobalArgs::default()
            },
            command: Some(Commands::Doctor),
        };
        let mut output = Vec::new();

        execute_with_environment(&cli, &BTreeMap::new(), Path::new("."), &mut output)
            .expect("quiet command should succeed");

        assert!(output.is_empty());
    }

    #[test]
    fn run_from_maps_help_to_success() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run_from(["missive", "--help"], &mut stdout, &mut stderr);

        assert_eq!(code, MissiveExitCode::Success.as_i32());
        assert!(stderr.is_empty());
        assert!(
            String::from_utf8(stdout)
                .expect("help should be UTF-8")
                .contains("Usage: missive")
        );
    }
}
