# Handles per wake — no UUIDs in model context

**Status:** design
**Date:** 2026-05-13
**Owner:** Heinrich
**Scope:** `crates/core/src/mcp/handles.rs`, `crates/core/src/mcp/mod.rs` (`McpToolCtx`), `crates/core/src/personality/context.rs` (`PersonalityToolContext`), `crates/core/src/wake/token_store.rs` (`WakeTokenContext`), `crates/mcp-server/src/server.rs` (`McpToolHost::ctx`, `call_harness_tool`), every substrate tool under `crates/core/src/personality/tools/`, every MCP tool output that today emits a raw UUID.
**Out of scope:** persisting handles across wakes; rendering handles in Shell UI (handles are model-facing only — Shell consumes UUIDs); rewriting audit-log payloads (audit records intentionally keep raw UUIDs for stable cross-wake identity); the broader question of whether substrate tools should be merged into the MCP-tool registry.
**Related:**
- `docs/superpowers/specs/2026-05-12-proxima-harness-design.md` — the in-process LLM loop that consumes this surface; substrate dispatch bridge lives in its tool path.
- `docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md` — defines the four-param wake context (`type_id`, `instance_id`, `current_root_perspective_memory_id`, `triggering_event_memory_id`) that this spec pre-seeds into the handle table.
- `docs/superpowers/specs/2026-05-10-personality-mcp-crud-design.md` — the MCP CRUD surface that already mints `P` / `W` / `N` handles correctly (the pattern this spec generalizes).
- Prior step (already merged on this branch): the duplicate `request_key`/`idempotency_key` MCP-arg removal in `flavors/code/src/mcp/emit_execution_request.rs` and `workspace_review/{types,tools}.rs`.

## Problem

On 2026-05-13 a Planner wake (`change_event_seq` `019e21aa-33fe-7240-8acc-e467821f1098`, GPT-5.5, model `gpt-5-codex`) ran 16 rounds and was truncated by `max_rounds_reached` after two failed attempts to call `proxima-code/code_emit_execution_request`:

- **Round 15** — `invalid input: request_key must match idempotency_key` (now fixed by removing the duplicate arg).
- **Round 16** — `unknown handle: goal: 019e1b37-c1f0-7c71-b3b9-af5c2ae84d45 Single Endpoint for Graph View of personalities; evidence_count=0`.

The round-16 failure is the load-bearing one for this spec. The model needed a `goal_activated_memory` (a `MemoryId` of a `proxima-goal/goal-activated-v1` Fact). What it had to work with:

1. `core/list_active_goals` had returned each goal as `{goal_id: <UUID>, schema_id, schema_version, title, text, payload: Vec<u8>}` — a **raw `goal_id`**, no `goal_activated_memory_id`.
2. `core/fetch_memory` at round 14 had read a `goal-activated-v1` Fact and returned its `payload` (which contains `evidence_count`) — but the model had to **synthesize** a string out of the goal id, title, and a payload field to attempt the call, because no tool output had given it a typed handle for "the goal-activated memory that woke me."
3. The wake context already knew the triggering memory (`WakeTokenContext.triggering_event_memory_id` at `crates/core/src/wake/token_store.rs:22`), but that id was never surfaced to the model in handle form.

The 11 prior rounds were all read-only exploration (`code_search_chunks` × 18, `code_open_file_revision` × 16) — the model was searching for something the contract should have given it for free.

Independently, the same principle is violated by every substrate tool. `core/fetch_memory` returns `memory_id: <UUID>` in its JSON output (`crates/core/src/personality/tools/fetch_memory.rs:92`). `core/list_active_goals` returns raw `goal_id` (`list_active_goals.rs:38`). `core/list_self_perspectives` returns raw `personality_instance_id` (`list_self_perspectives.rs:59`). `core/emit_perspective`, `core/emit_abstraction` return raw `memory_id`. The model sees a UUID, types it back into the next tool call, and the harness re-parses it — round-tripping `Uuid::parse_str` through model tokens for no reason.

## Decision

**Model-facing tool I/O carries handles, never UUIDs.** A handle is an opaque short string (`N1`, `G7`, `P1`, `E12`, `W3`, …) scoped to one wake invocation. The wake's HandleTable is the single ID↔Handle store for that wake. Every UUID-typed field in any tool's serialized output is replaced with a handle assigned from that table. Every tool input accepts a handle string and resolves through the same table.

This is already the established pattern for some MCP tools (59 `ctx.handles.assign_*` call sites in `crates/`, `flavors/`, `apps/` today). It is **not** the pattern for substrate tools, because `PersonalityToolContext` has no handle table access. It is also not enforced as a *lifetime contract* — today `McpToolHost` holds one `Arc<HandleTable>` for the process lifetime, so handle numbers drift forever and handles minted in one wake can in principle resolve from another.

Three structural moves:

1. **HandleTable lifetime = wake invocation** (not host, not session). Attach `Arc<HandleTable>` to `WakeTokenContext`; the harness's substrate-dispatch bridge reads from there.
2. **`PersonalityToolContext` gains `handles: Arc<HandleTable>`** in parity with `McpToolCtx`. Substrate tools call `ctx.handles.assign_*` exactly like MCP tools.
3. **Wake bootstrap pre-seeds** the obvious entities before the first LLM turn, and the wake brief / system prompt names them. The model's round-1 context already knows `N1`, `N2`, `P1`.

## Architecture

### HandleTable lifetime

Today (`crates/mcp-server/src/server.rs:14-44`):

```rust
pub struct McpToolHost {
    pool: sqlx::PgPool,
    owner: Owner,
    handles: Arc<HandleTable>,        // <-- process-lifetime
    registry: Arc<FlavorRegistryFrozen>,
    engine: Option<Arc<Engine>>,
}
```

`McpToolHost::ctx` (server.rs:87-103) clones `self.handles` into every `McpToolCtx`. For wake-dispatched calls this is wrong: the wake should own a fresh table whose entries are scoped to one invocation.

Proposed:

```rust
// crates/core/src/wake/token_store.rs
pub struct WakeTokenContext {
    pub invocation_id: Uuid,
    // ...existing fields...
    pub handles: Arc<HandleTable>,    // NEW
}
```

Construction site: wherever `WakeTokenContext` is built (the wake-fire path that hands a token to the harness). At construction the table is freshly empty, then immediately pre-seeded (see next section).

`McpToolHost::call_harness_tool` (server.rs:273-317) already resolves the wake from the token. It builds `McpAuthContext` from `wake`, then calls `self.call_tool(...)`. Inside that path, `Self::ctx` constructs an `McpToolCtx`. **Change**: the context's handle table and output mode are selected from auth:

```rust
// crates/mcp-server/src/server.rs::ctx (sketch)
let (handles, mode) = match auth.wake() {
    Some(wake) => (Some(wake.handles.clone()), OutputMode::Handles),
    None       => (None,                       OutputMode::RawIds),
};
```

`McpToolHost.handles` is deleted entirely. Master-token (Shell admin) callers run in `OutputMode::RawIds` — see §Master-token paths use raw IDs.

### Pre-seed contract

Before the first LLM turn, the wake bootstrap registers the three entities the wake context already names and returns a `PreSeededHandles` struct:

```rust
pub struct PreSeededHandles {
    pub triggering: Handle,           // typically "N1"
    pub root_perspective: Handle,     // typically "N2"
    pub self_instance: Handle,        // typically "P1"
}
```

| Pre-seed                              | Source                                                    | Typical handle |
| ------------------------------------- | --------------------------------------------------------- | -------------- |
| Triggering memory                     | `WakeTokenContext.triggering_event_memory_id`             | `N1`           |
| Self root perspective                 | `WakeTokenContext.current_root_perspective_memory_id`     | `N2`           |
| Self personality instance             | `WakeTokenContext.personality_instance_id`                | `P1`           |

`HandleTable` assigns counters in registration order (`handles.rs:107`, `*next = next.checked_add(1)`), so pre-seeding produces `N1`/`N2`/`P1` *at construction time*. **The contract is the struct, not the literal string.** Any code that mints handles between pre-seed and first turn (e.g., a future brief formatter that pre-registers handles for recent goals) would shift the numbering. Brief formatters MUST read handles from the `PreSeededHandles` struct; hard-coding `N1`/`N2`/`P1` is a contract violation.

The example brief uses the typical numbers for readability; the formatter would interpolate from the struct:

> You were woken by `{triggering}`, a `proxima-goal/goal-activated-v1` Fact. Your root perspective is `{root_perspective}`. You are `{self_instance}`. When emitting an execution request, pass `{triggering}` as `goal_activated_memory`.

For Planner wakes specifically, the contract collapses to "the memory that woke you is the memory to cite" — no `list_active_goals` round needed unless the planner wants to inspect siblings.

**Property test:** seed a fresh table; register N additional unrelated entities; assert `PreSeededHandles::triggering` still resolves to the original `MemoryId`. Construction-order numbering is not part of the public contract.

### Substrate-tool integration

`PersonalityToolContext` (`crates/core/src/personality/context.rs:16-34`) gains:

```rust
pub struct PersonalityToolContext<'a> {
    // ...existing fields...
    pub handles: Arc<HandleTable>,    // NEW
}
```

The MCP-substrate bridge that constructs this context (in the harness substrate dispatch path) reads `handles` from the wake-resolved `WakeTokenContext`. Every substrate tool's output path that currently emits a UUID switches to a handle:

| Tool                                       | Today                                                                                       | After                                                                              |
| ------------------------------------------ | ------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| `core/fetch_memory`                        | `"memory_id": snapshot.memory_id.into_inner()` (`fetch_memory.rs:92`)                       | `"memory": ctx.handles.assign_memory(snapshot.memory_id).as_str()`                 |
| `core/list_active_goals` (`ActiveGoalSummary`) | `goal_id: GoalId` + `payload: Vec<u8>` (`list_active_goals.rs:38-44`)                       | `goal: <G-handle>`, `goal_activated_memory: <N-handle>`, `title: String`; drop `schema_id`, `schema_version`, `text`, `payload: Vec<u8>` (per step 8) |
| `core/list_self_perspectives`              | `"personality_instance_id": row.personality_instance_id.into_inner()` (`list_self_perspectives.rs:59`) | `"personality": <P-handle>`                                                        |
| `core/emit_perspective`                    | `"memory_id": memory_id.into_inner()` (`emit_perspective.rs:126`)                           | `"memory": <N-handle>`                                                             |
| `core/emit_abstraction`                    | `"memory_id": memory_id.into_inner()` (`emit_abstraction.rs:124`)                           | `"memory": <N-handle>`                                                             |
| `core/emit_goal`                           | (audit-only) — verify it emits a goal handle in model-facing output                          | `"goal": <G-handle>`, `"goal_proposed_memory": <N-handle>`                          |
| `core/create_edge`                         | input takes `source_memory_id: uuid::Uuid` + `target_memory_id: uuid::Uuid` (`create_edge.rs:23,25`) | input takes `source: <N-handle>`, `target: <N-handle>` (resolved via `resolve_memory`); output emits `"edge": <E-handle>` |
| `core/walk_lineage`                        | input takes `memory_id: uuid::Uuid` (`walk_lineage.rs:20`)                                  | input takes `memory: <N-handle>`                                                   |

For input symmetry, every substrate tool's `Args` struct that today carries a `uuid::Uuid` field becomes a `String` field resolved via `ctx.handles.resolve_memory(...)` / `resolve_goal(...)` etc. — exactly the pattern MCP tools already use (e.g., `flavors/code/src/mcp/emit_execution_request.rs::resolve_memory_id`).

Both `format_*` and `resolve_*` helpers are mode-aware (see §Master-token paths use raw IDs). In `Handles` mode they speak handles; in `RawIds` mode they speak raw UUIDs. Tool implementations always go through the helpers — they never branch on mode themselves.

MCP-side audit (the parallel pass on `crates/core/src/mcp/core_tools/` and flavor MCP tools): every model-facing JSON output containing `"memory_id"`, `"goal_id"`, `"edge_id"`, `"personality_instance_id"`, `"wake_entry_id"`, `"source_memory_id"`, `"target_memory_id"` becomes a handle field. The internal `proxima_core::change_event` / audit-log records keep raw UUIDs — those are storage, not model context.

### Kind-aware resolver errors

`HandleTable::resolve_memory` today returns `Option<MemoryId>` (`handles.rs:129-138`) — `None` on either "unknown handle" or "wrong kind." Callers throw a flat `UnknownHandle(raw.to_string())`. The model can't tell whether it typed a typo or passed the wrong kind.

Replace with a typed result:

```rust
pub enum ResolveError {
    Unknown { input: String },
    WrongKind { input: String, got: EntityKind, expected: EntityKind },
}
```

Resolver methods return `Result<TypedId, ResolveError>`. `McpToolError` gains a variant (or rewords its existing `UnknownHandle`) so the error string to the model reads:

> `goal_activated_memory`: expected Memory handle (`N…`), got Goal handle `G7`. The activated fact for `G7` is at `N1` (set by the wake that triggered you).

The "the activated fact for `G7` is at `N1`" hint requires the table to remember the pre-seed mapping, which is already the case. For non-pre-seeded mistakes (random typo), drop the hint and keep the kind correction.

### Error messages are model context

A tool's `Err(...)` branch reaches the model the same way its `Ok(...)` JSON does. Every error string emitted from a model-facing tool path is in this audit:

- Resolver errors (covered above) reference handles, using kind-aware framing.
- Validation errors (`invalid input: …`, `unknown goal …`, `memory not found …`) reference handles, never UUIDs.
- Errors caught from downstream substrate calls that bubble up carrying raw UUIDs MUST be re-mapped through the wake's handle table (`assign_*` first) before being surfaced. A nested `sqlx::Error` whose `Display` exposes a UUID column is a leak; the tool layer wraps and re-handles.
- Errors that name an entity the model could not have minted a handle for (lookup failed before assignment) reference the raw input string the model typed, never the resolved UUID.

Steps 5 and 6 of the implementation order audit both `Ok` and `Err` paths.

### Why per-wake, not per-session or per-process

- **Per-process (today)** — `McpToolHost.handles` grows for the host's lifetime. Handle numbers drift up forever (one handle table for the entire Tauri Shell run). One personality's `N12` could in principle resolve when another personality asks. Bad.
- **Per-session** — better, but the MCP server's session lifecycle is HTTP/SSE-connection-shaped. Wakes can span seconds to minutes; sessions can span days. Wake is the natural scope: model context starts and ends with the wake, so does the handle table.
- **Per-wake (this spec)** — fresh table, fully ephemeral, scope matches the model's working memory. The handles in `wake_invocation_log.message_tail` like `N1` mean "the first memory referenced in *that* wake" — recoverable by replaying the wake's read log against the same input order, but never by guessing across wakes.

### Master-token paths use raw IDs

Wake-dispatched callers get per-wake handles in their outputs; master-token callers (Shell admin paths, admin scripts) do not. The MCP tool layer carries an `OutputMode`:

```rust
pub enum OutputMode {
    Handles,    // wake-dispatched; model-facing
    RawIds,     // master-token; human-facing (Shell UI, admin scripts)
}
```

`McpToolCtx` exposes mode-aware helpers for both directions:

- **Output:** `format_memory(id)`, `format_goal(id)`, `format_edge(id)`, `format_personality(id)`, `format_wake_entry(id)` — return handle strings in `Handles` mode; raw UUID strings in `RawIds` mode.
- **Input:** `resolve_memory(s)`, `resolve_goal(s)`, etc. — parse `s` as a handle (lookup in table) in `Handles` mode; parse `s` as a UUID string in `RawIds` mode.

`McpToolHost.handles` is deleted: two clean regimes, no shared fallback. Shell UI already renders human-readable strings from UUIDs internally and has no need for handles. If a Shell-hosted personality ever runs an LLM in-process, that's a wake — covered by the per-wake table.

## Implementation order

Sized for a single Vibe delegation per step where useful, with verification checkpoints.

1. **`HandleTable` + `PreSeededHandles` on `WakeTokenContext`** — add `handles: Arc<HandleTable>` field; pre-seed at construction with triggering memory, root perspective, self instance; return a `PreSeededHandles` struct from the seed function. Property test per §Pre-seed contract.
2. **Plumb `Arc<HandleTable>` into `PersonalityToolContext`** — add field, update `PersonalityToolContext::new` callers (the substrate-dispatch bridge in `mcp-server`, test harnesses). Reads from `wake.handles` when a wake is bound, else a fresh empty table (test-only path).
3. **MCP path: `OutputMode` + wake handles** — `McpToolHost::ctx` builds `McpToolCtx` with `(Some(wake.handles), OutputMode::Handles)` when `auth.wake.is_some()`, else `(None, OutputMode::RawIds)`. Delete `McpToolHost.handles` field. Add mode-aware `format_*` and `resolve_*` helpers on `McpToolCtx` (per §Master-token paths use raw IDs).
4. **Kind-aware resolver errors** — introduce `ResolveError`, swap call sites, update `McpToolError` formatting. Error messages reference handles, never UUIDs (per §Error messages are model context).
5. **Substrate-tool I/O audit (Ok + Err paths)** — sweep every tool listed in the §Substrate-tool integration table. Input `uuid::Uuid` fields in `Args` structs become handle-typed `String`s resolved via `ctx.handles.resolve_*`. Output JSON uses handle fields. Both `Ok(...)` JSON and `Err(...)` error strings are audited: downstream substrate errors that carry raw UUIDs get re-mapped through `assign_*` before surfacing. One PR per tool family is reasonable (substrate / flavor-goal / flavor-code).
6. **MCP-tool I/O audit (Ok + Err paths)** — parallel sweep on `crates/core/src/mcp/core_tools/` and flavor MCP tools (`flavors/code/src/mcp/`, `flavors/goal/src/mcp/`). Same rules as step 5.
7. **Wake-context preamble in the round-1 system prompt** — after pre-seed, the harness's wake bootstrap formats a 3–4 sentence "wake context" block referencing `PreSeededHandles.triggering`, `root_perspective`, `self_instance` by their assigned handle strings, and prepends it to the system prompt. Recipe content follows unchanged. The preamble formatter reads from `PreSeededHandles` (never hard-codes `N1`/`N2`/`P1`). Recipe authoring surface is untouched.
8. **`ActiveGoalSummary` rename + shrink** — emit `[{goal: <G-handle>, goal_activated_memory: <N-handle>, title: String}]` per active goal. Drop `schema_id`, `schema_version`, `text`, and `payload: Vec<u8>` from the output — the model fetches detail via `fetch_memory(<N-handle>)` when it picks a goal to act on. Closes the round-16 failure mode and applies the edge-hygiene discipline (list = triage, fetch = detail).

Each step compiles cleanly on its own. Steps 5 and 6 can run in parallel by tool family.

## Out-of-scope clarifications

- **Audit-log payload schemas keep UUIDs.** `personality_config_changed_v1`, `wake_invocation_log`, `goal_activated_v1`, etc. are records of *what happened*, not model context. Handles are ephemeral; audit needs stable cross-wake identity.
- **Shell UI keeps UUIDs.** The Tauri frontend reads `proxima_core` types directly and renders human strings — it doesn't tokenize through an LLM, so handles are a model-context concern only.
- **No handle persistence.** Handles are not stored in Postgres. They live in `HandleTable.inner` (in-memory `Mutex<HashMap>`), get dropped with the wake. Replaying a wake by reading `worker-session.jsonl` reconstructs handles in the same order because tool-call order is deterministic.
- **Flavor objects.** `EntityRef::FlavorObject { kind, id }` (`handles.rs:11`) is already supported; the per-flavor prefix (e.g. `R` for repo, `C` for commit) is the flavor's choice. This spec doesn't touch flavor-object handle vocabulary.
- **Flavor-payload codec exposure.** Whether tool outputs decode `payload: Vec<u8>` via the flavor registry into JSON, ship the bytes through as-is, or drop them entirely is a separate question about flavor-registry surfaces. `ActiveGoalSummary` (step 8) drops `payload: Vec<u8>` from its output independently.

