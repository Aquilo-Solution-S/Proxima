# 10 - Configuration

Current runtime configuration contract. Build-time registration owns
schemas, relations, prompts, tools, source types, wake filters, and
personality types (see [08](08-core-and-flavors.md)). Runtime config
selects the Postgres connection, the MCP endpoint and its authentication,
deployment-level artefact storage, and an optional host-injected
embedding client for retrieval plus an optional host-injected model-seat
client.

Proxima hosts no model loop. It does not register inference targets,
tier registries, or LLM credentials — external harnesses own model
selection. The only LLM-adjacent runtime knob is the embedding client a
host injects for vector retrieval and an optional model-seat client.

<a id="scope"></a>
## Scope

| Surface | Scope | Current contract |
|---|---|---|
| Postgres connection | binary-wide | `DATABASE_URL` |
| MCP endpoint | binary-wide | bind addr, network exposure, origin allowlist |
| MCP authentication | per request | host `Authenticator`, master token, or insecure single-owner |
| Embedding client | binary-wide | optional `Arc<dyn EmbeddingClient>` injected at boot |
| Anthropic model client | binary-wide | optional `Arc<dyn AnthropicClient>` host-injected; programmatic only |
| Large artefact S3 storage | binary-wide | process env + AWS SDK credential chain |
| EventSource credentials | per source instance | source-owned, not engine-owned |

Not runtime configurable: schema ids, payload types, relation
descriptors, prompts, tool definitions, source types, wake-filter kinds,
and personality type registration.

Wake **config** (a personality's detect rule — trigger schema_id +
authored_by, probability, read-scope, target) is per-personality data,
edited through the core MCP wake-config tools, not an env/boot surface.
See [08](08-core-and-flavors.md) and the protocol surface
[14](14-protocol-surface.md).

<a id="framework-facade-host-app-boot"></a>
## Framework facade (host-app boot)

```rust
Proxima::<App>::app().from_env().authenticator(auth).run().await?;
```

| Env var | Meaning |
|---|---|
| `DATABASE_URL` | Postgres connection for core tables (`proxima_core` schema). |
| `PROXIMA_MCP_BIND` | MCP socket address; enables the listener when set. |
| `PROXIMA_MCP_MASTER_TOKEN` | Master bearer token for MCP when no host authenticator is wired. |
| `PROXIMA_EXPOSE_NETWORK` | Network exposure gate for non-loopback binds. |
| `PROXIMA_ALLOWED_ORIGINS` | Comma-separated MCP origin allowlist. |
| `PROXIMA_ALLOWED_HOSTS` | Comma-separated inbound `Host` allowlist (hostnames or `host:port`, no wildcards) for the DNS-rebinding guard; defaults to the host of `PROXIMA_PUBLIC_URL` + the allowed origins. Loopback always permitted. |
| `PROXIMA_STREAM_MAX_LIFETIME` | Max lifetime (seconds) of a subscribe stream. |
| `PROXIMA_STREAM_EPOCH_INTERVAL` | Stream epoch re-check interval (seconds). |
| `MISTRAL_API_KEY` | Enables `proxima-mcp` embeddings with Mistral. |
| `PROXIMA_EMBED_MODEL` | Optional embedding model for `proxima-mcp`; defaults to `mistral-embed`. |
| `MISTRAL_API_BASE` | Optional Mistral-compatible API base; defaults to `https://api.mistral.ai/v1`. |
| `PROXIMA_TOOL_PROFILE` | `proxima-mcp` deployment tool profile: `full` (default) or `memory`. |
| `PROXIMA_TOOL_ALLOW` | Optional comma-separated canonical scope keys unioned into the resolved profile. |
| `PROXIMA_TOOL_DENY` | Optional comma-separated canonical scope keys subtracted from the resolved profile. |
| `PROXIMA_S3_BUCKET` | Enables cited-blob S3 storage. |
| `PROXIMA_S3_REGION` | S3 region for cited-blob storage. |
| `PROXIMA_S3_ENDPOINT_URL` | Optional S3-compatible endpoint URL. |
| `PROXIMA_S3_FORCE_PATH_STYLE` | S3 path-style addressing flag. |
| `PROXIMA_S3_UPLOAD_TTL_SECONDS` | Presigned upload URL TTL. |
| `PROXIMA_S3_READ_TTL_SECONDS` | Presigned read URL TTL. |

Builder methods override env per field. Defaults, precedence
(`configure < env < explicit`), and the fail-closed network/auth matrix
are specified by `crates/proxima` rustdoc:
[`RuntimeBuilder`](../crates/proxima/src/runtime_config.rs),
[`RuntimeConfig::validate`](../crates/proxima/src/runtime_config.rs),
[`EmbedConfig`](../crates/proxima/src/config.rs), and
[`Proxima<A>`](../crates/proxima/src/runtime.rs).

<a id="mcp-endpoint-and-auth"></a>
## MCP Endpoint and Authentication

The Streamable HTTP MCP listener turns on when `PROXIMA_MCP_BIND` (or
`with_mcp()` / `mcp_bind(..)`) is set. A non-loopback bind requires
`PROXIMA_EXPOSE_NETWORK`. `validate()` fails closed unless one auth mode
is present:

| Mode | How | Identity model |
|---|---|---|
| Host `Authenticator` | `.authenticator(Arc<dyn Authenticator>)` | per-user actor resolved from the bearer (e.g. tenant JWT) over a shared company graph |
| Master token | `--master-token` / `PROXIMA_MCP_MASTER_TOKEN` | single trusted bearer for the configured Owner |
| Insecure single-owner | `.allow_insecure_single_owner()` | dev only; no auth, one Owner |

Origins are gated by `PROXIMA_ALLOWED_ORIGINS`. The inbound `Host`
header is independently gated by rmcp's DNS-rebinding guard: loopback
binds accept loopback hosts only, and a network-exposed bind must
resolve at least one public host (`PROXIMA_ALLOWED_HOSTS`, else the host
of `PROXIMA_PUBLIC_URL` / the allowed origins) or `validate()` fails
closed. Secrets are never streamed to clients.

### Tool Surface Profile

`apps/proxima-mcp` resolves one deployment-wide `ToolScope` at boot:

```text
profile -> + PROXIMA_TOOL_ALLOW -> - PROXIMA_TOOL_DENY
```

Profiles:

| Profile | Scope |
|---|---|
| `full` | Default. No filtering (`ToolScope::All`) when allow/deny are unset; otherwise all registered ids resolved to a palette. |
| `memory` | Curated memory-brain palette: memory authoring/retrieval, citations, graph/schema introspection, non-destructive Fact/citation actions (the destructive `core_fact:tombstone` and retention/cleanup stay host/config-only — excluded), the full goal lifecycle, and code-as-memory repository/chunk/commit reads. |

Allow/deny ids use canonical scope keys: tool ids (`core_search_memories`),
group-action leaf keys (`core_wake:add`, `core_fact:tombstone`), resource
keys (`resource:memory`, `resource:events`), or flavor ids
(`proxima-code_search_chunks`). Unknown profile names fail boot. Unknown
ids in allow/deny log `warn` and do not fail boot.

<a id="embedding-client"></a>
## Embedding Client

Embedding-for-retrieval is host-injected, not configured by Proxima:

```rust
builder.embed_client(client: Arc<dyn EmbeddingClient>)
```

Proxima holds no embedding-model registry and no active-model singleton —
those tables and their config tools were removed. The host wires its own
provider (e.g. via `crates/llm-openai-compat`) and is responsible for
keeping vector dimensions consistent: vector rows are shared
infrastructure, so a binary uses one embedding space and changing it may
require re-embedding. If no client is injected, semantic search modes are
unavailable; lexical paths still work.

`apps/proxima-mcp` injects a Mistral client only when `MISTRAL_API_KEY`
is present:

| Env var | Required | Default | Meaning |
|---|---:|---|---|
| `MISTRAL_API_KEY` | yes (enables embeddings) | - | Bearer token for Mistral embeddings. |
| `PROXIMA_EMBED_MODEL` | no | `mistral-embed` | Model id sent to `/embeddings`. |
| `MISTRAL_API_BASE` | no | `https://api.mistral.ai/v1` | OpenAI-compatible embeddings API base. |

When `MISTRAL_API_KEY` is absent, `proxima-mcp` starts in degraded mode:
no embedding client is installed,
`proxima://graph.embeddings_client_configured` is `false`,
semantic/hybrid search reports the missing capability, and lexical-only
paths remain available. When a client is configured, `proxima-mcp`
drains queued embedding jobs automatically in-process every few seconds;
no external drain cron is required.

Embedding job reconciliation remains a global maintenance command for
backfill / stale re-embedding, not an owner-scoped MCP tool:

```sh
proxima-mcp reconcile-embeddings
```

Deploy command form:

```yaml
command: ["proxima-mcp", "reconcile-embeddings"]
```

`--drain` processes queued jobs inline with the same Mistral client and
therefore requires `MISTRAL_API_KEY`; it is no longer required for
steady-state draining.

`EmbedCaps { dim, matryoshka }` and `LlmCaps { tool_use, json_mode,
long_context, vision }` remain core vocabulary types but are not a
runtime-config surface here.

<a id="large-artefact-s3"></a>
## Large Artefact S3

Large cited-object storage is deployment infrastructure, not per-Owner
configuration. Resolved by `EmbedConfig`/`S3RuntimeConfig` from env:

| Key | Required | Default |
|---|---:|---|
| `PROXIMA_S3_BUCKET` | yes (enables S3) | - |
| `PROXIMA_S3_REGION` | with bucket | - |
| `PROXIMA_S3_ENDPOINT_URL` | no | AWS region endpoint |
| `PROXIMA_S3_FORCE_PATH_STYLE` | no | `false` |
| `PROXIMA_S3_UPLOAD_TTL_SECONDS` | no | `900` |
| `PROXIMA_S3_READ_TTL_SECONDS` | no | `300` |

Credentials use the standard AWS SDK provider chain. Missing S3 config
does not fail boot; cited-blob commands fail typed at call time. Commands
return presigned URLs only, never `bucket` or `object_key`.

<a id="owner-scoping"></a>
## Owner Scoping

Owner access control is per-row on graph data (see
[01 §Owner](01-event-source.md#owner--scoping-primitive)). `Owner =
Principal` (Track B / S0 removed the tenant field from Core). Runtime
config selects the binary's default Owner principal and, under a host
`Authenticator`, resolves a per-request actor from the bearer. There is
no per-Owner inference/credential table — that surface was removed.

<a id="bootstrap"></a>
## Bootstrap

Boot sequence:

1. Build-time registries are linked into the binary.
2. `RuntimeBuilder` resolves config (`configure < env < explicit`) and
   `validate()` fails closed on a missing Owner or an unauthenticated
   exposed MCP bind.
3. Migrations create core tables.
4. The embedding client, if injected, is wired; otherwise semantic search
   is disabled.
5. The MCP listener starts when a bind is configured.

<a id="deployment-shapes"></a>
## Deployment Shapes

| Shape | Config source |
|---|---|
| Embedded host app | host builds `Proxima<App>` programmatically over a local Engine/Postgres; injects its own authenticator + embedding client |
| Headless MCP host | process env (`apps/proxima-mcp`) + master token or host authenticator |
| Hosted deployment | provisioned env/secrets + tenant authenticator |

The same Engine contract applies in every shape: build-time types,
runtime endpoint instances.

<a id="what-this-doc-is-not"></a>
## What This Doc Is Not

Not a protocol spec (see [14](14-protocol-surface.md)). Not a storage
schema source of truth (see migrations and [07](07-storage.md)). Not a
source-instance contract (see [01](01-event-source.md)). Proxima ships no
frontend — any UI is the consumer's, built over the MCP surface.
