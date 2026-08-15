#!/usr/bin/env bash
# Idempotent Cloud Agent bootstrap for Proxima.
#
# Durable, source-derived setup only (skill: `install` lifecycle). Installs the
# system dependencies the substrate needs (Postgres 16 + pgvector, plus the
# cmake/pkg-config the native crypto deps build against), provisions the local
# dev database, and warms the workspace build. Per-boot service startup lives in
# `start.sh`; the long-running issuer + MCP server live in `terminals`.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "[install] system packages"
export DEBIAN_FRONTEND=noninteractive
sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends \
  postgresql-16 postgresql-16-pgvector postgresql-client-16 \
  cmake pkg-config

echo "[install] postgres cluster online"
# The package's post-install cannot start the service in a build sandbox
# (policy-rc.d denies it), so start it here. Idempotent: a no-op if already up.
sudo pg_ctlcluster 16 main start 2>/dev/null || true
for _ in $(seq 1 30); do pg_isready -h localhost -p 5432 -q && break || sleep 1; done

echo "[install] role, database, and pgvector extension"
# The `proxima` role is SUPERUSER on purpose: the substrate's first migration
# runs `CREATE EXTENSION vector`, which is not a trusted extension, so the
# connecting role must be able to create it. This matches the pgvector dev
# container, whose POSTGRES_USER is likewise the superuser.
sudo -u postgres psql -v ON_ERROR_STOP=1 <<'SQL'
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'proxima') THEN
    CREATE ROLE proxima LOGIN SUPERUSER PASSWORD 'proxima';
  ELSE
    ALTER ROLE proxima LOGIN SUPERUSER PASSWORD 'proxima';
  END IF;
END $$;
SQL
if ! sudo -u postgres psql -tAc "SELECT 1 FROM pg_database WHERE datname = 'proxima'" | grep -q 1; then
  sudo -u postgres createdb -O proxima proxima
fi
# Enable in the dev db and template1 so cloned test databases inherit it.
sudo -u postgres psql -d proxima   -c "CREATE EXTENSION IF NOT EXISTS vector;"
sudo -u postgres psql -d template1 -c "CREATE EXTENSION IF NOT EXISTS vector;"

echo "[install] warm the workspace build"
# SQLX_OFFLINE uses the committed .sqlx cache, so no live DB is needed to
# typecheck the sqlx::query! macros at build time.
export SQLX_OFFLINE=true
cargo build -p proxima-mcp --features code -p proxima-dev-idp

echo "[install] done"
