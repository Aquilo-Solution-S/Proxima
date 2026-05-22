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

# Build caches live on a persistent named volume mounted at /cache, outside
# /workspace — so they survive across wakes and never land in the per-wake
# clone (which would commit them onto the wake branch). The container runs as
# an arbitrary host uid, so /cache must be world-writable for it to create
# the cargo/pnpm subdirs.
RUN mkdir -p /cache && chmod 777 /cache

# A system-level git identity so commits work for any uid with no per-wake
# `git config`; `safe.directory '*'` because the bind-mounted clone is owned
# by the host uid, which git would otherwise refuse to operate on.
RUN git config --system user.name "Proxima Wake" \
    && git config --system user.email "wake@proxima.local" \
    && git config --system --add safe.directory '*'

ENV HOME=/tmp \
    CI=true \
    CARGO_HOME=/cache/cargo \
    PNPM_HOME=/cache/pnpm \
    PATH=/cache/pnpm:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

WORKDIR /workspace
