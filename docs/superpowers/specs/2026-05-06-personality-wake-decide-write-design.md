# Personality Wake/Decide/Write Architecture

**Status:** Draft
**Date:** 2026-05-06
**Owner:** Heinrich
**Scope:** Substrate (`crates/core`, `crates/storage-pg`), Code flavor (`flavors/code`), wire (`proto/proxima/v1`, TS bindings), Code frontend (`packages/frontend-core`, Tauri shell), and the related numbered docs (`docs/04`, `docs/06`, `docs/08`, `docs/13`).

## Goal

Replace the F2A/A2P operator pair with a single unified architectural model in which Reality Events wake Personalities, the personality runs a multi-turn agent tool-loop with an injected typed context, and the loop emits typed memory + edge writes. The personality is the decider (not a separate operator). Tool calling is the only LLM invocation pattern — JSON-mode parsing is rejected. Personalities are first-class with a `(TypeId, InstanceId)` identity so multiple instances of the same type can coexist (parallel Workers, Engineer-Alice ↔ Engineer-Bob conversations, reasoning chains). v1 ships the full architecture with behavior held minimal — both today's CommitSummary and Engineer collapse into Personalities with a single forced tool; richer multi-turn deliberation, mid-loop read tools, and edge authoring land in v1.1.

## Architecture (summary)

Personalities replace `F2AOperator` and `A2POperator`. Each personality declares a self-Perspective schema, system prompt, tool palette, writeable schemas/relations whitelists, and default wake filters via an extended `PersonalityFlavor` trait. Goals link to a personality's self-Perspective via the `core/inspires` edge — Goals carry direction, NOT operational policy. Wake configuration (operational policy) lives in a dedicated typed core table `personality_wake_config` separate from Goals. Wake filters are a flavor-extensible enum (`OnMemory`, `OnEdge`, plus flavor-defined kinds via `WakeFilterKind` trait registered in `proxima_flavor!`); filter payloads live in a single typed JSONB column at the leaf, validated Rust-side via serde+schemars + an envelope `version: u16` to support post-v1 schema evolution. Self-wake is forbidden by dispatcher invariant, not freeze-time guard — the dispatcher always excludes events authored by the waking instance. Cross-personality A↔B ping-pong is bounded by a `wake_chain_depth` slot on memory rows and a per-personality maximum (default 10). Personalities are addressable as `(personality_type_id, personality_instance_id)` where instance_id is a UUID minted at instantiation (independent of any memory_id); each instance carries a `current_self_perspective_memory_id` pointer that advances on Supersedes-driven self-Perspective evolution. `instantiate_personality` is the substrate verb that mints a new instance.

## Tech Stack

Rust 2021 workspace, `proxima-core`, `proxima-storage-pg` + SQLx, `flavors/code`, `proto/proxima/v1` (gRPC + specta TS bindings), Postgres, schemars for tool schema codegen, **Anthropic SDK with structured tool calling — committed at the contract level for v1**. A provider-abstraction `LlmAdapter` seam is deferred to v1.1+ and called out in Out of Scope; v1's tool-call shape and message protocol are Anthropic-specific.

## Summary

Current state:

- `crates/core/src/operators.rs` — `F2AOperator` and `A2POperator` traits live as separate operator families. `OperatorRegistry` lifecycle just landed (post `flavor-registry-single-source-of-truth.md`); operators are flavor-shipped and registered through `proxima_flavor! { f2a_operators = [...], a2p_operators = [...] }`.
- `crates/core/src/personality.rs` — `PersonalityFlavor` trait exposes `personality_id()` + `snapshot(ctx) -> PersonalitySnapshot`. The snapshot today is `(personality_id, captured_at)` only (post `drop-personality-state-hash.md`).
- `crates/core/src/engine/operators.rs::run_pending_a2p` — pulls personalities from registry, runs each registered A2P operator against Abstraction heads. Personality is identity-only; the operator is the decider.
- `apps/proxima-shell/src-tauri/src/commands/repo_ingest.rs` and `apps/proxima-code/src/main.rs` — call `run_pending_f2a` / `run_pending_a2p` imperatively after each ingest poll.
- `flavors/code/src/operators/` — `CommitSummaryOperator` (F2A) and `CodeDevelopmentPerspectiveOperator` (A2P) are hand-coded LLM pipelines that read inputs, build prompts, parse JSON output, write Abstractions/Perspectives. Tool calling is not used; output parsing is fragile.
- `crates/core/src/storage.rs` — Storage trait method signatures carry `personality_id` as a single field on memory rows. No multi-instance shape.
- `proxima_core.memories` and downstream tables — `personality_id text NOT NULL` slot.
- No `personality_wake_config`, no `personality_wake_invocations`, no `core/inspires` relation.

Target outcome:

- `PersonalityFlavor` is the unit of declaration. Each personality is a Rust type registered through `proxima_flavor! { personalities = [...] }`. `F2AOperator` and `A2POperator` traits are removed.
- Personalities carry: self-Perspective schema, default self-payload, system prompt, tool palette (read + write), writeable_schemas + writeable_relations whitelists, default wake filters.
- `proxima_core.personality_wake_config(owner, type_id, instance_id, current_self_perspective_memory_id, wake_filters JSONB, status)` carries operational policy, separate from Goals. `status` enum: `active | needs_repair` (set when a stored wake_filter fails strict deserialization after a schema migration — surfaced via repair queue to the frontend).
- `proxima_core.personality_wake_cursor(owner, type_id, instance_id, last_considered_seq)` — advanced after every dispatch tick regardless of match outcome (so probabilistic personalities at low rates don't re-walk the entire change_event stream every tick).
- `proxima_core.personality_wake_invocations` provides idempotency keyed by `(owner, type_id, instance_id, change_event_seq)` for actually-fired wakes only.
- Memory rows carry `personality_type_id` + `personality_instance_id` slots (split from today's single `personality_id`) plus a new `wake_chain_depth: smallint` slot (external Facts = 0; agent writes inherit `max(read_event.depth) + 1`; dispatcher refuses wakes when triggering event's depth ≥ `MAX_WAKE_CHAIN_DEPTH`).
- New core relation `core/inspires(source: any, target: any-perspective-with-is_self_schema)`.
- New verbs: `provision_owner`, `instantiate_personality`, `set_wake_config`.
- Dispatcher walks `change_event` from `personality_wake_cursor.last_considered_seq + 1` → evaluates wake filters with self-exclusion invariant + chain-depth bound → idempotency-checks against `personality_wake_invocations` → runs an agent tool-loop with stop conditions → advances cursor regardless of match.
- Substrate ships a default tool pack (5 read + 4 write tools as `PersonalityTool` impls); flavors add specialized tools.
- Code's CommitSummary and Engineer migrate to Personalities. Code frontend lists Engineer instances, supports multi-instance creation, supports per-instance wake-config edit.

## Architectural Model

### The wake/decide/write loop

```
Reality Event (change_event row)
    → Dispatcher reads personality_wake_config rows for the event's owner
    → For each row, evaluate WakeFilter list
        - dispatcher self-exclusion: skip if event.authoring_(type_id, instance_id) == row.(type_id, instance_id)
        - if filter matches AND probability check passes, enqueue wake invocation (type_id, instance_id, change_event.seq)
    → Idempotency check against personality_wake_invocations
    → Agent tool-loop:
        - Build context (system prompt + injected slots + tool palette)
        - LLM call → tool calls
        - Substrate validates each tool call (palette / writeable_schemas / writeable_relations)
        - Substrate invokes tool implementations → results fed back
        - Loop until LLM stops calling tools, or stop conditions trip
    → Tool calls produce typed memory + edge writes
    → Those writes generate new change_event rows on the stream
    → Cycle
```

### Identity: (TypeId, InstanceId)

- `personality_type_id: &'static str` — type-level, static. Resolves tool palette, system prompt, self-schema, default-wake-filter template at compile time from the `PersonalityFlavor` Rust type.
- `personality_instance_id: Uuid` — per-instance UUID, **minted at `instantiate_personality` time, independent of any memory_id**. Stable across self-Perspective evolution: superseding a self-Perspective advances the instance's `current_self_perspective_memory_id` pointer (column on `personality_wake_config`) without changing `instance_id`. Memory rows authored by the instance carry this UUID in their `personality_instance_id` slot — those rows survive lineage transitions intact.
- Earlier-draft equation `instance_id = self-Perspective memory_id` is rejected — that shape doesn't compose with Supersedes-driven self-Perspective evolution (each successor would mint a new instance_id, orphaning all wake_config rows and historical memory rows). Instance UUID + pointer column resolves the discontinuity.
- `personality_wake_config` keyed on `(owner, type_id, instance_id)`.
- Single-instance is the trivial case (one row per type per owner, default-instantiated at owner provisioning). Multi-instance enables parallel Workers, Engineer ↔ Engineer conversations, reasoning chains.

### Loop bounding: wake_chain_depth

A↔B ping-pong (Engineer-Alice authoring a Perspective, Engineer-Bob countering, Alice countering back, ...) is structurally bounded — not by per-wake budgets (those cap individual LLM calls but not the chain) but by a `wake_chain_depth: u16` slot on every memory row.

- External Facts ingested from event sources have `wake_chain_depth = 0`.
- Memories authored during a wake inherit `wake_chain_depth = max(triggering_event.wake_chain_depth, max(memory_id.wake_chain_depth for each memory the personality READ this wake)) + 1`.
- Dispatcher refuses to fire a wake when the triggering event's `wake_chain_depth >= personality.max_wake_chain_depth()`. Default `MAX_WAKE_CHAIN_DEPTH = 10`; per-personality override via `PersonalityFlavor::max_wake_chain_depth() -> u16`.
- Logged when the bound trips; not an error — chains naturally terminate when external entropy runs out (no new commit-fact = no new wake), the depth cap exists to bound pathological loops.
- This is local data, not a graph walk. Survives crash/restart, can't be circumvented by Provenance manipulation (Provenance is auto-wired by substrate, not LLM-authored).

### Wake filter (flavor-extensible)

```rust
enum WakeFilter {
    OnMemory { schema_id: SchemaId, authored_by: AuthorFilter, probability: f32 },
    OnEdge   { relation_id: RelationId, target: WakeTarget, probability: f32 },
    Custom   { kind_id: String, params: serde_json::Value, probability: f32 },
}
enum AuthorFilter { Specific(PersonalityRef), Any }
enum PersonalityRef {
    Type(PersonalityTypeId),
    Instance(PersonalityTypeId, MemoryId),
}
enum WakeTarget { SelfPerspective, Memory(MemoryId) }
```

Storage shape: a single typed JSONB column on `personality_wake_config`. Each filter is wrapped in an envelope with a version field: `{ "version": 1, "kind": "core/on-memory" | "core/on-edge" | "<flavor>/<kind>", ...params }`. Validated Rust-side at write-time via serde + schemars; never composed across flavors in one row, never queried by internal field. Bends the strict-typing principle at one leaf, justified because dispatcher is the only reader.

**Versioning + repair flow.** Each kind owns its `params` schema and a version number. When the dispatcher loads a wake_config row whose filters fail strict deserialization (e.g. an old shape after a schema migration), it skips evaluation, marks the row's `status = needs_repair`, logs a warning, and surfaces the offender in a repair queue read by the frontend. The user re-edits the affected wake config; on save, status returns to `active`. Pre-v1 fresh DB ships everything at version=1.

Flavor extension: `proxima_flavor! { wake_filter_kinds = [MyCustomFilterKind] }`. Each kind implements:

```rust
#[async_trait]
trait WakeFilterKind: Send + Sync + 'static {
    fn kind_id(&self) -> &'static str;
    fn params_schema(&self) -> &'static schemars::Schema;
    fn version(&self) -> u16;
    async fn matches(
        &self,
        params: &serde_json::Value,
        event: &ChangeEvent,
        ctx: &dyn WakeFilterCtx,
    ) -> Result<bool, ProtocolError>;
}

trait WakeFilterCtx: Send + Sync {
    async fn fetch_memory(&self, id: MemoryId) -> Result<Option<MemoryRow>, ProtocolError>;
    async fn current_self_perspective(
        &self,
        owner: &Owner,
        type_id: PersonalityTypeId,
        instance_id: PersonalityInstanceId,
    ) -> Result<Option<MemoryId>, ProtocolError>;
    // …other read primitives the dispatcher already needs
}
```

The async + storage-context shape is required because filters with `target: SelfPerspective` need to resolve "what is the current self-Perspective for this instance" against storage. Core's `OnMemory` and `OnEdge` are evaluated through the same `WakeFilterCtx` for parity; Custom variants get the same surface so flavors can express non-trivial filters without core help.

### Self-Perspective

Each instance has exactly one *current* self-Perspective per owner, addressed by the `current_self_perspective_memory_id` pointer on the wake_config row. Owner-scoped. Immutable in v1; evolution path post-v1 = author a new self-Perspective + Supersedes edge from the prior head + advance the pointer. Instance UUID is unaffected.

- Each personality declares its self-schema via `PersonalityFlavor::self_schema()`.
- Self-schemas are flagged `is_self_schema` in the registry.
- `instantiate_personality(owner, type_id, payload_overrides?)` mints a new instance UUID, writes a fresh self-Perspective row via `derive_append`, writes a `personality_wake_config` row whose `current_self_perspective_memory_id` points at that new self-Perspective, and writes a `personality_wake_cursor` row at `last_considered_seq = current MAX(change_event.seq) for this owner` (so the new instance starts at "now" rather than walking the entire historical event stream on its first dispatch tick). Returns the InstanceId.
- Owner provisioning may default-instantiate one instance per registered type for v1 ergonomics. Idempotent (re-running provisioning does not create duplicate instances).
- `list_self_perspectives(owner)` is a substrate-default read tool — returns each instance's current self-Perspective across all flavors, enabling cross-personality discovery (Visionary picks an Engineer based on self-Perspective payload content).
- Cross-personality discovery resolves "current" not "all historical" — Supersedes-superseded self-Perspectives don't appear in the discovery surface.

### Goals link to self-Perspectives via `core/inspires`

- Goals carry direction (the four-pillar payload). They do NOT carry wake config.
- A Goal becomes "direction for personality P" by being edge-linked to P's self-Perspective via `core/inspires(goal_memory, perspective_memory)`.
- The targeted personality wakes on the edge-creation event via the auto-added `OnEdge { relation: core/inspires, target: SelfPerspective, probability: 1.0 }` filter.
- The personality is *truly free* — wake does not imply obligation. Personality reads the linked Goal in context, weighs against its self-Perspective + active Goal lineage, freely accepts/ignores/counters.
- Cross-personality coordination: Personality A authors a Goal + edge to Personality B's self-Perspective. B wakes; B decides.

### Self-wake forbidden — dispatcher invariant

The dispatcher always filters out events whose `(authoring_type_id, authoring_instance_id) == waking instance`. No filter language can opt back in. This is structural, not freeze-time-checked. Self-iteration as deliberation lives inside one wake (multi-turn agent tool-loop), not as self-wake across events.

## Declaration Surface

### Extended `PersonalityFlavor` trait

```rust
trait PersonalityFlavor: Send + Sync + 'static {
    fn personality_type_id(&self) -> &'static str;
    fn self_schema(&self) -> SchemaId;
    fn default_self_payload(&self, owner: &Owner) -> NewPerspective;
    fn system_prompt(&self) -> &'static str;
    fn tools(&self) -> &'static [Arc<dyn PersonalityTool>];
    fn writeable_schemas(&self) -> &'static [&'static str];
    fn writeable_relations(&self) -> &'static [&'static str];
    fn default_wake_filters(&self) -> Vec<WakeFilter>;
    fn tier(&self) -> ModelTier { ModelTier::Smart }                  // default
    fn max_wake_chain_depth(&self) -> u16 { 10 }                       // default; loop bound
}
```

Provenance and Supersedes are NEVER in `writeable_relations()` — system-authored only.

### Freeze-time guards (enforced in `FlavorRegistry::freeze()`)

For every registered personality `P`:

1. `P.self_schema() ∉ P.writeable_schemas()` — a personality cannot author its own self via the tool palette; self-Perspective writes go through `instantiate_personality` and the (deferred) self-evolution verb only. Panic on violation, name `P` and the offending schema.
2. `{core/provenance, core/supersedes} ∩ P.writeable_relations() = ∅` — the auto-wired relations are NEVER personality-authorable. Panic on violation.
3. Every `schema_id` in `P.writeable_schemas()` is registered in the frozen schema registry. Panic if not.
4. Every `relation_id` in `P.writeable_relations()` is registered in the frozen relation registry. Panic if not.

These guards mirror the freeze-time pattern from the prior `flavor-registry-single-source-of-truth` plan; they catch flavor misconfigurations at build time rather than first-failed-write at runtime.

### `proxima_flavor!` macro extension

```rust
proxima_flavor! {
    name = "proxima-code",
    fact_schemas = [...],
    abstraction_schemas = [...],
    perspective_schemas = [...],
    relations = [...],
    personalities = [
        personality::CommitSummaryPersonality,
        personality::CodeEngineerPersonality,
    ],
    wake_filter_kinds = [],   // core's OnMemory/OnEdge auto-registered
    mcp_tools = [...],
}
```

`f2a_operators` / `a2p_operators` macro fields are removed.

## Tool Palette + Authorization

### `PersonalityTool` trait

```rust
trait PersonalityTool: Send + Sync + 'static {
    fn tool_id(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn args_schema(&self) -> &'static schemars::Schema;
    async fn invoke(
        &self,
        ctx: &PersonalityToolContext<'_>,  // engine + owner + (type_id, instance_id) + wake event
        args: serde_json::Value,
    ) -> Result<PersonalityToolResult, ProtocolError>;
}
```

### Substrate-shipped tool pack (auto-included in every palette)

Read:
- `list_self_perspectives(owner)` — all registered self-Perspectives across flavors (cross-personality discovery).
- `fetch_memory(memory_id)` — typed retrieval.
- `walk_lineage(memory_id, direction, depth)` — follows Provenance/Supersedes.
- `search_by_embedding(query, k, schema_filter?)` — RAG retrieval.
- `list_active_goals(owner, scope: linked_to_self | owner_wide)` — Goals filtered.

Write:
- `emit_perspective(schema_id, typed_payload)`
- `emit_abstraction(schema_id, typed_payload)`
- `emit_goal(payload, inspires_target_self_perspective?, motivated_by?)`
- `create_edge(source_memory_id, relation_id, target_memory_id)` — gated by `writeable_relations`; never accepts `core/provenance` or `core/supersedes`.

**Provenance is fully substrate-authored.** Personalities never pass `provenance_memory_ids` — that contradicts the auto-wiring story. The tool runtime tracks read-side-effects per wake (every `fetch_memory`, `walk_lineage`, `search_by_embedding`, `list_active_goals`, `list_self_perspectives` call records the memory_ids returned), and `emit_*` tools auto-wire Provenance edges from the union of `{triggering_event_memory_id} ∪ {memory_ids the personality read this wake}`. The personality cannot narrow or override this set in v1. If a personality wants to surface a *specific* causal connection beyond auto-wiring, that's an explicit `core/cites` (or flavor-specific) relation authored via `create_edge`, gated by `writeable_relations` — distinct from auto-wired Provenance, deferred to v1.1+ when the use case lands. Supersedes edges are also substrate-authored, wired automatically when an `emit_*` call's typed payload identifies a prior head (via the registered Supersedes resolver).

The `wake_chain_depth` slot on each emitted memory is computed by the substrate from the same read-side-effect log (see Loop bounding above) — `max(triggering_event.depth, max(read.depth)) + 1`. Personalities cannot reset or lie about depth.

### Three-layer authorization (substrate-side, before any pg write)

1. **Tool palette**: `tool_id ∈ substrate_pack ∪ personality.tools()` → reject otherwise.
2. **Writeable schemas**: For `emit_*`, `schema_id ∈ personality.writeable_schemas()` → reject. Self-schema NEVER in this set.
3. **Writeable relations**: For `create_edge`, `relation_id ∈ personality.writeable_relations()` → reject. Provenance and Supersedes never appear here.

LLMs can hallucinate any tool call; the substrate is the authorization bottleneck. None of these checks are LLM-trustable.

## Decider Loop

### Context-builder (substrate-shipped, one shape for all personalities)

When personality `P[type=T, inst=I]` wakes on `change_event(seq=N)` for owner `O`:

```
system_prompt = T.system_prompt()

context = {
    triggering_event:  typed change_event row,
    self_perspective:  <my self-Perspective for O>,
    active_goals:      list_active_goals(owner=O, scope=linked_to_self),
    recent_lineage:    last K Perspectives I authored (typed payloads + memory_ids),
    related_memories:  embedding search top-L (over schemas in my writeable + consumed set),
}

tools = substrate_pack ∪ T.tools()
```

Defaults: K=5, L=10. Configurable per personality via additional trait methods with default impls. Flavor-specialized retrieval (e.g., "last 3 commits touching files in this perspective's provenance") doesn't bloat the context-builder — the personality calls flavor read-tools mid-loop in v1.1+.

### Agent-turn flow

1. Substrate builds context + tool schemas.
2. LLM call (Anthropic SDK with structured tool calling) — system prompt + context + tool palette; turn-1 input = triggering event description.
3. LLM responds with tool calls (or no tool calls = natural end).
4. Substrate validates each call against three-layer authorization.
5. Substrate invokes tool implementations, collects results.
6. Results fed as next turn's input.
7. Loop until: (a) LLM stops calling tools, (b) max_turns hit, (c) cost budget exceeded, (d) wall-clock deadline.

Wakes serialize per `(owner, type_id, instance_id)` via advisory lock — no overlapping wakes for the same instance. Mirrors the per-lineage A2P lock.

### Stop conditions (substrate defaults; per-personality override later)

- `max_turns: 5`
- `max_cost_usd: 0.10` per wake
- `max_wall_clock_secs: 60`

If a stop condition trips, wake recorded as `truncated`; partial output stays committed (real Reality Events with proper provenance — no rollback). Tool errors fed back to LLM as tool error results (not fatal). LLM-level errors (rate limit, network) mark wake `failed`; no auto-retry — next change_event triggers a fresh wake.

### Idempotency

`proxima_core.personality_wake_invocations(owner_…, type_id, instance_id, change_event_seq, status, started_at, finished_at, turn_count, cost_usd)` with `UNIQUE(owner, type_id, instance_id, change_event_seq)`. Dispatcher checks before launching a wake — replay-safe across engine restarts and crash recovery.

### Catch-up cursor (separate from idempotency)

Two tables, two jobs:

- `personality_wake_cursor(owner_…, type_id, instance_id, last_considered_seq, updated_at)` — the dispatcher's resumable position. **Advanced after every dispatch tick regardless of match outcome**, so probabilistic personalities at low rates (Visionary at 0.001) don't re-walk the entire change_event stream every tick. Per `(owner, type_id, instance_id)` row.
- `personality_wake_invocations(owner_…, type_id, instance_id, change_event_seq, status, …)` UNIQUE on the four-tuple — actually-fired wakes only, used for idempotency.

Dispatcher tick per instance:
1. Read `last_considered_seq` from cursor. The cursor row is written by `instantiate_personality` at `last_considered_seq = MAX(change_event.seq) for this owner` at instantiation time — new instances start at "now," not at seq 0.
2. Iterate `change_event` rows where `seq > last_considered_seq AND owner = …` in seq order.
3. For each, run the wake-filter pipeline. If a wake fires, write to `personality_wake_invocations` (or fail idempotency check if already there — replay-safe).
4. Advance cursor to the highest `seq` considered, regardless of whether any filter matched.

Engine offline → events accumulate → next tick walks unhandled events from the cursor forward. Same end-to-end behavior as today's `run_pending_a2p`, but the cursor is decoupled from the wake-fired marker — fixes the "low-probability instance never advances cursor" bug.

### Wake queue safety net

Per-instance max-in-flight wake queue depth = 10 (substrate constant). On overflow, the dispatcher drops the oldest queued wake, advances the cursor past its `change_event_seq`, logs a warning. This is a v1 safety net — bursty events for one instance + 60s LLM wakes can pile up otherwise. Formal multi-event coalescing (single LLM call seeing `[N, M, …]` events at once) is a v1.1 feature; v1 just guarantees bounded memory under burst.

## Probabilistic Wake

`probability: f32` (0.0..=1.0, default 1.0) on each `WakeFilter`. Visionary subscribes to most schemas at 0.001 but pins assigned-Goal triggers at 1.0. Probability source: stable hash of `(change_event.seq, type_id, instance_id, filter_index)` → uniform float, so wake decisions are deterministic per event. No `random()` in the dispatcher.

**Determinism caveat.** Wake-decision determinism is *conditional on the change_event stream*. The stream itself depends on prior wakes' tool calls, which depend on LLM nondeterminism — so full system replay is NOT deterministic, only "given the same change_event stream, the same wake set fires." Don't use this for replay-based testing strategies that assume identical event streams between runs.

## Decisions

- **Personality replaces Operator entirely.** F→A and A→P collapse into one shape. F2AOperator and A2POperator traits are deleted; today's CommitSummary and Engineer migrate to Personalities. Justification: closed-loop story, runtime-configurability, agent-loop semantics, marketplace shape — all served by one model.
- **Tool calling, never JSON parsing.** All LLM agent invocations use Anthropic structured tool calling. Tool schemas codegen'd from typed payloads via schemars. Rejecting JSON parsing aligns with strict-typing principle and avoids parser fragility.
- **Goals and wake config are separate surfaces.** Goals = direction (four-pillar memory); wake config = operational policy (typed core table). Conflating them stretches the ontology incorrectly. Goals authored by personalities flow through wake filters as Reality Events.
- **Self-wake forbidden by dispatcher invariant.** Not a freeze-time check. Cleaner with multi-instance (per-instance self-exclusion is structural, not configurable). Self-iteration lives inside one wake's multi-turn loop.
- **Personality identity is `(TypeId, InstanceId)`.** TypeId resolves compile-time config; InstanceId is a UUID minted at `instantiate_personality` time, decoupled from any memory_id (see the dedicated "Instance UUID separate from self-Perspective memory_id" decision below for the rationale). Multiple instances per type per owner are first-class; enables parallel Workers + cross-Engineer conversations.
- **Self-Perspective is the personality identity anchor.** Goal-to-personality assignment via edge to self-Perspective. No fifth pillar, no new substrate slot. Personality is truly free to interpret linked Goals.
- **Wake filter is a flavor-extensible enum with typed JSONB at the leaf.** Single column, dispatcher-only reads, validated Rust-side via serde+schemars. Extension via `WakeFilterKind` trait registered through `proxima_flavor!`. Bends strict-typing at one leaf — accepted because the leaf is dispatcher-internal (never queried by internal field, never composed across flavors).
- **Three-layer substrate authorization.** Tool palette / writeable_schemas / writeable_relations whitelists. Substrate is the bottleneck; LLM cannot author outside its lane regardless of hallucinated tool calls.
- **Substrate ships a default tool pack.** Cross-flavor read primitives (self-Perspective discovery, lineage walk, embedding search) are universal. Specialized read tools and all write tools are per-personality flavor declarations.
- **v1 holds Engineer's palette to one forced tool.** `emit_perspective` only — preserves today's effective behavior while landing the architecture. Multi-turn deliberation, mid-loop read tools, edge authoring, speech-act tools (reply firmly, counter-perspective, propose abstraction) are v1.1+.
- **Cross-personality loops bounded by chain depth, not provenance graph walks.** A↔B ping-pong is structurally bounded by `wake_chain_depth` on memory rows + a per-personality cap (default 10). Local data, no graph traversal at dispatch time. Captures the "two Engineers ping-ponging forever" pathology cleanly without per-(owner, lineage) budgets. Provenance walks for cycle detection were rejected as both more expensive and less reliable (LLMs can't reach Provenance edges anyway since they're auto-wired, but the local-data approach is uniformly faster).
- **Cursor split from idempotency table.** Wake firing and event consideration are separately tracked. Without this split, low-probability personalities (Visionary @ 0.001) would re-walk the full change_event stream every dispatch tick because their `personality_wake_invocations` table never advances. Two-table design: `personality_wake_cursor` (advances regardless of match), `personality_wake_invocations` (advances only on fired wakes, used for idempotency).
- **Instance UUID separate from self-Perspective memory_id.** The earlier draft equated them, which doesn't compose with Supersedes-driven self-Perspective evolution (each successor would mint a new instance_id, orphaning historical state). Instance UUID is minted at instantiation and stable; the wake_config row carries a `current_self_perspective_memory_id` pointer that advances on evolution.
- **Provenance is fully substrate-authored from read-side-effect tracking.** `emit_*` tool signatures do NOT take a `provenance_memory_ids` parameter — the substrate auto-wires Provenance from `{triggering_event} ∪ {memory_ids returned by read-tools called this wake}`. If a personality wants to surface a *specific* causal claim beyond auto-wiring, that's a `core/cites` relation gated by `writeable_relations` (v1.1+, distinct from auto-wired Provenance). **Known limitation:** auto-wiring from *all* read-tool returns is a conservative over-approximation — `search_by_embedding(k=10)` returns 10 memories that are similarity-matched, not necessarily causally consulted, but they all become Provenance. Lineage walks will be wider/noisier than strict causality. v1.1 refinement options (refine in a follow-up spec): (a) embedding-search results route through a separate `core/recalled` relation rather than Provenance, or (b) personalities must explicitly `cite_memory(id)` to escalate a read into Provenance. Tracked as v1.1 follow-up, not blocking v1.
- **WakeFilterKind matches is async + storage-aware.** `OnEdge { target: SelfPerspective }` and any flavor-defined Custom filter need to read storage to resolve "current self-Perspective for this instance" or similar. Trait takes `&dyn WakeFilterCtx` exposing read primitives. Sync/pure was rejected as too restrictive.
- **Wake-filter envelope carries `version: u16`.** Strict-deserialize on read; failed rows mark the wake_config row `needs_repair` and surface in a frontend repair queue. v1 ships everything at version=1; supports post-v1 schema evolution without silent corruption.

## v1 Scope

### Substrate (`crates/core`, `crates/storage-pg`)

1. Storage migration: split `personality_id` → `personality_type_id` + `personality_instance_id` across `memories`, `source_batch_f2a`, `a2p_invocations`, change_event mirrors. Add `wake_chain_depth: smallint NOT NULL DEFAULT 0` slot on memory rows (and the change_event mirror) — substrate-computed at write time, never user-provided. Pre-v1, fresh-DB migration, no backfill.
2. New tables:
   - `personality_wake_config(owner_…, type_id, instance_id, current_self_perspective_memory_id, wake_filters JSONB, status)` — operational policy + instance pointer.
   - `personality_wake_cursor(owner_…, type_id, instance_id, last_considered_seq, updated_at)` — advances every dispatch tick regardless of match.
   - `personality_wake_invocations(owner_…, type_id, instance_id, change_event_seq, status, …)` — UNIQUE on the four-tuple, idempotency for fired wakes.
3. New core relation `core/inspires(source: any-memory, target: any-perspective WHERE is_self_schema)`.
4. `PersonalityFlavor` trait extension: `personality_type_id` (replaces `personality_id`), `self_schema`, `default_self_payload`, `system_prompt`, `tools`, `writeable_schemas`, `writeable_relations`, `default_wake_filters`, `tier`, `max_wake_chain_depth`. Today's `snapshot()` method is dropped (no longer needed — identity is captured by type_id + instance_id directly). `FlavorRegistry::freeze()` enforces the four guard clauses: self_schema ∉ writeable_schemas; {core/provenance, core/supersedes} ∉ writeable_relations; every writeable_schema is registered; every writeable_relation is registered.
5. New traits: `PersonalityTool`, `WakeFilterKind`. Core handles `OnMemory` and `OnEdge` directly in the dispatcher; flavor-registered `WakeFilterKind` implementations evaluate `Custom` variants.
6. Substrate tool pack — 5 read + 4 write tools as `PersonalityTool` impls.
7. Dispatcher: walk `change_event` from `personality_wake_cursor.last_considered_seq + 1` → wake filter evaluation (with self-exclusion invariant + chain-depth bound) → idempotency check against `personality_wake_invocations` → agent tool-loop with stop conditions and read-side-effect tracking → cursor advance regardless of match. Per-instance max-in-flight queue depth = 10; overflow drops oldest with cursor-advance + warn-log.
8. Verbs: `provision_owner`, `instantiate_personality`, `set_wake_config`.
9. `proxima_flavor!` macro: add `wake_filter_kinds = [...]` field. Remove `f2a_operators` / `a2p_operators` fields.
10. Delete `F2AOperator` / `A2POperator` traits and any remaining `OperatorRegistry` storage.

### Code flavor (`flavors/code`)

11. `CommitSummaryOperator` → `CommitSummaryPersonality`. Self-schema `code/commit-summarizer-self-v1`. Tool palette = substrate pack (no flavor-specialized tools). Effective writable surface = `emit_abstraction` restricted to `code/commit-summary-v1` via `writeable_schemas`. System prompt = today's CommitSummary prompt, single-turn instructed. Default wake filter = `OnMemory { schema: code/commit-fact-v1, authored_by: Any, probability: 1.0 }`. Writeable schemas = `["code/commit-summary-v1"]`. Writeable relations = `[]`. Tier = `Cheap`.
12. `CodeDevelopmentPerspectiveOperator` → `CodeEngineerPersonality`. Self-schema `code/engineer-self-v1` (typed payload: `display_name: String`, `purpose: String`). Tool palette = substrate pack only. Effective writable surface = `emit_perspective` restricted to `code/development-perspective-v1` via `writeable_schemas`. System prompt = today's A2P prompt unchanged, single-turn instructed. Default wake filter = `OnMemory { schema: code/commit-summary-v1, authored_by: Any, probability: 1.0 }` + auto-added `OnEdge { core/inspires → SelfPerspective, probability: 1.0 }`. Writeable schemas = `["code/development-perspective-v1"]`. Writeable relations = `[]`. Tier = `Smart`.
13. Owner-provisioning default-instantiates one CommitSummarizer + one Engineer per new owner.

### Frontend (`packages/frontend-core`, Tauri shell)

14. Engineer instances list view (display_name from self-payload).
15. "Create another Engineer" button → `instantiate_personality` verb.
16. Per-instance wake-config editor → `set_wake_config` verb.

### Wire (gRPC, codegen)

17. Protos: split `personality_id` → `personality_type_id` + `personality_instance_id`. Add messages for `provision_owner`, `instantiate_personality`, `set_wake_config`. Regenerate TS bindings via specta.

### Docs (`docs/`)

18. `docs/04-consolidation.md`: replace F→A / A→P framing with wake/decide/write loop.
19. `docs/06-goals-and-self.md`: describe self-Perspective, Goal-to-self linking via `core/inspires`, "truly free" personality framing.
20. `docs/08-core-and-flavors.md`: personality declaration surface (extended trait, macro shape).
21. `docs/13-flavor-marketplace.md`: personality + tool palette as marketplace unit.
22. New section (or expand existing) — canonical wake/decide/write reference, dispatcher contract, multi-instance shape.

### Acceptance criteria

- `proxima_flavor!` accepts `personalities = [...]` and `wake_filter_kinds = [...]`; consumers compile.
- `provision_owner(owner)` is idempotent.
- `instantiate_personality(owner, type_id, ...)` produces distinct instances per call; two Engineers for one owner have distinct instance UUIDs, distinct wake_config rows, and distinct self-Perspectives. Memory rows authored by either instance carry the correct `personality_instance_id` slot.
- Dispatcher self-exclusion invariant: instance authoring a memory does NOT trigger its own wake. New PG integration test pins this.
- Dispatcher chain-depth bound: a chain `Fact → Engineer-A Perspective → Engineer-B Counter → Engineer-A Counter → ...` terminates at depth `MAX_WAKE_CHAIN_DEPTH`. New PG integration test pins this with depth=3 to keep test fast.
- `personality_wake_cursor` advances when no filter matches: a Visionary instance with `probability=0.001` whose filters never fire has `last_considered_seq` equal to the latest change_event seq after one dispatch tick. New PG integration test pins this.
- `personality_wake_cursor` initialization at "now": `instantiate_personality` against an owner with prior change_event history writes a cursor row at `last_considered_seq = MAX(change_event.seq)`; the new instance does NOT wake on historical events on its first dispatch tick. New PG integration test pins this.
- `FlavorRegistry::freeze()` panics for each of the four guard violations: (a) `writeable_schemas` containing `self_schema`, (b) `writeable_relations` containing `core/provenance` or `core/supersedes`, (c) `writeable_schemas` referencing an unregistered schema, (d) `writeable_relations` referencing an unregistered relation. One unit test per guard, pinning the panic message.
- Wake-filter version repair round-trip: a wake_config row whose JSONB filters fail strict deserialization has `status = needs_repair` after the next dispatch tick and the dispatcher does not fire wakes for it; after `set_wake_config` rewrites the row with valid filters, `status = active` and wakes resume. New PG integration test pins both halves.
- Queue overflow drop-oldest: enqueueing wake #11 for one instance (queue cap 10) drops the oldest queued wake, advances `personality_wake_cursor` past its `change_event_seq`, and emits the warning log line; the dropped wake is NOT recorded in `personality_wake_invocations`. New PG integration test pins this.
- Provenance auto-wiring: `emit_perspective` called within a wake produces a Perspective whose Provenance edges point at `{triggering_event} ∪ {memory_ids returned by every read-tool the personality called this wake}`. New PG integration test pins this.
- `cargo check --workspace`, `cargo test -p proxima-core`, `-p proxima-storage-pg`, `-p proxima-code` all pass.
- `pnpm typecheck` (regenerated bindings) passes.
- Code frontend lists Engineer instances and supports wake-config edit + multi-instance create end-to-end.
- F2AOperator / A2POperator traits no longer exist in the codebase.

## Out of Scope (deferred, captured here for v1.1+ planning)

- **Real decider loop for Engineer.** Read tools mid-turn (`code_search_chunks`, `open_file_revision`, `walk_lineage` etc.), multi-turn deliberation, edge authoring (writeable_relations populated), speech-act tools (reply firmly, counter-perspective, propose abstraction, agree, question). v1 ships substrate pack only with Engineer's writeable schemas restricted to one Perspective; v1.1 expands palette and gives Engineer real tool-calling agency. **Conversational richness** (Engineer-Alice ↔ Engineer-Bob debates with multiple speech-act tools per turn) lands here.
- **Per-owner system prompt overrides.** Today's flavor-default `&'static str`; v1.1 adds a `personality_settings` table (separate from wake_config — operational config but distinct concern) with override path.
- **Listen/Notify-driven dispatch.** v1 uses cursor catch-up. v1.1 adds `LISTEN` on `proxima_change_event` for sub-second wake latency.
- **Self-Perspective evolution.** Immutable in v1. Successor self-Perspectives via Supersedes lands when "personality state evolves" becomes a real product story.
- **Additional Code-flavor personalities.** Visionary, Worker, Tester chain — additive, no rework needed (per multi-instance design).
- **Cross-flavor wake-config editing in non-Code frontends.** Code is the demo; other flavors may stay hard-coded.
- **Per-personality tier override surface.** Engineer gets a tier slot (defaults to `Smart`); user-side override per personality (e.g., "use cheap model for casual wakes") is v1.1.
- **Multi-process / distributed dispatcher.** Far future. Single-engine cursor walk is v1's contract.
- **LLM provider abstraction.** v1 commits to Anthropic SDK structured tool calling at the contract level. A provider-abstraction `LlmAdapter` seam (so we can swap to other providers, on-prem inference, etc.) is deferred to v1.1+ when multi-provider becomes a real product requirement. v1's tool-call shape, message protocol, and turn structure are Anthropic-specific.
- **Formal multi-event wake coalescing.** v1 ships a queue safety net (drop-oldest at depth 10). True coalescing — where bursty events for the same instance feed into a single multi-event-aware LLM call rather than serial wakes — is v1.1.
- **Wake-config schema migration framework.** v1 has the version field + repair-queue mechanism. A flavor-side "migrate v1 → v2 in place" tool (vs. surface-and-reauth) is v1.1.
- **Provenance fan-out refinement.** v1 auto-wires Provenance from every read-tool return (including `search_by_embedding` similarity results). This is a conservative over-approximation — Provenance edges include "memories within personality's read context" alongside "memories causally consulted." Lineage walks will be wider than strict causality demands. v1.1 explores either (a) routing embedding-search results through a separate `core/recalled` relation distinct from Provenance, or (b) requiring an explicit `cite_memory(id)` opt-in to promote a read into Provenance. Tracked here so a future reader sees this was acknowledged, not overlooked.

## v1.1+ Implications (informational, not in this plan's scope)

- Engineer's full decider palette includes:
  - Read: `code_search_chunks`, `open_file_revision`, `code_search_commits`, `walk_lineage` (deeper).
  - Write: `reply_firmly(target_perspective, content)`, `counter_perspective(target, alternative)`, `propose_abstraction(observation)`, `agree_with(target)`, `ask_question(target, question)` — composite tools that bundle `emit_perspective` + `create_edge` for specific speech-act relations like `code/replies-to`, `code/counters`, `code/agrees-with`, `code/asks`.
- Worker scale-out: instantiate 5 Workers, each with its own wake_config keyed to a task-shard; dispatcher already supports it via multi-instance.
- Visionary as a pluggable flavor that ships its own personality type registering a probabilistic wake filter (e.g., 0.001 on most events) and a `propose_direction` tool that authors Goals + `core/inspires` edges.

## Docs Alignment

- `AGENTS.md invariant 7` (build-time registration only): personalities, tools, wake-filter kinds all register through `proxima_flavor!` exclusively. No runtime registration of code.
- `AGENTS.md invariant 8` (no feature flags for inclusion): the dispatcher self-exclusion invariant is structural, not configurable. Wake-config errors fail at write time.
- `docs/04-consolidation.md`: rewrites the consolidation framing.
- `docs/06-goals-and-self.md`: introduces the self-Perspective + `core/inspires` model.
- `docs/08-core-and-flavors.md`: declaration surface section gets the personality unit.
- `docs/13-flavor-marketplace.md`: marketplace shape lists personality + tool palette as the unit of shipment.

## Notes

- This spec is the binding architectural decision record. Implementation plans that operate against it land in `.plans/` (per `.plans/CONVENTIONS.md`) and may decompose this spec into substrate / Code-flavor / frontend-wire / docs slices.
- Two prior plans are prerequisites that already landed: `.plans/2026-05-06T18-32-12+0200-flavor-registry-single-source-of-truth.md` and `.plans/2026-05-06T18-47-49+0200-drop-personality-state-hash.md`. This spec assumes their post-state.
- The "v1 holds Engineer's palette to one forced tool" decision is the load-bearing scoping call. It keeps v1 tractable while landing the full architecture. v1.1's scope is correspondingly larger (real decider + speech-act tools).
