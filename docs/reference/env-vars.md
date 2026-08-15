# Environment Variable Reference

This table is a human reference. Source code and deployment manifests remain authoritative.

**Empty means unset.** Every variable below is trimmed before it is read, and a
variable set to the empty string — or to nothing but whitespace — is treated
exactly as if it were absent, taking the `Default` column. Exporting `FOO=` is
therefore never a way to say something different from leaving `FOO` out. This
also means a trailing newline picked up from a here-doc or a mounted secret
file is harmless rather than a startup error naming a value nobody typed.

The one deliberate exception is the `env:` secret scheme, where an empty
variable resolves as a present-but-empty secret and the consumer decides
whether that is legal.

## Runtime and Deployment Variables

| Variable | Scope | Default | Required when | Notes |
|---|---|---|---|---|
| `DATABASE_URL` | storage | binary default: `postgres://postgres@localhost/proxima_dev` | any non-default DB | dev compose uses `postgres://proxima:proxima@localhost:5434/proxima` |
| `PROXIMA_MCP_BIND` | MCP server | `127.0.0.1:31415` for `proxima-mcp` | custom listener / deployment | non-loopback requires `PROXIMA_EXPOSE_NETWORK=true` |
| `PROXIMA_EXPOSE_NETWORK` | MCP server | unset/false | non-loopback bind | fail-closed exposure gate |
| `PROXIMA_ALLOWED_ORIGINS` | MCP HTTP | unset/deployment-specific | browser/front-door exposure | listener-wide CORS allowlist; comma-separated; never wildcard |
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
| `PROXIMA_PG_SEMANTIC_INDEX_FIRST` | Postgres search | `pushdown` | restoring legacy semantic membership | `off` \| `overfetch` \| `pushdown`. Where the semantic branch's nearest-neighbour scan sits relative to the eligibility joins. The index-first modes (`overfetch`, `pushdown`) change result membership: the eligibility and query filters apply to a bounded ANN candidate window, so a matching row past the window can be missed (an ANN-window approximation — recall, never scope: no mode ever returns a row the filters exclude). `pushdown` is the new default and additionally pushes the owner scope onto the index scan. `off` restores the exact legacy membership: every filter applies under the scan's limit and the branch is exact |
| `PROXIMA_PG_CANDIDATE_WINDOW_DEDUP` | Postgres search | `true` | restoring legacy statement text | window-function candidate dedup and a unique-join supersedes anti-join instead of `DISTINCT ON` and a per-row `NOT EXISTS` probe. Result membership is identical either way; `off` restores the legacy SQL text |
| `PROXIMA_PG_HNSW_EF_SEARCH` | Postgres search | `100` | tuning ANN recall/latency | pgvector `hnsw.ef_search` for the semantic branch's session; range `1..=1000` (the GUC's own bounds); out-of-range refuses at boot |
| `PROXIMA_PG_HNSW_ITERATIVE_SCAN` | Postgres search | `relaxed_order` | tuning filtered ANN scans | `off` \| `strict_order` \| `relaxed_order`; pgvector `hnsw.iterative_scan` |
| `PROXIMA_PG_HNSW_MAX_SCAN_TUPLES` | Postgres search | `20000` | bounding iterative scans | only sent when it differs from pgvector's default and iterative scan is on; range `1..=2147483647` (the GUC's own bounds); out-of-range refuses at boot. **At its default value Proxima does not send the setting at all**, so a server-level (`postgresql.conf`) or database-level (`ALTER DATABASE … SET hnsw.max_scan_tuples = …`) override wins and the session runs with a scan ceiling Proxima never asserted; set this variable explicitly to pin it. This is the one search knob that behaves that way, and it is the one that matters most: `hnsw.max_scan_tuples`, not the SQL `LIMIT`, is what actually bounds the default (`pushdown`) arm's index scan |
| `PROXIMA_PG_SEMANTIC_OVERFETCH_PER_RESULT` | Postgres search | `64` | widening the ANN candidate window | nearest-neighbour candidates fetched per requested result; range `1..=4096`; out-of-range refuses at boot |
| `PROXIMA_PG_SEMANTIC_OVERFETCH_MIN` | Postgres search | `512` | flooring the ANN candidate window | the window never drops below this; range `1..=100000`; out-of-range refuses at boot |
| `PROXIMA_CHANGE_EVENT_COMMIT_GRACE_MS` | change events | unset (`0`, disabled) | concurrent writers with slow commits | withholds events newer than `now - grace` so a forward cursor cannot skip a late commit |
| `PROXIMA_S3_MAX_BLOB_BYTES` | cited blobs | `104857600` | bounding cited-blob size | non-negative integer |
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
| `PROXIMA_TEST_PG_URL` | tests | pg-testkit integration test source DB |
| `PROXIMA_TEST_DATABASE_URL` | tests | HTTP/OIDC e2e dedicated DB |
| `MISTRAL_EMBED_BASE_URL` | source constant | Rust constant for the default Mistral base URL, not an environment variable |
| `MISTRAL_EMBED_MODEL` | source constant | Rust constant for the default Mistral model, not an environment variable |
| `PROXIMA_S3_` | source prefix | configuration prefix constant used to resolve the S3 variables listed above |

## Source Inventory Reconciliation

Inventory sources checked: `docs/10-configuration.md`, `docs/15-deployment.md`,
`apps/proxima-mcp/src/lib.rs`, `crates/proxima/src/runtime_config.rs`,
`crates/proxima/src/config.rs`, `crates/storage-pg/src/lib.rs`,
`crates/storage-pg/src/tuning.rs`,
`crates/storage-pg/src/verbs/consolidate/events.rs`,
`crates/blob-s3/src/config.rs`, and `.github/workflows/ci.yml`. Runtime variables from that inventory are listed in
the runtime table. Test-only and source-constant names are listed under
Build/Test/Internal Variables.
