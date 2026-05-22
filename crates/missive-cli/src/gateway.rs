//! Gateway daemon command.

use std::collections::BTreeMap;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use clap::{Args, Subcommand};
use missive_core::{LoadedConfig, MissiveError, Result};
use missive_gateway::{
    DEFAULT_GATEWAY_HEALTH_PATH, DEFAULT_GATEWAY_READY_PATH, DEFAULT_GATEWAY_STATUS_PATH,
    GatewayDaemonConfig, GatewayDaemonSummary, GatewayRuntimeEvent, run_gateway_daemon,
};
use missive_store::StatePathResolver;
use tokio::sync::mpsc;

use crate::GlobalArgs;
use crate::output::{OutputMode, render_stream_item, render_success};

/// Gateway daemon subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum GatewayCommands {
    /// Run the local gateway daemon skeleton.
    Run(GatewayRunArgs),
}

impl GatewayCommands {
    /// Stable subcommand spelling used in structured output messages.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Run(_) => "run",
        }
    }
}

/// Arguments for `missive gateway run`.
#[derive(Debug, Clone, Args)]
pub struct GatewayRunArgs {
    /// IP address to bind. Defaults to the selected profile gateway bind address.
    #[arg(long = "bind-address", value_name = "IP")]
    pub bind_address: Option<IpAddr>,

    /// TCP port to bind. Defaults to the selected profile gateway bind port.
    #[arg(long = "port", value_name = "PORT")]
    pub port: Option<u16>,

    /// HTTP path for liveness checks.
    #[arg(
        long = "health-path",
        value_name = "PATH",
        default_value = DEFAULT_GATEWAY_HEALTH_PATH
    )]
    pub health_path: String,

    /// HTTP path for readiness checks.
    #[arg(
        long = "ready-path",
        value_name = "PATH",
        default_value = DEFAULT_GATEWAY_READY_PATH
    )]
    pub ready_path: String,

    /// HTTP path for detailed component status.
    #[arg(
        long = "status-path",
        value_name = "PATH",
        default_value = DEFAULT_GATEWAY_STATUS_PATH
    )]
    pub status_path: String,
}

/// Executes one gateway subcommand.
pub(crate) fn execute_gateway_command<W>(
    command: &GatewayCommands,
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
        GatewayCommands::Run(args) => {
            let config = build_daemon_config(args, globals, loaded_config, environment)?;
            run_daemon_and_render(config, mode, writer)
        }
    }
}

fn build_daemon_config(
    args: &GatewayRunArgs,
    globals: &GlobalArgs,
    loaded_config: &LoadedConfig,
    environment: &BTreeMap<String, String>,
) -> Result<GatewayDaemonConfig> {
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
    let shutdown_after = globals
        .timeout
        .as_deref()
        .map(|value| parse_duration_arg("--timeout", value))
        .transpose()?;

    let gateway_config = effective_gateway_config(loaded_config)?;
    let config = GatewayDaemonConfig {
        profile: loaded_config.selected_profile.clone(),
        bind_addr,
        state_paths,
        shutdown_after,
        health_path: args.health_path.clone(),
        ready_path: args.ready_path.clone(),
        status_path: args.status_path.clone(),
        job_concurrency: gateway_config.job_concurrency,
    };
    config.validate()?;
    Ok(config)
}

fn effective_gateway_config(loaded_config: &LoadedConfig) -> Result<&missive_core::GatewayConfig> {
    let profile = loaded_config.selected_profile_config()?;
    Ok(profile
        .gateway
        .as_ref()
        .unwrap_or(&loaded_config.config.gateway))
}

fn effective_gateway_bind(loaded_config: &LoadedConfig) -> Result<SocketAddr> {
    let gateway = effective_gateway_config(loaded_config)?;
    gateway.bind_address.parse::<SocketAddr>().map_err(|error| {
        MissiveError::config("gateway.bind_address is not a valid socket address")
            .with_source(error)
            .with_help("Use a value such as 127.0.0.1:7347 in missive configuration.")
    })
}

fn run_daemon_and_render<W>(
    config: GatewayDaemonConfig,
    mode: OutputMode,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| MissiveError::io("creating gateway daemon runtime", error))?;

    runtime.block_on(async move {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let daemon = run_gateway_daemon(config, event_tx);
        tokio::pin!(daemon);
        let mut sequence = 0_u64;

        loop {
            tokio::select! {
                event = event_rx.recv() => {
                    if let Some(event) = event {
                        render_runtime_event(writer, mode, sequence, &event)?;
                        sequence += 1;
                    }
                }
                summary = &mut daemon => {
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
    event: &GatewayRuntimeEvent,
) -> Result<()>
where
    W: Write,
{
    match event {
        GatewayRuntimeEvent::Started(started) => render_stream_item(
            writer,
            mode,
            "gateway_started",
            sequence,
            started,
            &started.message,
        ),
        GatewayRuntimeEvent::Component(component) => render_stream_item(
            writer,
            mode,
            "gateway_component",
            sequence,
            component,
            &component.message,
        ),
    }
}

fn render_summary<W>(
    writer: &mut W,
    mode: OutputMode,
    sequence: u64,
    summary: &GatewayDaemonSummary,
) -> Result<()>
where
    W: Write,
{
    match mode {
        OutputMode::Ndjson => render_stream_item(
            writer,
            mode,
            "gateway_stopped",
            sequence,
            summary,
            &summary.message,
        ),
        _ => render_success(writer, mode, "gateway_stopped", summary, &summary.message),
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
    fn effective_bind_and_concurrency_use_profile_gateway_override() {
        let mut loaded = loaded_config();
        let profile = loaded
            .config
            .profiles
            .get_mut(&loaded.selected_profile)
            .expect("profile");
        profile.gateway = Some(GatewayConfig {
            bind_address: "127.0.0.1:9124".to_owned(),
            job_concurrency: 7,
            ..GatewayConfig::default()
        });

        assert_eq!(
            effective_gateway_bind(&loaded).expect("bind").to_string(),
            "127.0.0.1:9124"
        );
        assert_eq!(
            effective_gateway_config(&loaded)
                .expect("gateway")
                .job_concurrency,
            7
        );
    }

    #[test]
    fn build_daemon_config_resolves_state_paths_and_timeout() {
        let temp = tempdir().expect("tempdir");
        let env = BTreeMap::from([(
            "MISSIVE_HOME".to_owned(),
            temp.path().to_string_lossy().into_owned(),
        )]);
        let loaded = ConfigDiscovery::new()
            .with_env(env.iter().map(|(key, value)| (key.clone(), value.clone())))
            .load()
            .expect("config");
        let args = GatewayRunArgs {
            bind_address: Some("127.0.0.1".parse().expect("ip")),
            port: Some(0),
            health_path: DEFAULT_GATEWAY_HEALTH_PATH.to_owned(),
            ready_path: DEFAULT_GATEWAY_READY_PATH.to_owned(),
            status_path: DEFAULT_GATEWAY_STATUS_PATH.to_owned(),
        };
        let globals = GlobalArgs {
            timeout: Some("2s".to_owned()),
            ..GlobalArgs::default()
        };

        let config = build_daemon_config(&args, &globals, &loaded, &env).expect("config");

        assert_eq!(config.bind_addr.port(), 0);
        assert_eq!(config.shutdown_after, Some(Duration::from_secs(2)));
    }
}
