#![doc = "Observability scaffolding for missive."]

use std::collections::BTreeMap;
use std::fmt;

use missive_core::{MissiveError, Result};
use serde_json::{Map, Number, Value};
use tracing::{Dispatch, Event, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::field::RecordFields;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::fmt::{FmtContext, FormattedFields};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::{LookupSpan, Registry};

/// Cargo package name for this crate.
pub const CRATE_NAME: &str = "missive-observe";

/// Short description of this crate's target responsibility.
pub const CRATE_PURPOSE: &str = "tracing, logs, diagnostics, and event export helpers";

/// Environment variable used by tracing-subscriber compatible filters.
pub const RUST_LOG_ENV: &str = "RUST_LOG";

/// Environment variable used to request human or JSON missive log formatting.
pub const MISSIVE_LOG_FORMAT_ENV: &str = "MISSIVE_LOG_FORMAT";

/// Backwards-compatible boolean environment variable for JSON log formatting.
pub const MISSIVE_LOG_JSON_ENV: &str = "MISSIVE_LOG_JSON";

/// Redaction marker used by the observability layer for secret-like values.
pub const REDACTED: &str = "[REDACTED]";

/// Human-readable default filter used when no diagnostic flags or env filters are set.
pub const DEFAULT_FILTER: &str = "warn";

/// Log format produced by the observability subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Compact human-readable one-line events.
    Human,
    /// One JSON object per event.
    Json,
}

impl LogFormat {
    /// Stable config/env spelling for this format.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Json => "json",
        }
    }
}

/// Subscriber initialization settings shared by the CLI, gateway, adapters, and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObserveConfig {
    /// EnvFilter directive string, usually sourced from `RUST_LOG` or CLI verbosity.
    pub filter: String,
    /// Event formatter selection.
    pub format: LogFormat,
    /// Whether human logs may include ANSI color. The current formatter is plain,
    /// but the setting is preserved for callers and future formatters.
    pub ansi: bool,
}

impl ObserveConfig {
    /// Builds a config directly from explicit values.
    #[must_use]
    pub fn new(filter: impl Into<String>, format: LogFormat, ansi: bool) -> Self {
        Self {
            filter: filter.into(),
            format,
            ansi,
        }
    }

    /// Builds a diagnostics config from deterministic environment input and
    /// parsed global flags.
    pub fn from_environment(
        environment: &BTreeMap<String, String>,
        trace: bool,
        verbose: u8,
        no_color: bool,
    ) -> Result<Self> {
        let filter = environment
            .get(RUST_LOG_ENV)
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| filter_from_verbosity(trace, verbose).to_owned());
        let format = log_format_from_environment(environment)?;
        let ansi = !no_color && format == LogFormat::Human && !environment.contains_key("NO_COLOR");

        Ok(Self {
            filter,
            format,
            ansi,
        })
    }

    /// Returns a secret-free summary suitable for bootstrap diagnostics.
    #[must_use]
    pub fn redacted_summary(&self) -> BTreeMap<&'static str, String> {
        BTreeMap::from([
            ("filter", redact_text(&self.filter)),
            ("format", self.format.as_str().to_owned()),
            ("ansi", self.ansi.to_string()),
        ])
    }
}

fn filter_from_verbosity(trace: bool, verbose: u8) -> &'static str {
    if trace {
        "trace"
    } else {
        match verbose {
            0 => DEFAULT_FILTER,
            1 => "info",
            2 => "debug",
            _ => "trace",
        }
    }
}

fn log_format_from_environment(environment: &BTreeMap<String, String>) -> Result<LogFormat> {
    if truthy_env(environment.get(MISSIVE_LOG_JSON_ENV)).unwrap_or(false) {
        return Ok(LogFormat::Json);
    }

    let Some(raw_format) = environment.get(MISSIVE_LOG_FORMAT_ENV) else {
        return Ok(LogFormat::Human);
    };
    let format = raw_format.trim().to_ascii_lowercase();
    match format.as_str() {
        "" | "human" | "text" | "compact" => Ok(LogFormat::Human),
        "json" => Ok(LogFormat::Json),
        other => Err(MissiveError::config(format!(
            "{MISSIVE_LOG_FORMAT_ENV} value {other:?} is not supported"
        ))
        .with_help("Use MISSIVE_LOG_FORMAT=human or MISSIVE_LOG_FORMAT=json.")),
    }
}

fn truthy_env(value: Option<&String>) -> Option<bool> {
    let raw = value?.trim().to_ascii_lowercase();
    Some(matches!(raw.as_str(), "1" | "true" | "yes" | "on" | "json"))
}

/// Builds a tracing dispatch that writes logs to stderr.
pub fn dispatch(config: ObserveConfig) -> Result<Dispatch> {
    dispatch_with_writer(config, std::io::stderr)
}

/// Builds a tracing dispatch with a caller-provided writer.
///
/// This is primarily used by tests and embedders that need to capture logs
/// without redirecting process stderr.
pub fn dispatch_with_writer<W>(config: ObserveConfig, make_writer: W) -> Result<Dispatch>
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let filter = EnvFilter::try_new(&config.filter).map_err(|error| {
        MissiveError::config(format!(
            "invalid {RUST_LOG_ENV} tracing filter {:?}",
            redact_text(&config.filter)
        ))
        .with_source(error)
        .with_help(
            "Use tracing EnvFilter syntax such as 'info', 'missive_cli=debug', or 'missive=trace'.",
        )
    })?;

    let layer = tracing_subscriber::fmt::layer()
        .event_format(RedactingEventFormatter {
            format: config.format,
        })
        .fmt_fields(RedactingFields)
        .with_ansi(config.ansi)
        .with_writer(make_writer);
    let subscriber = Registry::default().with(filter).with(layer);

    Ok(Dispatch::new(subscriber))
}

/// Runs a closure with a scoped tracing subscriber.
///
/// Scoped dispatches keep CLI tests deterministic because each invocation can
/// use its own filter and writer without racing to install a process-global
/// subscriber.
pub fn with_observer<T, F>(config: ObserveConfig, f: F) -> Result<T>
where
    F: FnOnce() -> T,
{
    let dispatch = dispatch(config)?;
    Ok(tracing::dispatcher::with_default(&dispatch, f))
}

/// Installs the observability subscriber as the process-global tracing default.
///
/// Gateway and adapter embedders can use this when they own the process. The CLI
/// uses [`with_observer`] so tests can reconfigure diagnostics per invocation.
pub fn init_global(config: ObserveConfig) -> Result<()> {
    let dispatch = dispatch(config)?;
    tracing::dispatcher::set_global_default(dispatch).map_err(|error| {
        MissiveError::orchestration("failed to initialize global tracing subscriber")
            .with_source(error)
            .with_help("Only initialize missive tracing once per process, or use scoped observers in tests.")
    })
}

/// Emits a redacted bootstrap diagnostic after the subscriber is installed.
///
/// This gives `RUST_LOG`, `--verbose`, and `--trace` deterministic behavior
/// before later tickets add operation-level spans and events.
pub fn emit_bootstrap_diagnostic(config: &ObserveConfig) {
    let summary = config.redacted_summary();
    let filter = summary
        .get("filter")
        .map(String::as_str)
        .unwrap_or(DEFAULT_FILTER);
    let format = summary
        .get("format")
        .map(String::as_str)
        .unwrap_or(LogFormat::Human.as_str());
    let ansi = summary.get("ansi").map(String::as_str).unwrap_or("false");

    tracing::info!(
        target: "missive_observe",
        filter = %filter,
        format = %format,
        ansi = %ansi,
        "diagnostics initialized"
    );
    tracing::trace!(
        target: "missive_observe",
        filter = %filter,
        format = %format,
        ansi = %ansi,
        "trace diagnostics enabled"
    );
}

#[derive(Debug, Clone)]
struct RedactingEventFormatter {
    format: LogFormat,
}

impl<S, N> FormatEvent<S, N> for RedactingEventFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        match self.format {
            LogFormat::Human => self.format_human_event(ctx, &mut writer, event),
            LogFormat::Json => self.format_json_event(ctx, &mut writer, event),
        }
    }
}

impl RedactingEventFormatter {
    fn format_human_event<S, N>(
        &self,
        ctx: &FmtContext<'_, S, N>,
        writer: &mut Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
        N: for<'writer> FormatFields<'writer> + 'static,
    {
        let metadata = event.metadata();
        let mut fields = RedactingVisitor::default();
        event.record(&mut fields);
        let message = fields.remove_string("message");

        write!(writer, "{} {}: ", metadata.level(), metadata.target())?;
        if let Some(message) = message {
            write!(writer, "{}", message)?;
        }

        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                let extensions = span.extensions();
                let formatted = extensions.get::<FormattedFields<N>>();
                if let Some(formatted) = formatted {
                    if !formatted.fields.is_empty() {
                        write!(writer, " span.{}{{{}}}", span.name(), formatted.fields)?;
                    } else {
                        write!(writer, " span.{}", span.name())?;
                    }
                } else {
                    write!(writer, " span.{}", span.name())?;
                }
            }
        }

        write_human_fields(writer, &fields.fields)?;
        writeln!(writer)
    }

    fn format_json_event<S, N>(
        &self,
        ctx: &FmtContext<'_, S, N>,
        writer: &mut Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
        N: for<'writer> FormatFields<'writer> + 'static,
    {
        let metadata = event.metadata();
        let mut fields = RedactingVisitor::default();
        event.record(&mut fields);

        let mut object = Map::new();
        object.insert(
            "level".to_owned(),
            Value::String(metadata.level().to_string()),
        );
        object.insert(
            "target".to_owned(),
            Value::String(metadata.target().to_owned()),
        );
        object.insert(
            "fields".to_owned(),
            Value::Object(fields.into_json_object()),
        );

        let spans = json_scope(ctx);
        if !spans.is_empty() {
            object.insert("spans".to_owned(), Value::Array(spans));
        }

        let line = serde_json::to_string(&Value::Object(object)).map_err(|_| fmt::Error)?;
        writeln!(writer, "{line}")
    }
}

fn json_scope<S, N>(ctx: &FmtContext<'_, S, N>) -> Vec<Value>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    let Some(scope) = ctx.event_scope() else {
        return Vec::new();
    };

    scope
        .from_root()
        .map(|span| {
            let mut object = Map::new();
            object.insert("name".to_owned(), Value::String(span.name().to_owned()));
            let extensions = span.extensions();
            if let Some(fields) = extensions.get::<FormattedFields<N>>() {
                if !fields.fields.is_empty() {
                    object.insert(
                        "fields".to_owned(),
                        Value::String(redact_text(&fields.fields)),
                    );
                }
            }
            Value::Object(object)
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
struct RedactingFields;

impl<'writer> FormatFields<'writer> for RedactingFields {
    fn format_fields<R: RecordFields>(
        &self,
        mut writer: Writer<'writer>,
        fields: R,
    ) -> fmt::Result {
        let mut visitor = RedactingVisitor::default();
        fields.record(&mut visitor);
        write_human_fields(&mut writer, &visitor.fields).map(|_| ())
    }
}

#[derive(Debug, Clone, Default)]
struct RedactingVisitor {
    fields: BTreeMap<String, Value>,
}

impl RedactingVisitor {
    fn insert(&mut self, name: &str, value: Value) {
        let redacted = if is_secret_key(name) {
            Value::String(REDACTED.to_owned())
        } else {
            redact_json_value(value)
        };
        self.fields.insert(name.to_owned(), redacted);
    }

    fn remove_string(&mut self, name: &str) -> Option<String> {
        self.fields
            .remove(name)
            .map(|value| display_value(&value, false))
    }

    fn into_json_object(self) -> Map<String, Value> {
        self.fields.into_iter().collect()
    }
}

impl tracing::field::Visit for RedactingVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        self.insert(
            field.name(),
            Value::String(redact_text(&format!("{value:?}"))),
        );
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.insert(field.name(), Value::String(redact_text(value)));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.insert(field.name(), Value::Bool(value));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.insert(field.name(), Value::Number(value.into()));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.insert(field.name(), Value::Number(value.into()));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        let value = Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(redact_text(&value.to_string())));
        self.insert(field.name(), value);
    }
}

fn write_human_fields(writer: &mut Writer<'_>, fields: &BTreeMap<String, Value>) -> fmt::Result {
    for (key, value) in fields {
        write!(writer, " {key}={}", display_value(value, true))?;
    }
    Ok(())
}

fn display_value(value: &Value, quote_strings: bool) -> String {
    match value {
        Value::String(text) if quote_strings && needs_quotes(text) => {
            serde_json::to_string(text).unwrap_or_else(|_| format!("\"{}\"", redact_text(text)))
        }
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| redact_text(&other.to_string())),
    }
}

fn needs_quotes(text: &str) -> bool {
    text.is_empty()
        || text
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, '"' | '\'' | '='))
}

fn redact_json_value(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let redacted = object
                .into_iter()
                .map(|(key, value)| {
                    if is_secret_key(&key) {
                        (key, Value::String(REDACTED.to_owned()))
                    } else {
                        (key, redact_json_value(value))
                    }
                })
                .collect();
            Value::Object(redacted)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(redact_json_value).collect()),
        Value::String(text) => Value::String(redact_text(&text)),
        other => other,
    }
}

/// Redacts secret-like fragments in free-form log text.
#[must_use]
pub fn redact_text(input: &str) -> String {
    let mut output = input.to_owned();

    for scheme in ["Bearer", "Basic", "Token", "ApiKey", "Api-Key"] {
        output = redact_after_auth_scheme(&output, scheme);
    }

    redact_secret_assignments(&output)
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

fn redact_secret_assignments(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;

    while index < chars.len() {
        if let Some(key_end) = secret_key_end_at(&chars, index) {
            let mut separator = key_end;
            if separator < chars.len() && matches!(chars[separator], '"' | '\'') {
                separator += 1;
            }
            while separator < chars.len() && chars[separator].is_whitespace() {
                separator += 1;
            }

            if separator < chars.len() && matches!(chars[separator], '=' | ':') {
                for character in &chars[index..=separator] {
                    output.push(*character);
                }
                index = separator + 1;
                while index < chars.len() && chars[index].is_whitespace() {
                    output.push(chars[index]);
                    index += 1;
                }

                if index < chars.len() && matches!(chars[index], '"' | '\'') {
                    let quote = chars[index];
                    output.push(quote);
                    output.push_str(REDACTED);
                    index += 1;
                    while index < chars.len() && chars[index] != quote {
                        index += 1;
                    }
                    if index < chars.len() {
                        output.push(chars[index]);
                        index += 1;
                    }
                } else {
                    output.push_str(REDACTED);
                    while index < chars.len() && !is_unquoted_secret_delimiter(chars[index]) {
                        index += 1;
                    }
                }
                continue;
            }
        }

        output.push(chars[index]);
        index += 1;
    }

    output
}

fn secret_key_end_at(chars: &[char], index: usize) -> Option<usize> {
    if index > 0 && is_key_character(chars[index - 1]) {
        return None;
    }
    if !is_key_character(chars[index]) {
        return None;
    }

    let mut end = index;
    let mut normalized = String::new();
    while end < chars.len() && is_key_character(chars[end]) {
        if chars[end].is_ascii_alphanumeric() {
            normalized.extend(chars[end].to_lowercase());
        }
        end += 1;
    }

    is_secret_key_normalized(&normalized).then_some(end)
}

fn is_key_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
}

fn is_unquoted_secret_delimiter(character: char) -> bool {
    character.is_whitespace() || matches!(character, ',' | ';' | '}' | ']')
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn is_secret_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();

    is_secret_key_normalized(&normalized)
}

fn is_secret_key_normalized(normalized: &str) -> bool {
    matches!(
        normalized,
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

/// Returns metadata for this crate.
#[must_use]
pub const fn crate_info() -> missive_core::CrateInfo {
    missive_core::CrateInfo::new(CRATE_NAME, CRATE_PURPOSE)
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedBuffer {
        fn text(&self) -> String {
            let bytes = self.0.lock().expect("buffer lock").clone();
            String::from_utf8(bytes).expect("logs should be UTF-8")
        }
    }

    struct SharedBufferWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for SharedBufferWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("buffer lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for SharedBuffer {
        type Writer = SharedBufferWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            SharedBufferWriter(Arc::clone(&self.0))
        }
    }

    #[test]
    fn crate_info_describes_observe_layer() {
        let info = crate_info();

        assert_eq!(info.name(), CRATE_NAME);
        assert!(info.purpose().contains("tracing"));
    }

    #[test]
    fn config_resolves_env_filter_verbosity_and_json_format() {
        let environment = BTreeMap::new();
        let config =
            ObserveConfig::from_environment(&environment, false, 0, false).expect("config");
        assert_eq!(config.filter, DEFAULT_FILTER);
        assert_eq!(config.format, LogFormat::Human);

        let config =
            ObserveConfig::from_environment(&environment, false, 1, false).expect("config");
        assert_eq!(config.filter, "info");

        let config =
            ObserveConfig::from_environment(&environment, false, 2, false).expect("config");
        assert_eq!(config.filter, "debug");

        let config = ObserveConfig::from_environment(&environment, true, 0, false).expect("config");
        assert_eq!(config.filter, "trace");

        let environment = BTreeMap::from([
            (RUST_LOG_ENV.to_owned(), "missive_cli=debug".to_owned()),
            (MISSIVE_LOG_FORMAT_ENV.to_owned(), "json".to_owned()),
        ]);
        let config = ObserveConfig::from_environment(&environment, true, 3, true).expect("config");
        assert_eq!(config.filter, "missive_cli=debug");
        assert_eq!(config.format, LogFormat::Json);
        assert!(!config.ansi);
    }

    #[test]
    fn invalid_log_format_is_actionable() {
        let environment = BTreeMap::from([(MISSIVE_LOG_FORMAT_ENV.to_owned(), "xml".to_owned())]);
        let error = ObserveConfig::from_environment(&environment, false, 0, false)
            .expect_err("invalid log format should fail");

        assert!(error.to_string().contains(MISSIVE_LOG_FORMAT_ENV));
        assert_eq!(
            error.help(),
            Some("Use MISSIVE_LOG_FORMAT=human or MISSIVE_LOG_FORMAT=json.")
        );
    }

    #[test]
    fn human_logs_are_filtered_and_redacted() {
        let buffer = SharedBuffer::default();
        let config = ObserveConfig::new("info", LogFormat::Human, false);
        let dispatch = dispatch_with_writer(config, buffer.clone()).expect("dispatch");

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::debug!(target: "missive_observe_test", token = "debug-secret", "hidden");
            tracing::info!(
                target: "missive_observe_test",
                authorization = "Bearer human-secret",
                x_request_id = "visible-id",
                "connected with Basic message-secret"
            );
        });

        let output = buffer.text();
        assert!(output.contains("INFO missive_observe_test"));
        assert!(output.contains("connected with Basic [REDACTED]"));
        assert!(output.contains("authorization=[REDACTED]"));
        assert!(output.contains("x_request_id=visible-id"));
        assert!(!output.contains("human-secret"));
        assert!(!output.contains("message-secret"));
        assert!(!output.contains("debug-secret"));
    }

    #[test]
    fn json_logs_are_machine_readable_and_redacted() {
        let buffer = SharedBuffer::default();
        let config = ObserveConfig::new("info", LogFormat::Json, false);
        let dispatch = dispatch_with_writer(config, buffer.clone()).expect("dispatch");

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!(
                target: "missive_observe_test",
                cookie = "private-cookie",
                answer = 42_u64,
                "json log Bearer json-secret"
            );
        });

        let output = buffer.text();
        let line = output.lines().next().expect("one JSON log line");
        let value: Value = serde_json::from_str(line).expect("JSON log line");

        assert_eq!(value["level"], "INFO");
        assert_eq!(value["target"], "missive_observe_test");
        assert_eq!(value["fields"]["cookie"], REDACTED);
        assert_eq!(value["fields"]["answer"], 42);
        assert_eq!(value["fields"]["message"], "json log Bearer [REDACTED]");
        assert!(!output.contains("private-cookie"));
        assert!(!output.contains("json-secret"));
    }

    #[test]
    fn redacts_secret_assignments_in_debug_text() {
        let input = r#"headers={"Authorization":"Bearer raw-secret","client_secret":"hidden"} token=another-secret safe=visible"#;
        let redacted = redact_text(input);

        assert!(redacted.contains("Authorization\":\"[REDACTED]"));
        assert!(redacted.contains("client_secret\":\"[REDACTED]"));
        assert!(redacted.contains("token=[REDACTED]"));
        assert!(redacted.contains("safe=visible"));
        assert!(!redacted.contains("raw-secret"));
        assert!(!redacted.contains("another-secret"));
        assert!(!redacted.contains("hidden"));
    }
}
