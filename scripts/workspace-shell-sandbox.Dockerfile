FROM rust:1-bookworm

# Workspace command sandbox only.
# Proxima Shell/Engine/MCP, Postgres, embeddings, and provider secrets run
# on the host. This image carries build/test tooling for the mounted worktree.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        curl \
        git \
        nodejs \
        npm \
        pkg-config \
        libssl-dev \
    && rm -rf /var/lib/apt/lists/*

RUN npm install -g pnpm

ENV HOME=/tmp \
    CI=true \
    CARGO_HOME=/workspace/.sandbox/cargo \
    PNPM_HOME=/workspace/.sandbox/pnpm \
    PATH=/workspace/.sandbox/pnpm:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

WORKDIR /workspace
