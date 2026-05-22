#![doc = "Command-line bootstrap scaffolding for missive."]

/// Cargo package name for this crate.
pub const CRATE_NAME: &str = "missive-cli";

/// Installed command name for the CLI binary.
pub const BINARY_NAME: &str = missive_core::PROJECT_NAME;

/// Short description of this crate's target responsibility.
pub const CRATE_PURPOSE: &str = "command parsing, output rendering, and exit codes";

/// Returns metadata for this crate.
#[must_use]
pub const fn crate_info() -> missive_core::CrateInfo {
    missive_core::CrateInfo::new(CRATE_NAME, CRATE_PURPOSE)
}

/// Returns the target workspace crate layout.
#[must_use]
pub fn workspace_crates() -> [missive_core::CrateInfo; 8] {
    [
        crate_info(),
        missive_core::crate_info(),
        missive_a2a::crate_info(),
        missive_store::crate_info(),
        missive_router::crate_info(),
        missive_gateway::crate_info(),
        missive_adapters::crate_info(),
        missive_observe::crate_info(),
    ]
}

/// Short status text emitted until the real CLI commands land.
#[must_use]
pub fn bootstrap_message() -> String {
    format!(
        "{BINARY_NAME} workspace: {} crates ready; CLI commands land in a later ticket",
        workspace_crates().len()
    )
}

/// Runs the current placeholder binary.
#[must_use]
pub fn run() -> i32 {
    println!("{}", bootstrap_message());
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_name_matches_project_name() {
        assert_eq!(BINARY_NAME, "missive");
    }

    #[test]
    fn workspace_crates_cover_target_layout() {
        let names: Vec<_> = workspace_crates().iter().map(|info| info.name()).collect();

        assert_eq!(
            names,
            [
                "missive-cli",
                "missive-core",
                "missive-a2a",
                "missive-store",
                "missive-router",
                "missive-gateway",
                "missive-adapters",
                "missive-observe",
            ]
        );
    }

    #[test]
    fn bootstrap_message_mentions_crate_count() {
        let message = bootstrap_message();

        assert!(message.starts_with(BINARY_NAME));
        assert!(message.contains("8 crates"));
    }
}
