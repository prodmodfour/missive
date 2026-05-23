//! Local diagnostic log inspection command.
//!
//! missive foreground commands write diagnostics to stderr. The `logs` command
//! therefore inventories local diagnostic sources for the selected profile and
//! reads bounded records from the profile log directory when a supervisor or
//! operator has redirected logs there.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use clap::Args;
use missive_core::{LoadedConfig, MissiveError, Result};
use missive_gateway::{
    GatewayServiceAction, GatewayServiceOptions, GatewayServiceScope, captured_environment_keys,
    validate_service_environment,
};
use missive_store::{ProcessLock, ProcessLockKind, StatePathResolver, Store};
use serde::Serialize;
use serde_json::Value;

use crate::output::{OutputMode, redact_json, redact_text, render_stream_item, render_success};

const DEFAULT_LOG_LIMIT: usize = 50;
const PROFILE_LOG_SOURCE: &str = "profile-files";
const EVENT_JOURNAL_SOURCE: &str = "event-journal";
const GATEWAY_SERVICE_SOURCE: &str = "gateway-service";
const PROFILE_LOG_DIR_NAME: &str = "logs";

/// Arguments for `missive logs`.
#[derive(Debug, Clone, Args)]
pub struct LogsArgs {
    /// Restrict output to one diagnostic source: profile-files, event-journal, or gateway-service.
    #[arg(long = "source", value_name = "SOURCE")]
    pub source: Option<String>,

    /// Maximum number of profile log records to return. Use 0 to list sources only.
    #[arg(long = "limit", value_name = "N", default_value_t = DEFAULT_LOG_LIMIT)]
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct LogsOutput {
    profile: String,
    paths: LogPathView,
    filters: LogFilterView,
    sources: Vec<LogSourceView>,
    count: usize,
    records: Vec<LogRecordView>,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct LogPathView {
    state_dir: String,
    log_dir: String,
    database_path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct LogFilterView {
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct LogSourceView {
    name: String,
    kind: String,
    available: bool,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    message: String,
    hints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct LogRecordView {
    sequence: u64,
    source: String,
    source_kind: String,
    path: String,
    line_number: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fields: Option<Value>,
}

#[derive(Debug, Clone)]
struct ProfileLogRead {
    source: LogSourceView,
    records: Vec<LogRecordView>,
}

/// Executes the local diagnostic log inspection command.
pub(crate) fn execute_logs_command<W>(
    args: &LogsArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let output = collect_logs(args, loaded_config, environment)?;
    render_logs(writer, mode, &output)
}

fn collect_logs(
    args: &LogsArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
) -> Result<LogsOutput> {
    let source_filter = args.source.as_deref().map(canonical_source).transpose()?;
    let resolver = StatePathResolver::new().with_env(
        environment
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let paths = resolver.resolve_loaded(loaded_config)?;
    paths.ensure_directories()?;
    let log_dir = paths.state_dir().join(PROFILE_LOG_DIR_NAME);

    let mut sources = Vec::new();
    let mut records = Vec::new();

    if source_matches(source_filter, PROFILE_LOG_SOURCE) {
        let profile_logs = read_profile_log_source(&log_dir, args.limit)?;
        sources.push(profile_logs.source);
        records.extend(profile_logs.records);
    }

    if source_matches(source_filter, EVENT_JOURNAL_SOURCE) {
        sources.push(event_journal_source(
            paths.database_path(),
            &paths,
            args.limit,
        ));
    }

    if source_matches(source_filter, GATEWAY_SERVICE_SOURCE) {
        sources.push(gateway_service_source(loaded_config, environment));
    }

    let count = records.len();
    let source_count = sources.len();
    let message = if count == 0 {
        format!(
            "No local log records found for profile '{}'; inspected {source_count} diagnostic source(s)",
            loaded_config.selected_profile
        )
    } else {
        format!(
            "Collected {count} local log record(s) for profile '{}' from {source_count} diagnostic source(s)",
            loaded_config.selected_profile
        )
    };

    Ok(LogsOutput {
        profile: loaded_config.selected_profile.clone(),
        paths: LogPathView {
            state_dir: paths.state_dir().display().to_string(),
            log_dir: log_dir.display().to_string(),
            database_path: paths.database_path().display().to_string(),
        },
        filters: LogFilterView {
            source: source_filter.map(str::to_owned),
            limit: args.limit,
        },
        sources,
        count,
        records,
        message,
    })
}

fn canonical_source(raw: &str) -> Result<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "profile" | "profiles" | "file" | "files" | "profile-files" => Ok(PROFILE_LOG_SOURCE),
        "event" | "events" | "journal" | "event-journal" => Ok(EVENT_JOURNAL_SOURCE),
        "service" | "gateway" | "gateway-service" => Ok(GATEWAY_SERVICE_SOURCE),
        other => Err(
            MissiveError::validation(format!("unsupported logs source {other:?}")).with_help(
                "Use --source profile-files, --source event-journal, or --source gateway-service.",
            ),
        ),
    }
}

fn source_matches(filter: Option<&str>, source: &str) -> bool {
    filter.is_none_or(|filter| filter == source)
}

fn read_profile_log_source(log_dir: &Path, limit: usize) -> Result<ProfileLogRead> {
    if !log_dir.exists() {
        return Ok(ProfileLogRead {
            source: LogSourceView {
                name: PROFILE_LOG_SOURCE.to_owned(),
                kind: "file_directory".to_owned(),
                available: false,
                status: "unavailable".to_owned(),
                path: Some(log_dir.display().to_string()),
                service_name: None,
                command: None,
                message: "No profile log directory exists yet; foreground missive commands write diagnostics to stderr unless a supervisor redirects them.".to_owned(),
                hints: vec![
                    "Create this directory or redirect a supervisor to it when you want file-backed local logs.".to_owned(),
                    "Use `missive events tail` for the SQLite event journal; it is separate from stderr logs.".to_owned(),
                ],
            },
            records: Vec::new(),
        });
    }

    if !log_dir.is_dir() {
        return Ok(ProfileLogRead {
            source: LogSourceView {
                name: PROFILE_LOG_SOURCE.to_owned(),
                kind: "file_directory".to_owned(),
                available: false,
                status: "error".to_owned(),
                path: Some(log_dir.display().to_string()),
                service_name: None,
                command: None,
                message: "Profile log path exists but is not a directory.".to_owned(),
                hints: vec![
                    "Move or remove the path before writing profile log files there.".to_owned(),
                ],
            },
            records: Vec::new(),
        });
    }

    let mut notes = Vec::new();
    let mut files = Vec::new();
    match fs::read_dir(log_dir) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry) => {
                        let path = entry.path();
                        if is_log_file(&path) {
                            files.push(path);
                        }
                    }
                    Err(error) => notes.push(format!("Skipped one log directory entry: {error}")),
                }
            }
        }
        Err(error) => {
            return Ok(ProfileLogRead {
                source: LogSourceView {
                    name: PROFILE_LOG_SOURCE.to_owned(),
                    kind: "file_directory".to_owned(),
                    available: false,
                    status: "error".to_owned(),
                    path: Some(log_dir.display().to_string()),
                    service_name: None,
                    command: None,
                    message: redact_text(&format!("Could not read profile log directory: {error}")),
                    hints: vec!["Check local state directory permissions.".to_owned()],
                },
                records: Vec::new(),
            });
        }
    }
    files.sort();

    let mut records = Vec::new();
    for path in &files {
        match read_log_file(log_dir, path) {
            Ok(file_records) => records.extend(file_records),
            Err(error) => notes.push(format!(
                "Skipped {}: {}",
                redact_text(&relative_log_path(log_dir, path)),
                redact_text(error.message())
            )),
        }
    }

    if limit == 0 {
        records.clear();
    } else if records.len() > limit {
        records.drain(0..records.len() - limit);
    }
    for (index, record) in records.iter_mut().enumerate() {
        record.sequence = u64::try_from(index).unwrap_or(u64::MAX);
    }

    let status = if files.is_empty() || records.is_empty() {
        "empty"
    } else {
        "available"
    };
    let message = if files.is_empty() {
        "Profile log directory exists but contains no .log, .txt, .ndjson, .jsonl, or .json files."
            .to_owned()
    } else if records.is_empty() {
        format!(
            "Profile log directory contains {} log file(s), but no records were returned with the current limit.",
            files.len()
        )
    } else {
        format!(
            "Read {} record(s) from {} profile log file(s).",
            records.len(),
            files.len()
        )
    };

    Ok(ProfileLogRead {
        source: LogSourceView {
            name: PROFILE_LOG_SOURCE.to_owned(),
            kind: "file_directory".to_owned(),
            available: true,
            status: status.to_owned(),
            path: Some(log_dir.display().to_string()),
            service_name: None,
            command: None,
            message,
            hints: notes
                .into_iter()
                .map(|note| redact_text(&note))
                .collect::<Vec<_>>(),
        },
        records,
    })
}

fn event_journal_source(
    database_path: &Path,
    paths: &missive_store::StatePaths,
    limit: usize,
) -> LogSourceView {
    if !database_path.exists() {
        return LogSourceView {
            name: EVENT_JOURNAL_SOURCE.to_owned(),
            kind: "sqlite_event_journal".to_owned(),
            available: false,
            status: "unavailable".to_owned(),
            path: Some(database_path.display().to_string()),
            service_name: None,
            command: Some("missive events tail --ndjson".to_owned()),
            message: "No SQLite event journal exists yet for this profile.".to_owned(),
            hints: vec![
                "Run an implemented command that persists events, or use `missive events tail --timeout 5s --ndjson` once the database exists.".to_owned(),
            ],
        };
    }

    let event_count = count_event_records(database_path, paths).unwrap_or_else(|error| {
        tracing::debug!(
            target: "missive_cli",
            error = %redact_text(error.message()),
            "failed to count event journal records for logs command"
        );
        0
    });

    LogSourceView {
        name: EVENT_JOURNAL_SOURCE.to_owned(),
        kind: "sqlite_event_journal".to_owned(),
        available: true,
        status: "available".to_owned(),
        path: Some(database_path.display().to_string()),
        service_name: None,
        command: Some(if limit == 0 {
            "missive events tail --timeout 5s --ndjson".to_owned()
        } else {
            format!("missive events list --limit {limit} --json")
        }),
        message: format!(
            "SQLite event journal is available with {event_count} record(s); use `missive events list`, `missive events tail`, or `missive events export` for event payloads."
        ),
        hints: vec![
            "Events are structured control-plane records, not stderr logs.".to_owned(),
            "Use `missive events tail --ndjson` for machine-readable live event diagnostics."
                .to_owned(),
        ],
    }
}

fn count_event_records(database_path: &Path, paths: &missive_store::StatePaths) -> Result<usize> {
    let _lock = ProcessLock::acquire(paths, ProcessLockKind::StateMutation)?;
    Store::open(database_path)?
        .list_events()
        .map(|events| events.len())
}

fn gateway_service_source(
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
) -> LogSourceView {
    match gateway_service_plan(loaded_config, environment) {
        Ok(plan) => {
            let command = plan
                .planned_commands
                .first()
                .map(|command| command.display.clone());
            let service_file_exists = Path::new(&plan.service_path).exists();
            let mut hints = plan.notes.clone();
            if let Some(command) = &command {
                hints.push(format!("Status command: {command}"));
            }
            LogSourceView {
                name: GATEWAY_SERVICE_SOURCE.to_owned(),
                kind: plan.manager,
                available: service_file_exists,
                status: if service_file_exists {
                    "available".to_owned()
                } else {
                    "unavailable".to_owned()
                },
                path: None,
                service_name: Some(plan.service_name),
                command,
                message: if service_file_exists {
                    "Gateway service manager source is installed; inspect supervisor-captured stderr/stdout with the listed command.".to_owned()
                } else {
                    "Gateway service manager source is not installed for this user profile; foreground `missive gateway run` logs to stderr.".to_owned()
                },
                hints: hints.into_iter().map(|hint| redact_text(&hint)).collect(),
            }
        }
        Err(error) => LogSourceView {
            name: GATEWAY_SERVICE_SOURCE.to_owned(),
            kind: "service_manager".to_owned(),
            available: false,
            status: "unsupported".to_owned(),
            path: None,
            service_name: Some("missive-gateway".to_owned()),
            command: None,
            message: redact_text(error.message()),
            hints: vec![
                "Run `missive gateway run` under your existing supervisor and redirect stderr to the profile log directory if needed.".to_owned(),
            ],
        },
    }
}

fn gateway_service_plan(
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
) -> Result<missive_gateway::GatewayServiceResult> {
    let mut service_environment = BTreeMap::new();
    for key in captured_environment_keys() {
        if let Some(value) = environment.get(*key).filter(|value| !value.is_empty()) {
            validate_service_environment(key, value)?;
            service_environment.insert((*key).to_owned(), value.clone());
        }
    }

    let options = GatewayServiceOptions {
        action: GatewayServiceAction::Status,
        scope: GatewayServiceScope::User,
        dry_run: true,
        force: false,
        platform: None,
        executable: None,
        config_path: loaded_config.source.path.clone(),
        profile: loaded_config.selected_profile.clone(),
        environment: service_environment,
    };
    missive_gateway::service::build_gateway_service_plan(&options)
}

fn read_log_file(log_dir: &Path, path: &Path) -> Result<Vec<LogRecordView>> {
    let file = File::open(path).map_err(|error| {
        MissiveError::io(format!("opening log file {}", path.display()), error)
            .with_help("Check local log file permissions.")
    })?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    for (line_index, line) in reader.lines().enumerate() {
        let line = line.map_err(|error| {
            MissiveError::io(format!("reading log file {}", path.display()), error)
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let mut record = log_record_from_line(&line);
        record.path = relative_log_path(log_dir, path);
        record.line_number = line_index + 1;
        records.push(record);
    }
    Ok(records)
}

fn log_record_from_line(line: &str) -> LogRecordView {
    match serde_json::from_str::<Value>(line) {
        Ok(value) => {
            let fields = redact_json(&value);
            LogRecordView {
                sequence: 0,
                source: PROFILE_LOG_SOURCE.to_owned(),
                source_kind: "file".to_owned(),
                path: String::new(),
                line_number: 0,
                timestamp: string_at_paths(
                    &fields,
                    &[&["timestamp"], &["time"], &["fields", "timestamp"]],
                ),
                level: string_at_paths(&fields, &[&["level"], &["fields", "level"]])
                    .map(|value| value.to_ascii_lowercase()),
                target: string_at_paths(&fields, &[&["target"], &["fields", "target"]]),
                message: string_at_paths(&fields, &[&["message"], &["fields", "message"]])
                    .unwrap_or_else(|| redact_text(line)),
                fields: Some(fields),
            }
        }
        Err(_) => LogRecordView {
            sequence: 0,
            source: PROFILE_LOG_SOURCE.to_owned(),
            source_kind: "file".to_owned(),
            path: String::new(),
            line_number: 0,
            timestamp: None,
            level: infer_level(line),
            target: None,
            message: redact_text(line),
            fields: None,
        },
    }
}

fn string_at_paths(value: &Value, paths: &[&[&str]]) -> Option<String> {
    for path in paths {
        let mut current = value;
        let mut found = true;
        for segment in *path {
            match current.get(*segment) {
                Some(next) => current = next,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found && let Some(text) = current.as_str() {
            return Some(redact_text(text));
        }
    }
    None
}

fn infer_level(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    for level in ["trace", "debug", "info", "warn", "error"] {
        if lower.contains(level) {
            return Some(level.to_owned());
        }
    }
    None
}

fn is_log_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "log" | "txt" | "ndjson" | "jsonl" | "json"
                )
            })
}

fn relative_log_path(log_dir: &Path, path: &Path) -> String {
    path.strip_prefix(log_dir)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn render_logs<W>(writer: &mut W, mode: OutputMode, output: &LogsOutput) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Human => write_logs_human(writer, output),
        OutputMode::Json => render_success(writer, mode, "logs", output, &output.message),
        OutputMode::Ndjson => {
            let mut sequence = 0_u64;
            for source in &output.sources {
                render_stream_item(
                    writer,
                    mode,
                    "log_source",
                    sequence,
                    source,
                    &source.message,
                )?;
                sequence += 1;
            }
            for record in &output.records {
                render_stream_item(
                    writer,
                    mode,
                    "log_record",
                    sequence,
                    record,
                    &record.message,
                )?;
                sequence += 1;
            }
            Ok(())
        }
        OutputMode::Quiet => Ok(()),
    }
}

fn write_logs_human<W>(writer: &mut W, output: &LogsOutput) -> Result<()>
where
    W: Write,
{
    writeln!(
        writer,
        "Logs for profile '{}' ({} record(s)):",
        redact_text(&output.profile),
        output.count
    )
    .map_err(|error| MissiveError::io("writing logs output", error))?;
    writeln!(writer, "  log dir: {}", redact_text(&output.paths.log_dir))
        .map_err(|error| MissiveError::io("writing logs output", error))?;
    writeln!(
        writer,
        "  event journal: {}",
        redact_text(&output.paths.database_path)
    )
    .map_err(|error| MissiveError::io("writing logs output", error))?;
    writeln!(writer, "Sources:").map_err(|error| MissiveError::io("writing logs output", error))?;
    for source in &output.sources {
        let status = if source.available {
            "available"
        } else {
            &source.status
        };
        writeln!(
            writer,
            "  {} [{}] {} - {}",
            redact_text(&source.name),
            redact_text(status),
            redact_text(&source.kind),
            redact_text(&source.message)
        )
        .map_err(|error| MissiveError::io("writing logs output", error))?;
        for hint in &source.hints {
            writeln!(writer, "    hint: {}", redact_text(hint))
                .map_err(|error| MissiveError::io("writing logs output", error))?;
        }
    }
    if output.records.is_empty() {
        writeln!(writer, "No local log records matched the selected filters.")
            .map_err(|error| MissiveError::io("writing logs output", error))?;
    } else {
        writeln!(writer, "Records:")
            .map_err(|error| MissiveError::io("writing logs output", error))?;
        for record in &output.records {
            let level = record.level.as_deref().unwrap_or("-");
            let timestamp = record.timestamp.as_deref().unwrap_or("-");
            writeln!(
                writer,
                "  #{} {} {} {}:{} {}",
                record.sequence,
                redact_text(timestamp),
                redact_text(level),
                redact_text(&record.path),
                record.line_number,
                redact_text(&record.message)
            )
            .map_err(|error| MissiveError::io("writing logs output", error))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_source_accepts_aliases() {
        assert_eq!(
            canonical_source("files").expect("files"),
            PROFILE_LOG_SOURCE
        );
        assert_eq!(
            canonical_source("journal").expect("journal"),
            EVENT_JOURNAL_SOURCE
        );
        assert_eq!(
            canonical_source("gateway").expect("gateway"),
            GATEWAY_SERVICE_SOURCE
        );
        assert!(canonical_source("unknown").is_err());
    }

    #[test]
    fn log_record_redacts_text_assignments_and_json_fields() {
        let raw = log_record_from_line("INFO token=value-hidden-in-output still visible");
        assert_eq!(
            raw.message,
            format!("INFO token={} still visible", crate::REDACTED)
        );

        let json = log_record_from_line(
            r#"{"level":"INFO","message":"Authorization: Bearer value-hidden-in-output","fields":{"api_key":"value-hidden-in-output"}}"#,
        );
        assert_eq!(json.level.as_deref(), Some("info"));
        assert_eq!(json.message, format!("Authorization: {}", crate::REDACTED));
        let fields = json.fields.expect("fields");
        assert_eq!(fields["fields"]["api_key"], crate::REDACTED);
        assert!(!fields.to_string().contains("value-hidden-in-output"));
    }
}
