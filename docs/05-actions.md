# 05 — Actions

Actions occupy the Goals → Actions → Reality arc of the wheel.

## Claim

Actions are **Events from a built-in `system` source**. No separate
entity, no separate `ActionSchema`, no lifecycle. Tool invocations land
as Facts via the same pipeline as external observations.

Component 05 = built-in source + tool registry. No new memory kind.
Universe stays F/A/P.

## System EventSource

Built-in to core. Implements the 01 `EventSource` trait.

```rust
const SYSTEM: SourceId = SourceId::builtin("system");

struct SystemEventSource;

impl EventSource for SystemEventSource {
    fn source_id(&self) -> SourceId { SYSTEM }
    fn source_uri(&self) -> Uri { "proxima://system".into() }
    fn schema_version(&self) -> Version { Version::new(1) }
}
```

Tool invocation → Event from `SYSTEM` → Fact under a registered
`FactSchema` (e.g. `system-question-asked`, `system-tool-call-X`).

The emitted `SYSTEM` event carries the `Owner` of the user/org being
actuated for. The decider supplies the `Owner` at invocation; the
engine validates that the invoking principal has permission to act in
that scope.

`SYSTEM` also emits dispatcher-driven `LlmCallV1` and `EmbeddingCallV1`
Facts for every LLM and embedding call the engine makes — see
§Dispatcher-emitted call Facts below.

## Tool registry

```rust
struct ToolRegistration {
    id:            ToolId,
    name:          String,         // "ask-question"
    schema_id:     SchemaId,       // payload schema (FactSchema)
    availability:  AvailabilityFn, // ctx -> bool
    callable:      ToolCallable,
}

enum ToolCallable {
    EngineLocal(BoxedFn),                  // Proxima invokes directly
    External { request_topic: String },    // emit; external executes
}
```

Available set at time `t` for `Owner ω`:
`{ T ∈ tools | T.availability(ctx) }` where `ctx` includes `ω`.
Cardinality `0..n`. Tool registry itself is global; availability per
owner is the per-call check.

Tools split into two tiers (see [12 — Tool Manifest](docs/12-tool-manifest.md) for the full tool manifest specification and T1/T2 tier details):

- **T2 (build-time):** registered at startup from the linked flavor
  crate's `register` call. v1 ships T2 only.
- **T1 (runtime, post-v1):** signed manifests installed via API,
  bodies sandboxed in WASM or proxied to MCP / HTTP. T1 cannot invent
  Fact schemas or relations — capabilities are validated against the
  T2-frozen set at install time.

A/P authorship is operator-only regardless of tier. Tools may only
emit Facts.

## Deciders — flavor-supplied

A decider is a loop that picks **which** tool to call when — the only
piece that decides *to act*. F→A, A→P, and edge operators interpret
state; the decider selects action. Substrate does **not** enforce one.

A flavor registers **zero, one, or many** deciders:

- **Zero** — fully manual or observation-only. The user (or another
  trusted EventSource — a chat command, a UI button) emits `SYSTEM`
  events directly; no automated loop exists. Pure-observation flavors
  that never act sit here permanently.
- **One** — typical for a flavor with a single action surface.
- **Many** — separate deciders per use case (`code/triage-decider`,
  `code/auto-merge-decider`), each gated by its own per-call
  `availability` predicate. Programmed-rule and LLM-driven deciders
  may coexist; the available set at time `t` is the union, filtered
  by availability.

Implementation styles compose freely:

- **Programmed rules:** `if X then Y`. No LLM call.
- **Tool-calling LLM agents:** model receives the available tool set
  and goal/perspective context, picks the act.
- **Human-in-the-loop:** decider emits a proposal Fact; a separate
  EventSource (UI, chat) emits the approval, which triggers the tool.
  Falls out of existing primitives — no new substrate machinery.

### Gradual scaling

Decider sophistication is use-case dependent and grown over time.
Substrate gives the primitives; the trajectory is a flavor + deployment
decision:

- **Phase 1 — manual.** No decider registered. User drives action via
  the UI; the F→A / A→P pipeline runs and surfaces interpretation, but
  every act is a human choice. Builds trust in the chain.
- **Phase 2 — selective automation.** Programmed-rule decider added for
  classes of action that have proven safe (e.g. auto-label issues that
  match a known pattern). Manual still handles novelty.
- **Phase 3 — full auto.** LLM-driven decider takes over once the
  action pipeline is well-understood. Manual remains as a fallback path
  for high-risk classes via the same per-tool availability gate.

Promotion between phases is a deployment-config event, not a substrate
event — register the new decider, gate the old one off, no schema
change.

### Routing and protocol

Deciders live **inside** the binary, registered alongside the flavor's
operators and prompts ([08 §What a flavor supplies](docs/08-core-and-flavors.md#what-a-flavor-supplies)).
LLM-driven deciders declare `tier` and `requires` like other operators
([10 §Operator declaration](docs/10-configuration.md#operator-declaration)) —
typically `tier: Standard` with `requires: { tool_use: true }` for the
agent-loop variant. Programmed-rule deciders omit both.

The substrate ships the dispatcher, the tool registry, and the
inference layer ([10 §LLM credential resolution](docs/10-configuration.md#llm-credential-resolution));
deciders ride on top — call "the LLM at this tier" and the engine
resolves credentials per the call's `Owner`.

From the protocol surface ([14](docs/14-protocol-surface.md)) deciders
are invisible — they never appear on the wire. Clients observe only
the resulting `SYSTEM` events.

## Dispatcher-emitted call Facts

The `SYSTEM` source emits more than tool invocations. The substrate's
LLM and embedding dispatcher emits **one `LlmCallV1` or `EmbeddingCallV1`
Fact per dispatcher-driven call**, success or failure, under the same
`SYSTEM` source and the same Fact pipeline as everything else.

```rust
struct LlmCallV1 {
    consumer:           ConsumerId,        // Operator | Decider | Source
    owner:              OwnerId,
    personality_id:     Option<PersonalityId>,
    tier:               ModelTier,
    vendor:             String,
    model_id:           String,
    prompt_tokens:      u32,               // uncached input tokens
    cache_read_tokens:  Option<u32>,       // when vendor reports cache hits
    cache_write_tokens: Option<u32>,       // tokens written to cache this call
    completion_tokens:  u32,
    latency_ms:         u32,
    cost_micro_usd:     Option<u64>,       // 1 USD = 1_000_000 micro-USD
    status:             CallStatus,        // Ok | Timeout | Refused | Error(kind)
}

struct EmbeddingCallV1 {
    consumer:       ConsumerId,
    owner:          OwnerId,
    vendor:         String,
    model_id:       String,
    dim:            u32,
    n_inputs:       u32,
    total_tokens:   u32,
    latency_ms:     u32,
    cost_micro_usd: Option<u64>,
    status:         CallStatus,
}
```

`ConsumerId` carries the typed identity of the caller (operator id,
decider id, or source id) so cost can be grouped without parsing
strings. `cost_micro_usd` is `None` when the price book has no entry
for `(vendor, model_id)` — typically BYOK against an unrecognised
model. Token counts are always present; downstream consumers can
reprice retroactively if they fill in the gap. The unit is
micro-USD (10⁻⁶ USD) so calls under one cent are still
distinguishable; computation of the field is specified in
[10 §Price book](docs/10-configuration.md#price-book).

### Invariant — all LLM and embedding traffic routes via dispatcher

Operators, deciders, and EventSources do not call vendor SDKs directly.
The dispatcher exposes typed `Llm` and `Embedder` handles; consumers
take one and call through it. Fact emission is unconditional — there
is no code path that talks to a vendor without producing the
corresponding `LlmCallV1` / `EmbeddingCallV1` Fact.

A flavor that adds a "small direct Anthropic call inside its source"
violates the invariant. It creates blind spots in:

- **Cost tracking** — usage metering ([10 §Operator concurrency](docs/10-configuration.md#operator-concurrency))
  reads from this Fact stream; off-dispatcher calls are invisible to
  billing and dashboards.
- **Quota enforcement** — `cost_cap.llm_concurrency` and
  `llm_tokens_per_minute` only bind on the dispatcher.
- **Credential resolution** — BYOK and per-Owner key routing
  ([10 §LLM credential resolution](docs/10-configuration.md#llm-credential-resolution))
  only run inside dispatcher entry.
- **Audit** — every external paid call should be a memory citable by
  whatever consumed its output.

### Read pattern — events or periodic, same surface

Cost-by-(Owner, personality, operator, tier) is a SQL group-by over
`proxima_core.fact_llm_call_v1`. Real-time consumers (dashboards,
anomaly detection, kill-switches) tail the change feed; batch
consumers (monthly billing, weekly cost reports) query periodically.

"Spending on self-model is up 4× this week" can itself be an
Abstraction — F→A over `core/llm-call-v1` Facts emitting a
`core/cost-anomaly` payload — surfaced through the same UI as every
other interpretation. No counter table; no separate metrics pipeline.
Calls don't get retracted, so the stream is append-only without
supersession.

## Execution boundary — per tool

`ToolCallable::EngineLocal` — Proxima invokes the callable.
`ToolCallable::External` — Proxima publishes a request message; an
external process executes and reports back via its own EventSource (or
re-publishes to `SYSTEM` with the result payload).

No single rule. Per-tool config.

## Effect on Reality

Real-world consequence re-enters via the normal EventSource path (chat
EventSource emits "user replied", etc.). Edge from action-Fact to
effect-Fact falls out of payload references (`message_id`,
`thread_id`, `request_id`) — same structural-edge mechanism as
anywhere else. No special wiring.

## Motivation

Motivations are interpretive — they live as **Abstractions citing
Action-Facts**, never on the action-Fact itself. An Action-Fact has
zero, one, or many motivation Abstractions:

- Zero: reflex / programmed-rule act with no motivating goal recorded.
- One: a single goal explains the act.
- Many: the act was poly-motivated (e.g. "ship the patch" and
  "unblock the team" together).

This preserves the F/A separation. Facts are deterministic (what
happened); motivations are interpretations under a Π and belong in A.

### `core/motivated-by` — decider's fast-path hint as an edge

Action-Facts may carry a `core/motivated-by` edge to the Goal that
prompted them — the structural fast-path hint, separate from the
richer `MotivationV1` Abstractions below. The edge is registered by
core ([06 §Goals participate in the edge graph](docs/06-goals-and-self.md#goals-participate-in-the-edge-graph)), class `Causal`,
authorship `EventSource(SYSTEM)` for tool-emitted Action-Facts.

Set by the decider when the motivating goal is known at decision
time (programmed rules, explicit selection). Engine does not
enforce. Not a substitute for motivation Abstractions — it is a
low-cost annotation, not the rich account.

Action-Fact payloads do **not** carry a `motivated_by_goal: Option<GoalId>`
field — the relation lives in the edge graph, not buried inside payload
structures. This keeps motivation queryable uniformly across Action-Facts
emitted by any flavor.

### `MotivationV1` — core `AbstractionPayload`

Bare core registers `MotivationV1` so every flavor has a uniform
motivation query surface.

```rust
struct MotivationV1 {
    goal_ids:   Vec<GoalId>,         // goals motivating this action; ≥ 1
    kind:       MotivationKind,
    confidence: f32,                  // [0.0, 1.0]
}

enum MotivationKind {
    Pursuit,        // toward a positive goal
    Avoidance,      // away from an undesired state
    Obligation,     // external commitment (deadline, promise)
    Curiosity,      // exploratory / information-seeking
    Maintenance,    // preserving an existing state
}
```

A motivation Abstraction cites the Action-Fact via the normal F→A
provenance edge (Abstraction → Fact). N such Abstractions citing one
Action express N motivations.

### Per-domain expansion

Flavors register richer schemas when the domain demands more fields
— e.g. a Code flavor's `BugFixMotivationV1` with `severity` and
`regression_risk`. Same registration machinery as any other
`AbstractionPayload` (03/08); core's `MotivationV1` is the default,
not a forcing function.

## Gating — out of scope

Per-tool. v1 tools are deterministic-output: system chooses *content*,
not *whether to act*. Any non-deterministic proposal/approval gating is
the tool's concern.

## Idempotency

`event_id` is the key. Same dedup as any EventSource. No `ActionId`.

## Validation at ingest

Registry lookup on every `SYSTEM` event: payload's tool reference must
match a registered `ToolRegistration`. Unknown tool → reject.

Publication is **synchronous**: the tool callable returns only after
the resulting Fact (and any structural edges from its payload) is
materialized. The agent loop must see its own action as a Fact before
the next decide step.

## Versioning

Tools follow `FactSchema` ([03](docs/03-schema-registry.md)) migration discipline. Schema bump on a
tool is a `SchemaMigration` over its sidecar; old version is dropped
after migration completes. Tool deregistration is migration to a
no-op schema or explicit drop with audit.

## Anchors

- `claim`
- `system-eventsource`
- `tool-registry`
- `deciders-flavor-supplied`
- `dispatcher-emitted-call-facts`
- `execution-boundary-per-tool`
- `effect-on-reality`
- `motivation`
- `core-motivated-by`
- `motivationv1-core-abstractionpayload`
- `per-domain-expansion`
- `gating-out-of-scope`
- `idempotency`
- `validation-at-ingest`
- `versioning`
