# 15 — Deployment

> **Status:** current + deferred sections. Deferred rows are design intent, not implementation claims.

Containerized Code-flavor MCP server, authenticated exclusively via
Zitadel bearer JWT, with a single unauthenticated route:
`GET /.well-known/oauth-protected-resource` (RFC 9728). The binary is
`apps/proxima-mcp` built with `--features code`.

## Runtime requirements

Postgres must have the `vector` extension with `hnsw` index type
(pgvector-enabled image). Migrations run automatically on first boot.
Optional dependencies: S3 for cited-blob storage (see [10](10-configuration.md#large-artefact-s3)),
and a Mistral-compatible embedding client; when configured, the server
drains embeddings in-process automatically. Without embeddings the server
operates in degraded lexical-only mode.

## Connecting to encrypted PostgreSQL

Verified TLS:

```sh
DATABASE_URL=postgres://USER:PASS@HOST:5432/DB?sslmode=verify-full&sslrootcert=/path/ca.pem
```

`sslmode=require` encrypts transport but does not verify hostname. At-rest
TDE / volume encryption is transparent to Proxima. Do NOT use pgcrypto column
encryption for searched columns: `embeddings.vec`, `memories.text`,
`goals.text`, `tags`. Run migrations with a DDL-capable role; run the app with
a narrower DML role.

## Environment contract

| Var | Required | Example | Purpose |
|---|---|---|---|
| `DATABASE_URL` | yes | `postgres://user:pass@host:5432/db` | Postgres connection string. |
| `PROXIMA_MCP_BIND` | yes | `0.0.0.0:8080` | MCP listener address. |
| `PROXIMA_EXPOSE_NETWORK=true` | yes | `true` | Required for non-loopback bind. |
| `PROXIMA_ALLOWED_ORIGINS` | yes | `https://claude.example.com,https://codex.example.com` | Comma-separated origin allowlist; never `*`. |
| `PROXIMA_ALLOWED_HOSTS` | no | `proxima.example.com` | Inbound `Host` allowlist (hostnames or `host:port`, no wildcards) for the DNS-rebinding guard. Defaults to the host of `PROXIMA_PUBLIC_URL` + the allowed origins; loopback always permitted. Set only to override. |
| `PROXIMA_PUBLIC_URL` | yes | `https://proxima.example.com` | Public HTTPS base for OIDC; its host is auto-allowed as an inbound `Host`. |
| `PROXIMA_OIDC_ISSUER` | yes | `https://zitadel.example.com` | Zitadel issuer URL. |
| `PROXIMA_OIDC_AUDIENCE` | yes | `https://proxima.example.com/mcp` | Resource id expected in token `aud`. |
| `PROXIMA_OIDC_JWKS_URI` | no | `https://zitadel.example.com/oauth/v2/keys` | Overrides OIDC discovery. |
| `PROXIMA_OIDC_SUBJECT_MAP_JSON` | yes* | `[{"iss":"https://zitadel.example.com","sub":"...","user_id":"550e8400-e29b-41d4-a716-446655440000"}]` | Issuer-aware `(iss, sub) -> user_id` identity map. Required whenever `PROXIMA_OIDC_ISSUER` is set, unless `PROXIMA_OIDC_SUBJECT_MAP` is given instead (the two are mutually exclusive). |
| `PROXIMA_OIDC_SUBJECT_MAP` | yes* | `sub-1:550e8400-e29b-41d4-a716-446655440000` | Legacy single-issuer shorthand `sub:<uuid>,sub2:<uuid2>`; every entry binds to `PROXIMA_OIDC_ISSUER`. Valid only because exactly one issuer is ever accepted here. |
| `PROXIMA_OIDC_ALLOWED_SUBJECTS` | no | `user1,user2` | Comma-separated `sub` allowlist layered on top of the subject map above; never an identity source by itself. |
| `PROXIMA_TOOL_PROFILE` | no | `memory` | Tool profile: `full` default, or curated `memory`. |
| `PROXIMA_TOOL_ALLOW` | no | `core_goal:set` | Comma-separated canonical scope keys added after profile resolution. |
| `PROXIMA_TOOL_DENY` | no | `core_goal:decompose` | Comma-separated canonical scope keys removed after allow. Compliance erase is not exposed as an MCP action. |
| `MISTRAL_API_KEY` | no | `sk-...` | Enables Mistral embeddings. |
| `PROXIMA_EMBED_MODEL` | no | `mistral-embed` | Embedding model id. |
| `MISTRAL_API_BASE` | no | `https://api.mistral.ai/v1` | Mistral-compatible API base. |
| `PROXIMA_S3_BUCKET` | no | `proxima-cited-blobs` | Cited-blob bucket. |
| `PROXIMA_S3_REGION` | no | `us-east-1` | S3 region. |
| `PROXIMA_S3_ENDPOINT_URL` | no | `https://s3.example.com` | S3-compatible endpoint. |

## No standing bypass

MCP serving has no Proxima-local static bearer fallback. Configure a host
`Authenticator` and `OwnerAccessPort`; stale local-token bearer prefixes fail
closed and are not forwarded to host auth. Break-glass during a Zitadel
outage belongs in the external identity layer as a short-lived, audited host
credential, never as a standing token on `/mcp`.

## Security guarantee

Only `/.well-known/oauth-protected-resource` is anonymous. All `/mcp`
endpoints require an `aud`-bound Zitadel JWT, validated in-process.
401 responses carry `WWW-Authenticate: Bearer resource_metadata="…"`
(RFC 9728). Defense in depth: the same JWT MUST be validated at the
cluster edge (see [§Edge defense-in-depth](#edge-defense-in-depth)).

> **Host-resolved OIDC identity, multi-owner session scope.** The token's
> `(iss, sub)` resolves through `PROXIMA_OIDC_SUBJECT_MAP_JSON` or
> `PROXIMA_OIDC_SUBJECT_MAP` to a Proxima `UserId`; `PgOwnerAccessResolver`
> reads current group memberships into `OwnerRoles`. The client selects one
> authorized owner during MCP `initialize` using `X-Proxima-Owner`:
> `personal:<uuid>`, `group:<uuid>`, or `world`. The server binds that owner
> to the returned `Mcp-Session-Id`. Every later request revalidates the bearer
> and narrows the freshly resolved roles to the bound owner; membership
> removal denies the next request, including an already-bound session. An
> absent subject-map entry, invalid owner key, or non-member owner selection
> fails closed.
>
> Tool advertisement is `frozen registry ∩ deployment ToolScope ∩ bound-owner
> role`: read-only tools require read access; write/unknown tools require
> Fact write access. Compliance erase stays Host API/admin-only.

The inbound `Host` header is gated by rmcp's DNS-rebinding guard
*before* auth runs: only loopback plus the resolved public host(s) are
accepted; any other `Host` is rejected with 403. The public host is
taken from `PROXIMA_ALLOWED_HOSTS` (else the host of `PROXIMA_PUBLIC_URL`
and the allowed origins), so the gateway forwards the real `Host`
unchanged — no `Host`-rewrite-to-localhost workaround is needed.

## Build & run

```sh
# The Dockerfile already builds with `--features code` and SQLX_OFFLINE=true.
docker build -t proxima-mcp .

docker run -p 8080:8080 \
  -e DATABASE_URL=postgres://... \
  -e PROXIMA_MCP_BIND=0.0.0.0:8080 \
  -e PROXIMA_EXPOSE_NETWORK=true \
  -e PROXIMA_ALLOWED_ORIGINS=https://claude.example.com \
  -e PROXIMA_PUBLIC_URL=https://proxima.example.com \
  -e PROXIMA_OIDC_ISSUER=https://zitadel.example.com \
  -e PROXIMA_OIDC_AUDIENCE=https://proxima.example.com/mcp \
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
  -e PROXIMA_OIDC_AUDIENCE=https://proxima.example.com/mcp \
  -e PROXIMA_OIDC_SUBJECT_MAP=zitadel-subject-id:550e8400-e29b-41d4-a716-446655440000 \
  -e PROXIMA_TOOL_PROFILE=memory \
  proxima-mcp
```

`PROXIMA_TOOL_PROFILE=memory` shrinks the advertised MCP surface for
better LLM tool selection and lower operational blast radius. It is not a
security boundary: every tool call remains gated by per-actor authz and
role checks.

MCP clients send `X-Proxima-Owner` on `initialize`; the bound owner is
server-side session state, not a per-call tool argument.

In multi-space hosts, call `core_memory_spaces` before durable memory writes. Use a returned `space` key in `core_remember`, `core_record_utterance`, `core_search_memories`, `core_derive`, and `core_link`; hydrate a memory through `proxima://memory/{id}`. Omitted `space` preserves the current bound owner.

## Zitadel setup

- Create a Zitadel project.
- Create an API/resource whose identifier equals `PROXIMA_OIDC_AUDIENCE`.
- Register an MCP client app (auth-code + PKCE) with redirect URIs for
  Claude Code / Codex / Cursor.
- The client must request the resource as audience.
- Optionally enable Dynamic Client Registration (RFC 7591).

## Edge defense-in-depth

Ingress MUST:
- Terminate TLS.
- Pass `/.well-known/oauth-protected-resource` through unauthenticated.
- Statelessly validate the Zitadel JWT (NOT a session login-proxy).
- Forward the `Authorization` header.
- Restrict pod ingress to the gateway with a `NetworkPolicy`.

Envoy `jwt_authn` filter snippet:
```yaml
http_filters:
  - name: envoy.filters.http.jwt_authn
    typed_config:
      "@type": type.googleapis.com/envoy.extensions.filters.http.jwt_authn.v3.JwtAuthentication
      providers:
        zitadel:
          issuer: https://zitadel.example.com
          audiences: ["https://proxima.example.com/mcp"]
          jwks_uri: https://zitadel.example.com/oauth/v2/keys
      rules:
        - match:
            prefix: /mcp
          requires:
            providers: [zitadel]
```

Ingress-nginx annotation:
```yaml
nginx.ingress.kubernetes.io/auth-url: https://zitadel.example.com/oauth/v2/introspect
nginx.ingress.kubernetes.io/auth-response-headers: X-Auth-Request-User, X-Auth-Request-Email
```
