# Shell completions and manpages

`missive` can generate shell completion scripts and the `missive(1)` manual page
from the same clap command tree used by the binary. Generation is local-only and
requires no network access.

## Shell completions

Generate a completion script with:

```bash
missive completion <shell>
```

Supported shells are `bash`, `zsh`, `fish`, and `powershell`.

### Bash

User-local installation:

```bash
mkdir -p ~/.local/share/bash-completion/completions
missive completion bash > ~/.local/share/bash-completion/completions/missive
```

System-wide installations usually use
`/usr/share/bash-completion/completions/missive` or
`/usr/local/share/bash-completion/completions/missive`, depending on the
platform/package manager.

### Zsh

Install the generated `_missive` file in a directory listed in `$fpath`:

```bash
mkdir -p ~/.local/share/zsh/site-functions
missive completion zsh > ~/.local/share/zsh/site-functions/_missive
```

Ensure the directory is in `fpath` before `compinit` runs, for example in
`~/.zshrc`:

```zsh
fpath=(~/.local/share/zsh/site-functions $fpath)
autoload -Uz compinit
compinit
```

System-wide zsh completion directories commonly include
`/usr/local/share/zsh/site-functions` and `/usr/share/zsh/site-functions`.

### Fish

```bash
mkdir -p ~/.config/fish/completions
missive completion fish > ~/.config/fish/completions/missive.fish
```

System-wide fish completions commonly live in
`/usr/share/fish/vendor_completions.d/missive.fish` or
`/usr/local/share/fish/vendor_completions.d/missive.fish`.

### PowerShell

Generate a script and dot-source it from your PowerShell profile:

```powershell
$CompletionDir = Join-Path (Split-Path $PROFILE) "Completions"
New-Item -ItemType Directory -Force -Path $CompletionDir | Out-Null
missive completion powershell | Out-File -Encoding utf8 (Join-Path $CompletionDir "missive.ps1")
Add-Content -Path $PROFILE -Value ". '$CompletionDir/missive.ps1'"
```

Package managers may instead install the generated script in their own profile
or module initialization path.

## Manpage

Generate the roff source for the `missive(1)` page with:

```bash
missive manpage > missive.1
```

User-local installation:

```bash
mkdir -p ~/.local/share/man/man1
missive manpage > ~/.local/share/man/man1/missive.1
mandb ~/.local/share/man 2>/dev/null || true
man missive
```

System-wide installations usually place the file at
`/usr/local/share/man/man1/missive.1` or `/usr/share/man/man1/missive.1` and run
`mandb` when that tool is available.

## Machine-readable generation

For automation that wants to capture metadata and content without relying on raw
stdout, `--json` and `--ndjson` wrap generated content in the standard
`missive.output.v1` envelope:

```bash
missive completion fish --json
missive manpage --json
```

Do not pass `--json` or `--ndjson` when redirecting directly into a shell
completion or manpage install location; those flags intentionally wrap the raw
script or roff content for machine processing.
