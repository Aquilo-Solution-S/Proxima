# 15 — Deployment

> **Status:** current + deferred sections. Deferred rows are design intent, not implementation claims.

Containerized Code-flavor server, authenticated exclusively via Zitadel
bearer JWT, with a single unauthenticated route:
`GET /.well-known/oauth-protected-resource` (RFC 9728). The binary is
`apps/proxima-mcp` (code flavor default-on); add the `rest` feature and
set `PROXIMA_REST_ENABLED=true` to serve `/v1` beside `/mcp`.

## Runtime requirements

Postgres must have pgvector `>= 0.8.0` with `hnsw` index type and
`hnsw.iterative_scan` support. Migrations run automatically on first boot and
fail closed if the extension version/GUC preflight is not satisfied.
Optional dependencies: S3 for cited-blob storage (see [10](10-configuration.md#large-artefact-s3)),
and an OpenAI-compatible embedding client; when configured, the server
drains embeddings in-process automatically. Without embeddings the server
operates in degraded lexical-only mode.

## PostgreSQL extensions and search performance

What actually moves retrieval quality and speed on the database side, in
descending order of leverage. Only the first row is required.

| Piece | Required | What it does for Proxima |
|---|---|---|
| `pgvector >= 0.8.0` | **yes** | The semantic arm: HNSW index + `hnsw.iterative_scan` for filtered vector search. Without it the server refuses to boot. |
| Snowball configurations (built in) | shipped | Lexical stemming. 30 configurations in `pg_catalog`. |
| hunspell dictionaries | no | Compound splitting for compounding languages: `german` alone never matches `Tür` in `Türbreite`. Requires the `.dict`/`.affix` files in the server's `$SHAREDIR/tsearch_data` and a `CREATE TEXT SEARCH CONFIGURATION` on top — a server-filesystem operation, not a migration. Stamp rows with the custom configuration via the `language` argument. |
| `pg_trgm` | no | Trigram GIN indexes can serve `LIKE '%…%'` substring predicates. Since v0.0.8 core's substring arm is its own statement against the base tables (the tsquery arms moved there to reach the v0.0.8 GIN indexes, and one unindexable arm in the same disjunction would have cost them their index path), but it still has no index of its own: for a sidecar the predicate is over the builder's `concat_ws` projection, and no migration can name that expression. So this still helps only flavor surfaces whose substring predicates hit a real column (e.g. code-chunk path/text bonuses) — measure before adding, every write pays for the index. Core's answer is instead to skip that statement whenever the tsquery arms already fill the page above the substring band. |
| `pg_stat_statements` | no | The profiling loop: find the queries that actually cost, before tuning anything. |

Two operational notes that repeatedly matter:

- **VACUUM after bulk ingest.** GIN indexes buffer inserts in a fastupdate
  pending list; until it is flushed the planner misprices the index and
  falls back to sequential scans (observed: bitmap plans only won naturally
  after `VACUUM (ANALYZE)` on a 40k-row bulk load). Autovacuum gets there
  eventually; after a large import, running it explicitly gets there now.
- **Custom text search configurations** are referenced by per-row
  `regconfig` values. Before dropping one, run
  `proxima_core.lexical_language_forget('cfg')` — `PostgreSQL` permits the
  drop while rows still reference it, leaving dangling OIDs that make those
  rows fail on UPDATE (see [07](07-storage.md); lexical
  language*).

## Connecting to encrypted PostgreSQL

Verified TLS:

```sh
DATABASE_URL=postgres://USER:PASS@HOST:5432/DB?sslmode=verify-full&sslrootcert=/path/ca.pem
```

`sslmode=require` encrypts transport but does not verify hostname. At-rest
TDE / volume encryption is transparent to Proxima. Do NOT use pgcrypto column
encryption for searched columns: `embeddings.vec`, sidecar text, `tags`.
Run migrations with a DDL-capable role; run the app with a narrower DML
role, with `PROXIMA_SKIP_MIGRATIONS=true` so the app never attempts DDL.

Migrations run automatically on first boot when that variable is unset. Check
[07](07-storage.md) / [how-to/migrations.md](how-to/migrations.md) before relying on that: a lane
that rewrites tables holds `ACCESS EXCLUSIVE` for the duration and is not
online-safe. The current v0.0.8 lane is one file, `0001_v008.sql`. It is a
reset, not an online ALTER.
Boot migrations set `lock_timeout = 5s`, so a migration that cannot take the
lock fails and retries on the next pod rather than queueing behind readers.

## Environment contract

| Var | Required | Example | Purpose |
|---|---|---|---|
| `DATABASE_URL` | yes | `postgres://user:pass@host:5432/db` | Postgres connection string. |
| `PROXIMA_MCP_BIND` | yes | `0.0.0.0:8080` | MCP listener address. |
| `PROXIMA_EXPOSE_NETWORK=true` | yes | `true` | Required for non-loopback bind. |
| `PROXIMA_ALLOWED_ORIGINS` | yes | `https://claude.example.com,https://codex.example.com` | Comma-separated listener-wide browser CORS allowlist; never `*`. |
| `PROXIMA_ALLOWED_HOSTS` | no | `proxima.example.com` | Inbound `Host` allowlist (hostnames or `host:port`, no wildcards) for the listener-wide DNS-rebinding guard. Defaults to the host of `PROXIMA_PUBLIC_URL` + the allowed origins; loopback always permitted. Set only to override. |
| `PROXIMA_PUBLIC_URL` | yes | `https://proxima.example.com` | Public HTTPS base for OIDC; its host is auto-allowed as an inbound `Host`. |
| `PROXIMA_OIDC_ISSUER` | yes | `https://zitadel.example.com` | Zitadel issuer URL. |
| `PROXIMA_OIDC_AUDIENCE` | yes | `https://proxima.example.com` | Public-origin resource id expected in token `aud`; one token covers `/mcp` and an enabled `/v1`. |
| `PROXIMA_OIDC_JWKS_URI` | no | `https://zitadel.example.com/oauth/v2/keys` | Overrides OIDC discovery. |
| `PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS` | no | `10` | Complete-request timeout for discovery and JWKS HTTP requests, including connect and body read. Default `10`; range `1..=300`; invalid values fail boot. |
| `PROXIMA_OIDC_SUBJECT_MAP_JSON` | yes* | `[{"iss":"https://zitadel.example.com","sub":"...","user_id":"550e8400-e29b-41d4-a716-446655440000"}]` | Issuer-aware `(iss, sub) -> user_id` identity map. Required whenever `PROXIMA_OIDC_ISSUER` is set, unless `PROXIMA_OIDC_SUBJECT_MAP` is given instead (the two are mutually exclusive). |
| `PROXIMA_OIDC_SUBJECT_MAP` | yes* | `sub-1:550e8400-e29b-41d4-a716-446655440000` | Legacy single-issuer shorthand `sub:<uuid>,sub2:<uuid2>`; every entry binds to `PROXIMA_OIDC_ISSUER`. Valid only because exactly one issuer is ever accepted here. |
| `PROXIMA_OIDC_ALLOWED_SUBJECTS` | no | `user1,user2` | Comma-separated `sub` allowlist layered on top of the subject map above; never an identity source by itself. |
| `PROXIMA_REST_ENABLED` | no | `true` | Serve `/v1` on the MCP listener. Default `false`; has no effect unless the binary was built with the `rest` feature. |
| `PROXIMA_TOOL_PROFILE` | no | `memory` | Tool profile. **Unset ⇒ fail-closed `memory`** (excludes `core_membership` + `core_transfer`). Set `full` to advertise the whole surface incl. `core_transfer` (moves a memory's owner to another group) — logged at startup. |
| `PROXIMA_TOOL_ALLOW` | no | `core_goal:set` | Comma-separated canonical scope keys added after profile resolution. |
| `PROXIMA_TOOL_DENY` | no | `core_goal:decompose` | Comma-separated canonical scope keys removed after allow. Compliance erase is not exposed as an MCP action. |
| `PROXIMA_EMBED_BASE_URL` | when enabled | `https://embeddings.example/v1` | OpenAI-compatible `/embeddings` base. Required with `PROXIMA_EMBED_MODEL` when embeddings are enabled; plaintext `http://` is accepted for loopback only. |
| `PROXIMA_EMBED_API_KEY` | no | `sk-...` | Bearer for a hosted embedding endpoint. Omit for a local one. |
| `PROXIMA_EMBED_MODEL` | when enabled | `provider-embedding-model` | Embedding model id. Required with `PROXIMA_EMBED_BASE_URL` when embeddings are enabled; must return 1024-dim vectors. |
| `PROXIMA_EMBED_MATRYOSHKA` | no | `false` | Request 1024 dimensions from a nested-prefix model wider than 1024. |
| `PROXIMA_EMBED_MAX_INPUT_CHARS` | no | `16384` | Longest input, in characters, that will be *sent*. Unset ⇒ no client-side bound. Over-cap input is refused without a request and split into chunked embeddings instead. Minimum `4095`; below that the split cannot satisfy the cap and boot fails. Set it for a provider that dies on over-long input rather than rejecting it (a local Ollama does) — see docs/10 §Bounding embedding input. |
| `PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS` | no | `120` | Complete provider-request timeout; range `1..=3600`. Enforced at the generic client boundary and by the shipped HTTP adapter. |
| `PROXIMA_EMBED_BATCH_SIZE` | no | `32` | Texts per provider call; range `1..=1024`. |
| `PROXIMA_EMBED_WORKER_INTERVAL_SECONDS` | no | `5` | Idle worker poll interval; range `1..=3600`. |
| `PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS` | no | `900` | Crash-reclaim window; range `1..=86400`, strictly greater than request timeout. Must cover the longest honest drain interval between successful claim renewals. Live claims heartbeat every third of the window; claim-token fencing rejects writes after a real reclaim. |
| `PROXIMA_SKIP_MIGRATIONS` | no | `true` | Boot without applying migrations, for the split-role topology above. The schema must already be at the current lane — boot fails closed otherwise. |
| `PROXIMA_S3_BUCKET` | no | `proxima-cited-blobs` | Cited-blob bucket. |
| `PROXIMA_S3_REGION` | no | `us-east-1` | S3 region. |
| `PROXIMA_S3_ENDPOINT_URL` | no | `https://s3.example.com` | S3-compatible endpoint. |
| `PROXIMA_PG_*` | no | see [env-vars](reference/env-vars.md) | Pool and search tuning (`MAX_CONNECTIONS`, `HNSW_*`, `SEMANTIC_*`). Defaults are the shipped path. |

## No standing bypass

The served transports have no Proxima-local static bearer fallback. Configure a host
`Authenticator` that returns server-resolved `OwnerRoles`; the shipped OIDC
authenticator obtains them through its constructor-injected `OwnerAccessPort`.
Stale local-token bearer prefixes fail closed and are not forwarded to host
auth. Break-glass during a Zitadel outage belongs in the external identity
layer as a short-lived, audited host credential, never as a standing token on
`/mcp` or `/v1`.

## Security guarantee

Only `/.well-known/oauth-protected-resource` is bearer-anonymous; it remains
behind the listener-wide Host and body-limit gates. All `/mcp` endpoints, and
all `/v1` endpoints when enabled, require the same public-origin `aud`-bound
Zitadel JWT validated in-process.
401 responses carry `WWW-Authenticate: Bearer resource_metadata="…"`
(RFC 9728). Defense in depth: the same JWT MUST be validated at the
cluster edge (see [§Edge defense-in-depth](#edge-defense-in-depth)).

> **Host-resolved OIDC identity, multi-owner session scope.** The token's
> `(iss, sub)` resolves through `PROXIMA_OIDC_SUBJECT_MAP_JSON` or
> `PROXIMA_OIDC_SUBJECT_MAP` to a Proxima `UserId`; `PgOwnerAccessResolver`
> reads current group memberships into `OwnerRoles`. The client selects one
> authorized owner during MCP `initialize` using `X-Proxima-Owner`:
> `personal:<uuid>` or `group:<uuid>`. The server binds that owner to
> the returned `Mcp-Session-Id`. Every later request revalidates the bearer and
> narrows the freshly resolved roles to the bound owner; membership
> removal denies the next request, including an already-bound session. An
> absent subject-map entry, invalid owner key, or non-member owner selection
> fails closed.
>
> Tool advertisement is `frozen registry ∩ deployment ToolScope ∩ bound-owner
> role`: read-only tools require read access; write/unknown tools require
> Fact write access. Compliance erase stays Host API/admin-only.

One non-empty `HostAllowlist` gates the complete listener *before* auth:
`/mcp`, `/v1`, mounted flavor routes, OAuth metadata, and fallback responses.
The same value configures rmcp's inner `/mcp` DNS-rebinding guard. Only
loopback plus the resolved public host(s) are accepted; any other `Host` is
rejected with 403. The public host is taken from `PROXIMA_ALLOWED_HOSTS`
(else the host of `PROXIMA_PUBLIC_URL` and the allowed origins), so the gateway
forwards the real `Host` unchanged — no `Host`-rewrite-to-localhost workaround
is needed. The allowlist is honored on loopback binds too, which supports a
same-host reverse proxy preserving the public `Host`; `PROXIMA_EXPOSE_NETWORK`
controls non-loopback socket binding, not Host-policy activation.

Browser order is body limit → Host → CORS → bearer auth. An allowed CORS
preflight (`OPTIONS` + `Origin` + `Access-Control-Request-Method`) returns
`204` without a bearer; the actual `/mcp`, `/v1`, or mounted flavor request
still requires its normal bearer. The response echoes the allowed Origin and
requested method/header names, never `*`, and does not enable cookie
credentials. Native clients that omit `Origin` retain the same auth path.

## Build & run

```sh
# The Dockerfile builds the default Code-flavor host.
# This default image serves MCP only; see below for the two REST gates.
docker build -t proxima-mcp .

docker run -p 8080:8080 \
  -e DATABASE_URL=postgres://... \
  -e PROXIMA_MCP_BIND=0.0.0.0:8080 \
  -e PROXIMA_EXPOSE_NETWORK=true \
  -e PROXIMA_ALLOWED_ORIGINS=https://claude.example.com \
  -e PROXIMA_PUBLIC_URL=https://proxima.example.com \
  -e PROXIMA_OIDC_ISSUER=https://zitadel.example.com \
  -e PROXIMA_OIDC_AUDIENCE=https://proxima.example.com \
  -e PROXIMA_OIDC_SUBJECT_MAP=zitadel-subject-id:550e8400-e29b-41d4-a716-446655440000 \
  proxima-mcp
```

Memory-brain surface:

```sh
docker run -p 8080:8080 \
  -e DATABASE_URL=postgres://... \
  -e PROXIMA_MCP_BIND=0.0.0.0:8080 \
  -e PROXIMA_EXPOSE_NETWORK=true \
  -e PROXIMA_ALLOWED_ORIGINS=https://claude.example.com \
  -e PROXIMA_PUBLIC_URL=https://proxima.example.com \
  -e PROXIMA_OIDC_ISSUER=https://zitadel.example.com \
  -e PROXIMA_OIDC_AUDIENCE=https://proxima.example.com \
  -e PROXIMA_OIDC_SUBJECT_MAP=zitadel-subject-id:550e8400-e29b-41d4-a716-446655440000 \
  -e PROXIMA_TOOL_PROFILE=memory \
  proxima-mcp
```

Optional REST surface:

```dockerfile
# In the image build stage; the repository Dockerfile currently omits REST.
RUN cargo build --release -p proxima-mcp --features rest
```

```sh
# In that REST-capable image. Omitting this variable still serves MCP only.
docker run ... \
  -e PROXIMA_REST_ENABLED=true \
  -e PROXIMA_OIDC_AUDIENCE=https://proxima.example.com \
  proxima-mcp
```

The Cargo feature only compiles the adapter; `PROXIMA_REST_ENABLED=true` only
mounts it. Both gates are required. Route `/v1` at the gateway only when both
are set (see [10 §REST Surface](10-configuration.md#rest-surface)).

The default `memory` profile keeps the advertised tool surface small (better LLM
tool selection, lower blast radius) and fail-closed — `core_transfer` and
`core_membership` are opt-in via `PROXIMA_TOOL_PROFILE=full`. The profile is not
itself a security boundary: every tool call remains gated by per-actor authz and
role checks.

**Per-user tool scope is a host concern, not a substrate feature.** The env
profile is one deployment-wide ceiling. A host that composes Proxima as a library
resolves a per-subject `ToolScope` (e.g. derived from the subject's resolved
role) and attaches it with
`AuthzContext::server_resolved(roles, path).with_tool_scope(scope)`; the shared edge
intersects it with the env ceiling (`ToolScope::intersect` only narrows, never
widens), so a per-user scope can restrict but never exceed the deployment
ceiling. Proxima ships the mechanism; which subject gets which scope is the
host's policy.

MCP clients send `X-Proxima-Owner` on `initialize`; the bound owner is
server-side session state, not a per-call tool argument.

In multi-space hosts, call `core_memory_spaces` before durable memory writes. Use a returned `space` key in `core_remember`, `core_record_utterance`, `core_search_memories`, `core_derive`, and `core_interpret`; hydrate a memory through `proxima://memory/{id}`. Omitted `space` preserves the current bound owner. A cross-space derivation or interpretation may ground in readable handles outside the selected write space.

## Embedding Ops

Runtime search:

| Item | Runtime value |
|---|---|
| vector type | `vector(1024)`. Any embedding model must return 1024 dimensions — see `PROXIMA_EMBED_MATRYOSHKA` for nested-prefix models wider than that |
| ANN index | shared `idx_embeddings_vec_hnsw` |
| provider batching | `PROXIMA_EMBED_BATCH_SIZE`; host policy, not a core provider assumption |
| durable claims | heartbeat during all provider calls; reconcile and observability use `PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS` |
| semantic-search GUCs | `SET LOCAL hnsw.ef_search = 100`; `SET LOCAL hnsw.iterative_scan = relaxed_order`; `SET LOCAL hnsw.max_scan_tuples = 20000` |
| cold owner subsets | planner may prefer owner btree + exact sort |

Host-only operator methods:

| Verb | Authorization | Contract |
|---|---|---|
| `Engine::embedding_ann_observability(authz)` | `AuthPath::System` or `ComplianceAdminPort::may_perform_operator_maintenance` | owner-agnostic rows/bytes/backlog/stale/orphan/recall-canary signals |
| `Engine::sweep_orphan_embedding_rows(authz)` | same | deletes embedding infra rows whose source memory/goal row no longer exists |

Compliance erase is separate: owner/source erasure deletes embeddings,
`embedding_heads`, and `embedding_jobs` synchronously at commit. The orphan
sweep is crash-residue maintenance only.

Day-2 operations — backup/restore, failed-migration behavior, readiness probe,
and the embedding signal→action runbook: [how-to/operate.md](how-to/operate.md).

## Zitadel setup

- Create a Zitadel project.
- Create an API/resource whose identifier equals `PROXIMA_OIDC_AUDIENCE`, the
  public origin (`https://proxima.example.com`), not a surface path.
- Register an MCP client app (auth-code + PKCE) with redirect URIs for
  Claude Code / Codex / Cursor.
- The client must request that origin as its RFC 8707 resource/audience; the
  resulting token covers `/mcp` and an enabled `/v1`.
- Optionally enable Dynamic Client Registration (RFC 7591).

## Edge defense-in-depth

Ingress MUST:

- Terminate TLS.
- Pass `/.well-known/oauth-protected-resource` through unauthenticated.
- Statelessly validate the Zitadel JWT on `/mcp`, and on `/v1` when that
  optional surface is routed (NOT a session login-proxy).
- Forward the `Authorization` header.
- Pass browser preflight `OPTIONS` without JWT validation, and forward
  `Origin`, `Access-Control-Request-Method`, and
  `Access-Control-Request-Headers`; Proxima owns the allowlist decision.
- Restrict pod ingress to the gateway with a `NetworkPolicy`.
- Cap request body size and apply a per-client rate/concurrency limit. The
  server rejects an oversized request body before parsing and caps individual
  tool-arg fields, but does no rate limiting of its own, so the proxy is the
  first bound on abusive request volume.

Envoy `jwt_authn` filter snippet:

```yaml
http_filters:
  - name: envoy.filters.http.jwt_authn
    typed_config:
      "@type": type.googleapis.com/envoy.extensions.filters.http.jwt_authn.v3.JwtAuthentication
      providers:
        zitadel:
          issuer: https://zitadel.example.com
          audiences: ["https://proxima.example.com"]
          jwks_uri: https://zitadel.example.com/oauth/v2/keys
      rules:
        - match:
            prefix: /mcp
          requires:
            providers: [zitadel]
        # Include this rule only when the gateway exposes the enabled
        # REST-capable deployment. The rule does not enable `/v1` itself.
        - match:
            prefix: /v1
          requires:
            providers: [zitadel]
```

Ingress-nginx annotation:

```yaml
nginx.ingress.kubernetes.io/auth-url: https://zitadel.example.com/oauth/v2/introspect
nginx.ingress.kubernetes.io/auth-response-headers: X-Auth-Request-User, X-Auth-Request-Email
```

Apply the same authenticated Ingress policy to `/v1` when routing it; never
place the optional path on a separate unauthenticated Ingress.

## Blob storage lifecycle

Uploaded cited blobs live under `objects/<owner_hash>/…`; in-flight uploads live
under `pending/<owner_hash>/…`. Proxima runs no in-process pending sweep, so the
bucket MUST carry an S3 lifecycle-expiration rule on the `pending/` prefix
(≥ the configured upload TTL) to reclaim uploads that never complete. Owner
erasure removes the canonical objects as part of compliance erase (see
[13 §External side effects](13-compliance.md#external-side-effects)).

In-process byte consumers use `CitedBlobReadService::collect_verified` with a
required non-zero ceiling. Ordinary callers pass `AuthzContext`; queued
workers redeem `DelegatedPhase` from the runtime's shared
`DelegatedAuthorityService`. The service gates phase/runtime/expiry and
Fact-read before SQL/S3, buffers at most that ceiling, and releases bytes only
after length+BLAKE3+SHA-256 verification. Presigned `read_url` remains the
unverified external-client lane. Owner reconciliation rejects delegated
authority.

## SSE stream revocation

The OIDC path carries no out-of-band revocation signal, so a live SSE stream is
bounded by `min(JWT exp, PROXIMA_STREAM_MAX_LIFETIME)`. For prompt revocation set
a low `PROXIMA_STREAM_MAX_LIFETIME` (e.g. a few minutes); a revoked token cannot
outlive that window.
