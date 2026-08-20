# 10 - Configuration

Current runtime configuration contract. Build-time registration owns
schemas, prompts, tools, source types, and wake trigger vocabulary
(see [08](08-core-and-flavors.md)). Runtime config
selects the Postgres connection, the MCP endpoint and its authentication,
deployment-level artefact storage, and an optional host-injected
embedding client for retrieval.

For a consolidated human inventory of environment variables, see
[reference/env-vars.md](reference/env-vars.md). Source code and deployment
manifests remain authoritative.

Proxima hosts no model loop. It does not register inference targets,
tier registries, or LLM credentials — external harnesses own model
selection. The only LLM-adjacent runtime knob is the embedding client a
host injects for vector retrieval.

<a id="scope"></a>
## Scope

| Surface | Scope | Current contract |
|---|---|---|
| Postgres connection | binary-wide | `DATABASE_URL` |
| MCP endpoint | binary-wide | bind addr, network exposure, origin + Host allowlists |
| REST surface | binary-wide | `rest` cargo feature + `PROXIMA_REST_ENABLED`; same listener, same layers |
| MCP authentication | per request | host `Authenticator` only; owner roles resolved server-side |
| Embedding client | binary-wide | optional `Arc<dyn EmbeddingClient>` injected at boot |
| Large artefact S3 storage | binary-wide | process env + AWS SDK credential chain |
| source credentials | per source instance | source-owned, not engine-owned |

Not runtime configurable: schema ids, payload types, edge kinds, prompts,
tool definitions, source types, wake trigger kinds, and agent type
registration.

Wake config is per-Goal data written through GoalWrite and stored as
`wake_config`, referenced by `Goal.wake_id`; it is not an env/boot surface or
graph node. See [08](08-core-and-flavors.md), [06](06-goals-and-self.md), and
the protocol surface [14](14-protocol-surface.md).

<a id="framework-facade-host-app-boot"></a>
## Framework facade (host-app boot)

```rust
Proxima::<App>::app()
    .from_env()
    .authenticator(auth)
    .tool_scope(ToolScope::All) // or ToolScope::Palette([...]) for a restricted keep-set
    .run()
    .await?;
```

| Env var | Meaning |
|---|---|
| `DATABASE_URL` | Postgres connection for core tables (`proxima_core` schema). |
| `PROXIMA_MCP_BIND` | MCP socket address; enables the listener when set. |
| `PROXIMA_EXPOSE_NETWORK` | Network exposure gate for non-loopback binds. |
| `PROXIMA_ALLOWED_ORIGINS` | Comma-separated browser-origin allowlist for listener-wide CORS. |
| `PROXIMA_ALLOWED_HOSTS` | Comma-separated inbound `Host` allowlist (hostnames or `host:port`, no wildcards) for the listener-wide DNS-rebinding guard; defaults to the host of `PROXIMA_PUBLIC_URL` + the allowed origins. Loopback always permitted. |
| `PROXIMA_STREAM_MAX_LIFETIME` | Max lifetime (seconds) of an authenticated MCP (Streamable HTTP) response stream before re-validation. |
| `PROXIMA_STREAM_EPOCH_INTERVAL` | Auth-epoch re-check interval (seconds) for an open MCP response stream. |
| `PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS` | Complete-request timeout for OIDC discovery and JWKS HTTP requests. Default `10`; range `1..=300` seconds. Covers connection establishment and response-body reads. |
| `PROXIMA_EMBED_BASE_URL` | OpenAI-compatible `/embeddings` base URL. Required with `PROXIMA_EMBED_MODEL` to enable embeddings. |
| `PROXIMA_EMBED_API_KEY` | Optional bearer for a hosted embedding endpoint. |
| `PROXIMA_EMBED_MODEL` | Embedding model id. Required with `PROXIMA_EMBED_BASE_URL` to enable embeddings. |
| `PROXIMA_EMBED_MATRYOSHKA` | Send a `dimensions` request parameter for nested-prefix models. Default `false`. |
| `PROXIMA_EMBED_MAX_INPUT_CHARS` | Longest input, in characters, the client will send. Unset (default) sends every input and lets the provider judge it. Set this when the provider does not reject over-long input cleanly — see below. Minimum 4095. |
| `PROXIMA_REST_ENABLED` | Serve the `/v1` REST rendering of the tool manifest beside `/mcp` (see [17](17-rest-surface.md)). Default `false`; requires the `rest` cargo feature at build time. |
| `PROXIMA_TOOL_PROFILE` | `proxima-mcp` deployment tool profile: `memory` (default, fail-closed) or `full` (opt-in). |
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
are specified by `crates/proxima` rustdoc and source:
[`RuntimeBuilder`](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs),
[`RuntimeConfig::validate`](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime_config.rs),
[`EmbedConfig`](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/config.rs), and
[`Proxima<A>`](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/src/runtime.rs).

<a id="mcp-endpoint-and-auth"></a>
## MCP Endpoint and Authentication

The Streamable HTTP MCP listener turns on when `PROXIMA_MCP_BIND` (or
`with_mcp()` / `mcp_bind(..)`) is set. A non-loopback bind requires
`PROXIMA_EXPOSE_NETWORK`. Serving requires a host authenticator:

| Mode | How | Identity model |
|---|---|---|
| Host `Authenticator` | `.authenticator(Arc<dyn Authenticator>)` | bearer resolves to an `AuthzContext` carrying current, server-resolved `OwnerRoles` |

The shipped OIDC resolver bounds each discovery and JWKS request, including
connect and body read, to `PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS` (`10` by default;
`1..=300`). Zero, malformed, and out-of-range values fail boot.

`OwnerAccessPort` is not a second runtime input. The shipped
`OidcAuthenticator` and each `OidcBinding` receive the port at construction
and use it inside `authenticate`; a custom authenticator owns the equivalent
server-side role resolution.

`PROXIMA_ALLOWED_ORIGINS` is the listener-wide browser CORS policy. It covers
`/mcp`, `/v1`, mounted flavor routes, OAuth metadata, and fallback responses.
An allowed preflight returns `204` before bearer auth; the subsequent actual
request still needs its normal bearer. Missing `Origin` preserves native-client
auth semantics. Origins are echoed exactly; wildcard origins and cookie
credentials are never enabled. A repeated/malformed
`Access-Control-Request-Method` or malformed
`Access-Control-Request-Headers` list returns `400`; repeated valid header-list
fields are all honored.

One non-empty `HostAllowlist` gates the same complete listener before CORS and
auth, and is passed to rmcp's inner `/mcp` DNS-rebinding guard unchanged.
Loopback is always present; a network-exposed bind must resolve at least one
public host (`PROXIMA_ALLOWED_HOSTS`, else the host of `PROXIMA_PUBLIC_URL` /
the allowed origins) or `validate()` fails closed. Secrets are never streamed
to clients.

### REST Surface

`/v1` is the same tool manifest rendered as REST
([17](17-rest-surface.md)). Two gates, both required, and they are
different kinds of decision: the `rest` cargo feature compiles the
module (a build decision), `PROXIMA_REST_ENABLED=true` serves it (a
deployment one). Default off at both.

It mounts on the MCP listener inside the listener-wide body/Host/CORS layers
and the same protected auth layer, so it inherits Host validation, browser
preflight handling, bearer validation, owner resolution and stream
revalidation unchanged, and grants no authority MCP does not already grant.
Setting `PROXIMA_REST_ENABLED` in a binary built without the feature logs a
warning at boot and serves nothing.

### Tool Surface Profile

`apps/proxima-mcp` resolves one deployment-wide `ToolScope` at boot:

```text
profile -> + PROXIMA_TOOL_ALLOW -> - PROXIMA_TOOL_DENY
```

Profiles:

| Profile | Scope |
|---|---|
| `full` | Opt-in. No filtering (`ToolScope::All`) when allow/deny are unset; otherwise all registered ids resolved to a palette. Includes controller/destructive tools such as `core_membership` and `core_transfer`. |
| `memory` | **Default.** Curated memory-brain palette: memory authoring/retrieval, citations, graph/schema introspection, citation-only Fact actions, the full goal lifecycle, the cited-blob upload lane, and code-as-memory repository/chunk/commit reads. The upload lane activates with `PROXIMA_S3_BUCKET` (without it, `core_upload` fails typed at call time) and its actions are individually deniable as `core_upload:prepare`, `core_upload:complete`, `core_upload:abort`, `core_upload:read_url`. Excludes `core_membership`, `core_transfer`, and compliance erase. |

Allow/deny ids use canonical scope keys: flat tool ids (`core_search_memories`),
dispatcher action leaf keys (`core_goal:set`, `core_fact:citation_of_fact`),
resource keys (`resource:memory`, `resource:change-events`), or flavor ids
(`proxima-code_search_chunks`). Unknown profile names and unknown allow/deny
ids fail boot.

Production hosts that expose `full` but do not intend roster or owner-transfer
operations should deny both controller surfaces:

```text
PROXIMA_TOOL_DENY=core_membership:add_member,core_membership:remove_member,core_membership:list_members,core_transfer:transfer_to_owner
```

<a id="embedding-client"></a>
## Embedding Client

The embedding client and model are host-injected:

```rust
let policy = EmbeddingRuntimePolicy::new(
    Duration::from_secs(120), // enforced request timeout
    32,                       // provider batch width
    Duration::from_secs(5),   // idle worker interval
    Duration::from_secs(900), // stale-claim timeout
)?;

builder
    .embed_client(client)
    .embedding_runtime_policy(policy)
```

Proxima holds no embedding-model registry and no active-model singleton —
those tables and their config tools were removed. The host wires its own
provider (e.g. via `crates/llm-openai-compat`) and is responsible for
keeping vector dimensions consistent: vector rows are shared
infrastructure, so a binary uses one embedding space and changing it may
require re-embedding. Re-embedding appends a new version and advances
`embedding_heads`; prior vector rows are not updated. If no client is
injected, semantic search modes are unavailable; lexical paths still work.

`apps/proxima-mcp` talks to any OpenAI-compatible `/embeddings` endpoint.
Base URL and model are explicit; a locally-hosted endpoint (Ollama,
llama.cpp, LM Studio, vLLM) needs no credential:

| Env var | Required | Default | Meaning |
|---|---:|---|---|
| `PROXIMA_EMBED_BASE_URL` | when embeddings enabled | - | OpenAI-compatible embeddings API base. Plaintext `http://` is accepted for loopback only. |
| `PROXIMA_EMBED_MODEL` | when embeddings enabled | - | Model id sent to `/embeddings`. |
| `PROXIMA_EMBED_API_KEY` | no | - | Bearer for a hosted endpoint. Omit for a local one. |
| `PROXIMA_EMBED_MATRYOSHKA` | no | `false` | Send a `dimensions` parameter so a nested-prefix model returns 1024 rather than its native width. |
| `PROXIMA_EMBED_MAX_INPUT_CHARS` | no | - | Longest input, in characters, that will be sent. Unset ⇒ no client-side bound. Minimum `4095`. |
| `PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS` | no | `120` | Complete provider-request timeout. Range `1..=3600`. Core bounds every installed-client future; the shipped adapter additionally applies it to connect, send, and response read. |
| `PROXIMA_EMBED_BATCH_SIZE` | no | `32` | Texts per provider call. Range `1..=1024`. Custom clients remain usable because batching is host policy, not a core provider constant. |
| `PROXIMA_EMBED_WORKER_INTERVAL_SECONDS` | no | `5` | Idle in-process worker poll interval. Range `1..=3600`. |
| `PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS` | no | `900` | Processing claim is reclaimable after this crash window. Range `1..=86400`; must be strictly greater than request timeout. |

Zero, malformed, out-of-range, and unsafe combinations fail boot. Programmatic
durations must also be integral seconds. The stale
window must cover the longest honest drain interval between successful claim
renewals. The drainer renews every live batch claim on a separate heartbeat every
third of this window, including during poison isolation and chunk rescue;
claim-token fencing still rejects any old worker write after a real reclaim.
Both the in-process worker and `maintain-embeddings --drain` claim at most one
configured provider batch and use the same batch rejection/isolation path.

<a id="bounding-embedding-input"></a>
### Bounding embedding input

`PROXIMA_EMBED_MAX_INPUT_CHARS` refuses an over-long input **before a
request is made**, rather than letting the provider judge it.

Leave it unset against a provider that rejects over-long input cleanly —
that is the normal case, and the rejection is what triggers the chunked
rescue below. Set it when the provider does *not*. A local Ollama sizes a
model runner's context when the runner loads; an input past that limit
kills the runner rather than being refused, and the death arrives as a
transport error, which is indistinguishable at the response from a runner
that was already down. Retried unchanged, one such input can take the
embedder down repeatedly and fail every *unrelated* embed while it is
down. A limit should never be discovered by killing a process.

Refused input is not lost. The refusal is permanent rather than transient,
which is what routes it into the bisecting rescue
(`proxima_core::llm::embed_in_chunks`): the text is halved until the pieces
fit and stored as one chunked embedding, with no request leaving the
process until a piece is inside the bound.

That coupling sets the **minimum of 4095 characters**
(`proxima_core::llm::MIN_EMBED_INPUT_CAP_CHARS`) — the largest piece the
split can emit. A lower cap would refuse pieces the split cannot make any
smaller, turning a rescuable input into a permanently un-embedded one, so
a cap below the floor fails at boot rather than at some later drain.

Characters, not tokens: no tokenizer is provider-independent. Pick the
value from your model's context window with room to spare — for a runner
loaded at 16k tokens, `16384` characters is comfortably conservative.

The model must return **1024-dimensional** vectors — the width of the
`vector(1024)` column that is the substrate's single embedding space.
`qwen3-embedding:0.6b` and `mxbai-embed-large` are 1024 natively. A wider
Matryoshka model needs
`PROXIMA_EMBED_MATRYOSHKA=true`; a model that is natively narrower cannot
be used without re-embedding into a different space.

Fully local example:

```sh
export PROXIMA_EMBED_BASE_URL=http://127.0.0.1:11434/v1
export PROXIMA_EMBED_MODEL=qwen3-embedding:0.6b
```

When no `PROXIMA_EMBED_*` setting is set, `proxima-mcp` starts in degraded
mode: no embedding client is installed,
`proxima://graph.embeddings_client_configured` is `false`,
semantic/hybrid search reports the missing capability, and lexical-only
paths remain available. A partial block fails at boot: base URL and model
must be set together. When a client is configured, `proxima-mcp`
drains queued embedding jobs automatically in-process every few seconds;
no external drain cron is required. At startup with a client configured,
the worker also runs one `missing-only` reconcile pass before its first
drain, so memories written during a degraded window (or left in the
`failed` retry dead-end) heal on the next restart without an operator
command.

Recurring embedding maintenance stays outside the process — the substrate
spawns no scheduler beyond the drain worker. One idempotent command runs
a full self-healing pass (orphan-row sweep, reconcile enqueue for
missing-head backfill / stale re-embedding, optional inline drain, health
report with job backlog, orphan counts, and the ANN recall canary):

```sh
proxima-mcp maintain-embeddings
```

Cron/deploy command form:

```yaml
command: ["proxima-mcp", "maintain-embeddings"]
```

### Blob store reconcile

A second, unrelated pass answers a different question: does the object store
still hold what the database says it holds?

```sh
proxima-mcp maintain-blobs
```

Read-only — it deletes nothing and repairs nothing. It reports three numbers
that are three different problems:

| Number | Meaning | Severity |
|---|---|---|
| `missing` | an artefact the corpus claims to hold whose object is absent | **a citation that cannot be resolved**; alert on this |
| `orphans` | objects no row claims | cost and retention only |
| `foreign` | rows naming another bucket, or a key outside `objects/` | a hand-written or forged locator |

The command boots the normal headless Proxima composition and passes its
runtime-issued `SystemAuthority` to the global reconcile verb. The store is
bound to that boot identity before it is published; a witness issued by a
different `Engine` is rejected before database or S3 access through the
runtime-provided handles. Linked Rust runs inside the trusted host process;
this API boundary is not a sandbox for code given raw backend credentials. An
owner-facing tool instead resolves `CitedBlobOwnerReconcileService`; that
lane is Fact-read-authorized, restricted to one Owner's rows and object
prefix, and omits every raw storage locator from its result.

`missing` cannot be repaired by re-ingesting: the upload lane skips artefacts
the corpus already claims to hold, so only a bucket version, a backup, or a
direct re-upload restores the bytes. This is why bucket versioning is listed
as a deployment requirement in [operate](how-to/operate.md).

Takes no advisory lock, unlike `maintain-embeddings`: the pass only reads, so
concurrent runs are redundant rather than unsafe, and a crashed holder must
not be able to block a health check. Exits `0` even when artefacts are
missing — a non-zero exit would mean the pass failed, and it did not; it
succeeded and the news is bad.

Passes are serialized by a Postgres advisory lock: an invocation that
finds the lock held prints a skip notice and exits `0`, so overlapping
cron fires are harmless by construction. `--drain` processes queued jobs
inline with the configured embedding client and therefore requires the same
`PROXIMA_EMBED_BASE_URL` + `PROXIMA_EMBED_MODEL` block; the API key remains
optional.

Retention maintenance follows the same doctrine — one idempotent,
cron-safe command, serialized by its own advisory lock, with no
in-process scheduler:

```sh
proxima-mcp maintain-retention --enforce-fact-retention \
    --prune-change-events-older-than 90d
```

`--enforce-fact-retention` forgets (cools) Facts older than their owner's
configured retention window, leaving a cold stub rather than a tombstone flag
(owners without a window are untouched; MCP-call audit Facts are never aged
out). A live (non-dry-run) action requires the same `PROXIMA_S3_*` block as
the serving host; it fails closed rather than writing cold objects to a
process-local store.
`--prune-change-events-older-than <DURATION>` deletes `announce`
rows older than the horizon (`3600s`, `45m`, `36h`, `90d`, `2w`). At
least one action flag is required and there is deliberately no default
horizon — destruction is always an explicit operator choice. Owners
under an active legal/security hold are skipped and reported.
`--dry-run` prints per-owner would-be counts without changing anything.
See [13 §Retention
enforcement](13-compliance.md#retention-enforcement--maintain-retention-pass)
for the compliance contract, including the forward-poller cursor-gap
caveat when choosing a prune horizon.

<a id="capability-vocabulary"></a>
## Capability vocabulary

`EmbedCaps { dim, matryoshka, max_input_chars }` is the live embedding
capability type: boot checks `dim` against the vector column;
`matryoshka` / `max_input_chars` are host-injected client flags (see
[Embedding Client](#embedding-client)).

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
| `PROXIMA_S3_MAX_BLOB_BYTES` | no | `104857600` |

Credentials use the standard AWS SDK provider chain. Missing S3 config
does not fail boot; cited-blob commands fail typed at call time. Commands
return presigned URLs only, never `bucket` or `object_key`.

In-process consumers resolve `CitedBlobReadService` from `FlavorServices`.
`collect_verified` requires a non-zero per-call byte ceiling in addition to
the upload cap, authorizes Fact-read before locator/Postgres/S3 access, and
returns no bytes until length, BLAKE3, and SHA-256 match stored metadata. Its
DTO carries id, hashes, byte length, MIME, filename, and bytes; no storage
locator.

<a id="owner-scoping"></a>
## Owner Scoping

Owner access control is per-row on graph data (see
[01 §Owner](01-event-source.md#owner--scoping-primitive)). `OwnerRef` is
the row-scoping handle. MCP serving does not configure a default owner:
the bearer resolves to `OwnerRoles`, the client selects an authorized
owner during `initialize` (`X-Proxima-Owner` for HTTP), and the server
binds it to `Mcp-Session-Id`. Embedded hosts may still configure a boot
owner for host-owned direct calls. There is no per-Owner
inference/credential table.

<a id="bootstrap"></a>
## Bootstrap

Boot sequence:

1. Build-time registries are linked into the binary.
2. `RuntimeBuilder` resolves config (`configure < env < explicit`) and
   `validate()` fails closed on MCP serving without host auth.
3. Migrations create core tables.
4. The embedding client, if injected, is wired; otherwise semantic search
   is disabled.
5. The MCP listener starts when a bind is configured.

<a id="deployment-shapes"></a>
## Deployment Shapes

| Shape | Config source |
|---|---|
| Embedded host app | host builds `Proxima<App>` programmatically over a local Engine/Postgres; injects its own authenticator + embedding client |
| Headless MCP host | process env (`apps/proxima-mcp`) + host authenticator; shipped OIDC constructs it with `OwnerAccessPort` |
| Hosted deployment | provisioned env/secrets + tenant authenticator |

The same Engine contract applies in every shape: build-time types,
runtime endpoint instances.

<a id="what-this-doc-is-not"></a>
## What This Doc Is Not

Not a protocol spec (see [14](14-protocol-surface.md)). Not a storage
schema source of truth (see migrations and [07](07-storage.md)). Not a
source-instance contract (see [01](01-event-source.md)). Proxima ships no
frontend — any UI is the consumer's, built over the MCP surface.
