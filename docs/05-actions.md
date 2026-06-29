# 05 — Actions

Actions occupy the Goals -> Actions -> Reality arc of the wheel.

## Claim

Action = attempted intervention in Reality.

Substrate shape:

| Concept | Rule |
|---|---|
| Action attempt | ordinary Fact emitted by a trusted source or tool path |
| External effect | later ordinary Fact emitted by the observing EventSource |
| Action identity | Fact `memory_id`; no `ActionId` |
| Action schema | registered `FactPayload`; no separate action payload family |
| Motivation | later A/P interpretation or Goal evidence, never Fact mutation |

No Action entity. No Action lifecycle. No special action edge class.
Actions become traceable because attempts and consequences re-enter the
append-only Fact stream.

## Shape

```
Goal / Perspective / user intent
  -> wake entry or trusted EventSource
  -> tool / external side-effect attempt
  -> Fact(action attempt)
  -> Reality changes or refuses
  -> EventSource observation
  -> Fact(effect)
  -> F->A / A->P / A->Goal interpretation
```

The wheel closes through observation, not by mutating the action row.

## Wake-trigger principle

Automated action selection is wake execution.

| Trigger | Selector | Output |
|---|---|---|
| `change_event` | matching wake entry | tool call, A/P/Goal/Edge write, or no output |
| UI / chat / trusted source | user or source policy | action-attempt Fact |
| external callback | EventSource | effect Fact |

There is no standalone action-selection registry in core. Wake entries are
detect-only. An external harness receives tool scope from token capabilities
intersected with the deployment tool-surface profile, plus visible read scope,
active Goals, and prompt/instructions from flavor/runtime config (see 04).

Manual action is the same shape: a user-facing surface emits or approves a
Fact through a trusted source. The distinction is authoring, not entity kind.

## Tool boundary

Tools are effect adapters.

| Boundary | Rule |
|---|---|
| Tool vocabulary | 12 owns build-time tool classes, MCP dispatch, and compliance declarations |
| Runtime tool scope | auth token scope ∩ deployment profile (`ToolScope::Palette` when narrowed) |
| Memory-space RBAC | memory reads/writes/actions are gated by owner-space grants plus tool scope; grants are host-resolved `(subject, Owner, action)` capabilities over the `search/read/write/publish/admin` action vocabulary |
| Persistence | tool result enters storage only as registered Fact / Edge writes |
| A/P writes | operator/wake output protocol only; tools do not bypass 04 |
| Failure | failed attempts are Facts when the source/tool schema models them |

Current v1 tool execution is MCP dispatch.
External HTTP/WASM body transports are deferred. That execution detail
is not an action ontology.

## Effect on Reality

External state is outside the substrate.

Rules:

- A successful tool call may change Reality before Proxima observes the result.
- The observed consequence returns through the normal EventSource path.
- Request ids, message ids, branch names, issue ids, and payload references may
  create ordinary structural edges.
- No action-effect shortcut relation is required.
- No rollback is implied by deleting or superseding Proxima rows (see 13).

## MCP-call Facts

MCP tool calls can be logged as ordinary content-addressed Facts by
`persist_mcp_call`.

Registered schemas:

| Schema | Kind | Contents |
|---|---|
| `core/mcp-call-logged-v1` | Fact | tool, actor, status, latency, I/O hash |
| `core/mcp-call-io-v1` | CitedObject | inline I/O bytes, byte length, truncation flag |
| `core/mcp-call-io-citation-v1` | CitationMapping | pure link; no sidecar table |

Invariant:

- Call audit is Fact-only; I/O bytes are cited as bibliographic payload.
- Idempotency is `McpCallLogInput::receipt_id`.
- Vendor LLM-call / embedding-call families, price books, tier accounting,
  token-count accounting, and cost tables are deferred.

## Human approval

Some tools require human approval before external execution.

| Case | Rule |
|---|---|
| Legal consequence | tool metadata marks the risk; user-authored approval remains required design intent |
| Proposal | wake/source emits a proposal Fact |
| Approval | user-authored EventSource Fact is the firing observation |
| Execution | approved tool call emits its own attempt/result Facts |

Approval is an EventSource pattern, not a new entity or lifecycle.

## Idempotency

Same rule as any EventSource:

| Path | Key |
|---|---|
| action-attempt Fact | source-defined `receipt_id` |
| effect Fact | observing source's `receipt_id` |
| tool result persistence | tool/source request id inside its Fact payload when needed |

No `ActionId`. Re-receipt dedups at the EventSource boundary.

## Validation at ingest

Every action-attempt or effect Fact follows the ordinary ingest contract:

| Check | Rule |
|---|---|
| owner | source/tool may write only within the authorized Owner |
| owner-space grant | memory writes require the resolved `write` action for that Owner; publish requires `read` + `publish` on the source space and `write` on the target space |
| schema | `schema_id` / version must resolve to a registered `FactPayload` |
| relation | structural edges must use registered relation descriptors |
| capability | tool output must stay within registered schemas/relations and resolved tool scope |
| atomicity | Fact, sidecar, structural edges, and change event commit together |

Publication is synchronous with Fact materialization for engine-mediated tool
paths: the wake run sees its own emitted Fact before any later step depends on
it.

## Versioning

Action-attempt and effect payloads follow Fact schema migration discipline
(see 03). Tool vocabulary follows build-time core/flavor versioning
(see 08, 12).

Schema evolution moves sidecar bytes only. Fact identity, citations, and
provenance stay fixed.

## Invariants

- EventSource membrane and Owner scoping: 01.
- Fact payload typing and migration: 03.
- Wake execution and output protocol: 04.
- Tool vocabulary and MCP dispatch: 12.
- External side effects and approval posture: 13.
- Motivation is interpretation over Facts or Goal evidence, not a core action
  field.
