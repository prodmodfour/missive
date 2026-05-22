#![doc = "Gateway daemon scaffolding for missive."]

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
pub fn dependent_crates() -> [missive_core::CrateInfo; 2] {
    [missive_router::crate_info(), missive_store::crate_info()]
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
    fn dependent_crates_include_router_and_store() {
        let dependencies = dependent_crates();
        let names: Vec<_> = dependencies.iter().map(|info| info.name()).collect();

        assert_eq!(names, ["missive-router", "missive-store"]);
    }
}
