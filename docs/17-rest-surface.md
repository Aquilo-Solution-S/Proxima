# 17 — REST Surface

> **Status:** current.

Second transport projection of the same engine contract. 17 owns REST
route derivation, HTTP status mapping, and the OpenAPI document. It owns
no verb semantics, no authorization, and no persistence — those stay
with [14](14-protocol-surface.md) and [12](12-tool-manifest.md). Where
this doc and 14 disagree, 14 wins.

## Claim

REST is a **rendering of the frozen tool manifest**, not a second API.
Every route is derived at startup from `FlavorRegistryFrozen`; no route
is hand-written per tool. Tool invocations and resource reads terminate in
the same dispatch seam MCP uses; catalogs and OpenAPI are pure projections of
the frozen descriptors. A tool added to a flavor crate appears on REST with
no REST-side edit, and cannot appear on REST without appearing on MCP.

| Rule | Contract |
|---|---|
| Derivation | routes generated from `McpToolDescriptor` + `CORE_RESOURCES` |
| Dispatch | invocation/resource routes use `McpToolHost::call_tool` / `read_resource` |
| Authorization | shared edge middleware + `ScopeGateBehavior`; not re-implemented |
| Persistence | none of its own; whatever the invoked tool already does |
| Advertisement | caller's scope-filtered surface, same filter as `tools/list` |
| Placement | feature-gated module, nested under the existing listener |

Two consequences worth stating plainly. REST grants no authority MCP
does not already grant — a token that cannot call `core_publish` over
MCP cannot call it over REST, because the gate runs below the seam.
And REST cannot drift from MCP by omission, because both surfaces read
the same frozen registry rather than two lists.

## Why Not Verb-Shaped Routes

The obvious alternative is REST endpoints mirroring the five verbs —
`POST /v1/query`, `POST /v1/facts`, `GET /v1/change-events`. It reads
more idiomatic and is rejected here.

The five verbs are the *engine* contract. The tools are already the
client-facing projection of them, and they carry argument validation,
per-action field sets, annotations, and scope keys that a verb-shaped
route would have to restate. Restating them is the duplication this
surface exists to avoid, and the restatement is exactly where the two
surfaces would diverge under later change. A verb-shaped REST route
would also bypass `ScopeGateBehavior`, whose gate is keyed on tool and
action names — deployment tool-surface profiles ([15](15-deployment.md))
would silently stop applying.

Consumers who want verb-shaped ergonomics build them client-side over
the generated OpenAPI document. Per 14, client framing is a consumer
concern.

## Routes

`{tool}` is the canonical registered tool id (`core_remember`,
`proxima-code_search_chunks`). `{action}` is a dispatcher action name.

| Route | Contract |
|---|---|
| `GET /v1/tools` | scope-filtered manifest: id, description, origin, annotations, args schema |
| `GET /v1/tools/{tool}` | single descriptor; 404 when outside the caller's scope |
| `POST /v1/tools/{tool}` | invoke; request body is the tool's arguments object |
| `QUERY /v1/tools/{tool}` | same, read-only tools only; `405` otherwise |
| `POST /v1/tools/{tool}/{action}` | invoke one dispatcher action with a narrowed body |
| `QUERY /v1/tools/{tool}/{action}` | same, read-only actions only; `405` otherwise |
| `GET /v1/resources` | scope-filtered resource catalog from `CORE_RESOURCES` |
| `GET /v1/resources/{path}` | read a `proxima://` resource; query string passes through |
| `GET /v1/how-to` | the generated self-documentation, `text/markdown` |
| `GET /v1/openapi.json` | OpenAPI 3.2 document for the caller's scope |

### Resource path mapping

The mapping is total and mechanical: `/v1/resources/{path}?{query}`
reconstructs `proxima://{path}?{query}` verbatim and hands the string
to `McpToolHost::read_resource`. All URI parsing, parameter validation,
clamping, and cursor handling stay where they already are; REST adds no
parser.

| REST | Engine URI |
|---|---|
| `GET /v1/resources/schemas?kind=fact` | `proxima://schemas?kind=fact` |
| `GET /v1/resources/tools` | `proxima://tools` |
| `GET /v1/resources/graph` | `proxima://graph` |
| `GET /v1/resources/memory/F:{uuid}?expand_neighbors=true` | `proxima://memory/F:{uuid}?expand_neighbors=true` |
| `GET /v1/resources/memories?ids=F:{a},A:{b}` | `proxima://memories?ids=F:{a},A:{b}` |
| `GET /v1/resources/memory/{id}/lineage?direction=ancestors&depth=3` | `proxima://memory/{id}/lineage?...` |
| `GET /v1/resources/change-events?since={seq}&limit=100` | `proxima://change-events?since={seq}&limit=100` |
| `GET /v1/resources/wake-candidates?fact=F:{uuid}` | `proxima://wake-candidates?fact=F:{uuid}` |
| `GET /v1/resources/goals?state=active&limit=50` | `proxima://goals?state=active&limit=50` |
| `GET /v1/resources/goal/G:{uuid}` | `proxima://goal/G:{uuid}` |

`proxima://how-to` is the one exception. It is synthesized per request
from the caller's advertised surface rather than served through the
dispatch seam, so it gets its own route and is not reachable through
`/v1/resources/how-to`.

### Methods: POST for writes, QUERY for reads

Tool arguments are arbitrarily nested JSON objects described by a JSON
Schema. Encoding them into a query string would require a second
serializer and a second schema dialect, so read-only tools cannot be
`GET`.

`QUERY` (RFC 10008) is exactly the method for this: safe, idempotent,
and specified with a request body from the start. Read-only tools are
therefore reachable by **both** `QUERY` and `POST`; write tools accept
`POST` only.

| Behavior declaration | Methods |
|---|---|
| flat-tool or action `read_only: true` | `QUERY`, `POST` |
| anything else | `POST` |

`POST` is retained alongside `QUERY` rather than replaced. Middleboxes
— corporate proxies, WAFs, CDNs, older gateways — routinely reject
unrecognized methods, and a read that is unreachable through a customer
proxy is worse than a read with imprecise semantics. Clients that can
use `QUERY` get honest semantics; clients behind hostile
infrastructure fall back without losing the surface. The generated
OpenAPI document advertises both, so the fallback is discoverable
rather than folklore.

What `QUERY` buys here is retry safety and correct modelling, not
caching. Every response is owner- and token-scoped, so all tool routes
carry `Cache-Control: private, no-store` regardless of method —
`QUERY`'s cacheability is deliberately unused. The gain is that a
client, proxy, or client library may replay a failed read without
asking whether replay is safe.

The read/write distinction remains authoritative where it already is:
`read_only` selects `may_read` versus `may_write` inside
`ScopeGateBehavior`, and reaches clients through the manifest and
through OpenAPI. `GET /v1/resources/…` remains the browsable read
surface, which is why resources exist as a separate concept.

Two implementation constraints, both small and both worth recording
because they are the sort of thing rediscovered painfully:

- `http` 1.5.0 carries `Method::QUERY` as a first-class constant with
  `is_idempotent()` true. The workspace pins `http = "1"` and resolves
  to 1.5.0, so no dependency change is needed.
- `axum` 0.8.9 — the pinned `axum = "0.8"` resolution, and the latest
  published release — has a `MethodFilter` closed over the nine classic
  methods, with `TryFrom<Method>` rejecting `QUERY`. Generated routes
  therefore cannot use `on(MethodFilter::…)`. They use a catch-all
  handler that matches on `Method` directly and emits `405` with an
  explicit `Allow: QUERY, POST` header, since `any()` calls
  `skip_allow_header()` and suppresses axum's own `Allow` generation.
  This is one shared handler, not per-route code.

The axum gap is lag, not design: 0.8.9 predates `http` 1.5.0 by about
three months, and `Method::QUERY` did not exist when it shipped. When a
later 0.8.x adds `MethodFilter::QUERY`, the caret pin picks it up and
the catch-all collapses into ordinary routing with no change to
anything in this document. Treat the handler as scaffolding with a
known removal condition.

### Dispatcher actions

A dispatcher is any tool declaring `ACTION_ARG_SPECS` — the five substrate
ones (`core_goal`, `core_fact`, `core_membership`, `core_publish`,
`core_upload`) and any flavor tool that declares its own. Its discriminator
must be `action`: this surface injects `"action"` into the body on the
narrowed route, and `try_freeze` refuses a dispatcher tagged on anything else
(see [12 §Action-Dispatch Tools](12-tool-manifest.md#action-dispatch-tools)).
Dispatchers advertise a flattened schema with an `action` discriminator and
per-action field sets under `x-proxima-actions`. REST exposes both forms:

- `POST /v1/tools/core_goal` — body carries `action`, as on MCP.
- `POST /v1/tools/core_goal/set` — the adapter injects
  `"action": "set"` into the body before dispatch.

The narrowed form is the better REST citizen and the better OpenAPI
operation: its request schema is built by selecting
`x-proxima-actions.set.allowed_fields` out of the flattened properties,
applying `required_fields` as the schema's `required`, and dropping the
`action` property. That is strictly more precise than the flattened
schema an MCP client sees, and it is generated, not authored.

Two failure modes must be explicit rather than silent:

- A body on the narrowed route that also carries `action` is rejected
  `400`, even when the values agree. Silent agreement invites a client
  that sets only the body field and breaks when the route changes.
- An unknown `{action}` is rejected `404` at the route layer, before
  dispatch, so it reads as "no such route" rather than as an argument
  error.

`POST` vs `QUERY` is resolved only from the action's
`McpActionArgSpec.annotations`, for substrate and flavor dispatchers alike.
There is no tool-level or `CoreActionMeta` fallback; missing annotations or
missing `read_only` fails closed as write/`POST`. The same spec drives the
owner-role gate, scope-filtered catalogs, REST method gate, and OpenAPI
operation. A flavor enum variant's doc comment is derived into
`x-proxima-actions.<action>.description` and rendered by the catalog and
OpenAPI action operation.

## Call Context

MCP sources author context partly from the JSON-RPC peer identity and
partly from reserved argument fields, which `strip_call_context_args`
removes before validation. REST has no peer identity, and a fresh
surface has no client-compatibility debt to carry, so call context is
header-borne and reserved body fields are an error rather than a
silently ignored duplicate.

| Header | Feeds | Contract |
|---|---|---|
| `Authorization: Bearer` | `AuthzContext` | required; unchanged from MCP |
| `X-Proxima-Owner` | `Owner` selection | selects the owner for this call |
| `X-Proxima-Model-Id` | `McpAuthorContext.model_id` | optional; falls back to the token's model id, then `unknown` |
| `X-Proxima-Self-Perspective` | `caller_self_perspective` | optional; `P:` reference |
| `User-Agent` | `client_name` / `client_version` | parsed; unattributed calls record the adapter's own name |

For generic SDK tools, the shared adapter projects `model_id`, `client_name`,
and `client_version` into `ToolCtx::caller()`. The caller Self Perspective
remains available separately through `ToolCtx::caller_self_perspective()`.

A request body containing `model_id`, `caller_self_perspective`,
`_proxima_caller_self_perspective`, or
`current_root_perspective_memory_id` is rejected `400`. These names
stay reserved on REST so a future move of a field between header and
body cannot be mistaken for a schema change.

Rejecting is not pedantry. These fields carry **provenance into an
append-only store**. A client that copies an MCP payload to REST and
has its `model_id` silently stripped gets `200 OK` on every call while
every Fact it writes is attributed to `unknown` — and because memory is
append-only, that attribution cannot be corrected in place afterward,
only superseded or rebuilt. The failure is invisible at write time and
expensive at read time, which is the worst possible shape.

The rejection is loud, immediate, and self-correcting:

```http
POST /v1/tools/core_remember
Authorization: Bearer …
X-Proxima-Owner: …

{"text": "…", "model_id": "example-model"}
```

```json
{
  "type": "https://proxima.dev/errors/reserved-argument",
  "title": "Reserved argument in request body",
  "status": 400,
  "detail": "`model_id` is call context on this surface; send it as the X-Proxima-Model-Id header",
  "instance": "/v1/tools/core_remember"
}
```

The cost is one failed request during integration, carrying the exact
fix. The alternative costs a corpus.

`Mcp-Session-Id` is not part of the REST contract. REST is stateless:
callers select the owner with `X-Proxima-Owner` on every request. The
shared middleware still honors a session id if one is sent, but no
REST client should rely on it, and the 404-on-unbound-session behavior
exists for Streamable-HTTP re-initialize semantics that REST does not
have.

## Errors

The core envelope is 14's: a typed code, a safe client-facing message,
and an optional `request_id`. REST renders it as RFC 9457
`application/problem+json`, which is the transport-specific `details`
extension 14 anticipates.

```json
{
  "type": "https://proxima.dev/errors/not-authorized",
  "title": "Not authorized",
  "status": 403,
  "detail": "tool core_publish not authorized for this MCP token",
  "instance": "/v1/tools/core_publish/publish_to_world"
}
```

`detail` is `McpToolError::client_message()`, never `Display`. That
single rule preserves the existing redaction: internal-kind errors
collapse to a generic string before the adapter ever sees them, and
`Unavailable` passes through verbatim because it is a caller-actionable
precondition.

### Status mapping

`McpToolErrorKind` is too coarse for HTTP — its `InvalidRequest` bucket
holds authorization, conflict, and capability failures that must not
share a status, and `ToolInvocationError::kind()` maps `ToolNotFound`
to `InvalidInput`, which is a `400` where HTTP wants `404`. The REST
map therefore matches on **variants**, not kinds.

| Error | Status |
|---|---|
| `ToolInvocationError::ToolNotFound` | 404 |
| `ToolInvocationError::NotAuthorized` | 403 |
| `McpToolError::InvalidInput` | 400 |
| `McpToolError::NotFound` | 404 |
| `McpToolError::NotAuthorized` | 403 |
| `McpToolError::LayeringViolation` | 422 |
| `McpToolError::Unavailable` | 503 |
| `Protocol(AuthRequired)` | 401 |
| `Protocol(Forbidden)` | 403 |
| `Protocol(UnknownSchema)` | 400 |
| `Protocol(ToolNotRegistered)` | 404 |
| `Protocol(AlreadyIngested)` | 409 |
| `Protocol(IdempotencyConflict)`, `Storage(IdempotencyConflict)` | 409 |
| `Protocol(TriggerConflict)`, `Protocol(DuplicateTriggerInRequest)` | 409 |
| `Storage(Conflict)` | 409 |
| `Protocol(Suppressed)`, `Storage(Suppressed)` | 409 |
| kind `Internal` (storage retryable/unavailable/internal, `Other`) | 500 |

Suppression maps to `409` with a distinct `type`, not `451`.
Suppression is a compliance primitive ([13](13-compliance.md)) and is
not necessarily a legal hold; `451` would overclaim.

The map is an exhaustive `match` with no wildcard arm and lives beside
the error definitions, so adding an error variant is a compile error
until someone chooses its status.

## OpenAPI

`GET /v1/openapi.json` returns OpenAPI **3.2**. Both bounds are forced,
not preferred. Below 3.1, `args_schema` — schemars-generated JSON
Schema draft 2020-12 — would need a down-converter, which is new code
that can be wrong. Below 3.2, the `query` field does not exist on the
Path Item Object, so a `QUERY` operation could only be expressed
through `additionalOperations` or not at all. 3.2 keeps the 2020-12
dialect, so the newer floor costs nothing in schema fidelity.

| Element | Source |
|---|---|
| path per tool | `McpToolDescriptor.name` |
| path per dispatcher action | `McpToolDescriptor.action_arg_specs` |
| path per resource | `CoreResourceMeta.uri_template` |
| `post` / `query` operations | `is_read_only()` / `action_is_read_only()` |
| `operationId` | structurally tagged `tool` / `action` / `resource` target with byte-length-prefixed name components and an explicit method tag |
| `summary` / `description` | `McpToolDescriptor.description`; substrate action description from `CoreActionMeta`, flavor action description from `x-proxima-actions.<action>.description` |
| request schema | `args_schema`, narrowed per action |
| success response schema | `output_schema`, derived from the tool's Rust `Output` type |
| `x-proxima-read-only`, `-destructive`, `-idempotent` | flat tool `resolved_annotations()`; dispatcher `McpActionArgSpec.annotations` |
| security scheme | HTTP bearer |

The document is generated per caller and reflects that caller's
`ToolScope`, exactly as `tools/list` does. It is therefore
token-specific and served `Cache-Control: private, no-store`.

Embedding hosts and offline conformance tests call
`proxima::host::build_openapi_document(registry, public_url)`. It emits the
complete frozen registry plus all core resources. The served route applies its
caller-scoped authorization context to the same generator. The facade owns
resource enumeration, so consumers never depend on `proxima-mcp-server`'s
descriptor-level generator or transport auth types.

`/v1/resources` is generated from `CoreResourceMeta` alone: flavors cannot
declare resources, which is deliberate rather than a gap — see
[08 §Substrate MCP Surface](08-core-and-flavors.md#substrate-mcp-surface).

Idempotency stays an argument-level concern. Tools that support replay
already take `idempotency_key`, `request_id`, or `receipt_id`; REST
adds no `Idempotency-Key` header, because a second mechanism would need
its own storage and its own conflict semantics.

Every `McpToolDescriptor` carries `output_schema`, derived by
`mcp_output_schema::<T::Output>()` exactly as `args_schema` is derived from
`T::Args`. MCP publishes it as `outputSchema`; OpenAPI uses the same value as
the tool route's `200` response schema. `produces_schema_ids` remains a
separate annotation naming registry payloads the tool writes, not the reply
envelope.

## Invariants

- **R1.** No REST route reaches `Engine` or storage directly. Invocation and
  resource routes terminate in `McpToolHost::call_tool` or
  `McpToolHost::read_resource`; catalog routes read frozen descriptors only.
- **R2.** No hand-written per-tool or per-resource route. Routes are
  derived from the frozen registry at startup.
- **R3.** Authorization is not re-implemented. The shared edge
  middleware resolves `AuthzContext` and `Owner`; `ScopeGateBehavior`
  is the only tool/action gate.
- **R4.** Error `detail` is `client_message()`. Never `Display`, never
  a formatted source chain.
- **R5.** Status mapping is exhaustive over error variants; a new
  variant fails compilation until mapped.
- **R6.** REST advertises exactly the caller's scope-filtered surface —
  the same filter `tools/list` applies, including per-action narrowing.
- **R7.** No route writes an edge. Inherited from the kernel via the
  dispatch path; REST introduces no write path of its own.

## Placement and Configuration

The adapter is a feature-gated module inside the existing MCP server
crate, not a new crate, binary, or service. It mounts as a nested
router beside `/mcp` on the same listener, inside the same body-limit,
Host, listener-wide CORS, and auth layers — which is why it inherits browser
preflight handling, bearer validation, owner resolution, and stream
revalidation without restating any of them.

The surface is off by default. The `rest` Cargo feature compiles it;
`PROXIMA_REST_ENABLED=true` mounts it. Both gates are required. Deployment
guidance, including whether to expose `/v1` through the same gateway as
`/mcp`, belongs to [15](15-deployment.md).

### Protected-resource identifier

`/.well-known/oauth-protected-resource` advertises `{public_url}` as the RFC
9728 protected-resource identifier. Clients send that public origin as the
RFC 8707 `resource`; `PROXIMA_OIDC_AUDIENCE` matches it. One audience covers
both `/mcp` and an enabled `/v1`.

## Out of Scope

- Verb-shaped or resource-shaped route aliases beyond the generated
  surface: a consumer concern, built over the OpenAPI document.
- Compliance operations. Erase, export, and pause/resume are admin
  actions, not graph calls, and are not exposed on MCP either:
  [13](13-compliance.md).
- Streaming, subscriptions, and server-sent events. REST responses are unary
  ([14](14-protocol-surface.md)).
- Runtime registration of tools, schemas, sources, or flavors.
- gRPC and any third transport.

## Contract Tests

`crates/mcp-server/tests/rest_surface.rs` keeps "no duplication" executable:

- **Surface parity.** For a given `ToolScope`, the REST tool list
  equals the MCP `tools/list` projection, id for id and action for
  action.
- **Gate parity.** A tool or action denied over MCP is denied over
  REST, with `403` rather than a `404` that leaks existence.
- **Status exhaustiveness.** The variant match compiles without a
  wildcard arm.
- **Reserved fields.** Each of the four reserved argument names is
  rejected `400` in a request body, with a `detail` naming its header.
- **Method gating.** `QUERY` on a write tool or a write dispatcher
  action is `405` with `Allow: POST`; `QUERY` and `POST` on a read-only
  tool produce byte-identical responses.
- **Action injection.** A narrowed route with a conflicting body
  `action` is rejected `400`; an unknown action is `404`.
- **Resource passthrough.** Malformed resource query parameters
  produce the same error class as the equivalent MCP resource read.

## Cross-References

- Engine verb semantics, error envelope, auth model:
  [14](14-protocol-surface.md).
- Tool registration, tool classes, dispatcher actions:
  [12](12-tool-manifest.md).
- Flavor composition and build-time registration:
  [08](08-core-and-flavors.md).
- Runtime configuration: [10](10-configuration.md).
- Deployment, network exposure, tool-surface profiles:
  [15](15-deployment.md).
- Compliance primitives and the admin surface:
  [13](13-compliance.md).
