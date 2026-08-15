#!/usr/bin/env bash
# Per-boot reconciliation for Proxima (skill: `start` lifecycle).
#
# Brings Postgres up, re-asserts the dev role/database/extension (cheap and
# idempotent — usually already present in the booted snapshot), and writes the
# shared env file that the `proxima-mcp` terminal sources. It then returns; the
# long-running issuer and MCP server run as `terminals`.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "[start] ensure postgres online"
if ! pg_isready -h localhost -p 5432 -q; then
  sudo pg_ctlcluster 16 main start 2>/dev/null || true
fi
for _ in $(seq 1 60); do pg_isready -h localhost -p 5432 -q && break || sleep 1; done
pg_isready -h localhost -p 5432

echo "[start] ensure role/database/extension"
sudo -u postgres psql -v ON_ERROR_STOP=1 <<'SQL' >/dev/null
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'proxima') THEN
    CREATE ROLE proxima LOGIN SUPERUSER PASSWORD 'proxima';
  END IF;
END $$;
SQL
sudo -u postgres psql -tAc "SELECT 1 FROM pg_database WHERE datname = 'proxima'" | grep -q 1 \
  || sudo -u postgres createdb -O proxima proxima
sudo -u postgres psql -d proxima -c "CREATE EXTENSION IF NOT EXISTS vector;" >/dev/null

# The dev owner id is derived deterministically from the issuer subject, exactly
# as `proxima-dev-idp` derives it (uuid v5 over the fixed "proxima-dev-idp\0"
# namespace and the "proxima-dev" subject), so the MCP server's subject map and
# any client's `X-Proxima-Owner` header agree without parsing issuer output.
USER_ID="$(python3 - <<'PY'
import uuid
print(uuid.uuid5(uuid.UUID(bytes=b'proxima-dev-idp\x00'), 'proxima-dev'))
PY
)"

cat > /tmp/proxima-dev.env <<EOF
export DATABASE_URL=postgres://proxima:proxima@localhost/proxima
export PROXIMA_TEST_PG_URL=postgres://proxima:proxima@localhost/proxima
export PROXIMA_OIDC_ISSUER=http://127.0.0.1:31416
export PROXIMA_OIDC_AUDIENCE=proxima-mcp
export PROXIMA_OIDC_SUBJECT_MAP=proxima-dev:${USER_ID}
export PROXIMA_PUBLIC_URL=http://127.0.0.1:31415
export PROXIMA_DEV_OWNER=personal:${USER_ID}
EOF

echo "[start] postgres ready; wrote /tmp/proxima-dev.env (owner personal:${USER_ID})"
