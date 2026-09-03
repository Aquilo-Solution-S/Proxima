# Environment Variable Reference

This table is a human reference. Source code and deployment manifests remain authoritative.

**Runtime parsing.** Ordinary Proxima runtime and deployment settings are
trimmed before they are read. A variable set to the empty string — or to
nothing but whitespace — is treated exactly as if it were absent, taking the
`Default` column. Exporting `FOO=` is therefore never a way to say something
different from leaving `FOO` out. A trailing newline picked up from a here-doc
is harmless rather than a startup error naming a value nobody typed.

The one deliberate exception is the `env:` secret scheme, where an empty
variable resolves as a present-but-empty secret and the consumer decides
whether that is legal.

**Dev Compose interpolation.** The Dev Compose variables below are consumed by
Docker Compose, not Proxima's runtime parser. The `${VAR:-default}` form selects
`default` when `VAR` is unset or empty. Compose does not trim values, so a
whitespace-only value is passed through and rejected as an invalid `hostPort`;
set a valid non-whitespace host port such as `55432` instead.

## Runtime and Deployment Variables

| Variable | Scope | Default | Required when | Notes |
|---|---|---|---|---|
| `DATABASE_URL` | storage | binary default: `postgres://postgres@localhost/proxima_dev` | any non-default DB | dev compose uses `postgres://proxima:proxima@localhost:${PROXIMA_DEV_POSTGRES_PORT:-5434}/proxima` |
| `PROXIMA_MCP_BIND` | MCP server | `127.0.0.1:31415` for `proxima-mcp` | custom listener / deployment | non-loopback requires `PROXIMA_EXPOSE_NETWORK=true` |
| `PROXIMA_EXPOSE_NETWORK` | MCP server | unset/false | non-loopback bind | fail-closed exposure gate |
| `PROXIMA_ALLOWED_ORIGINS` | MCP HTTP | unset/deployment-specific | browser/front-door exposure | listener-wide CORS allowlist; comma-separated; never wildcard |
| `PROXIMA_ALLOWED_HOSTS` | MCP HTTP | public URL host + allowed origins | non-loopback exposure override | DNS-rebinding guard; loopback always permitted |
| `PROXIMA_PUBLIC_URL` | MCP/OIDC | unset | OIDC/public deployment | public HTTPS base; host is auto-allowed |
| `PROXIMA_OIDC_ISSUER` | OIDC auth | unset | OIDC deployment | issuer URL |
| `PROXIMA_OIDC_AUDIENCE` | OIDC auth | unset | OIDC deployment | expected token audience |
| `PROXIMA_OIDC_JWKS_URI` | OIDC auth | discovery default | non-default JWKS | overrides discovery |
| `PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS` | OIDC auth | `10` | slower issuer response budget | complete discovery/JWKS request timeout, including connect and body read; range `1..=300`; invalid values fail boot |
| `PROXIMA_OIDC_ALLOWED_SUBJECTS` | OIDC auth | unset | subject allowlist desired | comma-separated `sub` values |
| `PROXIMA_OIDC_SUBJECT_MAP_JSON` | OIDC auth | unset | OIDC deployment unless shorthand is used | issuer-aware `(iss, sub) -> user_id` JSON map; mutually exclusive with `PROXIMA_OIDC_SUBJECT_MAP`; optional per-entry `trusted_model_id` — see below |
| `PROXIMA_OIDC_SUBJECT_MAP` | OIDC auth | unset | OIDC deployment unless JSON map is used | single-issuer `sub:<uuid>` shorthand bound to `PROXIMA_OIDC_ISSUER`; mutually exclusive with JSON map; never yields a `trusted_model_id` |
| `PROXIMA_STREAM_MAX_LIFETIME` | MCP stream auth | source default | long-lived Streamable HTTP responses | max seconds before re-validation |
| `PROXIMA_STREAM_EPOCH_INTERVAL` | MCP stream auth | source default | open response stream auth checks | auth-epoch re-check seconds |
| `PROXIMA_REST_ENABLED` | REST surface | `false` | serving `/v1` beside `/mcp` | renders the tool manifest as REST ([17](../17-rest-surface.md)); also needs the `rest` cargo feature at build time |
| `PROXIMA_TOOL_PROFILE` | MCP tool surface | `memory` | widening to the full surface | `full` is opt-in and adds `core_membership`/`core_transfer` |
| `PROXIMA_TOOL_ALLOW` | MCP tool surface | unset | profile extension | comma-separated canonical scope keys unioned into profile |
| `PROXIMA_TOOL_DENY` | MCP tool surface | unset | profile restriction | comma-separated canonical scope keys removed after allow |
| `PROXIMA_EMBED_BASE_URL` | embeddings | unset | any OpenAI-compatible endpoint, local or hosted | required with `PROXIMA_EMBED_MODEL` to enable embeddings; loopback needs no key |
| `PROXIMA_EMBED_API_KEY` | embeddings | unset | hosted embedding endpoint | bearer sent to `/embeddings` |
| `PROXIMA_EMBED_MODEL` | embeddings | unset | embeddings enabled | required with `PROXIMA_EMBED_BASE_URL`; must yield 1024-dim vectors |
| `PROXIMA_EMBED_MATRYOSHKA` | embeddings | `false` | nested-prefix model wider than 1024 | sends a `dimensions` request parameter |
| `PROXIMA_EMBED_MAX_INPUT_CHARS` | embeddings | unset | provider needs a client-side input bound | longest input, in characters, sent before chunked rescue; unset/empty/whitespace means no client-side bound; minimum `4095`; invalid values fail boot |
| `PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS` | embedding runtime | `120` | slow provider | complete provider request timeout; range `1..=3600`; invalid values fail boot |
| `PROXIMA_EMBED_BATCH_SIZE` | embedding runtime | `32` | provider batch tuning | texts per provider call; range `1..=1024`; invalid values fail boot |
| `PROXIMA_EMBED_WORKER_INTERVAL_SECONDS` | embedding runtime | `5` | worker cadence tuning | idle poll seconds; range `1..=3600`; invalid values fail boot |
| `PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS` | embedding runtime | `900` | crash-reclaim tuning | range `1..=86400`; must be strictly greater than request timeout and cover the longest honest drain interval between successful claim renewals |
| `PROXIMA_SKIP_MIGRATIONS` | boot | `false` | split-role GitOps deploys | boot without applying migrations; the schema must already be at the current lane or boot fails closed |
| `PROXIMA_PG_MAX_CONNECTIONS` | Postgres pool | `10` | tuning pool size | minimum 1; invalid values fail config resolution |
| `PROXIMA_PG_STATEMENT_TIMEOUT_MS` | Postgres pool | `300000` | tuning request timeouts | `0` disables by omitting the Postgres option; migrations and bulk erase opt out separately |
| `PROXIMA_PG_ACQUIRE_TIMEOUT_SECS` | Postgres pool | `5` | tuning pool acquisition | seconds; `0` is passed to SQLx unchanged |
| `PROXIMA_PG_IDLE_TIMEOUT_SECS` | Postgres pool | `600` | tuning connection reuse | seconds; `0` is passed to SQLx unchanged |
| `PROXIMA_PG_MAX_LIFETIME_SECS` | Postgres pool | `1800` | tuning connection recycling | seconds; `0` is passed to SQLx unchanged |
| `PROXIMA_PG_HNSW_EF_SEARCH` | Postgres search | `100` | tuning ANN recall/latency | pgvector `hnsw.ef_search` for the semantic branch's session; range `1..=1000` (the GUC's own bounds); out-of-range refuses at boot |
| `PROXIMA_PG_HNSW_ITERATIVE_SCAN` | Postgres search | `relaxed_order` | tuning filtered ANN scans | `off` \| `strict_order` \| `relaxed_order`; pgvector `hnsw.iterative_scan` |
| `PROXIMA_PG_HNSW_MAX_SCAN_TUPLES` | Postgres search | `20000` | bounding iterative scans | sent on every iterative-scan session (`SET LOCAL`); range `1..=2147483647` (the GUC's own bounds); out-of-range refuses at boot. This, not the SQL `LIMIT`, bounds the semantic branch's index scan |
| `PROXIMA_CHANGE_EVENT_COMMIT_GRACE_MS` | change events | unset (`0`, disabled) | concurrent writers with slow commits | withholds events newer than `now - grace` so a forward cursor cannot skip a late commit |
| `PROXIMA_S3_MAX_BLOB_BYTES` | cited blobs | `104857600` | bounding cited-blob size | non-negative integer |
| `PROXIMA_S3_BUCKET` | cited blobs | unset | enable S3 cited-blob storage | credentials use AWS SDK provider chain |
| `PROXIMA_S3_REGION` | cited blobs | unset | S3 bucket configured | S3 region |
| `PROXIMA_S3_ENDPOINT_URL` | cited blobs | AWS region endpoint | S3-compatible endpoint | optional |
| `PROXIMA_S3_FORCE_PATH_STYLE` | cited blobs | `false` | path-style S3 endpoint | optional |
| `PROXIMA_S3_UPLOAD_TTL_SECONDS` | cited blobs | `900` | non-default upload TTL | presigned upload URL TTL |
| `PROXIMA_S3_READ_TTL_SECONDS` | cited blobs | `300` | non-default read TTL | presigned read URL TTL |

Removed in v0.0.8: `PROXIMA_PG_SEMANTIC_INDEX_FIRST`, `PROXIMA_PG_CANDIDATE_WINDOW_DEDUP`, `PROXIMA_PG_SEMANTIC_OVERFETCH_PER_RESULT`, `PROXIMA_PG_SEMANTIC_OVERFETCH_MIN`. The knobs they set were consumed by no query path; leaving one of them set now refuses boot with an error naming the removal, so a deployment carrying a dead knob learns immediately instead of silently running the shipped path.

### Subject map entries

`PROXIMA_OIDC_SUBJECT_MAP_JSON` is an array of `{iss, sub, user_id}` objects,
each with an optional `trusted_model_id`:

```json
[
  {"iss": "https://zitadel.example.com",
   "sub": "human-subject-id",
   "user_id": "550e8400-e29b-41d4-a716-446655440000"},
  {"iss": "https://zitadel.example.com",
   "sub": "runner-service-user-id",
   "user_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
   "trusted_model_id": "acme/runner-v3"}
]
```

`trusted_model_id` is trimmed, must be non-blank, and is bounded at 120
characters (the operator-label bound); an invalid value fails boot, as does an
unrecognised key anywhere in an entry — a typo such as `trusted_model` must
not degrade silently into "this principal binds nothing". It
certifies **which configured runner principal reached the edge** — the
deployment's own statement about a credential it issued — not that a model
produced any particular content. It reaches tools as
`AuthzContext::trusted_model_id()` / `ToolCaller::trusted_model_id` and
becomes the persisted operator label, outranking a caller-supplied
`model_id` argument or `X-Proxima-Model-Id` header (a *differing*
caller-supplied label is refused, a blank one treated as absent; see
[17 §Call Context](../17-rest-surface.md#call-context)). Entries without the
field, and principals absent from the map, are unchanged. The
`PROXIMA_OIDC_SUBJECT_MAP` shorthand has no field for it and never yields
one.

## CLI Flags That Behave Like Configuration

| Flag | Required when | Notes |
|---|---|---|
| `--database-url <URL>` | non-default DB without `DATABASE_URL` | overrides the Postgres URL |
| `--bind <ADDR>` | custom loopback listener | non-loopback binds use env-gated deployment config |

## Dev Compose Variables

These variables affect only the host ports published by
`docker-compose.dev.yml`. Set them before `docker compose up`; container ports
remain fixed at `5432` (Postgres) and `9000` (RustFS).

| Variable | Scope | Default | Required when | Notes |
|---|---|---|---|---|
| `PROXIMA_DEV_POSTGRES_PORT` | dev Compose | `5434` | the default host port is occupied | published host port for pgvector Postgres; use the same value in `DATABASE_URL` |
| `PROXIMA_DEV_S3_PORT` | dev Compose | `9100` | the default host port is occupied | published host port for RustFS; use `http://127.0.0.1:<port>` as `PROXIMA_S3_ENDPOINT_URL` |

## Build/Test/Internal Variables

| Variable | Scope | Notes |
|---|---|---|
| `PROXIMA_TEST_PG_URL` | tests | pg-testkit integration test source DB |
| `PROXIMA_TEST_DATABASE_URL` | tests | HTTP/OIDC e2e dedicated DB |
| `PROXIMA_S3_` | source prefix | configuration prefix constant used to resolve the S3 variables listed above |

## Source Inventory Reconciliation

Inventory sources checked: `docs/10-configuration.md`, `docs/15-deployment.md`,
`docker-compose.dev.yml`,
`apps/proxima-mcp/src/lib.rs`, `crates/proxima/src/runtime_config.rs`,
`crates/proxima/src/config.rs`, `crates/storage-pg/src/lib.rs`,
`crates/storage-pg/src/pool_config.rs`, `crates/storage-pg/src/tuning.rs`,
`crates/storage-pg/src/verbs/consolidate/events.rs`,
`crates/blob-s3/src/config.rs`, and `.github/workflows/ci.yml`. Runtime variables from that inventory are listed in
the runtime table. Test-only and source-constant names are listed under
Build/Test/Internal Variables.
