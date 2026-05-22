#![doc = "Local persistence scaffolding for missive."]

/// Cargo package name for this crate.
pub const CRATE_NAME: &str = "missive-store";

/// Short description of this crate's target responsibility.
pub const CRATE_PURPOSE: &str = "SQLite migrations and repository APIs";

/// Returns metadata for this crate.
#[must_use]
pub const fn crate_info() -> missive_core::CrateInfo {
    missive_core::CrateInfo::new(CRATE_NAME, CRATE_PURPOSE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_info_describes_store_layer() {
        let info = crate_info();

        assert_eq!(info.name(), CRATE_NAME);
        assert!(info.purpose().contains("SQLite"));
    }
}
