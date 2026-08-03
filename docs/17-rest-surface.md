# 17 — REST Surface

> **Status:** design intent. No REST routes ship today; this doc is the
> contract a REST adapter must satisfy before implementation lands.

Second transport projection of the same engine contract. 17 owns REST
route derivation, HTTP status mapping, and the OpenAPI document. It owns
no verb semantics, no authorization, and no persistence — those stay
with [14](14-protocol-surface.md) and [12](12-tool-manifest.md). Where
this doc and 14 disagree, 14 wins.

## Claim

REST is a **rendering of the frozen tool manifest**, not a second API.
Every route is derived at startup from `FlavorRegistryFrozen`; no route
is hand-written per tool; every route terminates in the same dispatch
seam MCP uses. A tool added to a flavor crate appears on REST with no
REST-side edit, and cannot appear on REST without appearing on MCP.

| Rule | Contract |
|---|---|
| Derivation | routes generated from `McpToolDescriptor` + `CORE_RESOURCES` |
| Dispatch | `McpToolHost::call_tool` / `read_resource`; no other entry |
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
| `GET /v1/openapi.json` | OpenAPI 3.1 document for the caller's scope |

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
| `GET /v1/resources/edges?kind=origin&source=A:{uuid}` | `proxima://edges?kind=origin&source=A:{uuid}` |

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

| Tool annotation | Methods |
|---|---|
| `read_only: true` | `QUERY`, `POST` |
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

`POST` vs `QUERY` is resolved per action, from the per-action manifest entry
first and the tool's own annotations only when there is no entry. A *flavor*
dispatcher has no per-action entry today, so its tool-level annotations
decide the method for all of its actions — a stated gap with a named hazard,
see [12 §Known gaps for flavor dispatchers](12-tool-manifest.md#known-gaps-for-flavor-dispatchers).

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

{"text": "…", "model_id": "claude-opus-5"}
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
  "instance": "/v1/tools/core_publish/to_world"
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
| `post` / `query` operations | `resolved_annotations().read_only` |
| `operationId` | `{tool}` or `{tool}__{action}`, suffixed per method |
| `summary` / `description` | `McpToolDescriptor.description`, per-action `field_descriptions` |
| request schema | `args_schema`, narrowed per action |
| `x-proxima-read-only`, `-destructive`, `-idempotent` | `resolved_annotations()` |
| security scheme | HTTP bearer |

The document is generated per caller and reflects that caller's
`ToolScope`, exactly as `tools/list` does. It is therefore
token-specific and served `Cache-Control: private, no-store`.

`/v1/resources` is generated from `CoreResourceMeta` alone: flavors cannot
declare resources, which is deliberate rather than a gap — see
[08 §Substrate MCP Surface](08-core-and-flavors.md#substrate-mcp-surface).

Idempotency stays an argument-level concern. Tools that support replay
already take `idempotency_key`, `request_id`, or `receipt_id`; REST
adds no `Idempotency-Key` header, because a second mechanism would need
its own storage and its own conflict semantics.

**Response schemas are the known gap.** `McpToolDescriptor` carries
`args_schema` but no output schema — only `produces_schema_ids`. Until
that changes, generated responses are `type: object` annotated with the
produced schema ids.

`output_schema`, derived from `T::Output` exactly as `args_schema` is
derived from `T::Args`, is worth doing on its own merits rather than
for REST: rmcp 3 carries `outputSchema` on `Tool`, so MCP clients gain
structured-output validation from the same change, and a derived schema
cannot drift from the type the way hand-written response documentation
does.

It is not free, and it is not a REST slice. `McpTool::Output` is bound
`Serialize + Send + 'static` today; adding `JsonSchema` is a
flavor-SDK-breaking trait-bound change that every flavor tool's output
type must satisfy. That is acceptable pre-1.0 but belongs in its own
reviewed slice ahead of this one, not smuggled in beside a transport
adapter.

## Invariants

- **R1.** No REST route reaches `Engine` or storage directly. Every
  route terminates in `McpToolHost::call_tool` or
  `McpToolHost::read_resource`.
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
router beside `/mcp` on the same listener, inside the same auth and
body-limit layers — which is why it inherits bearer validation, origin
allowlisting, owner resolution, and stream revalidation without
restating any of them.

The surface is off by default and enabled by an environment flag whose
spelling is owned by [10](10-configuration.md). Deployment guidance,
including whether to expose `/v1` through the same gateway as `/mcp`,
belongs to [15](15-deployment.md).

### Protected-resource identifier

`/.well-known/oauth-protected-resource` currently advertises
`{public_url}/mcp` as the RFC 9728 protected-resource identifier. That
identifier is per-surface, and a second surface makes it wrong.

**Decision: broaden the identifier to `{public_url}`, covering both
`/mcp` and `/v1`, and do it in the slice that introduces `/v1` — not
after.** One identifier means one audience, one metadata document, and
one token that reaches both surfaces. Two identifiers would mean two
audiences and therefore non-interchangeable tokens, which is a feature
only for deployments that want surface-scoped credentials, and a
permanent tax for everyone else.

The timing is the substance of the decision. The identifier is the
`resource` value clients pass under RFC 8707 and the audience an
authorization server stamps into tokens, so changing it invalidates
issued tokens and requires every client to re-request. Today that
population is small and pre-1.0; once `/v1` ships under a separate
identifier, the two-audience split is baked into every deployment and
every issued credential, and consolidating later is a coordinated
break across two client populations instead of one.

This is a deliberate MCP-auth-visible breaking change and needs a
`MIGRATING.md` entry alongside the REST slice.

## Out of Scope

- Verb-shaped or resource-shaped route aliases beyond the generated
  surface: a consumer concern, built over the OpenAPI document.
- Compliance operations. Erase, export, and pause/resume are admin
  actions, not graph calls, and are not exposed on MCP either:
  [13](13-compliance.md).
- Streaming, subscriptions, and server-sent events. `Subscribe` is
  retired in [14](14-protocol-surface.md); REST responses are unary.
- Runtime registration of tools, schemas, sources, or flavors.
- gRPC and any third transport.

## Test Obligations

An implementation is not complete without these, because they are what
make "no duplication" checkable rather than aspirational:

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
