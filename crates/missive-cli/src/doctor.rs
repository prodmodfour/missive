//! Health checks for `missive doctor`.
//!
//! This module checks local binary/configuration/storage/tooling concerns plus
//! safe, non-mutating configured A2A endpoint discovery and local gateway status.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use missive_a2a::{AgentCardClient, AgentCardExt, AgentCardFetchOutcome, ServiceParameters};
use missive_core::config::AgentConfig;
use missive_core::{ConfigDiscovery, LoadedConfig, MissiveError, MissiveExitCode, Result};
use missive_gateway::daemon::DEFAULT_GATEWAY_STATUS_PATH;
use missive_store::{
    CURRENT_SCHEMA_VERSION, StatePathResolver, StatePathSource, StatePaths, applied_migrations,
    embedded_migrations, open_sqlite_database, schema_version,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::auth::auth_headers_for_config_agent;
use crate::output::{OutputMode, REDACTED, redact_text, render_success};
use crate::{BINARY_NAME, CRATE_NAME, GlobalArgs, service_parameters_from_config_and_globals};

/// Result metadata returned to the command dispatcher after rendering a doctor report.
#[derive(Debug, Clone)]
pub(crate) struct DoctorOutcome {
    pub(crate) selected_profile: String,
    pub(crate) output_mode: OutputMode,
    pub(crate) exit_code: MissiveExitCode,
    pub(crate) failure_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DoctorOutput {
    profile: String,
    scope: String,
    overall: DoctorOverall,
    checks: Vec<DoctorCheck>,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DoctorOverall {
    status: DoctorStatus,
    exit_code: u8,
    check_count: usize,
    passed: usize,
    warnings: usize,
    failed: usize,
    skipped: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DoctorCheck {
    id: String,
    category: String,
    status: DoctorStatus,
    severity: DoctorSeverity,
    message: String,
    hints: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<u8>,
    #[serde(skip_serializing_if = "Value::is_null")]
    data: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DoctorStatus {
    Pass,
    Warning,
    Fail,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DoctorSeverity {
    Info,
    Warning,
    Error,
}

impl DoctorStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warning => "warning",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
        }
    }
}

impl DoctorSeverity {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl DoctorCheck {
    fn pass(
        id: impl Into<String>,
        category: impl Into<String>,
        message: impl Into<String>,
        data: Value,
    ) -> Self {
        Self {
            id: id.into(),
            category: category.into(),
            status: DoctorStatus::Pass,
            severity: DoctorSeverity::Info,
            message: message.into(),
            hints: Vec::new(),
            exit_code: None,
            data,
        }
    }

    fn warning(
        id: impl Into<String>,
        category: impl Into<String>,
        message: impl Into<String>,
        hints: Vec<String>,
        data: Value,
    ) -> Self {
        Self {
            id: id.into(),
            category: category.into(),
            status: DoctorStatus::Warning,
            severity: DoctorSeverity::Warning,
            message: message.into(),
            hints,
            exit_code: None,
            data,
        }
    }

    fn fail(
        id: impl Into<String>,
        category: impl Into<String>,
        message: impl Into<String>,
        exit_code: MissiveExitCode,
        hints: Vec<String>,
        data: Value,
    ) -> Self {
        Self {
            id: id.into(),
            category: category.into(),
            status: DoctorStatus::Fail,
            severity: DoctorSeverity::Error,
            message: message.into(),
            hints,
            exit_code: Some(exit_code.as_u8()),
            data,
        }
    }

    fn skipped(
        id: impl Into<String>,
        category: impl Into<String>,
        message: impl Into<String>,
        hints: Vec<String>,
        data: Value,
    ) -> Self {
        Self {
            id: id.into(),
            category: category.into(),
            status: DoctorStatus::Skipped,
            severity: DoctorSeverity::Info,
            message: message.into(),
            hints,
            exit_code: None,
            data,
        }
    }
}

/// Executes local doctor checks and renders the resulting report.
pub(crate) fn execute_doctor_command<W>(
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
    writer: &mut W,
) -> Result<DoctorOutcome>
where
    W: Write,
{
    let report = collect_doctor_report(globals, environment, current_dir)?;
    let outcome = DoctorOutcome {
        selected_profile: report.output.profile.clone(),
        output_mode: report.output_mode,
        exit_code: report.output.overall_exit_code(),
        failure_message: report.output.failure_message(),
    };
    render_doctor(writer, report.output_mode, &report.output)?;
    Ok(outcome)
}

#[derive(Debug, Clone)]
struct DoctorReport {
    output_mode: OutputMode,
    output: DoctorOutput,
}

fn collect_doctor_report(
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    current_dir: &Path,
) -> Result<DoctorReport> {
    let mut checks = vec![binary_version_check()];

    let load_result = load_config_for_doctor(globals, environment, current_dir);
    let (loaded_config, selected_profile, output_mode) = match load_result {
        Ok(loaded_config) => {
            let output_mode =
                OutputMode::from_globals_and_config(globals, loaded_config.output_format()?)?;
            checks.push(config_success_check(&loaded_config));
            (
                Some(loaded_config.clone()),
                loaded_config.selected_profile.clone(),
                output_mode,
            )
        }
        Err(error) => {
            let output_mode = OutputMode::from_globals(globals)?;
            let selected_profile = globals
                .profile
                .clone()
                .unwrap_or_else(|| "unknown".to_owned());
            checks.push(config_failure_check(&error, globals));
            (None, selected_profile, output_mode)
        }
    };

    let resolved_paths = if let Some(loaded_config) = &loaded_config {
        match resolve_state_paths(loaded_config, environment) {
            Ok(paths) => {
                checks.push(state_paths_success_check(&paths));
                Some(paths)
            }
            Err(error) => {
                checks.push(state_paths_failure_check(&error));
                None
            }
        }
    } else {
        checks.push(state_paths_skipped_check());
        None
    };

    if let Some(paths) = &resolved_paths {
        checks.push(sqlite_migration_check(paths.database_path()));
    } else {
        checks.push(sqlite_migration_skipped_check());
    }

    if let Some(loaded_config) = &loaded_config {
        let probe_timeout = doctor_probe_timeout(globals, loaded_config)?;
        checks.extend(a2a_endpoint_checks(
            globals,
            loaded_config,
            environment,
            probe_timeout,
        ));
        checks.push(gateway_status_check(loaded_config, probe_timeout));
    } else {
        checks.push(a2a_endpoints_config_skipped_check());
        checks.push(gateway_status_config_skipped_check());
    }

    checks.extend(tool_availability_checks(environment));

    let overall = summarize_checks(&checks);
    let message = doctor_message(&selected_profile, &overall);
    let output = DoctorOutput {
        profile: selected_profile,
        scope: "local_remote_gateway".to_owned(),
        overall,
        checks,
        message,
    };

    Ok(DoctorReport {
        output_mode,
        output,
    })
}

fn load_config_for_doctor(
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

fn binary_version_check() -> DoctorCheck {
    DoctorCheck::pass(
        "binary.version",
        "binary",
        format!(
            "{} {} is available for local diagnostics",
            BINARY_NAME,
            env!("CARGO_PKG_VERSION")
        ),
        json!({
            "binary": BINARY_NAME,
            "crate": CRATE_NAME,
            "package_version": env!("CARGO_PKG_VERSION"),
            "build_profile": option_env!("MISSIVE_BUILD_PROFILE").unwrap_or("unknown"),
            "build_target": option_env!("MISSIVE_BUILD_TARGET").unwrap_or("unknown"),
            "rustc_version": option_env!("MISSIVE_BUILD_RUSTC_VERSION").unwrap_or("unknown"),
            "debug_assertions": cfg!(debug_assertions),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        }),
    )
}

fn config_success_check(loaded_config: &LoadedConfig) -> DoctorCheck {
    DoctorCheck::pass(
        "config.discovery_validation",
        "config",
        format!(
            "Configuration loaded from {} and selected profile '{}' validated successfully",
            loaded_config.source.kind.as_str(),
            loaded_config.selected_profile
        ),
        json!({
            "source": loaded_config.source.kind.as_str(),
            "path": loaded_config.source.path.as_ref().map(|path| path.display().to_string()),
            "selected_profile": loaded_config.selected_profile,
            "profile_count": loaded_config.config.profiles.len(),
            "agent_count": loaded_config.config.agents.len(),
            "auth_ref_count": loaded_config.config.auth_refs.len(),
            "redacted_config": loaded_config.to_redacted_json().ok().map(|value| redact_doctor_config_refs(&value)),
        }),
    )
}

fn redact_doctor_config_refs(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut redacted = serde_json::Map::new();
            for (key, value) in object {
                let normalized: String = key
                    .chars()
                    .filter(|character| character.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect();
                if matches!(normalized.as_str(), "authref" | "authrefname" | "authrefs") {
                    redacted.insert(key.clone(), Value::String(REDACTED.to_owned()));
                } else {
                    redacted.insert(key.clone(), redact_doctor_config_refs(value));
                }
            }
            Value::Object(redacted)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_doctor_config_refs).collect()),
        other => other.clone(),
    }
}

fn config_failure_check(error: &MissiveError, globals: &GlobalArgs) -> DoctorCheck {
    let mut data = json!({
        "error": error.to_report(),
        "requested_profile": globals.profile,
    });
    if let Some(path) = &globals.config {
        data["explicit_path"] = Value::String(path.display().to_string());
    }

    DoctorCheck::fail(
        "config.discovery_validation",
        "config",
        "Configuration discovery or validation failed",
        error.exit_code(),
        vec![
            "Fix the selected config file, --profile value, or MISSIVE_CONFIG before running stateful commands.".to_owned(),
            "Use `missive --config <path> doctor --json` after edits to confirm the local config is valid.".to_owned(),
        ],
        data,
    )
}

fn resolve_state_paths(
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
) -> Result<StatePaths> {
    StatePathResolver::new()
        .with_env(
            environment
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        )
        .resolve_loaded(loaded_config)
}

fn state_paths_success_check(paths: &StatePaths) -> DoctorCheck {
    DoctorCheck::pass(
        "state.paths",
        "storage",
        format!(
            "Resolved local state paths for profile '{}' using {} roots",
            paths.profile(),
            state_path_source_label(paths.source())
        ),
        json!({
            "profile": paths.profile(),
            "source": state_path_source_label(paths.source()),
            "data_dir": path_value(paths.data_dir()),
            "state_dir": path_value(paths.state_dir()),
            "cache_dir": path_value(paths.cache_dir()),
            "locks_dir": path_value(paths.locks_dir()),
            "database_path": path_value(paths.database_path()),
            "data_dir_exists": paths.data_dir().is_dir(),
            "state_dir_exists": paths.state_dir().is_dir(),
            "cache_dir_exists": paths.cache_dir().is_dir(),
            "locks_dir_exists": paths.locks_dir().is_dir(),
            "database_exists": paths.database_path().exists(),
        }),
    )
}

fn state_paths_failure_check(error: &MissiveError) -> DoctorCheck {
    DoctorCheck::fail(
        "state.paths",
        "storage",
        "Selected profile state paths could not be resolved",
        error.exit_code(),
        vec![
            "Set MISSIVE_HOME to an absolute local state directory or configure HOME/XDG paths.".to_owned(),
            "Keep missive runtime state outside the source checkout unless you explicitly choose a disposable test path.".to_owned(),
        ],
        json!({ "error": error.to_report() }),
    )
}

fn state_paths_skipped_check() -> DoctorCheck {
    DoctorCheck::skipped(
        "state.paths",
        "storage",
        "State path checks were skipped because configuration did not load",
        vec!["Fix configuration discovery/validation first.".to_owned()],
        Value::Null,
    )
}

fn sqlite_migration_check(database_path: &Path) -> DoctorCheck {
    if !database_path.exists() {
        return DoctorCheck::skipped(
            "store.sqlite_migrations",
            "storage",
            "No profile SQLite database exists yet; migrations will run when a stateful command creates it",
            vec![
                "Run a stateful command such as `missive agent add` to create the profile database.".to_owned(),
            ],
            json!({
                "database_path": path_value(database_path),
                "exists": false,
                "expected_version": CURRENT_SCHEMA_VERSION,
            }),
        );
    }

    match fs::metadata(database_path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return DoctorCheck::fail(
                "store.sqlite_migrations",
                "storage",
                "Configured SQLite database path exists but is not a regular file",
                MissiveExitCode::TemporaryFailure,
                vec!["Choose a file path for storage.database_path or remove the conflicting filesystem entry.".to_owned()],
                json!({
                    "database_path": path_value(database_path),
                    "exists": true,
                    "is_file": false,
                    "expected_version": CURRENT_SCHEMA_VERSION,
                }),
            );
        }
        Err(error) => {
            return DoctorCheck::fail(
                "store.sqlite_migrations",
                "storage",
                "Configured SQLite database path could not be inspected",
                MissiveExitCode::TemporaryFailure,
                vec!["Check profile state directory permissions.".to_owned()],
                json!({
                    "database_path": path_value(database_path),
                    "error": error.to_string(),
                    "expected_version": CURRENT_SCHEMA_VERSION,
                }),
            );
        }
    }

    match inspect_existing_database(database_path) {
        Ok(DatabaseInspection {
            current_version,
            applied_versions,
        }) if current_version == Some(CURRENT_SCHEMA_VERSION)
            && applied_versions == expected_migration_versions() =>
        {
            DoctorCheck::pass(
                "store.sqlite_migrations",
                "storage",
                format!(
                    "SQLite database is migrated to schema version {CURRENT_SCHEMA_VERSION}"
                ),
                json!({
                    "database_path": path_value(database_path),
                    "exists": true,
                    "current_version": current_version,
                    "expected_version": CURRENT_SCHEMA_VERSION,
                    "applied_versions": applied_versions,
                }),
            )
        }
        Ok(DatabaseInspection {
            current_version,
            applied_versions,
        }) => DoctorCheck::fail(
            "store.sqlite_migrations",
            "storage",
            "SQLite database migration state is not current",
            MissiveExitCode::TemporaryFailure,
            vec![
                "Run a current missive stateful command to apply embedded migrations, or inspect the database before reuse.".to_owned(),
                "If this database was created by a newer missive binary, use that newer binary or a different profile.".to_owned(),
            ],
            json!({
                "database_path": path_value(database_path),
                "exists": true,
                "current_version": current_version,
                "expected_version": CURRENT_SCHEMA_VERSION,
                "applied_versions": applied_versions,
                "expected_applied_versions": expected_migration_versions(),
            }),
        ),
        Err(error) => DoctorCheck::fail(
            "store.sqlite_migrations",
            "storage",
            "SQLite database migration state could not be read",
            MissiveExitCode::TemporaryFailure,
            vec![
                "Check that the database is a missive SQLite database and that migration checksums were not edited.".to_owned(),
            ],
            json!({
                "database_path": path_value(database_path),
                "exists": true,
                "error": error.to_report(),
                "expected_version": CURRENT_SCHEMA_VERSION,
            }),
        ),
    }
}

fn sqlite_migration_skipped_check() -> DoctorCheck {
    DoctorCheck::skipped(
        "store.sqlite_migrations",
        "storage",
        "SQLite migration check was skipped because state paths were unavailable",
        vec!["Fix state path resolution first.".to_owned()],
        json!({ "expected_version": CURRENT_SCHEMA_VERSION }),
    )
}

fn doctor_probe_timeout(globals: &GlobalArgs, loaded_config: &LoadedConfig) -> Result<Duration> {
    if let Some(value) = globals.timeout.as_deref() {
        return parse_duration_arg("--timeout", value);
    }

    let profile = loaded_config.selected_profile_config()?;
    let connect_timeout = profile
        .qos
        .as_ref()
        .map_or(loaded_config.config.qos.connect_timeout.as_str(), |qos| {
            qos.connect_timeout.as_str()
        });
    parse_duration_arg("qos.connect_timeout", connect_timeout)
}

fn a2a_endpoint_checks(
    globals: &GlobalArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    probe_timeout: Duration,
) -> Vec<DoctorCheck> {
    if loaded_config.config.agents.is_empty() {
        return vec![DoctorCheck::skipped(
            "a2a.endpoints",
            "a2a",
            "No config-seeded A2A agents are configured; endpoint reachability is not applicable",
            vec!["Add [agents.<alias>] entries when this profile should monitor remote A2A endpoint reachability.".to_owned()],
            json!({
                "configured_agent_count": 0,
                "probe": "public_agent_card_discovery",
            }),
        )];
    }

    let service_parameters = match service_parameters_from_config_and_globals(
        loaded_config,
        globals,
    ) {
        Ok(parameters) => parameters,
        Err(error) => {
            return vec![DoctorCheck::fail(
                "a2a.service_parameters",
                "a2a",
                "A2A service parameters could not be prepared for endpoint checks",
                error.exit_code(),
                vec!["Fix [protocol] service parameters or --protocol-version/--a2a-extension/--service-param values.".to_owned()],
                json!({ "error": error.to_report() }),
            )];
        }
    };

    let client = match AgentCardClient::with_timeout(probe_timeout) {
        Ok(client) => client,
        Err(error) => {
            return vec![DoctorCheck::fail(
                "a2a.endpoints.client",
                "a2a",
                "A2A endpoint probe client could not be constructed",
                error.exit_code(),
                vec![
                    "Check local TLS/HTTP client configuration before retrying doctor.".to_owned(),
                ],
                json!({ "error": error.to_report() }),
            )];
        }
    };

    loaded_config
        .config
        .agents
        .iter()
        .map(|(alias, agent)| {
            a2a_endpoint_check(
                alias,
                agent,
                loaded_config,
                globals,
                environment,
                &service_parameters,
                &client,
            )
        })
        .collect()
}

fn a2a_endpoint_check(
    alias: &str,
    agent: &AgentConfig,
    loaded_config: &LoadedConfig,
    globals: &GlobalArgs,
    environment: &BTreeMap<String, String>,
    service_parameters: &ServiceParameters,
    client: &AgentCardClient,
) -> DoctorCheck {
    let auth_configured = config_agent_auth_configured(agent, globals);
    let auth_headers = match auth_headers_for_config_agent(
        loaded_config,
        alias,
        globals,
        environment,
    ) {
        Ok(headers) => headers,
        Err(error) => {
            return DoctorCheck::fail(
                format!("a2a.endpoint.{alias}"),
                "a2a",
                format!("A2A endpoint auth could not be resolved for configured agent '{alias}'"),
                error.exit_code(),
                vec!["Set the required auth environment variable, provision the configured keyring entry, or remove the auth_ref for this diagnostic run.".to_owned()],
                json!({
                    "agent": alias,
                    "base_url": agent.base_url.as_str(),
                    "probe": "public_agent_card_discovery",
                    "auth_configured": auth_configured,
                    "error": error.to_report(),
                }),
            );
        }
    };

    let outcome = client.fetch_public_agent_card_with_service_parameters_and_auth(
        &agent.base_url,
        None,
        service_parameters,
        &auth_headers,
    );

    match outcome {
        Ok(AgentCardFetchOutcome::Fetched(fetch)) => {
            let summary = fetch.card.summary();
            DoctorCheck::pass(
                format!("a2a.endpoint.{alias}"),
                "a2a",
                format!("A2A Agent Card discovery succeeded for configured agent '{alias}'"),
                json!({
                    "agent": alias,
                    "base_url": agent.base_url.as_str(),
                    "probe": "public_agent_card_discovery",
                    "agent_card_url": fetch.url,
                    "http_status": fetch.status,
                    "protocol_version_sent": service_parameters.protocol_version,
                    "auth_configured": auth_configured,
                    "agent_name": summary.name,
                    "agent_version": summary.agent_version,
                    "protocol_versions": summary.protocol_versions,
                    "supported_interface_count": summary.supported_interfaces.len(),
                    "default_input_modes": summary.default_input_modes,
                    "default_output_modes": summary.default_output_modes,
                    "streaming": summary.capabilities.streaming,
                    "push_notifications": summary.capabilities.push_notifications,
                }),
            )
        }
        Ok(AgentCardFetchOutcome::NotModified(not_modified)) => DoctorCheck::pass(
            format!("a2a.endpoint.{alias}"),
            "a2a",
            format!("A2A Agent Card discovery reached configured agent '{alias}'"),
            json!({
                "agent": alias,
                "base_url": agent.base_url.as_str(),
                "probe": "public_agent_card_discovery",
                "agent_card_url": not_modified.url,
                "http_status": not_modified.status,
                "protocol_version_sent": service_parameters.protocol_version,
                "auth_configured": auth_configured,
                "cache_status": "not_modified",
            }),
        ),
        Err(error) => DoctorCheck::fail(
            format!("a2a.endpoint.{alias}"),
            "a2a",
            format!("A2A Agent Card discovery failed for configured agent '{alias}'"),
            error.exit_code(),
            vec![
                "Verify the agent base URL serves /.well-known/agent-card.json and accepts the configured A2A-Version.".to_owned(),
                "Check local network/TLS access and any configured auth ref without putting token values in config.".to_owned(),
            ],
            json!({
                "agent": alias,
                "base_url": agent.base_url.as_str(),
                "probe": "public_agent_card_discovery",
                "protocol_version_sent": service_parameters.protocol_version,
                "auth_configured": auth_configured,
                "error": error.to_report(),
            }),
        ),
    }
}

fn a2a_endpoints_config_skipped_check() -> DoctorCheck {
    DoctorCheck::skipped(
        "a2a.endpoints",
        "a2a",
        "A2A endpoint reachability checks were skipped because configuration did not load",
        vec!["Fix configuration discovery/validation first.".to_owned()],
        json!({ "probe": "public_agent_card_discovery" }),
    )
}

fn config_agent_auth_configured(agent: &AgentConfig, globals: &GlobalArgs) -> bool {
    agent.auth_ref.is_some() || globals.bearer_token_env.is_some() || !globals.headers.is_empty()
}

fn gateway_status_check(loaded_config: &LoadedConfig, probe_timeout: Duration) -> DoctorCheck {
    let gateway = match loaded_config.gateway_config() {
        Ok(gateway) => gateway,
        Err(error) => {
            return DoctorCheck::fail(
                "gateway.status",
                "gateway",
                "Gateway configuration could not be resolved",
                error.exit_code(),
                vec!["Fix [gateway] or [profiles.<name>.gateway] configuration.".to_owned()],
                json!({ "error": error.to_report() }),
            );
        }
    };
    let (bind_addr, status_url) = match gateway_status_url(&gateway.bind_address) {
        Ok(values) => values,
        Err(error) => {
            return DoctorCheck::fail(
                "gateway.status",
                "gateway",
                "Gateway bind address could not be converted into a local status URL",
                error.exit_code(),
                vec![
                    "Use an IP socket address such as 127.0.0.1:7347 for gateway.bind_address."
                        .to_owned(),
                ],
                json!({
                    "configured_enabled": gateway.enabled,
                    "bind_address": gateway.bind_address,
                    "error": error.to_report(),
                }),
            );
        }
    };

    let client = match reqwest::blocking::Client::builder()
        .timeout(probe_timeout)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return DoctorCheck::fail(
                "gateway.status",
                "gateway",
                "Gateway status HTTP client could not be constructed",
                MissiveExitCode::Unavailable,
                vec![
                    "Check local TLS/HTTP client configuration before retrying doctor.".to_owned(),
                ],
                json!({
                    "configured_enabled": gateway.enabled,
                    "bind_address": bind_addr.to_string(),
                    "status_url": status_url,
                    "error": error.to_string(),
                }),
            );
        }
    };

    let response = match client
        .get(&status_url)
        .header("Accept", "application/json")
        .send()
    {
        Ok(response) => response,
        Err(error) if !gateway.enabled => {
            return DoctorCheck::skipped(
                "gateway.status",
                "gateway",
                "Gateway is not enabled for this profile and no local status endpoint is reachable",
                vec!["Set gateway.enabled = true or run `missive gateway run` when this profile should have a live gateway.".to_owned()],
                json!({
                    "configured_enabled": false,
                    "bind_address": bind_addr.to_string(),
                    "status_url": status_url,
                    "reachable": false,
                    "error": error.to_string(),
                }),
            );
        }
        Err(error) => {
            return DoctorCheck::fail(
                "gateway.status",
                "gateway",
                "Configured gateway status endpoint is unavailable",
                MissiveExitCode::Unavailable,
                vec!["Start the gateway with `missive gateway run` or update gateway.bind_address for this profile.".to_owned()],
                json!({
                    "configured_enabled": true,
                    "bind_address": bind_addr.to_string(),
                    "status_url": status_url,
                    "reachable": false,
                    "error": error.to_string(),
                }),
            );
        }
    };

    let status = response.status();
    if !status.is_success() {
        return gateway_unhealthy_response_check(
            gateway.enabled,
            bind_addr,
            status_url,
            status.as_u16(),
            "Gateway status endpoint returned a non-success HTTP status",
        );
    }

    let body = match response.json::<Value>() {
        Ok(body) => body,
        Err(error) => {
            return gateway_unparseable_response_check(
                gateway.enabled,
                bind_addr,
                status_url,
                error.to_string(),
            );
        }
    };

    if body.get("ok").and_then(Value::as_bool) != Some(true)
        || body.get("status").and_then(Value::as_str) != Some("ok")
    {
        return gateway_unhealthy_response_check(
            gateway.enabled,
            bind_addr,
            status_url,
            status.as_u16(),
            "Gateway status endpoint responded but did not report ok status",
        );
    }

    DoctorCheck::pass(
        "gateway.status",
        "gateway",
        "Local gateway status endpoint is reachable and healthy",
        gateway_status_data(
            gateway.enabled,
            bind_addr,
            status_url,
            status.as_u16(),
            &body,
        ),
    )
}

fn gateway_status_config_skipped_check() -> DoctorCheck {
    DoctorCheck::skipped(
        "gateway.status",
        "gateway",
        "Gateway status check was skipped because configuration did not load",
        vec!["Fix configuration discovery/validation first.".to_owned()],
        Value::Null,
    )
}

fn gateway_unhealthy_response_check(
    configured_enabled: bool,
    bind_addr: SocketAddr,
    status_url: String,
    http_status: u16,
    message: &'static str,
) -> DoctorCheck {
    let data = json!({
        "configured_enabled": configured_enabled,
        "bind_address": bind_addr.to_string(),
        "status_url": status_url,
        "reachable": true,
        "http_status": http_status,
    });
    if configured_enabled {
        DoctorCheck::fail(
            "gateway.status",
            "gateway",
            message,
            MissiveExitCode::Unavailable,
            vec![
                "Inspect `missive gateway run` logs or restart the configured gateway service."
                    .to_owned(),
            ],
            data,
        )
    } else {
        DoctorCheck::warning(
            "gateway.status",
            "gateway",
            "A local endpoint responded on the gateway bind address but was not a healthy missive gateway",
            vec!["If this is an unrelated local service, choose another gateway.bind_address before enabling the gateway.".to_owned()],
            data,
        )
    }
}

fn gateway_unparseable_response_check(
    configured_enabled: bool,
    bind_addr: SocketAddr,
    status_url: String,
    error: String,
) -> DoctorCheck {
    let data = json!({
        "configured_enabled": configured_enabled,
        "bind_address": bind_addr.to_string(),
        "status_url": status_url,
        "reachable": true,
        "error": error,
    });
    if configured_enabled {
        DoctorCheck::fail(
            "gateway.status",
            "gateway",
            "Configured gateway status endpoint did not return parseable JSON",
            MissiveExitCode::Unavailable,
            vec!["Ensure the configured address is running `missive gateway run` and not another local service.".to_owned()],
            data,
        )
    } else {
        DoctorCheck::warning(
            "gateway.status",
            "gateway",
            "A local endpoint responded on the gateway bind address but did not look like missive gateway JSON",
            vec!["If this is an unrelated local service, choose another gateway.bind_address before enabling the gateway.".to_owned()],
            data,
        )
    }
}

fn gateway_status_data(
    configured_enabled: bool,
    bind_addr: SocketAddr,
    status_url: String,
    http_status: u16,
    body: &Value,
) -> Value {
    let components = body
        .get("components")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(json!({
                        "name": item.get("name")?.as_str()?,
                        "status": item.get("status")?.as_str()?,
                    }))
                })
                .collect::<Vec<Value>>()
        })
        .unwrap_or_default();

    json!({
        "configured_enabled": configured_enabled,
        "bind_address": bind_addr.to_string(),
        "status_url": status_url,
        "reachable": true,
        "http_status": http_status,
        "remote_status": body.get("status").and_then(Value::as_str),
        "remote_profile": body.get("profile").and_then(Value::as_str),
        "remote_bind_address": body.get("bind_address").and_then(Value::as_str),
        "uptime_ms": body.get("uptime_ms").and_then(Value::as_u64),
        "job_concurrency": body.get("job_concurrency").and_then(Value::as_u64),
        "event_bus_events": body.get("event_bus_events").and_then(Value::as_u64),
        "component_count": components.len(),
        "components": components,
    })
}

fn gateway_status_url(bind_address: &str) -> Result<(SocketAddr, String)> {
    let bind_addr = bind_address.parse::<SocketAddr>().map_err(|_| {
        MissiveError::config(format!(
            "gateway.bind_address must be an IP socket address, got {bind_address:?}"
        ))
    })?;
    let host = local_probe_host(bind_addr.ip());
    Ok((
        bind_addr,
        format!(
            "http://{host}:{}{}",
            bind_addr.port(),
            DEFAULT_GATEWAY_STATUS_PATH
        ),
    ))
}

fn local_probe_host(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(address) if address.is_unspecified() => "127.0.0.1".to_owned(),
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) if address.is_unspecified() => "[::1]".to_owned(),
        IpAddr::V6(address) => format!("[{address}]"),
    }
}

fn parse_duration_arg(flag: &str, value: &str) -> Result<Duration> {
    let value = value.trim();
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1_u64)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000_u64)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000_u64)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 3_600_000_u64)
    } else {
        return Err(MissiveError::validation(format!(
            "{flag} value {value:?} must use a duration suffix: ms, s, m, or h"
        ))
        .with_help("Use values such as 500ms, 2s, 5m, or 1h."));
    };
    let number = number.parse::<u64>().map_err(|error| {
        MissiveError::validation(format!("{flag} value {value:?} has an invalid number"))
            .with_source(error)
            .with_help("Use a positive whole number followed by ms, s, m, or h.")
    })?;
    if number == 0 {
        return Err(MissiveError::validation(format!(
            "{flag} value {value:?} must be greater than zero"
        )));
    }
    let millis = number
        .checked_mul(multiplier)
        .ok_or_else(|| MissiveError::validation(format!("{flag} value {value:?} is too large")))?;
    Ok(Duration::from_millis(millis))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DatabaseInspection {
    current_version: Option<u32>,
    applied_versions: Vec<u32>,
}

fn inspect_existing_database(database_path: &Path) -> Result<DatabaseInspection> {
    let connection = open_sqlite_database(database_path)?;
    let current_version = schema_version(&connection)?;
    let applied = applied_migrations(&connection)?;

    for applied_migration in &applied {
        let Some(expected) = embedded_migrations()
            .iter()
            .find(|migration| migration.version() == applied_migration.version())
        else {
            return Err(MissiveError::storage(format!(
                "database contains unknown future migration version {} ({})",
                applied_migration.version(),
                applied_migration.name()
            ))
            .with_help(
                "Open this database with a newer missive binary, or use a profile database created by this version.",
            ));
        };

        if applied_migration.name() != expected.name()
            || applied_migration.checksum() != expected.checksum()
        {
            return Err(MissiveError::storage(format!(
                "migration {} checksum mismatch for {}",
                applied_migration.version(),
                applied_migration.name()
            ))
            .with_help(
                "Do not edit applied migration files; create a new migration for schema changes.",
            ));
        }
    }

    Ok(DatabaseInspection {
        current_version,
        applied_versions: applied
            .iter()
            .map(|migration| migration.version())
            .collect(),
    })
}

fn expected_migration_versions() -> Vec<u32> {
    embedded_migrations()
        .iter()
        .map(|migration| migration.version())
        .collect()
}

fn tool_availability_checks(environment: &BTreeMap<String, String>) -> Vec<DoctorCheck> {
    tool_specs()
        .iter()
        .map(|spec| tool_availability_check(spec, environment))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ToolSpec {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    hint: &'static str,
}

fn tool_specs() -> &'static [ToolSpec] {
    &[
        ToolSpec {
            id: "tool.rustc",
            name: "rustc",
            description: "Rust compiler for local builds and diagnostics",
            hint: "Install Rust with rustup or ensure rustc is on PATH.",
        },
        ToolSpec {
            id: "tool.cargo",
            name: "cargo",
            description: "Cargo workspace build and test runner",
            hint: "Install Rust with rustup or ensure cargo is on PATH.",
        },
        ToolSpec {
            id: "tool.rustfmt",
            name: "rustfmt",
            description: "Rust formatter used by the quality gate",
            hint: "Run `rustup component add rustfmt` or scripts/bootstrap-tools.sh.",
        },
        ToolSpec {
            id: "tool.cargo_clippy",
            name: "cargo-clippy",
            description: "Clippy lint driver used by the quality gate",
            hint: "Run `rustup component add clippy` or scripts/bootstrap-tools.sh.",
        },
        ToolSpec {
            id: "tool.shellcheck",
            name: "shellcheck",
            description: "Shell script linter used by the quality gate when installed",
            hint: "Install shellcheck with your OS package manager for stronger script validation.",
        },
        ToolSpec {
            id: "tool.sqlite3",
            name: "sqlite3",
            description: "SQLite inspection CLI useful for local store troubleshooting",
            hint: "Install sqlite3 with your OS package manager if you need manual database inspection.",
        },
    ]
}

fn tool_availability_check(spec: &ToolSpec, environment: &BTreeMap<String, String>) -> DoctorCheck {
    let found = find_executable(spec.name, environment);
    match found {
        Some(path) => DoctorCheck::pass(
            spec.id,
            "tool",
            format!("Found {} ({})", spec.name, spec.description),
            json!({
                "tool": spec.name,
                "path": path.display().to_string(),
                "description": spec.description,
            }),
        ),
        None => DoctorCheck::warning(
            spec.id,
            "tool",
            format!("{} was not found on PATH", spec.name),
            vec![spec.hint.to_owned()],
            json!({
                "tool": spec.name,
                "description": spec.description,
                "path_found": false,
                "path_configured": environment.get("PATH").is_some_and(|path| !path.is_empty()),
            }),
        ),
    }
}

fn find_executable(tool: &str, environment: &BTreeMap<String, String>) -> Option<PathBuf> {
    let path_value = environment.get("PATH")?;
    for directory in std::env::split_paths(path_value) {
        for candidate_name in executable_candidate_names(tool, environment) {
            let candidate = directory.join(candidate_name);
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn executable_candidate_names(tool: &str, environment: &BTreeMap<String, String>) -> Vec<String> {
    let mut candidates = vec![tool.to_owned()];
    if cfg!(windows) {
        let extensions = environment
            .get("PATHEXT")
            .map_or(".COM;.EXE;.BAT;.CMD", String::as_str);
        for extension in extensions
            .split(';')
            .filter(|extension| !extension.is_empty())
        {
            let lower_tool = tool.to_ascii_lowercase();
            let lower_extension = extension.to_ascii_lowercase();
            if !lower_tool.ends_with(&lower_extension) {
                candidates.push(format!("{tool}{extension}"));
            }
        }
    }
    candidates
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn summarize_checks(checks: &[DoctorCheck]) -> DoctorOverall {
    let passed = checks
        .iter()
        .filter(|check| check.status == DoctorStatus::Pass)
        .count();
    let warnings = checks
        .iter()
        .filter(|check| check.status == DoctorStatus::Warning)
        .count();
    let failed = checks
        .iter()
        .filter(|check| check.status == DoctorStatus::Fail)
        .count();
    let skipped = checks
        .iter()
        .filter(|check| check.status == DoctorStatus::Skipped)
        .count();
    let status = if failed > 0 {
        DoctorStatus::Fail
    } else if warnings > 0 {
        DoctorStatus::Warning
    } else {
        DoctorStatus::Pass
    };
    let exit_code = checks
        .iter()
        .find_map(|check| {
            if check.status == DoctorStatus::Fail {
                check.exit_code
            } else {
                None
            }
        })
        .unwrap_or(MissiveExitCode::Success.as_u8());

    DoctorOverall {
        status,
        exit_code,
        check_count: checks.len(),
        passed,
        warnings,
        failed,
        skipped,
    }
}

fn doctor_message(profile: &str, overall: &DoctorOverall) -> String {
    match overall.status {
        DoctorStatus::Pass => format!(
            "missive doctor: checks passed for profile '{profile}' ({} passed, {} skipped)",
            overall.passed, overall.skipped
        ),
        DoctorStatus::Warning => format!(
            "missive doctor: checks completed with warnings for profile '{profile}' ({} passed, {} warnings, {} skipped)",
            overall.passed, overall.warnings, overall.skipped
        ),
        DoctorStatus::Fail => format!(
            "missive doctor: checks failed for profile '{profile}' ({} failed, {} warnings, {} passed)",
            overall.failed, overall.warnings, overall.passed
        ),
        DoctorStatus::Skipped => format!("missive doctor: checks skipped for profile '{profile}'"),
    }
}

impl DoctorOutput {
    fn overall_exit_code(&self) -> MissiveExitCode {
        exit_code_from_u8(self.overall.exit_code).unwrap_or(MissiveExitCode::Software)
    }

    fn failure_message(&self) -> Option<String> {
        (self.overall.status == DoctorStatus::Fail).then(|| self.message.clone())
    }
}

fn exit_code_from_u8(code: u8) -> Option<MissiveExitCode> {
    match code {
        0 => Some(MissiveExitCode::Success),
        64 => Some(MissiveExitCode::Usage),
        69 => Some(MissiveExitCode::Unavailable),
        70 => Some(MissiveExitCode::Software),
        74 => Some(MissiveExitCode::Io),
        75 => Some(MissiveExitCode::TemporaryFailure),
        76 => Some(MissiveExitCode::Protocol),
        77 => Some(MissiveExitCode::Permission),
        78 => Some(MissiveExitCode::Config),
        80 => Some(MissiveExitCode::TaskFailed),
        81 => Some(MissiveExitCode::TaskCancelled),
        82 => Some(MissiveExitCode::TaskTimeout),
        83 => Some(MissiveExitCode::TaskInputRequired),
        _ => None,
    }
}

fn render_doctor<W>(writer: &mut W, mode: OutputMode, output: &DoctorOutput) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_doctor_human(writer, output),
        OutputMode::Json | OutputMode::Ndjson | OutputMode::Quiet => {
            render_success(writer, mode, "doctor", output, &output.message)
        }
    }
}

fn write_doctor_human<W>(writer: &mut W, output: &DoctorOutput) -> Result<()>
where
    W: Write,
{
    writeln!(writer, "{}", redact_text(&output.message))
        .map_err(|error| MissiveError::io("writing doctor output", error))?;
    writeln!(writer, "Scope: {}", output.scope)
        .map_err(|error| MissiveError::io("writing doctor output", error))?;
    writeln!(
        writer,
        "Overall: {} (exit {})",
        output.overall.status.as_str(),
        output.overall.exit_code
    )
    .map_err(|error| MissiveError::io("writing doctor output", error))?;
    writeln!(writer, "Checks:")
        .map_err(|error| MissiveError::io("writing doctor output", error))?;

    for check in &output.checks {
        writeln!(
            writer,
            "  [{}:{}] {} — {}",
            check.status.as_str(),
            check.severity.as_str(),
            check.id,
            redact_text(&check.message)
        )
        .map_err(|error| MissiveError::io("writing doctor output", error))?;
        for hint in &check.hints {
            writeln!(writer, "      hint: {}", redact_text(hint))
                .map_err(|error| MissiveError::io("writing doctor output", error))?;
        }
    }

    Ok(())
}

fn state_path_source_label(source: StatePathSource) -> &'static str {
    match source {
        StatePathSource::MissiveHome => "missive_home",
        StatePathSource::Xdg => "xdg",
        StatePathSource::PlatformFallback => "platform_fallback",
    }
}

fn path_value(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overall_status_warns_without_failing_for_missing_tools() {
        let checks = vec![
            DoctorCheck::pass("binary.version", "binary", "ok", Value::Null),
            DoctorCheck::warning(
                "tool.rustfmt",
                "tool",
                "missing",
                vec!["install rustfmt".to_owned()],
                Value::Null,
            ),
        ];

        let overall = summarize_checks(&checks);

        assert_eq!(overall.status, DoctorStatus::Warning);
        assert_eq!(overall.exit_code, MissiveExitCode::Success.as_u8());
    }

    #[test]
    fn overall_status_uses_first_failing_exit_code() {
        let checks = vec![
            DoctorCheck::fail(
                "config.discovery_validation",
                "config",
                "bad config",
                MissiveExitCode::Config,
                Vec::new(),
                Value::Null,
            ),
            DoctorCheck::fail(
                "store.sqlite_migrations",
                "storage",
                "bad db",
                MissiveExitCode::TemporaryFailure,
                Vec::new(),
                Value::Null,
            ),
        ];

        let overall = summarize_checks(&checks);

        assert_eq!(overall.status, DoctorStatus::Fail);
        assert_eq!(overall.exit_code, MissiveExitCode::Config.as_u8());
    }
}
