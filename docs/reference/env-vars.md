# Environment Variable Reference

This table is a human reference. Source code and deployment manifests remain authoritative.

## Runtime and Deployment Variables

| Variable | Scope | Default | Required when | Notes |
|---|---|---|---|---|
| `DATABASE_URL` | storage | binary default: `postgres://postgres@localhost/proxima_dev` | any non-default DB | dev compose uses `postgres://proxima:proxima@localhost:5434/proxima` |
| `PROXIMA_MCP_BIND` | MCP server | `127.0.0.1:31415` for `proxima-mcp` | custom listener / deployment | non-loopback requires `PROXIMA_EXPOSE_NETWORK=true` |
| `PROXIMA_EXPOSE_NETWORK` | MCP server | unset/false | non-loopback bind | fail-closed exposure gate |
| `PROXIMA_ALLOWED_ORIGINS` | MCP HTTP | unset/deployment-specific | browser/front-door exposure | comma-separated; never wildcard in production |
| `PROXIMA_ALLOWED_HOSTS` | MCP HTTP | public URL host + allowed origins | non-loopback exposure override | DNS-rebinding guard; loopback always permitted |
| `PROXIMA_PUBLIC_URL` | MCP/OIDC | unset | OIDC/public deployment | public HTTPS base; host is auto-allowed |
| `PROXIMA_OIDC_ISSUER` | OIDC auth | unset | OIDC deployment | issuer URL |
| `PROXIMA_OIDC_AUDIENCE` | OIDC auth | unset | OIDC deployment | expected token audience |
| `PROXIMA_OIDC_JWKS_URI` | OIDC auth | discovery default | non-default JWKS | overrides discovery |
| `PROXIMA_OIDC_ALLOWED_SUBJECTS` | OIDC auth | unset | subject allowlist desired | comma-separated `sub` values |
| `PROXIMA_OIDC_SUBJECT_MAP_JSON` | OIDC auth | unset | OIDC deployment unless shorthand is used | issuer-aware `(iss, sub) -> user_id` JSON map; mutually exclusive with `PROXIMA_OIDC_SUBJECT_MAP` |
| `PROXIMA_OIDC_SUBJECT_MAP` | OIDC auth | unset | OIDC deployment unless JSON map is used | single-issuer `sub:<uuid>` shorthand bound to `PROXIMA_OIDC_ISSUER`; mutually exclusive with JSON map |
| `PROXIMA_STREAM_MAX_LIFETIME` | MCP stream auth | source default | long-lived Streamable HTTP responses | max seconds before re-validation |
| `PROXIMA_STREAM_EPOCH_INTERVAL` | MCP stream auth | source default | open response stream auth checks | auth-epoch re-check seconds |
| `PROXIMA_REST_ENABLED` | REST surface | `false` | serving `/v1` beside `/mcp` | renders the tool manifest as REST ([17](../17-rest-surface.md)); also needs the `rest` cargo feature at build time |
| `PROXIMA_TOOL_PROFILE` | MCP tool surface | `memory` | widening to the full surface | `full` is opt-in and adds `core_membership`/`core_publish` |
| `PROXIMA_TOOL_ALLOW` | MCP tool surface | unset | profile extension | comma-separated canonical scope keys unioned into profile |
| `PROXIMA_TOOL_DENY` | MCP tool surface | unset | profile restriction | comma-separated canonical scope keys removed after allow |
| `PROXIMA_EMBED_BASE_URL` | embeddings | unset | any OpenAI-compatible endpoint, local or hosted | setting it alone enables embeddings; loopback needs no key |
| `PROXIMA_EMBED_API_KEY` | embeddings | unset | hosted embedding endpoint | bearer sent to `/embeddings` |
| `PROXIMA_EMBED_MODEL` | embeddings | `mistral-embed` | non-default embedding model | must yield 1024-dim vectors |
| `PROXIMA_EMBED_MATRYOSHKA` | embeddings | `false` | nested-prefix model wider than 1024 | sends a `dimensions` request parameter |
| `MISTRAL_API_KEY` | embeddings | unset | alias for `PROXIMA_EMBED_API_KEY` | absent and no base URL means semantic search degrades to lexical paths |
| `MISTRAL_API_BASE` | embeddings | `https://api.mistral.ai/v1` | alias for `PROXIMA_EMBED_BASE_URL` | OpenAI-compatible base URL |
| `PROXIMA_SKIP_MIGRATIONS` | boot | `false` | split-role GitOps deploys | boot without applying migrations; the schema must already be at the current lane or boot fails closed |
| `PROXIMA_PG_MAX_CONNECTIONS` | Postgres pool | `10` | tuning pool size | minimum 1 |
| `PROXIMA_PG_STATEMENT_TIMEOUT_MS` | Postgres pool | `300000` | tuning request timeouts | `0` disables; migrations and bulk erase opt out separately |
| `PROXIMA_PG_ACQUIRE_TIMEOUT_SECS` | Postgres pool | `5` | tuning pool acquisition | seconds |
| `PROXIMA_PG_IDLE_TIMEOUT_SECS` | Postgres pool | `600` | tuning connection reuse | seconds |
| `PROXIMA_PG_MAX_LIFETIME_SECS` | Postgres pool | `1800` | tuning connection recycling | seconds |
| `PROXIMA_CHANGE_EVENT_COMMIT_GRACE_MS` | change events | unset (`0`, disabled) | concurrent writers with slow commits | withholds events newer than `now - grace` so a forward cursor cannot skip a late commit |
| `PROXIMA_S3_MAX_BLOB_BYTES` | cited blobs | unset (uncapped) | bounding cited-blob size | non-negative integer |
| `PROXIMA_S3_BUCKET` | cited blobs | unset | enable S3 cited-blob storage | credentials use AWS SDK provider chain |
| `PROXIMA_S3_REGION` | cited blobs | unset | S3 bucket configured | S3 region |
| `PROXIMA_S3_ENDPOINT_URL` | cited blobs | AWS region endpoint | S3-compatible endpoint | optional |
| `PROXIMA_S3_FORCE_PATH_STYLE` | cited blobs | `false` | path-style S3 endpoint | optional |
| `PROXIMA_S3_UPLOAD_TTL_SECONDS` | cited blobs | `900` | non-default upload TTL | presigned upload URL TTL |
| `PROXIMA_S3_READ_TTL_SECONDS` | cited blobs | `300` | non-default read TTL | presigned read URL TTL |

## CLI Flags That Behave Like Configuration

| Flag | Required when | Notes |
|---|---|---|
| `--database-url <URL>` | non-default DB without `DATABASE_URL` | overrides the Postgres URL |
| `--bind <ADDR>` | custom loopback listener | non-loopback binds use env-gated deployment config |

## Build/Test/Internal Variables

| Variable | Scope | Notes |
|---|---|---|
| `SQLX_OFFLINE` | build/CI | CI sets `true` for offline sqlx query checking |
| `PROXIMA_TEST_PG_URL` | tests | pg-testkit integration test source DB |
| `PROXIMA_TEST_DATABASE_URL` | tests | HTTP/OIDC e2e dedicated DB |
| `PROXIMA_PERF_SESSION_DIR` | dev diagnostics | optional per-request recorder in `crates/mcp-server`; not required for normal runtime |
| `MISTRAL_EMBED_BASE_URL` | source constant | Rust constant for the default Mistral base URL, not an environment variable |
| `MISTRAL_EMBED_MODEL` | source constant | Rust constant for the default Mistral model, not an environment variable |
| `PROXIMA_S3_` | source prefix | configuration prefix constant used to resolve the S3 variables listed above |

## Source Inventory Reconciliation

Inventory sources checked: `docs/10-configuration.md`, `docs/15-deployment.md`,
`apps/proxima-mcp/src/lib.rs`, `crates/proxima/src/runtime_config.rs`,
`crates/proxima/src/config.rs`, `crates/storage-pg/src/lib.rs`,
`crates/storage-pg/src/verbs/consolidate/events.rs`,
`crates/blob-s3/src/config.rs`, and `.github/workflows/ci.yml`. Runtime variables from that inventory are listed in
the runtime table. Test-only and source-constant names are listed under
Build/Test/Internal Variables.
