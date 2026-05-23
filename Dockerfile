# syntax=docker/dockerfile:1

ARG RUST_IMAGE=rust:bookworm
FROM ${RUST_IMAGE} AS dev

ARG DEBIAN_FRONTEND=noninteractive
ARG MISSIVE_UID=1000
ARG MISSIVE_GID=1000
ARG RUST_TOOLCHAIN=stable
ARG MISSIVE_INSTALL_OPTIONAL_CARGO_TOOLS=1
ARG CARGO_AUDIT_VERSION=0.22.1
ARG CARGO_DENY_VERSION=0.19.6
ARG CARGO_MACHETE_VERSION=0.9.2

ENV CARGO_TERM_COLOR=always \
    MISSIVE_CONTAINER=1 \
    MISSIVE_HOME=/home/missive/.local/share/missive \
    RUSTUP_TOOLCHAIN=${RUST_TOOLCHAIN}

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        build-essential \
        ca-certificates \
        curl \
        git \
        jq \
        libsqlite3-dev \
        pkg-config \
        protobuf-compiler \
        python3 \
        python3-yaml \
        shellcheck \
        sqlite3 \
        sudo \
    && rm -rf /var/lib/apt/lists/*

RUN rustup toolchain install "${RUST_TOOLCHAIN}" --profile default --component rustfmt --component clippy \
    && rustup component add --toolchain "${RUST_TOOLCHAIN}" llvm-tools-preview

RUN if [ "${MISSIVE_INSTALL_OPTIONAL_CARGO_TOOLS}" = "1" ]; then \
        cargo install --locked cargo-audit --version "${CARGO_AUDIT_VERSION}"; \
        cargo install --locked cargo-deny --version "${CARGO_DENY_VERSION}"; \
        cargo install --locked cargo-machete --version "${CARGO_MACHETE_VERSION}"; \
    fi

RUN if ! getent group "${MISSIVE_GID}" >/dev/null; then groupadd --gid "${MISSIVE_GID}" missive; fi \
    && useradd --uid "${MISSIVE_UID}" --gid "${MISSIVE_GID}" --create-home --shell /bin/bash missive \
    && mkdir -p /workspace/missive "${MISSIVE_HOME}" /home/missive/.cargo/bin \
    && chown -R missive:"${MISSIVE_GID}" /workspace /home/missive \
    && printf 'missive ALL=(ALL) NOPASSWD:ALL\n' > /etc/sudoers.d/missive \
    && chmod 0440 /etc/sudoers.d/missive

ENV CARGO_HOME=/home/missive/.cargo \
    PATH=/home/missive/.cargo/bin:/usr/local/cargo/bin:${PATH}

WORKDIR /workspace/missive
USER missive

CMD ["bash"]
