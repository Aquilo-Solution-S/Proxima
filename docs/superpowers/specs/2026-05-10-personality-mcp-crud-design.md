# Personality CRUD via MCP

**Status:** Draft, 2026-05-10. Rides on the canonical vocabulary spec
(`2026-05-10-personality-vocabulary-and-archetype-discipline.md`).

## Goal

Expose the existing personality-config engine verbs through the MCP
surface so an LLM running as one personality can mutate the
configuration of itself or peer personalities, with audit-by-construction
inside the four-pillar memory graph. This is the v1 substrate for the
"self-evolution" thread — a personality that observes its own behavior
and rewrites its own WakeConfig in response.

## Non-Goals

- **No new engine verbs.** All mutation logic — uniqueness, palette
  membership, goose-CLI recipe validation, target/tier presence,
  idempotent-replay semantics — already lives in
  `crates/core/src/inference/set_wake_entries.rs` and the
  `Engine::{instantiate_personality, set_wake_entries,
  tombstone_personality, register_inference_target,
  remove_inference_target, bind_inference_tier}` facade. The CRUD-via-MCP
  layer wraps those, it does not duplicate them.
- **No frontend canvas migration.** The canvas continues calling gRPC
  through `engineClient`. CRUD-via-MCP is for LLMs running inside
  goose-recipe wakes and for remote admin clients (Shell, marketplace
  flavors, automation).
- **No proposal-edge / approval workflow.** Mutations land directly.
  Audit is the typed Fact memory emitted alongside each write, not a
  human-review gate.
- **No cross-owner mutations.** The MCP token fixes owner; tools enforce
  single-owner.
- **No optimistic-concurrency version stamps in the wire types.**
  Engine verbs do not carry them. Concurrency is handled in the
  storage layer via `SELECT ... FOR UPDATE` inside the
  read-modify-write transaction the granular tools open; concurrent
  granular ops on the same WakeConfig serialize, with the loser seeing
  a typed retry error after the wait.
- **No `register_owner_defaults` implementation.** That hook is named in
  the canonical vocabulary spec but not yet implemented as code.
  Independent concern, sequenced separately.
- **No recipe-file authorship.** Recipes are files on disk under
  `flavors/<name>/recipes/` or `owner_recipes_root`. Mutating their
  contents is a separate write surface (file CRUD), not part of
  personality-config CRUD.

## Architecture

The CRUD layer is a thin band of MCP tools that:

1. Accept tool-call args using **handles**, not raw UUIDs (per the
   `debbde3` discipline: UUIDs do not leak through MCP tool surface).
2. Resolve handles → engine-typed ids via the existing
   `crates/core/src/mcp/handles.rs::HandleTable`, extended with
   `EntityRef::Personality(PersonalityInstanceId)` and
   `EntityRef::WakeEntry(Uuid)` variants.
3. Call the existing engine verb. All validation runs unchanged.
4. On success, emit one typed Fact memory recording the mutation, with
   provenance walking back to the calling personality's Root
   Perspective (or to a substrate-shipped `shell-author` Root
   Perspective for master-token writes).
5. Assign handles to any newly-minted entities and return them.

No new postgres tables. No new validation. No proposal-edge machinery.
The whole layer is roughly: tool registration → handle resolve →
engine call → audit emit → handle assign → response.

## Tool surface

Tool names follow the existing `core/<verb>` naming convention used by
the substrate pack (`core/emit_perspective`, `core/walk_lineage`,
etc.).

### Personality CRUD

```
core/list_personalities
  args:    {}
  returns: [{handle: P, display_name, status,
             root_perspective: N, wake_entry_count: u32}]

core/get_personality
  args:    {personality: P}
  returns: {handle: P, display_name, status, root_perspective: N,
            wake_entries: [{handle: W, trigger_kind, trigger_id, label,
                            recipe_ref, model_tier, inference_target_ref,
                            substrate_tool_palette: [String],
                            workspace_tool_palette: [String],
                            execution_mode, authored_by,
                            probability_promille, max_rounds,
                            enabled, disabled_reason}]}

core/instantiate_personality
  args:    {display_name: String, purpose: String}
  returns: {handle: P}
  engine:  Engine::instantiate_personality

core/tombstone_personality
  args:    {personality: P}
  returns: {status: String, idempotent_replay: bool}
  engine:  Engine::tombstone_personality
```

### WakeEntry CRUD

```
core/list_wake_entries
  args:    {personality: P}
  returns: [{handle: W, trigger_kind, trigger_id, label, ...}]

core/set_wake_entries
  args:    {personality: P, entries: [WakeEntryDraftInput]}
  returns: {active_entries: u32}
  engine:  Engine::set_wake_entries  (replace-all; mirrors verb 1:1)

core/add_wake_entry
  args:    {personality: P, entry: WakeEntryDraftInput}
  returns: {handle: W}
  impl:    read current entries → append → set_wake_entries

core/update_wake_entry
  args:    {wake_entry: W, patch: WakeEntryPatch}
  returns: {handle: W}
  impl:    read current entries → diff onto matching entry → set_wake_entries

core/remove_wake_entry
  args:    {wake_entry: W}
  returns: {removed: bool}
  impl:    read current entries → drop matching → set_wake_entries
```

`WakeEntryDraftInput` is the input form of `proxima_core::WakeEntryDraft`
with `personality_instance_id` filled in by the tool layer (callers
never see the personality's own UUID — they pass `P` as the parent).
The `wake_entry_id` field is shape `Option<W>`:

- **Omitted** — the tool layer allocates a fresh UUID. Used by
  `add_wake_entry` and by entries newly introduced in a bulk
  `set_wake_entries`.
- **Present** — resolves to the existing UUID of a previously-known
  entry. Used by carry-over entries in a bulk `set_wake_entries`
  (callers preserve identity across a replace by passing the same `W`
  handle they read from `list_wake_entries`).

`update_wake_entry` and `remove_wake_entry` take the `W` handle as a
top-level arg, not embedded in a draft.

`WakeEntryPatch` is a struct of `Option<T>` fields covering
`label`, `enabled`, `recipe_ref`, `model_tier`, `inference_target_ref`,
`substrate_tool_palette`, `workspace_tool_palette`,
`probability_promille`, `max_rounds`, `execution_mode`, `authored_by`.
`trigger_kind` and `trigger_id` are immutable in update — changing them
is `remove_wake_entry` + `add_wake_entry`. This keeps the trigger
uniqueness check (`(trigger_kind, trigger_id)` per personality) trivial.

### Inference admin

```
core/list_inference_targets       args: {}                          returns: [{target_ref, config}]
core/register_inference_target    args: {target_ref, config}        returns: {target_ref, idempotent_replay}
core/remove_inference_target      args: {target_ref}                returns: {removed: bool}
core/bind_inference_tier          args: {tier, target_ref}          returns: {tier, target_ref, idempotent_replay}
core/list_inference_tier_bindings args: {}                          returns: [{tier, target_ref}]
```

These wrap the corresponding `Engine` methods 1:1. Included in v1 because
a self-evolving personality that registers a new wake entry pointing at a
specific `inference_target_ref` needs to be able to register that target
first; the two surfaces are coupled by validation.

### Discovery (read-only catalog tools)

Listing existing personalities is not enough — an LLM constructing a
`WakeEntryDraftInput` needs to know what `recipe_ref` strings are
valid, what tool ids are accepted by the substrate and workspace
palettes, what schema ids exist for `OnMemory` triggers, and what edge
types for `OnEdge`. Without this surface, the LLM either guesses (and
hits validation errors `recipe_not_found`, `tool_not_registered`) or
relies on the model-prior to know the configured space, which it does
not actually know in any given owner's deployment.

```
core/list_recipes
  args:    {}
  returns: [{recipe_ref: String, source: "flavor:<name>" | "owner",
             label: String, description: String}]
  impl:    enumerate flavors' recipe directories (via FlavorRegistryFrozen)
           + owner_recipes_root, return the union; do not parse parameter
           schemas (recipes are self-contained at wake time)

core/list_substrate_tools
  args:    {}
  returns: [{tool_id: String, source: "substrate" | "flavor:<name>",
             description: String}]
  impl:    substrate_pack() ∪ FlavorRegistryFrozen.mcp_tool_ids()

core/list_workspace_tools
  args:    {}
  returns: [{tool_id: String, description: String}]
  impl:    walk WORKSPACE_TOOL_CATALOG

core/list_schemas
  args:    {kind: Option<"Fact" | "Edge" | "Abstraction" | "Perspective" | "Goal">}
  returns: [{schema_id: String, schema_version: u32, kind: String,
             description: Option<String>}]
  impl:    project FlavorRegistryFrozen schemas

core/list_edge_types
  args:    {}
  returns: [{edge_type: String, description: Option<String>}]
  impl:    project FlavorRegistryFrozen edge types
```

All five run under the `personality.read` scope. They are pure
projections of build-time `FlavorRegistryFrozen` state plus the
substrate pack and on-disk recipe directories — no postgres reads, so
they're cheap and cacheable per session.

**Fixed enums are not discovered at runtime.** `WakeEntryTriggerKind`
(`OnMemory` / `OnEdge`), `ModelTier` (`Fast` / `Standard` / `Deep`),
`WakeEntryAuthoredBy` (`Any` / `SelfAuthor` / `Other`), and
`WakeExecutionMode` (`SubstrateOnly` / `Workspace`) are encoded as
JSON-schema `enum` constraints inside the args_schema of the write
tools. `list_tools` introspection surfaces them; an LLM reading the
schema sees the valid values directly without a separate roundtrip.
The `probability_promille: 0..=1000` bound is encoded the same way
(JSON-schema `minimum`/`maximum`).

**Recommended workflow for a self-evolving personality** authoring a
new wake entry from scratch:

1. `core/list_recipes` → pick a recipe_ref.
2. `core/list_substrate_tools` + `core/list_workspace_tools` → pick the
   palette.
3. `core/list_schemas{kind: "Fact"}` (or `core/list_edge_types`) → pick
   the trigger.
4. `core/list_inference_targets` (or rely on `model_tier` + tier
   binding) → pick how the wake will be powered.
5. `core/add_wake_entry(personality: P, entry: …)`.

The LLM can also call `core/get_personality(P)` first to look at
existing entries on the same personality as templates.

## Handle layer extension

`crates/core/src/mcp/handles.rs` extends:

```rust
pub enum EntityRef {
    Memory(MemoryId),
    Edge(EdgeId),
    Goal(GoalId),
    Repo(uuid::Uuid),
    Personality(PersonalityInstanceId),  // new — prefix 'P'
    WakeEntry(uuid::Uuid),                // new — prefix 'W'
}
```

`HandleTable` gains `assign_personality`, `assign_wake_entry`,
`resolve_personality`, `resolve_wake_entry`, mirroring the existing
helpers (per-session monotonic counters, idempotent assignment, malformed
handle rejection in `is_valid_handle_shape`).

`get_personality` and `list_personalities` populate the table as a side
effect, so a follow-up `add_wake_entry(P3, ...)` resolves cleanly.
`set_wake_entries` returning a `WakeEntryDraftInput[]` whose entries are
passed back without their newly-assigned `wake_entry_id`s in the response
will populate handles for each entry the caller can read in
`list_wake_entries`.

The handle layer is MCP-only. gRPC and frontend `engineClient` continue
to use UUIDs unchanged.

## Audit Fact memory

Every successful mutation emits exactly one Fact memory:

```
schema_id: core/personality_config_changed_v1
schema_version: 1
payload: {
  verb: "instantiate" | "tombstone" |
        "set_wake_entries" | "add_wake_entry" | "update_wake_entry" | "remove_wake_entry" |
        "register_inference_target" | "remove_inference_target" | "bind_inference_tier",
  subject: {
    kind: "personality" | "wake_entry" | "inference_target" | "tier_binding",
    id:   "<uuid or stable string ref>"
  },
  before: <opaque JSON snapshot of the relevant prior state, or null on create>,
  after:  <opaque JSON snapshot of the relevant new state, or null on tombstone>,
  caller: {
    kind: "wake_personality" | "master_token",
    personality_instance_id: "<uuid or null>"
  }
}
```

A single schema across all verbs (rather than one schema per verb) keeps
the registry small and the audit query simple — `walk_lineage` over
`core/personality_config_changed_v1` returns the full mutation history
of any subject. Per-verb specialization can be added later if a
downstream consumer demands it.

### Provenance

- **Wake-token caller.** Provenance is the calling personality's Root
  Perspective, already carried in
  `McpToolCtx.caller_self_perspective`. The Fact memory is authored
  *by* that personality — same authorship contract as any other Fact a
  personality emits.

- **Master-token caller.** No calling personality exists. Provenance is
  a substrate-shipped singleton `proxima/shell-author` personality
  with a stable Root Perspective and an empty WakeConfig. Empty
  WakeConfig means it never fires; it exists only to author Fact
  memories on behalf of admin clients. Substrate-shipped (not
  Code-flavor-shipped) because it's owner-scoped infrastructure, not a
  flavor-specific concept. Provisioned on owner creation (the same
  hook `register_owner_defaults` will eventually use, but for the
  shell-author this is a substrate concern that lands with this
  spec's implementation, not after).

### Emit order

The Fact memory is emitted **after** the engine verb succeeds, in the
same MCP-tool function. If the verb fails, no Fact. If the Fact emission
fails after the verb succeeds, the tool logs it and returns a typed
`audit_emit_failed` warning to the caller — the verb already landed,
retrying it would double-write. Same compensation discipline the
existing tool layer uses.

## Auth & scopes

`crates/mcp-server/src/auth.rs::McpToolScope` gates which tools a token
sees in `list_tools` and is allowed to call. Three new scope buckets:

- `personality.read` — `core/list_personalities`,
  `core/get_personality`, `core/list_wake_entries`,
  `core/list_inference_targets`,
  `core/list_inference_tier_bindings`, plus the discovery catalog
  tools (`core/list_recipes`, `core/list_substrate_tools`,
  `core/list_workspace_tools`, `core/list_schemas`,
  `core/list_edge_types`).
- `personality.write` — `core/instantiate_personality`,
  `core/tombstone_personality`, all WakeEntry CRUD verbs.
- `inference.write` — inference target + tier write verbs.

**Master tokens** (Shell admin, marketplace automation): all three.

**Wake tokens**: `personality.read` by default. `personality.write` and
`inference.write` are opt-in per personality through the wake-entry
palette declarations — a personality whose wake-entry palette includes
`core/add_wake_entry` gets the scope; a personality that doesn't
include those tools doesn't see them in `list_tools`. This makes
self-evolution opt-in per personality, not blanket.

## Validation & errors

All existing validation reused unchanged:
- `set_wake_entries`: trigger uniqueness within the new entry set,
  palette membership against substrate + workspace catalogs, recipe
  validation via `goose recipe validate`, target_ref or tier presence.
- `instantiate_personality`: display_name and purpose non-empty.
- `tombstone_personality`: idempotent (re-tombstoning returns
  `idempotent_replay: true`).

New `McpToolError` variants at the tool-layer boundary only:

- `WrongHandleKind { expected, got }` — `P` passed where `W` expected,
  etc. (extends the existing `UnknownHandle`.)
- `ConcurrentModification` — granular WakeEntry op detected the
  underlying entry list changed between read and write. Caller retries.
- `AuditEmitFailed { verb, subject_id }` — engine verb succeeded; Fact
  memory emit failed. Returned as a non-fatal warning attached to the
  successful response, not a failure (verb already landed).

Existing `ProtocolError` variants surfaced from the engine
(`recipe_not_found`, `recipe_invalid`, `tool_not_registered`,
`inference_target_missing`, `tier_unbound`,
`duplicate_trigger_in_request`, `trigger_conflict`, etc.) are mapped to
`McpToolError::Other(String)` with the engine error message preserved.

## Testing

- **Unit (handles).** `assign_personality`, `assign_wake_entry` allocate
  monotonic handles per kind. `resolve_personality("W3")` returns
  `None`. Round-trip preserves identity.
- **Unit (audit payload).** `before` / `after` snapshot serialization is
  deterministic, omits secrets, captures the fields a downstream
  observer needs to reconstruct the change.
- **Engine integration (one per tool).** Happy-path + one error path.
  Reuse the harness pattern from
  `crates/core/tests/engine_mcp_lifecycle.rs`.
- **Audit (one per mutation tool).** Each happy-path test asserts:
  - the Fact memory exists with `schema_id =
    core/personality_config_changed_v1`,
  - the `verb` field matches the tool,
  - provenance walks back to the expected Root Perspective (calling
    personality for wake-token, `proxima/shell-author` for master
    token),
  - `before` and `after` snapshots match the engine state pre/post
    call.
- **Discovery integration.** Each catalog tool's happy-path test
  asserts the projection matches the underlying source of truth: e.g.
  `core/list_substrate_tools` returns the union of `substrate_pack()`
  ids and `FlavorRegistryFrozen.mcp_tool_ids()`; `core/list_recipes`
  finds files in a temp owner_recipes_root + flavor recipe dirs.
- **Discovery → mutation flow.** A test wires the recommended
  workflow: `list_recipes` → `list_substrate_tools` → `list_schemas` →
  `add_wake_entry` using only the values returned by the discovery
  calls. Asserts the resulting entry validates and persists. This is
  the smoke test that an LLM with no prior knowledge of an owner's
  configuration can author a working wake entry.
- **E2E (the actual self-evolution smoke test).** A wake invocation in
  personality A calls `core/add_wake_entry` on its own
  `PersonalityInstanceId`. The MCP layer applies the change. The next
  dispatch tick picks up the new entry. Firing it produces the expected
  output. End-to-end through goose, the engine dispatcher, the storage
  layer, and back.
- **Scope enforcement.** A wake token without `personality.write`
  calling `core/instantiate_personality` returns `not_authorized` (HTTP
  status mapped through rmcp's `ErrorData::invalid_request`) and emits
  no Fact. The same token does not see the tool in `list_tools`.

## Failure modes & operator visibility

- **goose recipe-validate unavailable.** Engine returns
  `ProtocolError::goose_cli_unavailable`; tool layer surfaces it
  verbatim. Already covered by `set_wake_entries` validation.
- **Postgres constraint violation on trigger uniqueness.** Engine
  returns `trigger_conflict`; tool layer surfaces it. The granular
  `add_wake_entry` checks for the same trigger pair pre-flight to give
  a friendlier error, but the postgres constraint is the
  source-of-truth.
- **`audit_emit_failed`.** Logged at warn level with subject id and
  verb. The Postgres state is correct; the four-pillar audit row is
  missing. An operator running periodic audit-trail consistency checks
  can reconstruct the missed row from `wake_invocations` + the
  postgres row history if needed.
- **Concurrent granular ops.** Storage layer serializes via
  `SELECT ... FOR UPDATE` on the personality row inside the granular
  tool's transaction. The second op blocks until the first commits,
  then either succeeds against the post-commit state or surfaces a
  `ConcurrentModification` error if its precondition no longer holds
  (e.g., `update_wake_entry(W)` where W was just removed by the
  first op). No automatic retry — the LLM caller retries.
  Programmatic admin callers can wrap their own retry loop.

  This requires one storage-layer addition: a transactional
  `set_wake_entries_within(personality, |current_entries| -> new_entries)`
  primitive, or equivalent `(begin; SELECT FOR UPDATE; read; compute;
  set_wake_entries; commit)` flow. Engine-verb-callers (gRPC,
  frontend) keep using the existing `set_wake_entries` directly.

## Migration & rollout

- **No data migration.** No new tables, no schema changes (other than
  registering the new Fact-memory schema id in
  `proxima_core` schema registry).
- **Substrate provisioning.** The substrate-shipped `shell-author`
  personality is materialized for existing owners by a one-time backfill
  in the storage layer that runs on engine start (idempotent: insert if
  not exists). New owners get it from the regular owner-creation path.
- **Feature flag.** None. The tools either exist (registered at engine
  build) or don't. Scope-gating handles the "who can call them" axis;
  there's no need for a runtime toggle.

## What this enables (downstream)

- A personality whose recipe says "if your last three wakes truncated,
  call `core/update_wake_entry` to raise `max_rounds`" — observable
  self-tuning, no human in the loop.
- A "spinning wheel" composition where one personality `instantiate`s a
  new peer personality, sets its WakeConfig to fire on its own outputs,
  and lets the chain develop. Spec-supported by the canonical vocabulary
  spec; this CRUD layer is what makes it actually possible at runtime.
- Marketplace flavors that ship `register_owner_defaults` once it
  lands: that hook calls these same engine verbs, but
  flavor-shipped defaults installed via the hook and user mutations made
  via MCP both land in the same audit graph.
