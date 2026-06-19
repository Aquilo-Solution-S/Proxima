# 15 — Deployment

Containerized Code-flavor MCP server, authenticated exclusively via
Zitadel bearer JWT, with a single unauthenticated route:
`GET /.well-known/oauth-protected-resource` (RFC 9728). The binary is
`apps/proxima-mcp` built with `--features code`.

## Runtime requirements

Postgres must have the `vector` extension with `hnsw` index type
(pgvector-enabled image). Migrations run automatically on first boot.
Optional dependencies: S3 for cited-blob storage (see [10](10-configuration.md#large-artefact-s3)),
and a Mistral-compatible embedding client; without embeddings the server
operates in degraded lexical-only mode.

## Environment contract

| Var | Required | Example | Purpose |
|---|---|---|---|
| `DATABASE_URL` | yes | `postgres://user:pass@host:5432/db` | Postgres connection string. |
| `--owner-user <UUID>` | yes (CLI flag) | `550e8400-e29b-41d4-a716-446655440000` | Fixed owner user id. |
| `--owner-org <UUID>` | yes (CLI flag) | `550e8400-e29b-41d4-a716-446655440001` | Fixed owner org id. |
| `PROXIMA_MCP_BIND` | yes | `0.0.0.0:8080` | MCP listener address. |
| `PROXIMA_EXPOSE_NETWORK=true` | yes | `true` | Required for non-loopback bind. |
| `PROXIMA_ALLOWED_ORIGINS` | yes | `https://claude.example.com,https://codex.example.com` | Comma-separated origin allowlist; never `*`. |
| `PROXIMA_ALLOWED_HOSTS` | no | `proxima.example.com` | Inbound `Host` allowlist (hostnames or `host:port`, no wildcards) for the DNS-rebinding guard. Defaults to the host of `PROXIMA_PUBLIC_URL` + the allowed origins; loopback always permitted. Set only to override. |
| `PROXIMA_PUBLIC_URL` | yes | `https://proxima.example.com` | Public HTTPS base for OIDC; its host is auto-allowed as an inbound `Host`. |
| `PROXIMA_OIDC_ISSUER` | yes | `https://zitadel.example.com` | Zitadel issuer URL. |
| `PROXIMA_OIDC_AUDIENCE` | yes | `https://proxima.example.com/mcp` | Resource id expected in token `aud`. |
| `PROXIMA_OIDC_JWKS_URI` | no | `https://zitadel.example.com/oauth/v2/keys` | Overrides OIDC discovery. |
| `PROXIMA_OIDC_ALLOWED_SUBJECTS` | no | `user1,user2` | Comma-separated `sub` allowlist. |
| `MISTRAL_API_KEY` | no | `sk-...` | Enables Mistral embeddings. |
| `PROXIMA_EMBED_MODEL` | no | `mistral-embed` | Embedding model id. |
| `MISTRAL_API_BASE` | no | `https://api.mistral.ai/v1` | Mistral-compatible API base. |
| `PROXIMA_S3_BUCKET` | no | `proxima-cited-blobs` | Cited-blob bucket. |
| `PROXIMA_S3_REGION` | no | `us-east-1` | S3 region. |
| `PROXIMA_S3_ENDPOINT_URL` | no | `https://s3.example.com` | S3-compatible endpoint. |

## No standing bypass (master token)

Do NOT set `PROXIMA_MCP_MASTER_TOKEN` in a deployment. With
`PROXIMA_EXPOSE_NETWORK=true` the binary refuses to start if a master token
is set (enforced by `RuntimeConfig::validate()`). The master token is
the loopback-only dev path. Break-glass during a Zitadel outage: spin
a throwaway loopback master-token pod against the same Postgres, never
expose a standing token on `/mcp`.

## Security guarantee

Only `/.well-known/oauth-protected-resource` is anonymous. All `/mcp`
endpoints require an `aud`-bound Zitadel JWT, validated in-process.
401 responses carry `WWW-Authenticate: Bearer resource_metadata="…"`
(RFC 9728). Defense in depth: the same JWT MUST be validated at the
cluster edge (see [§Edge defense-in-depth](#edge-defense-in-depth)).

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
  proxima-mcp --owner-user 550e8400-e29b-41d4-a716-446655440000 --owner-org 550e8400-e29b-41d4-a716-446655440001
```

## Zitadel setup

- Create a Zitadel project.
- Create an API/resource whose identifier equals `PROXIMA_OIDC_AUDIENCE`.
- Register an MCP client app (auth-code + PKCE) with redirect URIs for
  Claude Code / Codex / Cursor.
- The client must request the resource as audience.
- Optionally enable Dynamic Client Registration (RFC 7591).

## Edge defense-in-depth (guidance)

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
