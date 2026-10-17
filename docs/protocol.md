# Protocol
> Status: current.

The contract is a registered action or resource. MCP and REST project the same
build-time manifest, authorization, typed inputs, and engine operations.

| Discover | Authority |
|---|---|
| Tools and per-action input schemas | MCP `tools/list`, `proxima://tools` |
| Resources and parameterized reads | MCP `resources/list`, `resources/templates/list` |
| Payload schemas | Live schema resources listed by that server |
| HTTP operations and schemas | `/v1/openapi.json` on a REST-enabled host |
| Agent usage instructions | MCP `proxima://how-to` |

Catalogs are caller-scoped. Discover them on the running deployment; linked
flavors, deployment profiles, and effective tool scope determine the result.
Types, defaults, and limits belong to rustdoc and the generated schemas.

## Owner scope

The host authenticates the bearer and resolves a user through `OwnerAccessPort`
into `OwnerRoles`. `X-Proxima-Owner` selects one raw owner UUID; it grants no
authority. Reads use the resolved readable owner set. A write selects one owner
and checks its required role at the command boundary. Kind restrictions remain
part of the effective scope.

MCP sessions bind the selected owner at initialization. Subsequent requests
recheck access. Server-issued memory-space keys are selectors resolved through
the same context. Missing and unreadable entity references have the same public
result. A readable source keeps its pins even when their target is redacted.

Session retention is scoped to the authenticated principal, independently of
the selected owner. Opening more sessions can evict only that principal's own
oldest binding; a full global binding store refuses a new principal instead
of evicting another principal's sessions.

A configured forwarder carries its selected owner in the verified token's
`owner` UUID claim and writer claims `by` (principal UUID) and `via`
(session UUID or explicit `null`). Missing or invalid claims refuse
authentication. The
owner header may agree or be absent; it cannot replace the claim. Every
session request must also agree with the current token's claim. The policy
role reaches that one registered personal or group owner, while the
authenticated subject and session principal remain the forwarder. Ordinary
subjects gain no selection or authority from this claim.

Enforcers: [`AuthzContext`](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/core/src/authz.rs),
the engine command ports, and PostgreSQL owner RLS. See [concepts](concepts.md)
and [host integration](host-integration.md).

## Reads and consistency

Reads return committed owner-visible rows. A `t` identifies one version;
`handle` identifies its series. Head queries and exact-version reads are
different operations. A Goal handle read returns its current head; explicit
version selectors can retrieve historical rows.

Pagination uses an opaque cursor bound to the read's filters. Memory recency
uses `(created_at, t)`; Goal pages use `(handle, t)`, so changing an Active Goal
to Paused does not move its series across the page boundary. A page is not a
long-lived database snapshot: concurrent writes can change later results.

Goal state selection is a union. Fact reads can select references to any
readable version of specified Goal series, intersected with other filters.
Unreadable and absent series both match nothing. Pin traversal follows stored
declarations; similarity influences retrieval scores only.

The publication outbox is the host change channel. It captures listenable
Facts in their write transaction and delivers them at least once to host
intakes. Delivery is separate from the client response; consumers deduplicate
by event identity. Host routing and execution stay in the host. See
[host integration](host-integration.md#fact-publication-outbox).

## Search across stores

An other-store search carries the caller's filters and requested mode through
`RemoteSearch`. Each store resolves its own embedding availability and returns
`RemoteSearchResult`, including whether it fell back to lexical even when it
found no hits. The merged answer reports a fallback if any answer did.

A store that cannot honor the request returns `UnsupportedSearch`; it is
reported as unanswered, without a retry that drops filters. Recency orders the
combined candidates newest first before the result limit. Other stores answer
on the first page; the local page retains its own cursor.

When other stores are wired but their list does not answer (it failed or timed
out), a search that names no store returns this store's hits with
`inventory_unanswered: true` and `unanswered: []`: other stores' hits are
missing, so retry for a complete page. A search that names another store fails
retryable with `store_unavailable`; one that names only this store runs as
asked.

## Writes

Typed payload registration fixes each schema's kind, references, validation,
sidecars, and listen declaration. A Fact receipt proves admission, not external
truth. Source ingestion deduplicates using its owner, source, and ingestion
key; keyless admissions mint a new version. Idempotent command retries must
reuse both key and body; a changed body conflicts.

An episode or explicit `UnitOfWork` commits its declared operations atomically.
Authoring derived content carries declared-input provenance. An interpretation
Perspective declares references without claiming a derivation. No operation
writes an independent edge.

Goal transitions append a later version. Pause's `pending: keep|discard` is an
instruction for the host's queue. The host decides how to execute that
instruction; Proxima persists intent and state. Hard deletion uses the explicit
host or flavor-scoped inverse paths in [data and erasure](data-and-erasure.md).

Caller scope names exact command/action and resource keys. A deployment's
served surface always includes declared reads and core resources; that does
not add keys to the caller. Resource templates use their template key rather
than a concrete URI, and the guide requires `resource:how-to`. MCP and REST
listings and invocation apply the same combined policy and owner-role checks.

## REST projection

Both the host's `rest` Cargo feature and its runtime REST switch must be on.
The shared authentication layer protects MCP and `/v1`. The stock Docker build
serves MCP; build a REST-capable host explicitly when needed.

| Route shape | Behavior |
|---|---|
| `GET /v1/tools` | Caller-visible tool catalog |
| `GET /v1/tools/{tool}` | Tool metadata and schemas |
| `POST /v1/tools/{tool}` | Flat call, including dispatcher `action` |
| `POST /v1/tools/{tool}/{action}` | Narrowed action call |
| `QUERY` on a read-only tool/action | Read with JSON body; `POST` also works |
| `GET /v1/resources/…` | Generated resource read |
| `GET /v1/openapi.json` | Generated OpenAPI contract |
| `GET /v1/how-to` | Caller-scoped guide |

Resource URIs map from `proxima://x` to `/v1/resources/x`; template arguments
become path or query parameters. The instructional `proxima://how-to` resource
also answers at `/v1/how-to` when its caller key is admitted. Tool and action `ToolEffect` declarations choose allowed methods;
write actions reject `QUERY` with `405` and an `Allow` header.

A narrowed action route injects `action`; including it in that route's body
is invalid even if the values agree. Unknown actions return `404`. The
generated action schema supplies the request shape; REST does not maintain a
second schema registry. Responses carry `Cache-Control: private, no-store`.

REST call context is header-borne: bearer, selected owner, optional
`X-Proxima-Model-Id` and `X-Proxima-Self-Perspective`, and `User-Agent`. A trusted
model identity bound by the host outranks a caller label; a conflicting label
is refused. Forwarded authority is available only through the host's explicit
forwarder policy.

## Errors

MCP tools return the shared structured tool error; resource parameter errors
name the parameter. An unknown resource and a missing or invisible entity
return a resource-not-found error. The negotiated MCP revision controls wire
error-code compatibility. Internal errors redact source chains and credentials.

REST returns `application/problem+json`: machine-readable `type`, `title`,
`status`, safe `detail`, and request `instance`. Its exhaustive variant map is
[`rest/error.rs`](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/mcp-server/src/rest/error.rs).

| Failure class | HTTP status |
|---|---|
| Missing authentication | 401 |
| Insufficient authority | 403 |
| Missing or invisible entity / route | 404 |
| Invalid input or unknown schema | 400 |
| Idempotency, storage, or suppression conflict | 409 |
| Layering violation | 422 |
| Missing capability or publication capacity exhausted | 503 |
| Another store did not answer (`store-unavailable`, retryable) | 503 |
| Internal/deployment failure | 500 |

A memory on another store that the store did not answer for (it failed, timed
out, or answered for a different memory) is never reported as missing: nothing
is known about whether it exists. A single read (`core_get_memory`, derive
sources, interpret subjects) fails with JSON-RPC `-32000` and `error.data`
`{"code": "store_unavailable", "store": N}` (REST `503`, type
`store-unavailable`); retry it. The batch read `proxima://memories` lists such
ids in `unanswered`, beside `missing` (an answered not-found, erased, or
unknown store). Lineage keeps such an origin as a node with status
`unavailable` and an `unavailable_reason`: `not readable`, `store
unavailable`, `deadline exceeded`, or `origin limit exceeded`. The edge to another
store's memory carries its recorded kind: `origin` for a derivation's source,
`reference` for an interpretation's subject.

Operation-specific schema and error variants remain in code. Retry only when
the operation and its idempotency contract allow it; a transport disconnect
does not prove that a write rolled back.
