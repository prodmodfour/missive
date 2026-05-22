#![doc = "Gateway daemon scaffolding for missive."]

pub mod daemon;
pub mod service;
mod subscription;
pub mod webhook;

pub use daemon::{
    DEFAULT_GATEWAY_HEALTH_PATH, DEFAULT_GATEWAY_READY_PATH, DEFAULT_GATEWAY_STATUS_PATH,
    GatewayComponentStatus, GatewayDaemonConfig, GatewayDaemonSummary, GatewayRuntimeEvent,
    GatewayStarted, GatewayStatusResponse, run_gateway_daemon,
};
pub use service::{
    DEFAULT_LAUNCHD_LABEL, DEFAULT_SERVICE_PATH, DEFAULT_SYSTEMD_UNIT, GatewayServiceAction,
    GatewayServiceCommand, GatewayServiceCommandResult, GatewayServiceOptions,
    GatewayServicePlatform, GatewayServiceResult, GatewayServiceScope, captured_environment_keys,
    execute_gateway_service_action, validate_service_environment,
};
pub use webhook::{
    DEFAULT_MAX_BODY_BYTES, DEFAULT_WEBHOOK_PATH, WebhookAccepted, WebhookAuth, WebhookAuthView,
    WebhookReceiverConfig, WebhookReceiverSummary, WebhookRejected, WebhookRuntimeEvent,
    WebhookStarted, WebhookTlsNote, run_webhook_receiver,
};

/// Cargo package name for this crate.
pub const CRATE_NAME: &str = "missive-gateway";

/// Short description of this crate's target responsibility.
pub const CRATE_PURPOSE: &str = "daemon, subscriptions, webhooks, jobs, and sessions";

/// Returns metadata for this crate.
#[must_use]
pub const fn crate_info() -> missive_core::CrateInfo {
    missive_core::CrateInfo::new(CRATE_NAME, CRATE_PURPOSE)
}

/// Returns the lower-level crates the gateway will coordinate as it grows.
#[must_use]
pub fn dependent_crates() -> [missive_core::CrateInfo; 3] {
    [
        missive_a2a::crate_info(),
        missive_router::crate_info(),
        missive_store::crate_info(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_info_describes_gateway_layer() {
        let info = crate_info();

        assert_eq!(info.name(), CRATE_NAME);
        assert!(info.purpose().contains("daemon"));
    }

    #[test]
    fn dependent_crates_include_a2a_router_and_store() {
        let dependencies = dependent_crates();
        let names: Vec<_> = dependencies.iter().map(|info| info.name()).collect();

        assert_eq!(names, ["missive-a2a", "missive-router", "missive-store"]);
    }
}
