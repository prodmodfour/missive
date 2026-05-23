//! Manual page generation for the missive CLI.

use std::io::Write;

use clap::CommandFactory;
use missive_core::{MissiveError, Result};
use serde::Serialize;

use crate::{BINARY_NAME, Cli, GlobalArgs, OutputMode, render_success};

/// Arguments for manual page generation.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct ManpageArgs {}

#[derive(Debug, Serialize)]
struct ManpageOutput {
    page: &'static str,
    section: &'static str,
    file_name: &'static str,
    install_hint: &'static str,
    roff: String,
}

/// Executes `missive manpage`.
pub fn execute_manpage_command<W>(globals: &GlobalArgs, writer: &mut W) -> Result<()>
where
    W: Write,
{
    let mode = OutputMode::from_globals(globals)?;
    let roff = generate_manpage_roff()?;

    match mode {
        OutputMode::Human => writer
            .write_all(roff.as_bytes())
            .map_err(|error| MissiveError::io("writing manpage roff", error)),
        OutputMode::Json | OutputMode::Ndjson => {
            let output = ManpageOutput {
                page: BINARY_NAME,
                section: "1",
                file_name: "missive.1",
                install_hint: "~/.local/share/man/man1/missive.1 or /usr/local/share/man/man1/missive.1",
                roff,
            };
            render_success(
                writer,
                mode,
                "manpage",
                &output,
                "missive: manpage generated",
            )
        }
        OutputMode::Quiet => Ok(()),
    }
}

fn generate_manpage_roff() -> Result<String> {
    let command = Cli::command();
    let mut buffer = Vec::new();
    clap_mangen::Man::new(command)
        .render(&mut buffer)
        .map_err(|error| MissiveError::io("generating manpage roff", error))?;
    String::from_utf8(buffer).map_err(|error| {
        MissiveError::orchestration("generated manpage was not valid UTF-8")
            .with_help(error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_manpage_includes_current_command_tree() {
        let roff = generate_manpage_roff().expect("manpage");

        assert!(roff.contains(BINARY_NAME));
        assert!(roff.contains("Manage A2A\\-native agent communication"));
        assert!(roff.contains("completion"));
        assert!(roff.contains("manpage"));
        assert!(roff.contains("protocol\\-version"));
    }
}
