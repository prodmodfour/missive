#![doc = "Command-line skeleton for missive."]

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand};
use missive_core::{MissiveError, MissiveExitCode, Result};

pub mod output;

pub use output::{
    CommandStatus, OUTPUT_SCHEMA_VERSION, OutputMode, REDACTED, redact_header, redact_headers,
    redact_json, redact_text, render_error, render_success,
};

/// Cargo package name for this crate.
pub const CRATE_NAME: &str = "missive-cli";

/// Installed command name for the CLI binary.
pub const BINARY_NAME: &str = missive_core::PROJECT_NAME;

/// Short description of this crate's target responsibility.
pub const CRATE_PURPOSE: &str = "command parsing, output rendering, and exit codes";

const REQUIRED_SUBCOMMANDS: [&str; 14] = [
    "agent",
    "send",
    "stream",
    "task",
    "context",
    "group",
    "gateway",
    "webhook",
    "push",
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
    long_about = "missive is a local control-plane CLI for A2A-native agent communication. This skeleton exposes the stable top-level command tree and global flags; operational subcommands are implemented by later tickets.",
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
    /// Manage configured A2A agents and cached Agent Cards.
    #[command(
        long_about = "Manage configured A2A agents, aliases, Agent Card discovery, and cached capability metadata. Registry operations are implemented by later agent tickets."
    )]
    Agent,

    /// Send one message to an A2A agent.
    #[command(
        long_about = "Send one message to an A2A agent and record the response or created task. Message execution is implemented by a later messaging ticket."
    )]
    Send,

    /// Stream message updates from an A2A agent.
    #[command(
        long_about = "Start an A2A streaming message exchange and render status or artifact events. Streaming execution is implemented by a later messaging ticket."
    )]
    Stream,

    /// Inspect, list, wait for, or cancel A2A tasks.
    #[command(
        long_about = "Inspect, list, wait for, and cancel A2A tasks while preserving local task state. Task operations are implemented by later task tickets."
    )]
    Task,

    /// Manage conversation contexts and session continuity.
    #[command(
        long_about = "Create, list, fork, close, and export A2A contexts used for conversation and task continuity. Context operations are implemented by a later context ticket."
    )]
    Context,

    /// Manage groups of agents for collective operations.
    #[command(
        long_about = "Manage named groups of agents for later broadcast, barrier, gather, reduce, and routing operations. Group persistence is implemented by later group tickets."
    )]
    Group,

    /// Run and manage the local missive gateway daemon.
    #[command(
        long_about = "Run and manage the local missive gateway daemon for subscriptions, webhooks, adapters, and background jobs. Gateway runtime behavior is implemented by later gateway tickets."
    )]
    Gateway,

    /// Receive A2A push notification callbacks locally.
    #[command(
        long_about = "Receive A2A push notification callbacks locally, validate payloads, and persist callback events. Webhook behavior is implemented by a later gateway ticket."
    )]
    Webhook,

    /// Manage A2A push notification configurations.
    #[command(
        long_about = "Create, inspect, list, and delete A2A push notification configurations for remote tasks. Push behavior is implemented by a later push ticket."
    )]
    Push,

    /// Diagnose local configuration, storage, gateway, and endpoint health.
    #[command(
        long_about = "Diagnose local configuration, storage, gateway status, tool availability, and A2A endpoint reachability. Diagnostic checks are implemented by a later observability ticket."
    )]
    Doctor,

    /// Inspect local missive logs.
    #[command(
        long_about = "Inspect local missive logs without exposing secrets. Log collection and filtering are implemented by a later observability ticket."
    )]
    Logs,

    /// Inspect, tail, replay, or export the local event journal.
    #[command(
        long_about = "Inspect, tail, replay, or export the local event journal in human, JSON, or NDJSON forms. Event persistence is implemented by a later events ticket."
    )]
    Events,

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
            Self::Agent => "agent",
            Self::Send => "send",
            Self::Stream => "stream",
            Self::Task => "task",
            Self::Context => "context",
            Self::Group => "group",
            Self::Gateway => "gateway",
            Self::Webhook => "webhook",
            Self::Push => "push",
            Self::Doctor => "doctor",
            Self::Logs => "logs",
            Self::Events => "events",
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
pub const fn required_subcommands() -> [&'static str; 14] {
    REQUIRED_SUBCOMMANDS
}

/// Executes an already parsed CLI skeleton command.
pub fn execute<W>(cli: &Cli, writer: &mut W) -> Result<()>
where
    W: Write,
{
    let mode = OutputMode::from_globals(&cli.globals)?;

    match &cli.command {
        Some(command) => {
            let status = CommandStatus::parsed(command.name());
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
            let status = CommandStatus::root_help_available();
            render_success(writer, mode, "command_status", &status, &status.message)
        }
    }
}

/// Runs the CLI using process arguments and standard streams.
#[must_use]
pub fn run() -> i32 {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    run_from(std::env::args_os(), &mut stdout, &mut stderr)
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
    match Cli::try_parse_from(args) {
        Ok(cli) => match execute(&cli, stdout) {
            Ok(()) => MissiveExitCode::Success.as_i32(),
            Err(error) => {
                let exit_code = error.exit_code().as_i32();
                if render_error(stderr, &cli.globals, &error).is_err() {
                    MissiveExitCode::Io.as_i32()
                } else {
                    exit_code
                }
            }
        },
        Err(error) => {
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
            "--trace",
            "--verbose",
            "--verbose",
        ])
        .expect("global flags should parse at subcommand scope");

        assert!(matches!(cli.command, Some(Commands::Agent)));
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
        assert!(cli.globals.trace);
        assert_eq!(cli.globals.verbose, 2);
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

        execute(&cli, &mut output).expect("help output should write");
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

        execute(&cli, &mut output).expect("quiet command should succeed");

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
