#!/usr/bin/env bash
# Terminal: the loopback OIDC issuer Proxima verifies bearers against.
#
# Prints a ready-to-paste bearer + client config and serves JWKS discovery on
# 127.0.0.1:31416. The signing key persists at ~/.proxima/dev-idp.pkcs8, so the
# printed token survives restarts. Output is teed to /tmp/dev-idp.log so a client
# can grab the token without scrolling the terminal.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=./target/debug/dev-idp
if [ ! -x "$BIN" ]; then
  SQLX_OFFLINE=true cargo build -p proxima-dev-idp
fi
"$BIN" 2>&1 | tee /tmp/dev-idp.log
