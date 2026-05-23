//! Shell completion generation for the missive CLI.

use std::io::Write;

use clap::{CommandFactory, ValueEnum};
use clap_complete::{generate, shells};
use missive_core::{MissiveError, Result};
use serde::Serialize;

use crate::{BINARY_NAME, Cli, GlobalArgs, OutputMode, render_success};

/// Shells supported by `missive completion <shell>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CompletionShell {
    /// Generate a bash completion script.
    Bash,
    /// Generate a zsh completion script.
    Zsh,
    /// Generate a fish completion script.
    Fish,
    /// Generate a PowerShell completion script.
    Powershell,
}

impl CompletionShell {
    /// Stable lower-case CLI spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::Powershell => "powershell",
        }
    }

    /// Conventional file name for installing this shell's completion script.
    #[must_use]
    pub const fn install_file_name(self) -> &'static str {
        match self {
            Self::Bash => "missive",
            Self::Zsh => "_missive",
            Self::Fish => "missive.fish",
            Self::Powershell => "missive.ps1",
        }
    }

    /// Human-readable local-user install location hint.
    #[must_use]
    pub const fn local_install_hint(self) -> &'static str {
        match self {
            Self::Bash => "~/.local/share/bash-completion/completions/missive",
            Self::Zsh => {
                "a directory on $fpath, such as ~/.local/share/zsh/site-functions/_missive"
            }
            Self::Fish => "~/.config/fish/completions/missive.fish",
            Self::Powershell => {
                "a script dot-sourced from $PROFILE, such as missive.ps1 next to the profile"
            }
        }
    }
}

/// Arguments for shell completion generation.
#[derive(Debug, Clone, clap::Args)]
pub struct CompletionArgs {
    /// Shell to generate completions for.
    #[arg(value_enum, value_name = "SHELL")]
    pub shell: CompletionShell,
}

#[derive(Debug, Serialize)]
struct CompletionOutput {
    shell: &'static str,
    command: &'static str,
    file_name: &'static str,
    install_hint: &'static str,
    script: String,
}

/// Executes `missive completion <shell>`.
pub fn execute_completion_command<W>(
    args: &CompletionArgs,
    globals: &GlobalArgs,
    writer: &mut W,
) -> Result<()>
where
    W: Write,
{
    let mode = OutputMode::from_globals(globals)?;
    let script = generate_completion_script(args.shell)?;

    match mode {
        OutputMode::Human => writer
            .write_all(script.as_bytes())
            .map_err(|error| MissiveError::io("writing completion script", error)),
        OutputMode::Json | OutputMode::Ndjson => {
            let output = CompletionOutput {
                shell: args.shell.as_str(),
                command: BINARY_NAME,
                file_name: args.shell.install_file_name(),
                install_hint: args.shell.local_install_hint(),
                script,
            };
            render_success(
                writer,
                mode,
                "completion",
                &output,
                "missive: completion script generated",
            )
        }
        OutputMode::Quiet => Ok(()),
    }
}

fn generate_completion_script(shell: CompletionShell) -> Result<String> {
    let mut command = Cli::command();
    command.build();
    let mut buffer = Vec::new();

    match shell {
        CompletionShell::Bash => generate(shells::Bash, &mut command, BINARY_NAME, &mut buffer),
        CompletionShell::Zsh => generate(shells::Zsh, &mut command, BINARY_NAME, &mut buffer),
        CompletionShell::Fish => generate(shells::Fish, &mut command, BINARY_NAME, &mut buffer),
        CompletionShell::Powershell => {
            generate(shells::PowerShell, &mut command, BINARY_NAME, &mut buffer);
        }
    }

    String::from_utf8(buffer).map_err(|error| {
        MissiveError::orchestration("generated completion script was not valid UTF-8")
            .with_help(error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_shells_have_stable_names_and_install_hints() {
        let shells = CompletionShell::value_variants();
        let names: Vec<_> = shells.iter().map(|shell| shell.as_str()).collect();

        assert_eq!(names, ["bash", "zsh", "fish", "powershell"]);
        assert_eq!(CompletionShell::Bash.install_file_name(), "missive");
        assert_eq!(CompletionShell::Zsh.install_file_name(), "_missive");
        assert_eq!(CompletionShell::Fish.install_file_name(), "missive.fish");
        assert_eq!(
            CompletionShell::Powershell.install_file_name(),
            "missive.ps1"
        );
        for shell in shells {
            assert!(shell.local_install_hint().contains("missive"));
        }
    }

    #[test]
    fn completion_scripts_include_current_command_tree() {
        for shell in CompletionShell::value_variants() {
            let script = generate_completion_script(*shell).expect("completion script");
            assert!(script.contains(BINARY_NAME));
            assert!(script.contains("completion"));
            assert!(script.contains("manpage"));
            assert!(script.contains("protocol-version"));
        }
    }
}
