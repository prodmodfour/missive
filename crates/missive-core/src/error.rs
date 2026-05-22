//! Error and result primitives shared across missive crates.

use std::error::Error as StdError;
use std::fmt::{self, Display};
use std::io;

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Standard result type for public missive APIs.
pub type Result<T, E = MissiveError> = std::result::Result<T, E>;

/// Stable high-level error categories used by humans, JSON output, and exit code mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    /// Local filesystem, process, or stream I/O failed.
    Io,
    /// Configuration discovery, parsing, or validation failed.
    Config,
    /// A2A protocol data, version, or semantic handling failed.
    Protocol,
    /// Network or transport negotiation failed.
    Transport,
    /// Local persistence or migration handling failed.
    Storage,
    /// Authentication, authorization, or secret lookup failed.
    Auth,
    /// User or API input failed validation before execution.
    Validation,
    /// Multi-step command, routing, gateway, or collective orchestration failed.
    Orchestration,
}

impl ErrorCategory {
    /// Returns the stable diagnostic code for this category.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Io => "missive::io",
            Self::Config => "missive::config",
            Self::Protocol => "missive::protocol",
            Self::Transport => "missive::transport",
            Self::Storage => "missive::storage",
            Self::Auth => "missive::auth",
            Self::Validation => "missive::validation",
            Self::Orchestration => "missive::orchestration",
        }
    }

    /// Returns the deterministic CLI exit code associated with this category.
    #[must_use]
    pub const fn exit_code(self) -> MissiveExitCode {
        match self {
            Self::Validation => MissiveExitCode::Usage,
            Self::Transport => MissiveExitCode::Unavailable,
            Self::Io => MissiveExitCode::Io,
            Self::Storage => MissiveExitCode::TemporaryFailure,
            Self::Protocol => MissiveExitCode::Protocol,
            Self::Auth => MissiveExitCode::Permission,
            Self::Config => MissiveExitCode::Config,
            Self::Orchestration => MissiveExitCode::Software,
        }
    }

    const fn human_label(self) -> &'static str {
        match self {
            Self::Io => "I/O",
            Self::Config => "configuration",
            Self::Protocol => "A2A protocol",
            Self::Transport => "transport",
            Self::Storage => "storage",
            Self::Auth => "authentication",
            Self::Validation => "validation",
            Self::Orchestration => "orchestration",
        }
    }
}

impl Display for ErrorCategory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Io => "io",
            Self::Config => "config",
            Self::Protocol => "protocol",
            Self::Transport => "transport",
            Self::Storage => "storage",
            Self::Auth => "auth",
            Self::Validation => "validation",
            Self::Orchestration => "orchestration",
        })
    }
}

/// Stable exit codes reserved for missive CLI error handling.
///
/// Values follow the widely used `sysexits.h` range where practical so shell
/// automation can distinguish usage, permission, protocol, I/O, and
/// configuration failures without parsing human text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum MissiveExitCode {
    /// Successful command completion.
    Success = 0,
    /// Command-line usage or validation failure.
    Usage = 64,
    /// Remote service, transport, or endpoint was unavailable.
    Unavailable = 69,
    /// Internal software or orchestration failure.
    Software = 70,
    /// Local or remote I/O failure.
    Io = 74,
    /// Temporary storage or state backend failure.
    TemporaryFailure = 75,
    /// Protocol-level failure.
    Protocol = 76,
    /// Authentication or authorization failure.
    Permission = 77,
    /// Configuration failure.
    Config = 78,
}

impl MissiveExitCode {
    /// Returns the numeric process exit status.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Returns the numeric process exit status as an `i32` for `std::process::exit`.
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self.as_u8() as i32
    }
}

/// Serializable error shape for `--json` and NDJSON event output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorReport {
    /// Stable diagnostic code, for example `missive::validation`.
    pub code: String,
    /// High-level error category.
    pub category: ErrorCategory,
    /// Human-readable message suitable for terminal output.
    pub message: String,
    /// Optional remediation hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// Source error chain rendered as strings from outermost source to root cause.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// Deterministic process exit code a CLI should use for this error.
    pub exit_code: u8,
}

/// Core missive error type with stable taxonomy and optional source chain.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct MissiveError {
    category: ErrorCategory,
    message: String,
    #[source]
    source: Option<Box<dyn StdError + Send + Sync + 'static>>,
    help: Option<String>,
}

impl MissiveError {
    /// Creates an error in the requested category with a category-prefixed message.
    #[must_use]
    pub fn new(category: ErrorCategory, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self {
            category,
            message: format!("{} error: {detail}", category.human_label()),
            source: None,
            help: None,
        }
    }

    /// Creates an I/O error with the failed action and source error preserved.
    #[must_use]
    pub fn io(action: impl Into<String>, source: io::Error) -> Self {
        let action = action.into();
        let source_message = source.to_string();
        Self {
            category: ErrorCategory::Io,
            message: format!("I/O error while {action}: {source_message}"),
            source: Some(Box::new(source)),
            help: None,
        }
    }

    /// Creates a configuration error.
    #[must_use]
    pub fn config(detail: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Config, detail)
    }

    /// Creates an A2A protocol error.
    #[must_use]
    pub fn protocol(detail: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Protocol, detail)
    }

    /// Creates a transport error.
    #[must_use]
    pub fn transport(detail: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Transport, detail)
    }

    /// Creates a storage error.
    #[must_use]
    pub fn storage(detail: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Storage, detail)
    }

    /// Creates an authentication or authorization error.
    #[must_use]
    pub fn auth(detail: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Auth, detail)
    }

    /// Creates a validation error.
    #[must_use]
    pub fn validation(detail: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Validation, detail)
    }

    /// Creates an orchestration error.
    #[must_use]
    pub fn orchestration(detail: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Orchestration, detail)
    }

    /// Attaches a source error while preserving the existing category and message.
    #[must_use]
    pub fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Attaches remediation help text for human and structured diagnostic output.
    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Returns the high-level category.
    #[must_use]
    pub const fn category(&self) -> ErrorCategory {
        self.category
    }

    /// Returns the stable diagnostic code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.category.code()
    }

    /// Returns the deterministic CLI exit code.
    #[must_use]
    pub const fn exit_code(&self) -> MissiveExitCode {
        self.category.exit_code()
    }

    /// Returns the formatted human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns optional remediation help.
    #[must_use]
    pub fn help(&self) -> Option<&str> {
        self.help.as_deref()
    }

    /// Converts this error to the stable JSON/NDJSON report shape.
    #[must_use]
    pub fn to_report(&self) -> ErrorReport {
        ErrorReport {
            code: self.code().to_owned(),
            category: self.category,
            message: self.message.clone(),
            help: self.help.clone(),
            sources: self.source_messages(),
            exit_code: self.exit_code().as_u8(),
        }
    }

    fn source_messages(&self) -> Vec<String> {
        let mut messages = Vec::new();
        let mut next = self.source();

        while let Some(source) = next {
            messages.push(source.to_string());
            next = source.source();
        }

        messages
    }
}

impl Diagnostic for MissiveError {
    fn code<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        Some(Box::new(self.code()))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        self.help
            .as_deref()
            .map(|help| Box::new(help) as Box<dyn Display + 'a>)
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use serde_json::json;

    use super::*;

    #[test]
    fn validation_error_renders_for_humans_and_miette() {
        let error = MissiveError::validation("agent alias must be lowercase")
            .with_help("Use lowercase ASCII letters, digits, '-' or '_'.");

        assert_eq!(
            error.to_string(),
            "validation error: agent alias must be lowercase"
        );
        assert_eq!(error.category(), ErrorCategory::Validation);
        assert_eq!(error.code(), "missive::validation");
        assert_eq!(error.exit_code(), MissiveExitCode::Usage);
        assert_eq!(
            miette::Diagnostic::code(&error).map(|code| code.to_string()),
            Some("missive::validation".to_owned())
        );
        assert_eq!(
            miette::Diagnostic::help(&error).map(|help| help.to_string()),
            Some("Use lowercase ASCII letters, digits, '-' or '_'.".to_owned())
        );
    }

    #[test]
    fn json_report_has_stable_fields() {
        let error = MissiveError::auth("missing bearer token environment variable")
            .with_help("Set the configured environment variable before retrying.");

        let value = serde_json::to_value(error.to_report()).expect("report should serialize");

        assert_eq!(
            value,
            json!({
                "code": "missive::auth",
                "category": "auth",
                "message": "authentication error: missing bearer token environment variable",
                "help": "Set the configured environment variable before retrying.",
                "exit_code": 77
            })
        );
    }

    #[test]
    fn io_error_preserves_source_chain_for_json() {
        let source = io::Error::new(ErrorKind::NotFound, "config file missing");
        let error = MissiveError::io("reading configuration", source);

        assert_eq!(
            error.message(),
            "I/O error while reading configuration: config file missing"
        );
        assert_eq!(error.to_report().sources, ["config file missing"]);
    }

    #[test]
    fn exit_codes_are_deterministic_for_all_categories() {
        let categories = [
            (ErrorCategory::Io, MissiveExitCode::Io),
            (ErrorCategory::Config, MissiveExitCode::Config),
            (ErrorCategory::Protocol, MissiveExitCode::Protocol),
            (ErrorCategory::Transport, MissiveExitCode::Unavailable),
            (ErrorCategory::Storage, MissiveExitCode::TemporaryFailure),
            (ErrorCategory::Auth, MissiveExitCode::Permission),
            (ErrorCategory::Validation, MissiveExitCode::Usage),
            (ErrorCategory::Orchestration, MissiveExitCode::Software),
        ];

        for (category, expected) in categories {
            assert_eq!(category.exit_code(), expected);
            assert_eq!(MissiveError::new(category, "example").exit_code(), expected);
        }
    }

    #[test]
    fn constructors_cover_core_taxonomy() {
        let examples = [
            MissiveError::config("profile is missing"),
            MissiveError::protocol("unsupported A2A version"),
            MissiveError::transport("endpoint timed out"),
            MissiveError::storage("database migration failed"),
            MissiveError::auth("token is unavailable"),
            MissiveError::validation("group name is invalid"),
            MissiveError::orchestration("barrier quorum was not reached"),
        ];

        let categories: Vec<_> = examples.iter().map(MissiveError::category).collect();

        assert_eq!(
            categories,
            [
                ErrorCategory::Config,
                ErrorCategory::Protocol,
                ErrorCategory::Transport,
                ErrorCategory::Storage,
                ErrorCategory::Auth,
                ErrorCategory::Validation,
                ErrorCategory::Orchestration,
            ]
        );
    }
}
