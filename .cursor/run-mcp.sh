#!/usr/bin/env bash
# Terminal: the Proxima Code-flavor MCP server.
#
# Sources the env `start.sh` wrote, then serves Streamable HTTP MCP on
# 127.0.0.1:31415/mcp. Migrations run automatically on boot. No embedding
# endpoint is configured, so the server starts in degraded mode: lexical search
# works; semantic/hybrid report the missing capability. Set PROXIMA_EMBED_BASE_URL
# / PROXIMA_EMBED_MODEL against any OpenAI-compatible /embeddings endpoint to
# enable them.
set -euo pipefail
cd "$(dirname "$0")/.."

for _ in $(seq 1 60); do [ -f /tmp/proxima-dev.env ] && break || sleep 1; done
# shellcheck disable=SC1091
source /tmp/proxima-dev.env

BIN=./target/debug/proxima-mcp
if [ ! -x "$BIN" ]; then
  SQLX_OFFLINE=true cargo build -p proxima-mcp --features code
fi
exec "$BIN"
