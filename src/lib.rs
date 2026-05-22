#![doc = "Bootstrap library for the missive command-line tool."]

/// Canonical project and binary name.
pub const NAME: &str = "missive";

/// Short status text emitted by the bootstrap binary until the real CLI lands.
pub const BOOTSTRAP_MESSAGE: &str = "missive bootstrap: workspace skeleton ready";

/// Returns the current bootstrap status message.
#[must_use]
pub const fn bootstrap_message() -> &'static str {
    BOOTSTRAP_MESSAGE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_name_is_lowercase_missive() {
        assert_eq!(NAME, "missive");
    }

    #[test]
    fn bootstrap_message_mentions_project() {
        assert!(bootstrap_message().starts_with(NAME));
    }
}
