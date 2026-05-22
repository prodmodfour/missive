#![doc = "Core domain primitives for the missive command-line tool and control plane."]

/// Canonical project and binary name.
pub const PROJECT_NAME: &str = "missive";

/// Cargo package name for this crate.
pub const CRATE_NAME: &str = "missive-core";

/// Short description of this crate's target responsibility.
pub const CRATE_PURPOSE: &str = "domain types, errors, config, IDs, and envelopes";

/// Static metadata used by the bootstrap CLI and crate-layout tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrateInfo {
    name: &'static str,
    purpose: &'static str,
}

impl CrateInfo {
    /// Creates static crate metadata.
    #[must_use]
    pub const fn new(name: &'static str, purpose: &'static str) -> Self {
        Self { name, purpose }
    }

    /// Returns the Cargo package name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Returns the crate's target responsibility.
    #[must_use]
    pub const fn purpose(self) -> &'static str {
        self.purpose
    }
}

/// Returns metadata for this crate.
#[must_use]
pub const fn crate_info() -> CrateInfo {
    CrateInfo::new(CRATE_NAME, CRATE_PURPOSE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_name_is_lowercase_missive() {
        assert_eq!(PROJECT_NAME, "missive");
    }

    #[test]
    fn crate_info_describes_core() {
        let info = crate_info();

        assert_eq!(info.name(), CRATE_NAME);
        assert!(info.purpose().contains("domain types"));
    }
}
