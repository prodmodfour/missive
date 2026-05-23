//! Gateway service manager integration.
//!
//! This module keeps platform-specific service file generation and supervisor
//! commands testable without starting the gateway.  Linux uses systemd unit
//! files and macOS uses launchd property lists.  Other platforms fail with a
//! clear diagnostic so callers can keep using `missive gateway run` under their
//! own supervisor.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use missive_core::{MissiveError, Result};
use serde::Serialize;

/// Default Linux systemd unit name for the gateway.
pub const DEFAULT_SYSTEMD_UNIT: &str = "missive-gateway.service";

/// Default macOS launchd label for the gateway.
pub const DEFAULT_LAUNCHD_LABEL: &str = "works.earendil.missive.gateway";

/// Conservative PATH captured into generated services when no PATH is supplied.
pub const DEFAULT_SERVICE_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

const DESCRIPTION: &str = "missive gateway daemon";
const DOC_URL: &str = "https://github.com/earendil-works/missive";
const CAPTURED_ENV_KEYS: [&str; 7] = [
    "HOME",
    "MISSIVE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "RUST_LOG",
];

/// Service manager action requested by the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayServiceAction {
    /// Generate and install the service file.
    Install,
    /// Start the service through the platform supervisor.
    Start,
    /// Stop the service through the platform supervisor.
    Stop,
    /// Query the service status.
    Status,
    /// Stop/unload and remove the installed service file.
    Uninstall,
}

impl GatewayServiceAction {
    /// Stable action name used in JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Status => "status",
            Self::Uninstall => "uninstall",
        }
    }
}

/// Service scope requested by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayServiceScope {
    /// Per-user service: systemd --user on Linux, LaunchAgent on macOS.
    User,
    /// System service: systemd system unit on Linux, LaunchDaemon on macOS.
    System,
}

impl GatewayServiceScope {
    /// Stable scope name used in output and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::System => "system",
        }
    }
}

/// Platform service manager supported by missive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayServicePlatform {
    /// Linux systemd unit files and `systemctl`.
    LinuxSystemd,
    /// macOS launchd plists and `launchctl`.
    MacosLaunchd,
}

impl GatewayServicePlatform {
    /// Detects the service platform for the current target OS.
    pub fn current() -> Result<Self> {
        if cfg!(target_os = "linux") {
            Ok(Self::LinuxSystemd)
        } else if cfg!(target_os = "macos") {
            Ok(Self::MacosLaunchd)
        } else {
            Err(MissiveError::validation(format!(
                "gateway service management is unsupported on {}",
                env::consts::OS
            ))
            .with_help(
                "missive can install services on Linux systemd and macOS launchd. On this platform, run `missive gateway run` under your existing supervisor.",
            ))
        }
    }

    /// Stable platform name used in output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LinuxSystemd => "linux",
            Self::MacosLaunchd => "macos",
        }
    }

    /// Stable manager name used in output.
    #[must_use]
    pub const fn manager(self) -> &'static str {
        match self {
            Self::LinuxSystemd => "systemd",
            Self::MacosLaunchd => "launchd",
        }
    }
}

/// Options used to build a platform service plan.
#[derive(Debug, Clone)]
pub struct GatewayServiceOptions {
    /// Selected action.
    pub action: GatewayServiceAction,
    /// User or system service scope.
    pub scope: GatewayServiceScope,
    /// Whether to print the plan without writing files or running supervisor commands.
    pub dry_run: bool,
    /// Whether installation may replace an existing generated service file.
    pub force: bool,
    /// Platform override for tests. Runtime callers normally leave this as `None`.
    pub platform: Option<GatewayServicePlatform>,
    /// Absolute path to the missive executable for service files.
    pub executable: Option<PathBuf>,
    /// Config path to pass to `missive gateway run`, if one was loaded.
    pub config_path: Option<PathBuf>,
    /// Profile passed to `missive gateway run`.
    pub profile: String,
    /// Non-secret environment captured into the generated service file.
    pub environment: BTreeMap<String, String>,
}

/// One external supervisor command planned or executed by a service action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatewayServiceCommand {
    /// Executable name.
    pub program: String,
    /// Command arguments.
    pub args: Vec<String>,
    /// Shell-like display string for docs and human output.
    pub display: String,
}

impl GatewayServiceCommand {
    fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        let program = program.into();
        let display = std::iter::once(program.clone())
            .chain(args.iter().cloned())
            .map(|part| shell_quote(&part))
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            program,
            args,
            display,
        }
    }
}

/// Result from running one supervisor command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatewayServiceCommandResult {
    /// Command that was executed.
    pub command: GatewayServiceCommand,
    /// Process exit code, when the process exited normally.
    pub status_code: Option<i32>,
    /// Whether the process exit status was successful.
    pub success: bool,
    /// Captured standard output, decoded lossily as UTF-8.
    pub stdout: String,
    /// Captured standard error, decoded lossily as UTF-8.
    pub stderr: String,
}

/// Service action plan and execution result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatewayServiceResult {
    /// Action name.
    pub action: String,
    /// Whether the action was a dry run.
    pub dry_run: bool,
    /// Target OS family.
    pub platform: String,
    /// Platform supervisor, for example `systemd` or `launchd`.
    pub manager: String,
    /// User or system service scope.
    pub scope: GatewayServiceScope,
    /// Service unit/plist file path.
    pub service_path: String,
    /// Systemd unit name or launchd label.
    pub service_name: String,
    /// launchd label when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launchd_label: Option<String>,
    /// Executable path embedded in an install plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    /// Arguments embedded after the executable in an install plan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<String>,
    /// Non-secret environment embedded in an install plan.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// Generated systemd unit or launchd plist. Present for install plans.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_file: Option<String>,
    /// Supervisor commands that would be or were executed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub planned_commands: Vec<GatewayServiceCommand>,
    /// Supervisor command results for non-dry-run actions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_results: Vec<GatewayServiceCommandResult>,
    /// Whether the service file was written during this action.
    pub file_written: bool,
    /// Whether the service file was removed during this action.
    pub file_removed: bool,
    /// Operator notes and follow-up commands.
    pub notes: Vec<String>,
    /// Human-readable summary.
    pub message: String,
}

/// Returns the allowlist of process environment variables service generation may capture.
#[must_use]
pub const fn captured_environment_keys() -> &'static [&'static str] {
    &CAPTURED_ENV_KEYS
}

/// Validates one environment variable intended for a service file.
pub fn validate_service_environment(name: &str, value: &str) -> Result<()> {
    validate_env_name(name)?;
    if is_sensitive_env_name(name) {
        return Err(MissiveError::validation(format!(
            "refusing to store sensitive-looking environment variable {name:?} in a service file"
        ))
        .with_help(
            "Use config auth refs backed by env/keyring for A2A credentials; do not bake tokens, cookies, passwords, or API keys into service files.",
        ));
    }
    if value.contains('\0') || value.contains('\n') || value.contains('\r') {
        return Err(MissiveError::validation(format!(
            "environment variable {name:?} contains unsupported control characters"
        ))
        .with_help("Use single-line UTF-8 environment values for service files."));
    }
    Ok(())
}

/// Executes a service action, or returns a dry-run plan without side effects.
pub fn execute_gateway_service_action(
    options: GatewayServiceOptions,
) -> Result<GatewayServiceResult> {
    let mut result = build_gateway_service_plan(&options)?;
    if options.dry_run {
        return Ok(result);
    }

    match options.action {
        GatewayServiceAction::Install => {
            let service_file = result.service_file.as_ref().ok_or_else(|| {
                MissiveError::orchestration("install plan did not include a service file")
            })?;
            write_service_file(Path::new(&result.service_path), service_file, options.force)?;
            result.file_written = true;
            result.command_results = run_commands(&result.planned_commands, true)?;
            result.message = format!(
                "Installed {} {} gateway service at {}",
                result.scope.as_str(),
                result.manager,
                result.service_path
            );
        }
        GatewayServiceAction::Start => {
            result.command_results = run_commands(&result.planned_commands, true)?;
            result.message = format!(
                "Started {} {} gateway service {}",
                result.scope.as_str(),
                result.manager,
                result.service_name
            );
        }
        GatewayServiceAction::Stop => {
            result.command_results = run_commands(&result.planned_commands, true)?;
            result.message = format!(
                "Stopped {} {} gateway service {}",
                result.scope.as_str(),
                result.manager,
                result.service_name
            );
        }
        GatewayServiceAction::Status => {
            result.command_results = run_commands(&result.planned_commands, false)?;
            result.message = format!(
                "Queried {} {} gateway service status for {}",
                result.scope.as_str(),
                result.manager,
                result.service_name
            );
        }
        GatewayServiceAction::Uninstall => {
            let daemon_reload_index = if result.manager == "systemd" {
                result.planned_commands.len().saturating_sub(1)
            } else {
                result.planned_commands.len()
            };
            let (before_remove, after_remove) =
                result.planned_commands.split_at(daemon_reload_index);
            result.command_results = run_commands(before_remove, true)?;
            let service_path = Path::new(&result.service_path);
            if service_path.exists() {
                fs::remove_file(service_path).map_err(|error| {
                    MissiveError::io(
                        format!("removing service file {}", service_path.display()),
                        error,
                    )
                    .with_help(
                        "Check permissions or rerun the command with appropriate privileges.",
                    )
                })?;
                result.file_removed = true;
            }
            result
                .command_results
                .extend(run_commands(after_remove, true)?);
            result.message = format!(
                "Uninstalled {} {} gateway service {}",
                result.scope.as_str(),
                result.manager,
                result.service_name
            );
        }
    }

    Ok(result)
}

/// Builds a service action plan without performing side effects.
pub fn build_gateway_service_plan(options: &GatewayServiceOptions) -> Result<GatewayServiceResult> {
    let platform = match options.platform {
        Some(platform) => platform,
        None => GatewayServicePlatform::current()?,
    };
    validate_options(options, platform)?;

    match platform {
        GatewayServicePlatform::LinuxSystemd => build_systemd_plan(options, platform),
        GatewayServicePlatform::MacosLaunchd => build_launchd_plan(options, platform),
    }
}

fn validate_options(
    options: &GatewayServiceOptions,
    platform: GatewayServicePlatform,
) -> Result<()> {
    if options.profile.trim().is_empty() {
        return Err(MissiveError::validation(
            "gateway service profile cannot be empty",
        ));
    }
    if matches!(options.action, GatewayServiceAction::Install) {
        let executable = options.executable.as_ref().ok_or_else(|| {
            MissiveError::validation("gateway service install requires an executable path")
                .with_help("Pass --bin $(command -v missive), or let missive use the current executable path.")
        })?;
        if !executable.is_absolute() {
            return Err(MissiveError::validation(format!(
                "gateway service executable path {} must be absolute",
                executable.display()
            ))
            .with_help(
                "Use an absolute path so the service manager does not depend on a login shell.",
            ));
        }
        if !options.dry_run && !executable.is_file() {
            return Err(MissiveError::validation(format!(
                "gateway service executable path {} does not exist or is not a file",
                executable.display()
            ))
            .with_help("Install the missive binary first or pass --bin with the installed path."));
        }
        for (name, value) in &options.environment {
            validate_service_environment(name, value)?;
        }
        if options.scope == GatewayServiceScope::System {
            require_system_scope_state_environment(&options.environment)?;
        }
    }

    if options.scope == GatewayServiceScope::System
        && platform == GatewayServicePlatform::MacosLaunchd
        && matches!(options.action, GatewayServiceAction::Install)
        && !options.environment.contains_key("MISSIVE_HOME")
    {
        return Err(MissiveError::validation(
            "macOS system LaunchDaemon installation requires MISSIVE_HOME",
        )
        .with_help(
            "Pass --env MISSIVE_HOME=/var/lib/missive or use the default user LaunchAgent.",
        ));
    }

    Ok(())
}

fn require_system_scope_state_environment(environment: &BTreeMap<String, String>) -> Result<()> {
    let Some(home) = environment.get("MISSIVE_HOME") else {
        return Err(MissiveError::validation(
            "--system gateway service installation requires MISSIVE_HOME in the generated environment",
        )
        .with_help(
            "Pass --env MISSIVE_HOME=/var/lib/missive (or another dedicated absolute runtime directory) so a system service does not accidentally write profile state under a user login directory or /root.",
        ));
    };
    if !Path::new(home).is_absolute() {
        return Err(MissiveError::validation(
            "--system gateway service MISSIVE_HOME must be an absolute path",
        )
        .with_help("Use a dedicated absolute directory such as /var/lib/missive."));
    }
    Ok(())
}

fn build_systemd_plan(
    options: &GatewayServiceOptions,
    platform: GatewayServicePlatform,
) -> Result<GatewayServiceResult> {
    let service_path = systemd_service_path(options.scope, &options.environment)?;
    let mut planned_commands = Vec::new();
    match options.action {
        GatewayServiceAction::Install => planned_commands.push(systemctl_command(
            options.scope,
            vec!["daemon-reload".to_owned()],
        )),
        GatewayServiceAction::Start => planned_commands.push(systemctl_command(
            options.scope,
            vec!["start".to_owned(), DEFAULT_SYSTEMD_UNIT.to_owned()],
        )),
        GatewayServiceAction::Stop => planned_commands.push(systemctl_command(
            options.scope,
            vec!["stop".to_owned(), DEFAULT_SYSTEMD_UNIT.to_owned()],
        )),
        GatewayServiceAction::Status => planned_commands.push(systemctl_command(
            options.scope,
            vec![
                "status".to_owned(),
                DEFAULT_SYSTEMD_UNIT.to_owned(),
                "--no-pager".to_owned(),
            ],
        )),
        GatewayServiceAction::Uninstall => {
            planned_commands.push(systemctl_command(
                options.scope,
                vec!["stop".to_owned(), DEFAULT_SYSTEMD_UNIT.to_owned()],
            ));
            planned_commands.push(systemctl_command(
                options.scope,
                vec!["daemon-reload".to_owned()],
            ));
        }
    }

    let (executable, arguments, environment, service_file) =
        if options.action == GatewayServiceAction::Install {
            let executable = options.executable.as_ref().expect("validated executable");
            let arguments = gateway_run_arguments(options);
            let service_file =
                render_systemd_unit(executable, &arguments, &options.environment, options.scope);
            (
                Some(executable.display().to_string()),
                arguments,
                options.environment.clone(),
                Some(service_file),
            )
        } else {
            (None, Vec::new(), BTreeMap::new(), None)
        };

    Ok(GatewayServiceResult {
        action: options.action.as_str().to_owned(),
        dry_run: options.dry_run,
        platform: platform.as_str().to_owned(),
        manager: platform.manager().to_owned(),
        scope: options.scope,
        service_path: service_path.display().to_string(),
        service_name: DEFAULT_SYSTEMD_UNIT.to_owned(),
        launchd_label: None,
        executable,
        arguments,
        environment,
        service_file,
        planned_commands,
        command_results: Vec::new(),
        file_written: false,
        file_removed: false,
        notes: systemd_notes(options.scope),
        message: dry_run_message(
            options.action,
            options.scope,
            platform,
            DEFAULT_SYSTEMD_UNIT,
        ),
    })
}

fn build_launchd_plan(
    options: &GatewayServiceOptions,
    platform: GatewayServicePlatform,
) -> Result<GatewayServiceResult> {
    let service_path = launchd_plist_path(options.scope, &options.environment)?;
    let mut planned_commands = Vec::new();
    match options.action {
        GatewayServiceAction::Install => {}
        GatewayServiceAction::Start => {
            let domain = launchd_domain(options.scope)?;
            planned_commands.push(GatewayServiceCommand::new(
                "launchctl",
                vec![
                    "bootstrap".to_owned(),
                    domain,
                    service_path.display().to_string(),
                ],
            ));
        }
        GatewayServiceAction::Stop => {
            let domain = launchd_domain(options.scope)?;
            planned_commands.push(GatewayServiceCommand::new(
                "launchctl",
                vec![
                    "bootout".to_owned(),
                    domain,
                    service_path.display().to_string(),
                ],
            ));
        }
        GatewayServiceAction::Status => {
            let domain = launchd_domain(options.scope)?;
            planned_commands.push(GatewayServiceCommand::new(
                "launchctl",
                vec![
                    "print".to_owned(),
                    format!("{domain}/{DEFAULT_LAUNCHD_LABEL}"),
                ],
            ));
        }
        GatewayServiceAction::Uninstall => {
            let domain = launchd_domain(options.scope)?;
            planned_commands.push(GatewayServiceCommand::new(
                "launchctl",
                vec![
                    "bootout".to_owned(),
                    domain,
                    service_path.display().to_string(),
                ],
            ));
        }
    }

    let (executable, arguments, environment, service_file) =
        if options.action == GatewayServiceAction::Install {
            let executable = options.executable.as_ref().expect("validated executable");
            let arguments = gateway_run_arguments(options);
            let service_file =
                render_launchd_plist(executable, &arguments, &options.environment, options.scope)?;
            (
                Some(executable.display().to_string()),
                arguments,
                options.environment.clone(),
                Some(service_file),
            )
        } else {
            (None, Vec::new(), BTreeMap::new(), None)
        };

    Ok(GatewayServiceResult {
        action: options.action.as_str().to_owned(),
        dry_run: options.dry_run,
        platform: platform.as_str().to_owned(),
        manager: platform.manager().to_owned(),
        scope: options.scope,
        service_path: service_path.display().to_string(),
        service_name: DEFAULT_LAUNCHD_LABEL.to_owned(),
        launchd_label: Some(DEFAULT_LAUNCHD_LABEL.to_owned()),
        executable,
        arguments,
        environment,
        service_file,
        planned_commands,
        command_results: Vec::new(),
        file_written: false,
        file_removed: false,
        notes: launchd_notes(options.scope),
        message: dry_run_message(
            options.action,
            options.scope,
            platform,
            DEFAULT_LAUNCHD_LABEL,
        ),
    })
}

fn gateway_run_arguments(options: &GatewayServiceOptions) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(config_path) = &options.config_path {
        args.push("--config".to_owned());
        args.push(config_path.display().to_string());
    }
    args.push("--profile".to_owned());
    args.push(options.profile.clone());
    args.push("gateway".to_owned());
    args.push("run".to_owned());
    args
}

fn systemd_service_path(
    scope: GatewayServiceScope,
    environment: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    Ok(match scope {
        GatewayServiceScope::System => {
            PathBuf::from("/etc/systemd/system").join(DEFAULT_SYSTEMD_UNIT)
        }
        GatewayServiceScope::User => {
            if let Some(config_home) = environment
                .get("XDG_CONFIG_HOME")
                .filter(|value| !value.is_empty())
            {
                Path::new(config_home)
                    .join("systemd")
                    .join("user")
                    .join(DEFAULT_SYSTEMD_UNIT)
            } else {
                let home = environment.get("HOME").ok_or_else(|| {
                    MissiveError::validation(
                        "gateway service install could not determine HOME for systemd user unit path",
                    )
                    .with_help("Set HOME or XDG_CONFIG_HOME, or use --system with an explicit MISSIVE_HOME.")
                })?;
                Path::new(home)
                    .join(".config")
                    .join("systemd")
                    .join("user")
                    .join(DEFAULT_SYSTEMD_UNIT)
            }
        }
    })
}

fn launchd_plist_path(
    scope: GatewayServiceScope,
    environment: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    Ok(match scope {
        GatewayServiceScope::System => {
            PathBuf::from("/Library/LaunchDaemons").join(format!("{DEFAULT_LAUNCHD_LABEL}.plist"))
        }
        GatewayServiceScope::User => {
            let home = environment.get("HOME").ok_or_else(|| {
                MissiveError::validation(
                    "gateway service install could not determine HOME for launchd user plist path",
                )
                .with_help("Set HOME or use --system with an explicit MISSIVE_HOME.")
            })?;
            Path::new(home)
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{DEFAULT_LAUNCHD_LABEL}.plist"))
        }
    })
}

fn launchd_log_dir(
    scope: GatewayServiceScope,
    environment: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    Ok(match scope {
        GatewayServiceScope::System => PathBuf::from("/var/log/missive"),
        GatewayServiceScope::User => {
            let home = environment.get("HOME").ok_or_else(|| {
                MissiveError::validation(
                    "gateway service install could not determine HOME for launchd logs",
                )
                .with_help("Set HOME or use --system with an explicit MISSIVE_HOME.")
            })?;
            Path::new(home).join("Library").join("Logs").join("missive")
        }
    })
}

fn launchd_domain(scope: GatewayServiceScope) -> Result<String> {
    Ok(match scope {
        GatewayServiceScope::System => "system".to_owned(),
        GatewayServiceScope::User => format!("gui/{}", current_uid()?),
    })
}

fn current_uid() -> Result<String> {
    if let Ok(uid) = env::var("UID") {
        if !uid.trim().is_empty() && uid.chars().all(|ch| ch.is_ascii_digit()) {
            return Ok(uid);
        }
    }
    let output = Command::new("id").arg("-u").output().map_err(|error| {
        MissiveError::io("resolving current user id with id -u", error)
            .with_help("Set UID in the environment or run launchd service commands from a normal user session.")
    })?;
    if !output.status.success() {
        return Err(MissiveError::orchestration(
            "id -u failed while resolving launchd user domain",
        )
        .with_help("Run launchd user service commands from a normal user session."));
    }
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if uid.is_empty() {
        return Err(MissiveError::orchestration("id -u returned an empty uid"));
    }
    Ok(uid)
}

fn render_systemd_unit(
    executable: &Path,
    arguments: &[String],
    environment: &BTreeMap<String, String>,
    scope: GatewayServiceScope,
) -> String {
    let wanted_by = match scope {
        GatewayServiceScope::User => "default.target",
        GatewayServiceScope::System => "multi-user.target",
    };
    let exec_start = std::iter::once(executable.display().to_string())
        .chain(arguments.iter().cloned())
        .map(|part| systemd_quote(&part))
        .collect::<Vec<_>>()
        .join(" ");

    let mut unit = String::new();
    unit.push_str("[Unit]\n");
    unit.push_str(&format!("Description={DESCRIPTION}\n"));
    unit.push_str(&format!("Documentation={DOC_URL}\n"));
    unit.push_str("After=network-online.target\n");
    unit.push_str("Wants=network-online.target\n\n");
    unit.push_str("[Service]\n");
    unit.push_str("Type=simple\n");
    unit.push_str(&format!("ExecStart={exec_start}\n"));
    for (name, value) in environment {
        unit.push_str(&format!(
            "Environment={}\n",
            systemd_quote(&format!("{name}={value}"))
        ));
    }
    unit.push_str("Restart=on-failure\n");
    unit.push_str("RestartSec=5s\n\n");
    unit.push_str("[Install]\n");
    unit.push_str(&format!("WantedBy={wanted_by}\n"));
    unit
}

fn render_launchd_plist(
    executable: &Path,
    arguments: &[String],
    environment: &BTreeMap<String, String>,
    scope: GatewayServiceScope,
) -> Result<String> {
    let log_dir = launchd_log_dir(scope, environment)?;
    let stdout_path = log_dir.join("missive-gateway.stdout.log");
    let stderr_path = log_dir.join("missive-gateway.stderr.log");

    let mut plist = String::new();
    plist.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    plist.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    plist.push_str("<plist version=\"1.0\">\n");
    plist.push_str("<dict>\n");
    push_plist_key_string(&mut plist, 1, "Label", DEFAULT_LAUNCHD_LABEL);
    plist.push_str("  <key>ProgramArguments</key>\n");
    plist.push_str("  <array>\n");
    push_plist_string(&mut plist, 2, &executable.display().to_string());
    for arg in arguments {
        push_plist_string(&mut plist, 2, arg);
    }
    plist.push_str("  </array>\n");
    if !environment.is_empty() {
        plist.push_str("  <key>EnvironmentVariables</key>\n");
        plist.push_str("  <dict>\n");
        for (name, value) in environment {
            push_plist_key_string(&mut plist, 2, name, value);
        }
        plist.push_str("  </dict>\n");
    }
    plist.push_str("  <key>KeepAlive</key>\n");
    plist.push_str("  <true/>\n");
    plist.push_str("  <key>RunAtLoad</key>\n");
    plist.push_str("  <true/>\n");
    push_plist_key_string(
        &mut plist,
        1,
        "StandardOutPath",
        &stdout_path.display().to_string(),
    );
    push_plist_key_string(
        &mut plist,
        1,
        "StandardErrorPath",
        &stderr_path.display().to_string(),
    );
    plist.push_str("</dict>\n");
    plist.push_str("</plist>\n");
    Ok(plist)
}

fn push_plist_key_string(output: &mut String, indent: usize, key: &str, value: &str) {
    let spaces = "  ".repeat(indent);
    output.push_str(&format!("{spaces}<key>{}</key>\n", xml_escape(key)));
    output.push_str(&format!("{spaces}<string>{}</string>\n", xml_escape(value)));
}

fn push_plist_string(output: &mut String, indent: usize, value: &str) {
    let spaces = "  ".repeat(indent);
    output.push_str(&format!("{spaces}<string>{}</string>\n", xml_escape(value)));
}

fn write_service_file(path: &Path, contents: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        return Err(MissiveError::validation(format!(
            "service file {} already exists",
            path.display()
        ))
        .with_help(
            "Pass --force to replace the existing generated service file after reviewing it.",
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            MissiveError::io(
                format!("creating service directory {}", parent.display()),
                error,
            )
            .with_help("Check service manager directory permissions.")
        })?;
    }
    fs::write(path, contents).map_err(|error| {
        MissiveError::io(format!("writing service file {}", path.display()), error)
            .with_help("Check permissions or rerun the command with appropriate privileges.")
    })
}

fn run_commands(
    commands: &[GatewayServiceCommand],
    fail_on_nonzero: bool,
) -> Result<Vec<GatewayServiceCommandResult>> {
    let mut results = Vec::new();
    for command in commands {
        let output = Command::new(&command.program)
            .args(&command.args)
            .output()
            .map_err(|error| {
                MissiveError::io(format!("running {}", command.display), error).with_help(
                    "Ensure the platform service manager is installed and available on PATH, or rerun with --dry-run to inspect the planned action.",
                )
            })?;
        let result = GatewayServiceCommandResult {
            command: command.clone(),
            status_code: output.status.code(),
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        };
        if fail_on_nonzero && !result.success {
            return Err(MissiveError::orchestration(format!(
                "service command failed: {}",
                result.command.display
            ))
            .with_help(format!(
                "Command exited with {:?}. stdout: {:?}; stderr: {:?}",
                result.status_code, result.stdout, result.stderr
            )));
        }
        results.push(result);
    }
    Ok(results)
}

fn systemctl_command(scope: GatewayServiceScope, mut args: Vec<String>) -> GatewayServiceCommand {
    let mut full_args = Vec::new();
    if scope == GatewayServiceScope::User {
        full_args.push("--user".to_owned());
    }
    full_args.append(&mut args);
    GatewayServiceCommand::new("systemctl", full_args)
}

fn systemd_notes(scope: GatewayServiceScope) -> Vec<String> {
    let mut notes = vec![
        "Inspect logs with: journalctl --user -u missive-gateway.service -f".to_owned(),
        "Check status with: systemctl --user status missive-gateway.service --no-pager".to_owned(),
    ];
    if scope == GatewayServiceScope::System {
        notes = vec![
            "System service commands may require sudo or root privileges.".to_owned(),
            "Inspect logs with: journalctl -u missive-gateway.service -f".to_owned(),
            "Check status with: systemctl status missive-gateway.service --no-pager".to_owned(),
        ];
    }
    notes
}

fn launchd_notes(scope: GatewayServiceScope) -> Vec<String> {
    match scope {
        GatewayServiceScope::User => vec![
            "Inspect unified logs with: log stream --predicate 'process == \"missive\"' --style compact".to_owned(),
            format!("Inspect launchd status with: launchctl print gui/$(id -u)/{DEFAULT_LAUNCHD_LABEL}"),
            "stdout/stderr are written under ~/Library/Logs/missive/ by the generated plist.".to_owned(),
        ],
        GatewayServiceScope::System => vec![
            "System LaunchDaemon commands may require sudo or root privileges.".to_owned(),
            "Inspect unified logs with: log stream --predicate 'process == \"missive\"' --style compact".to_owned(),
            format!("Inspect launchd status with: launchctl print system/{DEFAULT_LAUNCHD_LABEL}"),
            "stdout/stderr are written under /var/log/missive/ by the generated plist.".to_owned(),
        ],
    }
}

fn dry_run_message(
    action: GatewayServiceAction,
    scope: GatewayServiceScope,
    platform: GatewayServicePlatform,
    service_name: &str,
) -> String {
    let verb = if action == GatewayServiceAction::Status {
        "Prepared"
    } else {
        "Would"
    };
    format!(
        "{verb} {} {} gateway service action '{}' for {}",
        scope.as_str(),
        platform.manager(),
        action.as_str(),
        service_name
    )
}

fn validate_env_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(MissiveError::validation(
            "service environment variable name cannot be empty",
        ));
    }
    let mut chars = name.chars();
    let first = chars.next().expect("non-empty env name");
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return Err(MissiveError::validation(format!(
            "service environment variable name {name:?} must start with a letter or underscore"
        )));
    }
    if !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
        return Err(MissiveError::validation(format!(
            "service environment variable name {name:?} must contain only ASCII letters, digits, and underscores"
        )));
    }
    Ok(())
}

fn is_sensitive_env_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("passwd")
        || lower.contains("credential")
        || lower.contains("authorization")
        || lower.contains("cookie")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("private_key")
}

fn systemd_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn shell_quote(value: &str) -> String {
    if value.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '=' | '+')
    }) {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    fn base_environment() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("HOME".to_owned(), "/home/example".to_owned()),
            ("PATH".to_owned(), DEFAULT_SERVICE_PATH.to_owned()),
            ("MISSIVE_HOME".to_owned(), "/tmp/missive-home".to_owned()),
        ])
    }

    #[cfg(not(windows))]
    fn install_options(platform: GatewayServicePlatform) -> GatewayServiceOptions {
        GatewayServiceOptions {
            action: GatewayServiceAction::Install,
            scope: GatewayServiceScope::User,
            dry_run: true,
            force: false,
            platform: Some(platform),
            executable: Some(PathBuf::from("/usr/local/bin/missive")),
            config_path: Some(PathBuf::from("/home/example/.config/missive/config.toml")),
            profile: "default".to_owned(),
            environment: base_environment(),
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn systemd_dry_run_generates_unit_file() {
        let result =
            build_gateway_service_plan(&install_options(GatewayServicePlatform::LinuxSystemd))
                .expect("systemd plan");

        assert_eq!(result.manager, "systemd");
        assert_eq!(result.service_name, DEFAULT_SYSTEMD_UNIT);
        assert_eq!(
            result.service_path,
            "/home/example/.config/systemd/user/missive-gateway.service"
        );
        let unit = result.service_file.expect("unit file");
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("ExecStart=\"/usr/local/bin/missive\""));
        assert!(unit.contains("\"--profile\" \"default\" \"gateway\" \"run\""));
        assert!(unit.contains("Environment=\"PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin\""));
        assert!(unit.contains("Environment=\"MISSIVE_HOME=/tmp/missive-home\""));
        assert_eq!(
            result.planned_commands[0].display,
            "systemctl --user daemon-reload"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn launchd_dry_run_generates_plist() {
        let result =
            build_gateway_service_plan(&install_options(GatewayServicePlatform::MacosLaunchd))
                .expect("launchd plan");

        assert_eq!(result.manager, "launchd");
        assert_eq!(result.service_name, DEFAULT_LAUNCHD_LABEL);
        assert_eq!(
            result.service_path,
            "/home/example/Library/LaunchAgents/works.earendil.missive.gateway.plist"
        );
        let plist = result.service_file.expect("plist");
        assert!(plist.contains("<key>Label</key>"));
        assert!(plist.contains(DEFAULT_LAUNCHD_LABEL));
        assert!(plist.contains("<string>/usr/local/bin/missive</string>"));
        assert!(plist.contains("<key>EnvironmentVariables</key>"));
        assert!(plist.contains("<key>MISSIVE_HOME</key>"));
    }

    #[cfg(not(windows))]
    #[test]
    fn system_install_requires_explicit_missive_home() {
        let mut options = install_options(GatewayServicePlatform::LinuxSystemd);
        options.scope = GatewayServiceScope::System;
        options.environment.remove("MISSIVE_HOME");

        let error = build_gateway_service_plan(&options).expect_err("system scope should fail");
        assert!(error.message().contains("MISSIVE_HOME"));
    }

    #[test]
    fn service_environment_rejects_secret_names() {
        let error = validate_service_environment("API_TOKEN", "example")
            .expect_err("sensitive env should fail");
        assert!(error.message().contains("sensitive-looking"));
    }

    #[cfg(not(windows))]
    #[test]
    fn status_plan_does_not_need_executable() {
        let options = GatewayServiceOptions {
            action: GatewayServiceAction::Status,
            scope: GatewayServiceScope::User,
            dry_run: true,
            force: false,
            platform: Some(GatewayServicePlatform::LinuxSystemd),
            executable: None,
            config_path: None,
            profile: "default".to_owned(),
            environment: base_environment(),
        };
        let result = build_gateway_service_plan(&options).expect("status plan");

        assert_eq!(result.service_file, None);
        assert_eq!(
            result.planned_commands[0].display,
            "systemctl --user status missive-gateway.service --no-pager"
        );
    }

    #[cfg(windows)]
    #[test]
    fn service_platform_is_unsupported_on_windows() {
        let error =
            GatewayServicePlatform::current().expect_err("Windows service manager unsupported");
        assert!(error.message().contains("unsupported"));
        assert!(error.message().contains("windows"));
    }
}
