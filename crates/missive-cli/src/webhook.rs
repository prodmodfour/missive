//! Local A2A push notification webhook command.

use std::collections::BTreeMap;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use clap::{Args, Subcommand};
use missive_core::{LoadedConfig, MissiveError, MissiveExitCode, Result};
use missive_gateway::{
    DEFAULT_MAX_BODY_BYTES, DEFAULT_WEBHOOK_PATH, WebhookAuth, WebhookReceiverConfig,
    WebhookReceiverSummary, WebhookRuntimeEvent, run_webhook_receiver,
};
use missive_store::StatePathResolver;
use tokio::sync::mpsc;

use crate::output::{OutputMode, render_stream_item, render_success};
use crate::{GlobalArgs, service_parameters_from_config_and_globals};

/// A2A push notification webhook subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum WebhookCommands {
    /// Run a local HTTP receiver for A2A push notification callbacks.
    Run(WebhookRunArgs),
}

impl WebhookCommands {
    /// Stable subcommand spelling used in structured output messages.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Run(_) => "run",
        }
    }
}

/// Arguments for `missive webhook run`.
#[derive(Debug, Clone, Args)]
pub struct WebhookRunArgs {
    /// IP address to bind. Defaults to the selected profile gateway bind address.
    #[arg(long = "bind-address", value_name = "IP")]
    pub bind_address: Option<IpAddr>,

    /// TCP port to bind. Defaults to the selected profile gateway bind port.
    #[arg(long = "port", value_name = "PORT")]
    pub port: Option<u16>,

    /// HTTP path that receives A2A StreamResponse push payloads.
    #[arg(long = "path", value_name = "PATH", default_value = DEFAULT_WEBHOOK_PATH)]
    pub path: String,

    /// Require callbacks to include this header when --auth-token-env is set.
    #[arg(
        long = "auth-header",
        value_name = "HEADER",
        default_value = "Authorization"
    )]
    pub auth_header: String,

    /// Auth scheme prefix expected before the token; use 'none' for a raw token.
    #[arg(long = "auth-scheme", value_name = "SCHEME", default_value = "Bearer")]
    pub auth_scheme: String,

    /// Read the expected inbound webhook token from this environment variable.
    #[arg(long = "auth-token-env", value_name = "ENV")]
    pub auth_token_env: Option<String>,

    /// Stop gracefully after accepting this many valid push callbacks.
    #[arg(long = "max-events", value_name = "N")]
    pub max_events: Option<u64>,

    /// Maximum callback request body size in bytes.
    #[arg(
        long = "max-body-bytes",
        value_name = "BYTES",
        default_value_t = DEFAULT_MAX_BODY_BYTES
    )]
    pub max_body_bytes: usize,
}

/// Executes one webhook subcommand.
pub(crate) fn execute_webhook_command<W>(
    command: &WebhookCommands,
    globals: &GlobalArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    match command {
        WebhookCommands::Run(args) => {
            let config = build_receiver_config(args, globals, loaded_config, environment)?;
            run_receiver_and_render(config, mode, writer)
        }
    }
}

fn build_receiver_config(
    args: &WebhookRunArgs,
    globals: &GlobalArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
) -> Result<WebhookReceiverConfig> {
    if args.max_events.is_some_and(|value| value == 0) {
        return Err(MissiveError::validation(
            "--max-events must be greater than zero",
        ));
    }
    if args.max_body_bytes == 0 {
        return Err(MissiveError::validation(
            "--max-body-bytes must be greater than zero",
        ));
    }

    let gateway = effective_gateway_bind(loaded_config)?;
    let bind_addr = SocketAddr::new(
        args.bind_address.unwrap_or_else(|| gateway.ip()),
        args.port.unwrap_or_else(|| gateway.port()),
    );

    let resolver = StatePathResolver::new().with_env(
        environment
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let state_paths = resolver.resolve_loaded(loaded_config)?;
    let service_parameters = service_parameters_from_config_and_globals(loaded_config, globals)?;
    let auth = inbound_auth_from_args(args, environment)?;
    let shutdown_after = globals
        .timeout
        .as_deref()
        .map(|value| parse_duration_arg("--timeout", value))
        .transpose()?;

    let config = WebhookReceiverConfig {
        profile: loaded_config.selected_profile.clone(),
        bind_addr,
        path: args.path.clone(),
        state_paths,
        auth,
        max_events: args.max_events,
        shutdown_after,
        max_body_bytes: args.max_body_bytes,
        protocol_version: service_parameters.protocol_version,
    };
    config.validate()?;
    Ok(config)
}

fn effective_gateway_bind(loaded_config: &LoadedConfig) -> Result<SocketAddr> {
    let profile = loaded_config.selected_profile_config()?;
    let gateway = profile
        .gateway
        .as_ref()
        .unwrap_or(&loaded_config.config.gateway);
    gateway.bind_address.parse::<SocketAddr>().map_err(|error| {
        MissiveError::config("gateway.bind_address is not a valid socket address")
            .with_source(error)
            .with_help("Use a value such as 127.0.0.1:7347 in missive configuration.")
    })
}

fn inbound_auth_from_args(
    args: &WebhookRunArgs,
    environment: &BTreeMap<String, String>,
) -> Result<WebhookAuth> {
    let Some(env_name) = &args.auth_token_env else {
        return Ok(WebhookAuth::Disabled);
    };
    let token = environment.get(env_name).cloned().ok_or_else(|| {
        MissiveError::auth(format!(
            "webhook auth token environment variable {env_name:?} is not set"
        ))
        .with_exit_code(MissiveExitCode::Permission)
        .with_help("Set the environment variable before running missive webhook run.")
    })?;
    let scheme = normalize_auth_scheme(&args.auth_scheme)?;
    Ok(WebhookAuth::Header {
        name: args.auth_header.clone(),
        token,
        scheme,
    })
}

fn normalize_auth_scheme(value: &str) -> Result<Option<String>> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    if value.is_empty() {
        return Err(MissiveError::validation(
            "--auth-scheme cannot be empty; use 'none' for a raw token comparison",
        ));
    }
    Ok(Some(value.to_owned()))
}

fn run_receiver_and_render<W>(
    config: WebhookReceiverConfig,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| MissiveError::io("creating webhook receiver runtime", error))?;

    runtime.block_on(async move {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let receiver = run_webhook_receiver(config, event_tx);
        tokio::pin!(receiver);
        let mut sequence = 0_u64;

        loop {
            tokio::select! {
                event = event_rx.recv() => {
                    if let Some(event) = event {
                        render_runtime_event(writer, mode, sequence, &event)?;
                        sequence += 1;
                    }
                }
                summary = &mut receiver => {
                    let summary = summary?;
                    while let Ok(event) = event_rx.try_recv() {
                        render_runtime_event(writer, mode, sequence, &event)?;
                        sequence += 1;
                    }
                    render_summary(writer, mode, sequence, &summary)?;
                    return Ok(());
                }
            }
        }
    })
}

fn render_runtime_event<W>(
    writer: &mut W,
    mode: OutputMode,
    sequence: u64,
    event: &WebhookRuntimeEvent,
) -> Result<()>
where
    W: Write,
{
    match event {
        WebhookRuntimeEvent::Started(started) => render_stream_item(
            writer,
            mode,
            "webhook_started",
            sequence,
            started,
            &started.message,
        ),
        WebhookRuntimeEvent::Accepted(accepted) => render_stream_item(
            writer,
            mode,
            "webhook_event",
            sequence,
            accepted,
            &accepted.message,
        ),
        WebhookRuntimeEvent::Rejected(rejected) => render_stream_item(
            writer,
            mode,
            "webhook_rejected",
            sequence,
            rejected,
            &rejected.message,
        ),
    }
}

fn render_summary<W>(
    writer: &mut W,
    mode: OutputMode,
    sequence: u64,
    summary: &WebhookReceiverSummary,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Ndjson => render_stream_item(
            writer,
            mode,
            "webhook_stopped",
            sequence,
            summary,
            &summary.message,
        ),
        _ => render_success(writer, mode, "webhook_stopped", summary, &summary.message),
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

#[cfg(test)]
mod tests {
    use super::*;
    use missive_core::{ConfigDiscovery, GatewayConfig};
    use tempfile::tempdir;

    fn loaded_config() -> LoadedConfig {
        ConfigDiscovery::new()
            .with_env(std::iter::empty::<(String, String)>())
            .load()
            .expect("default config")
    }

    #[test]
    fn auth_token_env_builds_redacted_header_auth() {
        let args = WebhookRunArgs {
            bind_address: None,
            port: None,
            path: DEFAULT_WEBHOOK_PATH.to_owned(),
            auth_header: "Authorization".to_owned(),
            auth_scheme: "Bearer".to_owned(),
            auth_token_env: Some("MISSIVE_WEBHOOK_TOKEN".to_owned()),
            max_events: Some(1),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        };
        let env = BTreeMap::from([(
            "MISSIVE_WEBHOOK_TOKEN".to_owned(),
            "super-secret".to_owned(),
        )]);
        let auth = inbound_auth_from_args(&args, &env).expect("auth");
        assert!(matches!(auth, WebhookAuth::Header { .. }));
        assert_eq!(auth.redacted_view().token.as_deref(), Some("[REDACTED]"));
    }

    #[test]
    fn effective_bind_uses_profile_gateway_override() {
        let mut loaded = loaded_config();
        let profile = loaded
            .config
            .profiles
            .get_mut(&loaded.selected_profile)
            .expect("profile");
        profile.gateway = Some(GatewayConfig {
            bind_address: "127.0.0.1:9123".to_owned(),
            ..GatewayConfig::default()
        });
        assert_eq!(
            effective_gateway_bind(&loaded).expect("bind").to_string(),
            "127.0.0.1:9123"
        );
    }

    #[test]
    fn build_receiver_config_resolves_state_paths_and_timeout() {
        let temp = tempdir().expect("tempdir");
        let env = BTreeMap::from([(
            "MISSIVE_HOME".to_owned(),
            temp.path().to_string_lossy().into_owned(),
        )]);
        let loaded = ConfigDiscovery::new()
            .with_env(env.iter().map(|(key, value)| (key.clone(), value.clone())))
            .load()
            .expect("config");
        let args = WebhookRunArgs {
            bind_address: Some("127.0.0.1".parse().expect("ip")),
            port: Some(0),
            path: DEFAULT_WEBHOOK_PATH.to_owned(),
            auth_header: "Authorization".to_owned(),
            auth_scheme: "none".to_owned(),
            auth_token_env: None,
            max_events: Some(1),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        };
        let globals = GlobalArgs {
            timeout: Some("2s".to_owned()),
            ..GlobalArgs::default()
        };
        let config = build_receiver_config(&args, &globals, &loaded, &env).expect("config");
        assert_eq!(config.bind_addr.port(), 0);
        assert_eq!(config.shutdown_after, Some(Duration::from_secs(2)));
    }
}
