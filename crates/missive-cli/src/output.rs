//! Output rendering and redaction helpers for the missive CLI.
//!
//! The CLI keeps human output, single-document JSON, newline-delimited JSON,
//! and quiet mode behind one small contract so future command tickets can add
//! real behavior without inventing incompatible machine-readable shapes.

use std::collections::BTreeMap;
use std::io::Write;

use missive_core::{LoadedConfig, MissiveError, OutputFormat, Result};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::GlobalArgs;

/// Stable schema marker used by JSON and NDJSON command output.
pub const OUTPUT_SCHEMA_VERSION: &str = "missive.output.v1";

/// Redaction marker used whenever auth material, tokens, headers, or secret-like
/// values would otherwise be rendered.
pub const REDACTED: &str = "[REDACTED]";

/// The output mode selected for one CLI invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Human-readable terminal output.
    Human,
    /// A single machine-readable JSON document.
    Json,
    /// One machine-readable JSON object per line.
    Ndjson,
    /// Suppress non-error output.
    Quiet,
}

impl OutputMode {
    /// Resolves global CLI flags into one output mode.
    pub fn from_globals(globals: &GlobalArgs) -> Result<Self> {
        Self::from_globals_and_config(globals, OutputFormat::Human)
    }

    /// Resolves global CLI flags and a validated config output default into one
    /// output mode. Explicit flags always override configuration defaults.
    pub fn from_globals_and_config(
        globals: &GlobalArgs,
        default_format: OutputFormat,
    ) -> Result<Self> {
        if globals.quiet {
            return Ok(Self::Quiet);
        }

        if globals.json && globals.ndjson {
            return Err(MissiveError::validation(
                "--json and --ndjson cannot be used together for one command output stream",
            )
            .with_help(
                "Choose --json for a single document or --ndjson for one JSON object per line.",
            ));
        }

        if globals.ndjson {
            Ok(Self::Ndjson)
        } else if globals.json {
            Ok(Self::Json)
        } else {
            Ok(match default_format {
                OutputFormat::Human => Self::Human,
                OutputFormat::Json => Self::Json,
                OutputFormat::Ndjson => Self::Ndjson,
                OutputFormat::Quiet => Self::Quiet,
            })
        }
    }

    fn for_error(globals: &GlobalArgs) -> Self {
        if globals.ndjson {
            Self::Ndjson
        } else if globals.json {
            Self::Json
        } else {
            Self::Human
        }
    }
}

/// Secret-free summary of the configuration selected for this invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigLoadStatus {
    /// Discovery source, for example `explicit_path`, `environment`, `xdg`, or
    /// `built_in_default`.
    pub source: String,
    /// Config file path when a file was loaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Selected profile after applying `--profile` or `default_profile`.
    pub profile: String,
    /// Effective default output format from the selected config file.
    pub output_format: String,
    /// Number of config-seeded agent entries.
    pub agent_count: usize,
    /// Number of named auth references. Raw secrets are never part of this summary.
    pub auth_ref_count: usize,
}

impl ConfigLoadStatus {
    /// Builds a summary from a loaded, validated configuration.
    #[must_use]
    pub fn from_loaded(loaded: &LoadedConfig) -> Self {
        Self {
            source: loaded.source.kind.as_str().to_owned(),
            path: loaded
                .source
                .path
                .as_ref()
                .map(|path| path.display().to_string()),
            profile: loaded.selected_profile.clone(),
            output_format: loaded
                .output_format()
                .unwrap_or(loaded.config.output.format)
                .as_str()
                .to_owned(),
            agent_count: loaded.config.agents.len(),
            auth_ref_count: loaded.config.auth_refs.len(),
        }
    }
}

/// Stable status record emitted by the currently implemented command skeleton.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandStatus {
    /// CLI command name, or `root` when no subcommand was supplied.
    pub command: String,
    /// Stable status keyword for automation.
    pub status: String,
    /// Whether this command has operational behavior beyond parser/rendering work.
    pub implemented: bool,
    /// Secret-free configuration source summary for this invocation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigLoadStatus>,
    /// Human-readable status text.
    pub message: String,
}

impl CommandStatus {
    /// Creates the placeholder status for a parsed command whose real behavior is
    /// intentionally implemented by a later ticket.
    #[must_use]
    pub fn parsed(command: &str) -> Self {
        Self {
            command: command.to_owned(),
            status: "parsed".to_owned(),
            implemented: false,
            config: None,
            message: format!(
                "missive: '{command}' command parsed; implementation lands in a later ticket"
            ),
        }
    }

    /// Creates the structured status used when no subcommand was supplied in a
    /// machine-readable output mode.
    #[must_use]
    pub fn root_help_available() -> Self {
        Self {
            command: "root".to_owned(),
            status: "help_available".to_owned(),
            implemented: true,
            config: None,
            message: "missive: no command supplied; run 'missive --help' for usage".to_owned(),
        }
    }

    /// Attaches a secret-free loaded configuration summary.
    #[must_use]
    pub fn with_config(mut self, loaded: &LoadedConfig) -> Self {
        self.config = Some(ConfigLoadStatus::from_loaded(loaded));
        self
    }
}

/// Renders successful command output according to the selected mode.
pub fn render_success<W, T>(
    writer: &mut W,
    mode: OutputMode,
    kind: &str,
    data: &T,
    human_message: &str,
) -> Result<()>
where
    W: Write,
    T: Serialize,
{
    match mode {
        OutputMode::Human => writeln!(writer, "{}", redact_text(human_message))
            .map_err(|error| MissiveError::io("writing human output", error)),
        OutputMode::Json => write_envelope(writer, true, kind, None, data),
        OutputMode::Ndjson => write_envelope(writer, true, kind, Some(0), data),
        OutputMode::Quiet => Ok(()),
    }
}

/// Renders an error according to output flags that were already parsed.
pub fn render_error<W>(writer: &mut W, globals: &GlobalArgs, error: &MissiveError) -> Result<()>
where
    W: Write,
{
    let report = error.to_report();

    match OutputMode::for_error(globals) {
        OutputMode::Human | OutputMode::Quiet => {
            writeln!(writer, "{}", redact_text(error.message()))
                .map_err(|io_error| MissiveError::io("writing error output", io_error))
        }
        OutputMode::Json => write_envelope(writer, false, "error", None, &report),
        OutputMode::Ndjson => write_envelope(writer, false, "error", Some(0), &report),
    }
}

fn write_envelope<W, T>(
    writer: &mut W,
    ok: bool,
    kind: &str,
    sequence: Option<u64>,
    data: &T,
) -> Result<()>
where
    W: Write,
    T: Serialize,
{
    let envelope = envelope_value(ok, kind, sequence, data)?;

    if sequence.is_some() {
        serde_json::to_writer(&mut *writer, &envelope).map_err(json_write_error)?;
        writeln!(writer).map_err(|error| MissiveError::io("writing NDJSON output", error))
    } else {
        serde_json::to_writer_pretty(&mut *writer, &envelope).map_err(json_write_error)?;
        writeln!(writer).map_err(|error| MissiveError::io("writing JSON output", error))
    }
}

fn envelope_value<T>(ok: bool, kind: &str, sequence: Option<u64>, data: &T) -> Result<Value>
where
    T: Serialize,
{
    let data = serde_json::to_value(data).map_err(json_encode_error)?;
    let mut envelope = Map::new();

    envelope.insert(
        "schema_version".to_owned(),
        Value::String(OUTPUT_SCHEMA_VERSION.to_owned()),
    );
    envelope.insert("ok".to_owned(), Value::Bool(ok));
    envelope.insert("kind".to_owned(), Value::String(kind.to_owned()));
    if let Some(sequence) = sequence {
        envelope.insert("sequence".to_owned(), Value::from(sequence));
    }
    envelope.insert("data".to_owned(), redact_json(&data));

    Ok(Value::Object(envelope))
}

fn json_encode_error(error: serde_json::Error) -> MissiveError {
    MissiveError::orchestration("failed to encode structured output")
        .with_source(error)
        .with_help("Ensure command output data can be represented as JSON.")
}

fn json_write_error(error: serde_json::Error) -> MissiveError {
    MissiveError::io("writing structured output", error.into())
}

/// Redacts a header value when the header name or value is sensitive.
#[must_use]
pub fn redact_header(name: &str, value: &str) -> String {
    if is_secret_key(name) {
        return redact_header_value(value);
    }

    redact_text(value)
}

/// Redacts an iterable of HTTP-style headers into a deterministic map.
#[must_use]
pub fn redact_headers<I, K, V>(headers: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    headers
        .into_iter()
        .map(|(name, value)| {
            let name = name.into();
            let value = value.into();
            let redacted = redact_header(&name, &value);
            (name, redacted)
        })
        .collect()
}

/// Redacts a JSON value recursively by key name, header name, and auth-like text.
#[must_use]
pub fn redact_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(redact_json_object(object, false)),
        Value::Array(items) => Value::Array(items.iter().map(redact_json).collect()),
        Value::String(text) => Value::String(redact_text(text)),
        other => other.clone(),
    }
}

fn redact_json_object(object: &Map<String, Value>, headers_context: bool) -> Map<String, Value> {
    let mut redacted = Map::new();

    for (key, value) in object {
        let lower_key = key.to_ascii_lowercase();
        let child_headers_context = lower_key == "headers" || lower_key == "http_headers";
        let value = if headers_context {
            redact_header_json_value(key, value)
        } else if is_secret_key(key) {
            Value::String(REDACTED.to_owned())
        } else if child_headers_context {
            match value {
                Value::Object(headers) => Value::Object(redact_json_object(headers, true)),
                other => redact_json(other),
            }
        } else {
            redact_json(value)
        };

        redacted.insert(key.clone(), value);
    }

    redacted
}

fn redact_header_json_value(name: &str, value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_header(name, text)),
        other if is_secret_key(name) => {
            let _ = other;
            Value::String(REDACTED.to_owned())
        }
        other => redact_json(other),
    }
}

fn redact_header_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return REDACTED.to_owned();
    }

    if let Some((scheme, _)) = trimmed.split_once(char::is_whitespace) {
        if is_auth_scheme(scheme) {
            return format!("{scheme} {REDACTED}");
        }
    }

    REDACTED.to_owned()
}

/// Redacts secret-like fragments in free-form text.
#[must_use]
pub fn redact_text(input: &str) -> String {
    let mut output = input.to_owned();

    for scheme in ["Bearer", "Basic", "Token"] {
        output = redact_after_auth_scheme(&output, scheme);
    }

    output
}

fn redact_after_auth_scheme(input: &str, scheme: &str) -> String {
    let needle = format!("{scheme} ");
    let mut output = String::with_capacity(input.len());
    let mut remaining = input;

    while let Some(index) = find_ascii_case_insensitive(remaining, &needle) {
        let prefix_end = index + needle.len();
        output.push_str(&remaining[..prefix_end]);
        remaining = &remaining[prefix_end..];

        let secret_end = remaining
            .find(|character: char| {
                character.is_whitespace() || character == ',' || character == ';'
            })
            .unwrap_or(remaining.len());

        if secret_end > 0 {
            output.push_str(REDACTED);
        }
        remaining = &remaining[secret_end..];
    }

    output.push_str(remaining);
    output
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn is_auth_scheme(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "bearer" | "basic" | "token" | "apikey" | "api-key"
    )
}

fn is_secret_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();

    matches!(
        normalized.as_str(),
        "authorization"
            | "proxyauthorization"
            | "cookie"
            | "setcookie"
            | "apikey"
            | "xapikey"
            | "xauthtoken"
            | "xcsrftoken"
            | "token"
            | "secret"
            | "password"
            | "passwd"
            | "credential"
            | "credentials"
            | "clientsecret"
            | "refreshtoken"
            | "accesstoken"
            | "accesskey"
            | "privatekey"
            | "sessiontoken"
    ) || normalized.ends_with("token")
        || normalized.ends_with("secret")
        || normalized.ends_with("password")
        || normalized.ends_with("apikey")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn output_mode_resolves_global_flags() {
        assert_eq!(
            OutputMode::from_globals(&GlobalArgs::default()).expect("mode"),
            OutputMode::Human
        );
        assert_eq!(
            OutputMode::from_globals(&GlobalArgs {
                json: true,
                ..GlobalArgs::default()
            })
            .expect("mode"),
            OutputMode::Json
        );
        assert_eq!(
            OutputMode::from_globals(&GlobalArgs {
                ndjson: true,
                ..GlobalArgs::default()
            })
            .expect("mode"),
            OutputMode::Ndjson
        );
        assert_eq!(
            OutputMode::from_globals(&GlobalArgs {
                quiet: true,
                json: true,
                ndjson: true,
                ..GlobalArgs::default()
            })
            .expect("quiet wins"),
            OutputMode::Quiet
        );
    }

    #[test]
    fn output_mode_rejects_conflicting_machine_readable_flags() {
        let error = OutputMode::from_globals(&GlobalArgs {
            json: true,
            ndjson: true,
            ..GlobalArgs::default()
        })
        .expect_err("mode should be invalid");

        assert!(error.to_string().contains("--json and --ndjson"));
    }

    #[test]
    fn success_json_uses_stable_envelope_and_redacts_data() {
        let mut output = Vec::new();
        let data = json!({
            "command": "agent",
            "headers": {
                "authorization": "Bearer value-hidden-in-output",
                "x-request-id": "visible"
            }
        });

        render_success(
            &mut output,
            OutputMode::Json,
            "command_status",
            &data,
            "human message",
        )
        .expect("render JSON");

        let value: Value = serde_json::from_slice(&output).expect("output should parse");
        assert_eq!(value["schema_version"], OUTPUT_SCHEMA_VERSION);
        assert_eq!(value["ok"], true);
        assert_eq!(value["kind"], "command_status");
        assert_eq!(
            value["data"]["headers"]["authorization"],
            format!("Bearer {REDACTED}")
        );
        assert_eq!(value["data"]["headers"]["x-request-id"], "visible");
        assert!(
            !String::from_utf8(output)
                .expect("UTF-8")
                .contains("value-hidden-in-output")
        );
    }

    #[test]
    fn ndjson_output_is_one_json_object_per_line() {
        let mut output = Vec::new();
        let data = CommandStatus::parsed("events");

        render_success(
            &mut output,
            OutputMode::Ndjson,
            "command_status",
            &data,
            &data.message,
        )
        .expect("render NDJSON");

        let output = String::from_utf8(output).expect("UTF-8");
        let lines: Vec<_> = output.lines().collect();

        assert_eq!(lines.len(), 1);
        let value: Value = serde_json::from_str(lines[0]).expect("line should parse as JSON");
        assert_eq!(value["sequence"], 0);
        assert_eq!(value["data"]["command"], "events");
    }

    #[test]
    fn redacts_auth_headers_and_secret_json_fields() {
        assert_eq!(
            redact_header("Authorization", "Bearer value-hidden-in-output"),
            format!("Bearer {REDACTED}")
        );
        assert_eq!(
            redact_header("X-Api-Key", "value-hidden-in-output"),
            REDACTED
        );

        let value = json!({
            "token": "value-hidden-in-output",
            "nested": {
                "client_secret": "value-hidden-in-output",
                "safe": "Bearer value-hidden-in-output"
            }
        });
        let redacted = redact_json(&value);
        let rendered = redacted.to_string();

        assert_eq!(redacted["token"], REDACTED);
        assert_eq!(redacted["nested"]["client_secret"], REDACTED);
        assert_eq!(redacted["nested"]["safe"], format!("Bearer {REDACTED}"));
        assert!(!rendered.contains("value-hidden-in-output"));
    }

    #[test]
    fn redact_headers_returns_deterministic_map() {
        let headers = redact_headers([
            ("X-Request-Id", "visible"),
            ("Authorization", "Basic value-hidden-in-output"),
        ]);
        let entries: Vec<_> = headers.into_iter().collect();

        assert_eq!(
            entries,
            [
                ("Authorization".to_owned(), format!("Basic {REDACTED}")),
                ("X-Request-Id".to_owned(), "visible".to_owned()),
            ]
        );
    }
}
