# Container and devcontainer workflows

missive provides a local Docker development image and a VS Code/devcontainer definition for reproducible build and test environments. The image is for development and validation, not for distributing a production gateway service.

## What the image contains

The root [`Dockerfile`](../Dockerfile) builds from the official Rust Debian image and installs:

* the stable Rust toolchain with `rustfmt`, `clippy`, and `llvm-tools-preview`;
* build essentials, `pkg-config`, Git, curl, jq, ShellCheck, Python/YAML tooling, and sudo;
* protobuf tooling through `protobuf-compiler`;
* SQLite tooling through `sqlite3` and `libsqlite3-dev`;
* default quality-gate cargo helpers `cargo-audit` and `cargo-machete`.

No credentials, `.env` files, local missive state, databases, logs, or Git metadata are baked into the image. The accompanying [`.dockerignore`](../.dockerignore) keeps generated/private runtime files out of the build context.

## Build the image

From the repository root:

```bash
docker build --pull=false --tag missive-dev:local .
```

The default build tracks the Rust `stable` toolchain used by `rust-toolchain.toml`. For a pinned local rebuild, pass both the base image and rustup toolchain explicitly:

```bash
docker build \
  --pull=false \
  --build-arg RUST_IMAGE=rust:1.95.0-bookworm \
  --build-arg RUST_TOOLCHAIN=1.95.0 \
  --tag missive-dev:rust-1.95.0 \
  .
```

Useful build arguments:

| Argument | Default | Purpose |
| --- | --- | --- |
| `RUST_IMAGE` | `rust:bookworm` | Base image used for the development environment. |
| `RUST_TOOLCHAIN` | `stable` | Toolchain selected through `RUSTUP_TOOLCHAIN`. |
| `MISSIVE_UID` / `MISSIVE_GID` | `1000` / `1000` | UID/GID for the interactive `missive` user. |
| `MISSIVE_INSTALL_OPTIONAL_CARGO_TOOLS` | `1` | Set to `0` to skip installing `cargo-audit` and `cargo-machete`. |

## Run the quality gate in a container

The simplest validation path is the wrapper script:

```bash
scripts/docker-integration.sh
```

The script builds `missive-quality-gate:local` when needed, bind-mounts the current checkout, keeps Cargo and target output under temporary container paths, sets `MISSIVE_HOME` outside the repository, and runs:

```bash
scripts/quality-gate.sh
```

Useful controls:

```bash
MISSIVE_DOCKER_TEST_TAG=missive-dev:local scripts/docker-integration.sh
MISSIVE_DOCKER_FORCE_BUILD=1 scripts/docker-integration.sh
MISSIVE_DOCKER_RUN_QUALITY_GATE=0 scripts/docker-integration.sh
```

To run the gate manually:

```bash
docker run --rm \
  --workdir /workspace/missive \
  --mount "type=bind,source=$PWD,target=/workspace/missive" \
  --env MISSIVE_HOME=/tmp/missive-home \
  --env CARGO_TARGET_DIR=/tmp/missive-target \
  missive-dev:local \
  bash -c 'mkdir -p "$MISSIVE_HOME" "$CARGO_TARGET_DIR" && scripts/quality-gate.sh'
```

## Interactive development shell

```bash
docker run --rm -it \
  --workdir /workspace/missive \
  --mount "type=bind,source=$PWD,target=/workspace/missive" \
  --env MISSIVE_HOME=/home/missive/.local/share/missive-dev \
  missive-dev:local
```

Inside the container, use the normal local commands:

```bash
scripts/bootstrap-tools.sh --check
scripts/quality-gate.sh
cargo test --workspace --all-targets --all-features
cargo run -p missive-cli --bin missive -- --help
```

## Devcontainer

The [`.devcontainer/devcontainer.json`](../.devcontainer/devcontainer.json) file uses the same Dockerfile target. In VS Code or any devcontainer-compatible tool:

1. install the Dev Containers extension or CLI;
2. open this repository;
3. choose **Dev Containers: Reopen in Container**.

The devcontainer mounts the checkout at `/workspace/missive`, runs as the non-root `missive` user, enables UID/GID remapping, and sets `MISSIVE_HOME` under the container user's home directory so runtime state stays out of the repository.

## Security and hygiene notes

* Do not bake tokens, real `.env` files, private configs, or key material into custom images.
* Prefer runtime environment variables or keyring-backed auth refs for local experiments, and never commit those values.
* Keep gateway state, file-drop inbox/outbox directories, logs, SQLite databases, coverage output, and benchmark reports outside tracked files.
* The Docker image is a developer/test environment. It is not hardened as a production daemon image and does not enable systemd, launchd, public gateway exposure, TLS termination, or external chat adapters by default.
