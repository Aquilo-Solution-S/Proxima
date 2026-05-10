# Personality topology canvas: produces edges + relation nodes

**Status:** design
**Date:** 2026-05-10
**Owner:** Heinrich
**Related:**
- `docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md` (WakeEntry, palette, ToolContext)
- `docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md` (substrate emit tools, writeable_schemas)
- `docs/superpowers/specs/2026-05-09-personality-authorship-edge.md` (`core/authored` auto-wire)
- `crates/mcp-server/src/server.rs:253-278` (`writeable_schemas_for_palette`, `writeable_relations_for_palette`)
- `packages/frontend-core/src/views/personalities/canvas.tsx`, `layout.ts`, `types.ts`

## Problem

The Personalities view renders a 2D canvas of `Personality` nodes with
their `WakeEntry` ports and incoming **trigger** edges from shape
nodes (one node per `(trigger_kind, trigger_id)`, deduplicated). What
is missing is the other half of the spinning-wheel topology: the
**produces** direction — *which* memory schemas and edge relations
each entry could write when it fires.

Without the produces side, the canvas cannot answer:

- "If `Engineer` fires, what shapes of memory or edge could appear?"
- "Could `Engineer`'s output trigger another personality's wake entry?"
  (loop closure)
- "Is this entry a *terminal* (produces nothing) — i.e., does the
  chain dead-end here?"

Concretely: a Personality `P_i` has wake entries `W^j_{P_i}` and an
allowed action set `A_k` (its substrate tool palette). Each `A_k` has
typed consequences (`emit_abstraction`, `emit_perspective`,
`create_edge`, …). Those consequences produce shape outputs
(memory `schema_id` or edge `relation_id`). A produced shape that
matches another entry's trigger filter closes the loop into the next
wake. We want to render this static topology so config-time questions
have a visual answer.

The set of *which* substrate emit tools the v1 produces-derivation
covers is exactly the set the existing engine helper already covers
(see below). Adding more (e.g. `emit_goal`) is an additive change to
the same helper with no architectural impact.

## What this spec is — and isn't

**Is:** a *capability surface* (type-level) overlay. One ghost node
per unique `(schema_id)` or `(relation_id)`, no matter how many
producers; one edge per (entry, shape) pair. Renders at the personality
canvas only.

**Is not:**

- Runtime forecast ("what will this entry produce *this* wake?") —
  that requires LLM probing.
- Instance multiplicity ("up to `max_rounds` of each shape") — a
  cardinality bound is not load-bearing for capability questions; the
  user explicitly chose visual minimalism over multiplicity badges.
- Workspace-tool effects (`shell`, `text_editor`, `list_files`) —
  these touch the filesystem, not the typed memory/edge graph. They
  belong in a separate visualization.
- Auto-wired provenance / `core/authored` edges per emit. These are
  implicit on every memory write and rendering one per produces-edge
  multiplies the graph density without adding signal at this layer.
- `authored_by` filter (Any / SelfAuthor / Other) trimming the
  produces→trigger crossings. v1 draws raw topology; filtering is a
  later overlay.

## Critical existing primitive

The engine **already** derives the produces set from the palette in
`crates/mcp-server/src/server.rs:253-278`:

```rust
fn writeable_schemas_for_palette(engine: &Engine, palette: &[String])
    -> Vec<String>
{
    let allow_abstraction = palette.iter().any(|id| id == "core/emit_abstraction");
    let allow_perspective = palette.iter().any(|id| id == "core/emit_perspective");
    engine.registry().list().into_iter()
        .filter(|s| (allow_abstraction && s.kind == PayloadKind::Abstraction)
                 || (allow_perspective && s.kind == PayloadKind::Perspective))
        .map(|s| s.schema_id.into_inner())
        .collect()
}

fn writeable_relations_for_palette(engine: &Engine, palette: &[String])
    -> Vec<String>
{
    if !palette.iter().any(|id| id == "core/create_edge") { return Vec::new(); }
    engine.registry().list_relations().iter()
        .map(|r| r.relation.clone())
        .collect()
}
```

This is the source of truth used at fire-time to populate
`PersonalityToolContext::writeable_schemas` and
`writeable_relations`. **The capability preview must consume this same
derivation** — anything else is a parallel implementation that will
drift.

Therefore: no per-tool `produces` declaration on `McpToolDescriptor`,
no new method on `PersonalityTool`, no new field on `WakeEntryDraft`.
Only the IPC surface needs to grow.

> Why this matters: the auto-memory entry
> *feedback_no_doc_duplication* applies equivalently to logic. A FE
> reimplementation of the palette→shapes mapping would silently diverge
> the moment the Rust derivation changes.

## Design

### IPC surface

One new Tauri command, derived from the existing helpers:

```rust
// apps/proxima-shell/src-tauri/src/commands/tools.rs

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ProducesTs {
    pub schema_ids: Vec<String>,
    pub relation_ids: Vec<String>,
}

#[tauri::command]
#[specta::specta]
pub async fn wake_entry_produces(
    engine: State<'_, Arc<Engine>>,
    substrate_palette: Vec<String>,
) -> Result<ProducesTs, ProtocolError> { ... }
```

**Pure function** of `(palette, registry)`. No wake-entry id, no
personality id — same input → same output. Memoizable on the FE by
`palette.slice().sort().join(',')`.

`writeable_schemas_for_palette` and `writeable_relations_for_palette`
must move to a `pub` location callable from `proxima-shell` — most
naturally `crates/core/src/personality/` (alongside the trait and the
fire path that consumes them) so `mcp-server` and `proxima-shell` both
depend on `proxima-core`. The current placement in `mcp-server` is an
accident of where they were first needed.

### Canvas extension

Three FE changes inside `packages/frontend-core/src/views/personalities/`.

**1. Node taxonomy (`types.ts`).** `CanvasNodeKind` grows from
`"personality" | "schema"` to `"personality" | "schema" | "relation"`.
The `data` union grows by one variant
`{ kind: "relation"; relation_id: string }`.

**2. Layout (`layout.ts`).**

- For each `(instance, entry)`, fetch `produces` (memoized).
- Union all produced `schema_id`s into `triggerSchemas` so a schema
  used as both producer-output and another entry's trigger gets one
  node — loop closure becomes geometric, not bookkeeping.
- Add `relationNodes` (deduplicated) for every `relation_id` produced
  by any entry.
- Emit `WakeEntry → Schema` and `WakeEntry → Relation` edges out of
  EAST ports (currently only WEST trigger ports exist). ELK already
  supports per-port `port.side` constraints.
- Existing trigger edges (Schema → WakeEntry) are unchanged.

**3. Canvas rendering (`canvas.tsx`).**

- Trigger edges keep current style (solid, arrow into entry port).
- Produces edges render with a distinct style (dotted/colored arrow
  from entry into shape). The exact visual is an implementation choice
  but must read at a glance as "this entry could write this shape."
- Relation nodes get a visual distinct from schema nodes (e.g., a
  rounded chip vs. a square card) so eye can separate "memory shape"
  from "edge shape" without reading text.

### Loop visualization

Because schemas and relations are deduplicated across all
producers/consumers, a shape that closes a wake→shape→wake loop
automatically has both an incoming produces edge and an outgoing
trigger edge in the rendered graph. No special "cycle" detection is
needed — ELK's layered layout will route it as it routes any
back-edge, and the cycle is geometrically visible.

### Self-wake

The engine forbids self-wake at fire-time
(*personality_decision_loop* memory, "self-wake forbidden by
construction"). The canvas renders the geometry honestly: if entry W₁
produces shape S and entry W₂ on the *same personality* triggers on
S, both edges are drawn and the geometric cycle is visible, but
runtime will not actually fire it. A "would self-wake — guarded"
affordance can be added later as a small glyph on intra-personality
crossings; v1 omits it for simplicity.

### Authored-by filter

A wake entry's `authored_by` ∈ {`Any`, `SelfAuthor`, `Other`} narrows
*at fire-time* whose outputs may trigger it. v1 ignores this in the
canvas — produces→trigger crossings are drawn unconditionally. A later
overlay can dim or hide crossings that fail the filter.

## Data shape diagram

```
Engine                                     IPC               FE projection
──────                                  ──────────         ───────────────
list_relations()           ──existing──▶                ──▶ relation_ids
schema()                   ──existing──▶                ──▶ schema_ids
list_mcp_tools()           ──existing──▶                ──▶ palette options

writeable_schemas_for_palette(p)  \ NEW         ProducesTs    ──▶ canvas
writeable_relations_for_palette(p) /                              produces edges
                                                                  + relation nodes
```

## Failure modes

| Case | Behavior |
|---|---|
| Empty palette (read-only entry) | `produces = { [], [] }`. No produces edges. Entry renders as terminal — visually obvious it can't continue the chain. |
| Palette references unknown tool id | Ignored by the derivation. Matches existing engine semantics. |
| Schema referenced as trigger but unregistered | Existing canvas behavior; no change. |
| Same schema produced by N entries | One node, N incoming produces edges. ELK handles fan-in. |
| Personality with zero wake entries | Personality node renders alone, no produces edges, no triggers. Existing behavior; no change. |

## Files touched

| File | Change |
|---|---|
| `crates/core/src/personality/mod.rs` (or new submodule) | Lift `writeable_schemas_for_palette` / `writeable_relations_for_palette` here, `pub` |
| `crates/mcp-server/src/server.rs` | Replace local fns with `use proxima_core::personality::{...}` |
| `apps/proxima-shell/src-tauri/src/commands/tools.rs` | +`ProducesTs` + `wake_entry_produces` command |
| `apps/proxima-shell/src-tauri/src/commands/mod.rs` | Register command in `collect_commands!` |
| `packages/frontend-core/src/bindings.ts` | Regenerated |
| `packages/frontend-core/src/client.ts` | +`wakeEntryProduces(palette)` on `EngineClient` |
| `packages/frontend-core/src/tauri-client.ts` | +method impl |
| `packages/frontend-core/src/views/personalities/types.ts` | +`relation` kind, +data variant |
| `packages/frontend-core/src/views/personalities/layout.ts` | +produces edges, +relation nodes, +EAST ports |
| `packages/frontend-core/src/views/personalities/canvas.tsx` | +relation node visual, +produces edge style |
| `packages/frontend-core/src/views/personalities/index.tsx` | Fetch produces per entry, pass to layout |
| `packages/frontend-core/src/views/personalities.test.tsx` | Tests: produces edges rendered; relation nodes rendered; loop-closure case (one entry produces what another triggers on, deduplicated to one node) |
| `packages/frontend-core/src/graph-store.test.tsx`, `views/surface.test.tsx` | +`wakeEntryProduces` to `EngineClient` mock |

No Rust trait changes. No data model migration. No new `WakeEntryDraft` fields.

## Test plan

**Rust (engine derivation already covered by `mcp-server` tests).**
Add a unit test in the new home for the helpers asserting:

- empty palette → empty produces
- `["core/emit_abstraction"]` → all Abstraction schemas, no relations
- `["core/create_edge"]` → all relations, no schemas
- mixed palette → union behavior

**FE.** New tests in `personalities.test.tsx`:

- An entry with `emit_abstraction` in its palette renders a produces
  edge to each Abstraction schema in the registry.
- An entry with `create_edge` renders a produces edge to each relation
  node.
- An entry with neither renders no produces edges (terminal entry).
- A schema that is both an entry's trigger and another entry's
  produced shape renders as a single node with both kinds of edges
  attached (loop closure).
- Update existing `EngineClient` mocks across `graph-store.test.tsx`
  and `views/surface.test.tsx` to satisfy the interface.

## YAGNI / explicit non-goals (recap)

- No multiplicity rendering. Type-level only.
- No tool-level `produces` declaration in Rust.
- No `authored_by` filter trimming.
- No workspace-tool topology.
- No provenance/authored edge fan-out per produces.
- No self-wake-guard glyph.

Each of these can land later as a focused overlay on the same nodes
without touching the v1 data model.
