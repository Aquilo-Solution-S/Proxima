# Configuration
> Status: current

This page is the canonical inventory of settings read by the shipped source.
Defaults and validation limits live in the linked Rust reader. `DATABASE_URL`
is the serving connection; `PROXIMA_PLATFORM_DATABASE_URL` is the required
migration and maintenance connection. Both roles use owner RLS. The settings gate checks every `PROXIMA_*` row.

| Setting | Purpose | When needed | Reader |
|---|---|---|---|
| DATABASE_URL | serving database connection | required for the canonical host | [MCP args](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp/src/args.rs) |
| PROXIMA_PLATFORM_DATABASE_URL | platform database connection | required for boot and maintenance | [MCP runtime](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp/src/lib.rs) |
| PROXIMA_STORE_NUMBER | immutable host-assigned store number | required on every boot | `RuntimeBuilder::store_number` |
| PROXIMA_MCP_BIND | MCP listener address | serving MCP | [runtime config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_EXPOSE_NETWORK | non-loopback exposure gate | public listener | [runtime config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_ALLOWED_ORIGINS | CORS origin allowlist | browser clients | [runtime config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_ALLOWED_HOSTS | Host allowlist | non-loopback listener | [runtime config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_PUBLIC_URL | public OAuth URL | OIDC/public serving | [runtime config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_OIDC_ISSUER | OIDC issuer | OIDC serving | [OIDC](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/auth.rs) |
| PROXIMA_OIDC_AUDIENCE | token audience | OIDC serving | [OIDC](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/auth.rs) |
| PROXIMA_OIDC_REQUIRE_ACCESS_TOKEN_TYP | require an access-token JWT type | issuers using RFC 9068 | [OIDC](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/auth.rs) |
| PROXIMA_OIDC_JWKS_URI | explicit JWKS endpoint | pinned discovery override | [OIDC](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/auth.rs) |
| PROXIMA_OIDC_JWKS_JSON | pinned JWKS document | issuer network unavailable | [OIDC](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/auth.rs) |
| PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS | OIDC request timeout | tune issuer requests | [OIDC](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/auth.rs) |
| PROXIMA_OIDC_ALLOWED_SUBJECTS | subject allowlist | restrict principals | [OIDC](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/auth.rs) |
| PROXIMA_OIDC_SUBJECT_MAP_JSON | issuer-aware subject map | OIDC identity binding | [OIDC](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/auth.rs) |
| PROXIMA_STREAM_MAX_LIFETIME | authenticated stream lifetime | long-lived MCP streams | [MCP runtime](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_STREAM_EPOCH_INTERVAL | stream auth recheck interval | long-lived MCP streams | [MCP runtime](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_REST_ENABLED | REST projection switch | serving `/v1` | [MCP runtime](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_REQUEST_HEADERS | forwarded request-header names | tools need selected headers | [MCP runtime](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_FORWARDER_SUBJECTS | subjects requiring a verified owner claim | forwarded owner access | `ForwarderPolicy` |
| PROXIMA_FORWARDER_ROLE | non-managing forwarding ceiling | forwarded owner access | `ForwarderPolicy` |
| PROXIMA_HEALTH_ENDPOINTS | health route switch | probes need HTTP health | [MCP runtime](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_MAX_REQUEST_BODY_BYTES | request body ceiling | listener sizing | [MCP runtime](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_MCP_SSE_KEEP_ALIVE_SECS | SSE ping interval | proxy idle timeout | [MCP transport](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_MCP_SESSION_IDLE_SECS | session idle timeout | session serving | [MCP transport](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_MCP_SSE_RETRY_SECS | SSE retry hint | client reconnect tuning | [MCP transport](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_MCP_SESSIONS | server-side session switch | stateful MCP clients | [MCP transport](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_MCP_JSON_RESPONSE | JSON response switch | stateless MCP clients | [MCP transport](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_TOOL_PROFILE | served-surface profile | deployment commands plus mandatory reads | `served_surface_from_env` |
| PROXIMA_TOOL_ALLOW | served-surface additions | extend deployment commands | `served_surface_from_env` |
| PROXIMA_TOOL_DENY | served-surface removals | narrow commands; mandatory reads cannot be denied | `served_surface_from_env` |
| PROXIMA_CODE_REPO_ROOTS | OS path list of repository root directories (`:` on Unix, `;` on Windows); each canonicalized at boot, missing or non-directory roots fail boot | Code repository access; see [operate](operate.md#container-and-network-boundary) | [MCP app](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp/src/lib.rs) |
| PROXIMA_EMBED_BASE_URL | embedding endpoint; redirects are refused, so configure the provider's final endpoint directly | enable embeddings | [MCP app](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp/src/lib.rs) |
| PROXIMA_EMBED_API_KEY | embedding bearer | hosted embedding endpoint | [MCP app](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp/src/lib.rs) |
| PROXIMA_EMBED_MODEL | embedding model | enable embeddings | [MCP app](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp/src/lib.rs) |
| PROXIMA_EMBED_DIM | embedding vector width | non-default model width | [MCP app](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp/src/lib.rs) |
| PROXIMA_EMBED_MATRYOSHKA | nested-prefix request switch | Matryoshka model | [MCP app](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp/src/lib.rs) |
| PROXIMA_EMBED_MAX_INPUT_CHARS | embedding input ceiling | provider input tuning | [MCP app](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp/src/lib.rs) |
| PROXIMA_EMBED_QUERY_INSTRUCTION | memory query template | instruction-tuned model | [MCP app](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp/src/lib.rs) |
| PROXIMA_EMBED_QUERY_INSTRUCTION_CODE | code query template | code flavor search | [MCP app](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp/src/lib.rs) |
| PROXIMA_EMBED_CODE_SEMANTIC_WEIGHT | code semantic fusion weight | code hybrid search tuning | [MCP app](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp/src/lib.rs) |
| PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS | provider request timeout | embedding worker tuning | [embedding policy](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/core/src/llm.rs) |
| PROXIMA_EMBED_BATCH_SIZE | provider batch size | embedding worker tuning | [embedding policy](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/core/src/llm.rs) |
| PROXIMA_EMBED_WORKER_INTERVAL_SECONDS | worker poll interval | embedding worker tuning | [embedding policy](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/core/src/llm.rs) |
| PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS | stale claim timeout | embedding worker recovery | [embedding policy](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/core/src/llm.rs) |
| PROXIMA_MISSING_MIGRATION_POLICY | missing-version policy (`reject` or `allow`) | releases intentionally share a database | `missing_migration_policy_from_lookup` |
| PROXIMA_SKIP_MIGRATIONS | migration boot switch | split-role deployment | [runtime config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_RUNTIME_GRANTS | runtime grant boot switch | owner-RLS deployment | [runtime config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs) |
| PROXIMA_PG_MAX_CONNECTIONS | pool size | pool tuning | [pool config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/storage-pg/src/pool_config.rs) |
| PROXIMA_PG_STATEMENT_TIMEOUT_MS | statement timeout | database tuning | [pool config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/storage-pg/src/pool_config.rs) |
| PROXIMA_PG_ACQUIRE_TIMEOUT_SECS | pool acquire timeout | database tuning | [pool config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/storage-pg/src/pool_config.rs) |
| PROXIMA_PG_IDLE_TIMEOUT_SECS | idle connection timeout | database tuning | [pool config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/storage-pg/src/pool_config.rs) |
| PROXIMA_PG_MAX_LIFETIME_SECS | connection lifetime | database tuning | [pool config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/storage-pg/src/pool_config.rs) |
| PROXIMA_PG_HNSW_EF_SEARCH | HNSW search breadth | semantic search tuning | [PG tuning](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/storage-pg/src/tuning.rs) |
| PROXIMA_PG_HNSW_ITERATIVE_SCAN | iterative scan mode | filtered semantic search | [PG tuning](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/storage-pg/src/tuning.rs) |
| PROXIMA_PG_HNSW_MAX_SCAN_TUPLES | iterative scan bound | filtered semantic search | [PG tuning](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/storage-pg/src/tuning.rs) |
| PROXIMA_PUBLICATION_SOURCE | CloudEvent source identity | listenable Fact publication | [outbox config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/config.rs) |
| PROXIMA_OUTBOX_MAX_PENDING | pending outbox ceiling | outbox backpressure tuning | [outbox config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/config.rs) |
| PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES | captured payload ceiling | outbox export sizing | [outbox config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/config.rs) |
| PROXIMA_OUTBOX_HTTP_URL | HTTP outbox receiver | HTTP delivery | [HTTP outbox](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/outbox_http.rs) |
| PROXIMA_OUTBOX_HTTP_TOKEN_FILE | HTTP bearer token file | authenticated HTTP delivery | [HTTP outbox](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/outbox_http.rs) |
| PROXIMA_OUTBOX_HTTP_CA_FILE | HTTP CA bundle | private receiver CA | [HTTP outbox](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/outbox_http.rs) |
| PROXIMA_OUTBOX_PUBLISHED_RETENTION_SECS | published-record retention | reclaim delivered records | [outbox config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/config.rs) |
| PROXIMA_S3_MAX_BLOB_BYTES | cited blob size ceiling | S3 blob storage | [S3 config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/blob-s3/src/config.rs) |
| PROXIMA_S3_BUCKET | S3 bucket | enable S3 storage | [S3 config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/blob-s3/src/config.rs) |
| PROXIMA_S3_REGION | S3 region | S3 storage | [S3 config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/blob-s3/src/config.rs) |
| PROXIMA_S3_ACCESS_KEY_ID | S3 access key | S3 storage | [S3 config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/blob-s3/src/config.rs) |
| PROXIMA_S3_SECRET_ACCESS_KEY | S3 secret key | S3 storage | [S3 config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/blob-s3/src/config.rs) |
| PROXIMA_S3_ENDPOINT_URL | object I/O endpoint | non-AWS S3 storage | `S3RuntimeConfig::from_lookup` |
| PROXIMA_S3_PUBLIC_ENDPOINT_URL | endpoint for presigned links | holders reach a separate public address | `S3RuntimeConfig::from_lookup` |
| PROXIMA_PLAINTEXT_HOSTS | plaintext host allowlist | named internal HTTP sidecar | [network policy](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/core/src/net.rs) |
| PROXIMA_S3_FORCE_PATH_STYLE | S3 path-style switch | path-style endpoint | [S3 config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/blob-s3/src/config.rs) |
| PROXIMA_S3_UPLOAD_TTL_SECONDS | upload URL lifetime | cited blob uploads | [S3 config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/blob-s3/src/config.rs) |
| PROXIMA_S3_READ_TTL_SECONDS | read URL lifetime | cited blob reads | [S3 config](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/blob-s3/src/config.rs) |
| PROXIMA_DEV_POSTGRES_PORT | Compose Postgres host port | local Compose | [dev Compose](https://github.com/Aquilo-Solution-S/Proxima/blob/main/docker-compose.dev.yml) |
| PROXIMA_DEV_S3_PORT | Compose S3 host port | local Compose | [dev Compose](https://github.com/Aquilo-Solution-S/Proxima/blob/main/docker-compose.dev.yml) |
| PROXIMA_TEST_PG_URL | test Postgres admin URL | pg-testkit tests | [PG testkit](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/pg-testkit/src/lib.rs) |
| PROXIMA_PG_TESTKIT_CHILD | pg-testkit child-process marker | split-template tests | [split-template test](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/pg-testkit/tests/split_template.rs) |
| PROXIMA_PG_TESTKIT_TEMPLATE | pg-testkit template database name | split-template tests | [split-template test](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/pg-testkit/tests/split_template.rs) |
| PROXIMA_TEST_DATABASE_URL | e2e database URL | HTTP/OIDC e2e tests | [OIDC e2e](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp/tests/oidc_e2e.rs) |
| PROXIMA_DIFFERENTIAL_DIR | differential dump directory | differential test inspection | [differential tests](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/storage-pg/tests/owner_erase_differential.rs) |
| PROXIMA_SCHEMA_PG_URL | schema dump admin URL | schema regeneration | [schema script](https://github.com/Aquilo-Solution-S/Proxima/blob/main/scripts/regen-schema-sql.sh) |
| PROXIMA_PG_DUMP | pg_dump executable | schema regeneration | [schema script](https://github.com/Aquilo-Solution-S/Proxima/blob/main/scripts/regen-schema-sql.sh) |
| PROXIMA_RESET_CONFIRM | reset confirmation token | development database reset | [dev migrate](https://github.com/Aquilo-Solution-S/Proxima/blob/main/tools/dev-migrate/src/main.rs) |

Removed names are documented only in release notes; they are not settings and
must not be reintroduced as compatibility aliases.
