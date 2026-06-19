# 2026-06-15 — Memory surface into Core (engine-verb abstraction) — Spec A

- **Status:** design — revised 2026-06-15 after the cdx plan review. **Approach:
  Option B (engine-verb abstraction)**, chosen for long-term cleanliness on
  greenfield. Pending plan rewrite + re-review.
- **Branch:** `agent-memory-recall-findability` (off `road-to-v1`)
- **Scope owner:** Heinrich
- **Spec B (follow-on, blocked on this):**
  `2026-06-15-agent-memory-recall-findability-design.md`.
- **Related:** `docs/02-memory.md`, `docs/08-core-and-flavors.md`,
  `docs/14-protocol-surface.md`, memories `project_brain_hub_contraction`,
  `project_substrate_first_goal`, `project_no_uuids_in_model_context`,
  `project_strassberger_migration_strategy`, `project_flavor_marketplace`.

## Problem

There are **two overlapping agent-memory surfaces**:

- **Core primitives** (in `crates/core`): `core/emit_abstraction`,
  `core/emit_perspective`, `core/search_memories`, `core/walk_memory_lineage`,
  `core/facts_citing_object`, `Engine::ensure_fact_embedding`.
- **The `agent-memory` flavor** (`flavors/agent-memory/`, crate
  `proxima-agent-memory`, schema `proxima_agent_memory`): a parallel layer —
  `proxima_remember`, `proxima_derive`, `proxima_link`, `proxima_search_graph`,
  `proxima_open`, `record_utterance`.

They do the same job two ways, with behavioral divergence (derive doesn't
embed; emit does). Generic agent long-term memory is **substrate**, not a
domain flavor — it is exactly what the brain-hub contraction defines Proxima as
(memory + per-agent history + personality config). This spec makes the memory
surface core and collapses the two into one canonical palette.

## Why not a simple crate-fold (cdx plan review, verified)

The agent-memory MCP tools call `proxima_storage_pg::verbs::*` **directly** at
runtime (`derive.rs:6`, `remember.rs:9`, `link.rs:3`, `record_utterance.rs:6`).
`proxima-storage-pg` depends on `proxima-core`; core has storage-pg only as a
**dev-dependency** (`crates/core/Cargo.toml:35-37`). Moving these tools into
`crates/core` would require a runtime `core → storage-pg` dependency → a
**dependency cycle**. The tools live in a flavor crate *because* flavor crates
may depend on both core and storage-pg; core may not.

That direct tool→storage coupling is the actual smell, and core already shows
the clean alternative: `emit_abstraction`/`emit_perspective` author A/P
**through the Engine** (`ctx.engine` + `emit_personality_memory`, embedding
transactionally) and import **no** storage-pg
(`crates/core/src/personality/tools/emit_abstraction.rs:107-127`). The
agent-memory tools are the un-migrated legacy.

## Decision: Option B — engine-verb abstraction

Lift memory authoring into **core Engine verbs** (storage-pg implements behind
the Engine's storage handle, exactly as the emit path already does). The MCP
tools become **thin, core-resident, and call Engine verbs** instead of
storage-pg. This makes the Engine the single authority over every memory write,
removes the cycle, makes storage swappable behind the Engine (matters for the
future pluggable vector store, `docs/07`), and lets the tools be `core/*` with
**no namespace special-casing**.

## Goal & non-goals

**Goal:** one canonical, handle-based, **`core/*`** memory tool surface; all
memory authoring flows through core Engine verbs; the `proxima_agent_memory`
schema lives in `proxima_core`; the `proxima-agent-memory` crate is deleted.

**Non-goals (Spec B, or out entirely):** pgvector/HNSW, durable embedding-jobs
for the async Fact paths, read-scope-matrix fix, new search filters
(tags/time/order), recency lever. Any change to graph semantics, F/A/P
layering, citation model. New domain flavors.

(Note: **embed-on-derive is now in scope here** — the new derive Engine verb
embeds like emit — so it leaves Spec B.)

## Design

### Engine verb layer

Introduce/confirm core Engine verbs for memory authoring, each writing its
memory row + typed sidecar (+ embedding where applicable) in **one
transaction**, following `emit_personality_memory`:

| MCP tool (thin, core) | Engine verb it calls | Notes |
|---|---|---|
| `core/remember` | `Engine::ingest_fact_with_sidecar` | Fact + note sidecar. A sidecar-carrying ingest verb may already exist (`Engine::event_ingest_with_sidecar`, added for Strassberger — confirm in plan). Embedding for the async Fact path stays best-effort here; durable jobs are Spec B. |
| `core/record_utterance` | same ingest verb | utterance sidecar |
| `core/derive` | `Engine::author_derived` (new) | `ExternalAgent`-authored A/P + provenance edges; **embeds in-tx** (mirrors emit). Subsumes Spec B's embed-on-derive. |
| `core/link` | `Engine::append_edge` (new/confirm) | edge author |
| `core/search_graph` | `Engine::search_memories` (exists) | absorbs `core/search_memories`'s `kind`/`schema`/`reader` params; hybrid + neighbor edges + sidecar-aware |
| `core/get_memory` | a point-read engine accessor | payload + optional neighbor edges |
| `core/trace` | `Engine::walk_memory_lineage` (exists) | handle-fronted lineage |

storage-pg gains/keeps the concrete implementations (`append_derived_in_tx`,
`append_edge_in_tx`, ingest-with-sidecar); the Engine calls them via the
storage handle it already holds. The MCP tool crate imports **core only**.

### Tool dedup & naming

One palette, `core/*` (existing prefix policy; `add_substrate_mcp_tool` pins
`core`, `flavor.rs:392` — no change needed). `core/search_memories` and
`core/walk_memory_lineage` are subsumed by `core/search_graph` / `core/trace`.
**Display tool names change** (`proxima-agent-memory/proxima_remember` →
`core/remember`, etc.) — an accepted **breaking MCP API change** pre-v1; update
the asserting tests (`apps/proxima-mcp/tests/end_to_end.rs`,
`crates/mcp-server/tests/streamable_http_pg.rs`).

**Greenfield: rename the legacy ids clean.** No data and no external consumer
depend on these ids, so drop the `proxima-agent-memory/*` prefix → `core/*`:
schema ids (`core/agent-note-v1`, `core/agent-derivation-v1`,
`core/utterance-v1`, `core/agent-link-v1`), relation (`core/agent-link-refers-to`),
source ids (`core/agent`, `core/conversation`). This removes the only reason the
cutover looked complex — afterward `proxima_agent_memory`/`proxima-agent-memory`
appear nowhere.

### Schema relocation

Move `proxima_agent_memory.{agent_note_v1, agent_derivation_v1, utterance_v1,
agent_link sidecar}` + their indexes (note: `utterance_v1` has **no** index —
only table/PK/FK) into `proxima_core`. **Migration caveat (cdx):** SQLx tracks
applied migrations, so editing `0001_init.sql` only reaches **fresh** DBs. That
is correct here — pre-v1, fresh-DB cutover, no applied prod `0001`
(`project_strassberger_migration_strategy`). If any environment already has
`0001` applied, use a new `0002_*` instead. Drop `flavors/agent-memory/migrations/`.

### Handle-based I/O

Canonical tools take/return handles (prefixed ids / session handles), not raw
UUIDs (`project_no_uuids_in_model_context`); the subsumed UUID-based tools
(`search_memories`, `walk_memory_lineage`) get handle-fronted via the existing
`HandleTable` (`crates/core/src/mcp/handles.rs`).

## Constraints from review (apply throughout)

- **Greenfield: wholesale move, verify the end state.** No incremental
  backward-compatible steps, re-export shims, or atomic-cutover choreography to
  keep intermediate commits green — do the move and gate on a final green
  `cargo build --workspace` + `cargo test --workspace`. (The engine verbs land
  first only because the tools depend on them, not for incremental safety.)
- **Delegated execution:** per `AGENTS.md`, a delegated worktree agent must
  **not** `git add`/commit — the plan's steps express verification, not commits;
  the human/orchestrator commits.
- **Tool count:** core default registry asserts a substrate-tool count
  (`flavor.rs:769-803`, currently 25). Net = **+7 added** (`core/remember`,
  `core/record_utterance`, `core/derive`, `core/link`, `core/search_graph`,
  `core/get_memory`, `core/trace`) **−2 removed** (`core/search_memories`,
  `core/walk_memory_lineage`) → **30**. Confirm the exact number by running the
  registration test (whether `emit_*` sit inside this count or are registered
  separately changes nothing about the +7/−2 delta).

## Consumer / composition impact

Rewire every `proxima_agent_memory` / `proxima-agent-memory` reference:
`apps/proxima-mcp` (register + migrator + `args.rs` help text + `end_to_end`
tests, `apps/proxima-mcp/src/lib.rs:16-24`), `tools/dev-migrate`
(`src/main.rs:29-35`, `Cargo.toml`), test deps in `crates/proxima`,
`crates/mcp-server`, `crates/storage-pg`, and Strassberger `Apps/backend`
(cross-repo follow-up; uses no agent-memory retrieval in prod → near-zero
functional impact). Agent-memory is composed into every binary by default once
it is core.

## Invariants preserved

- Append-only; owner scope; F/A/P layering; citation model; read-scope behavior
  (Spec B changes read-scope). Engine becomes the single memory-write authority.
- Same writes/reads; the changes are structural (verb layer + relocation) plus
  the one behavioral addition (derive embeds).

## Testing strategy

- The moved agent-memory tests stay green **after** their imports, tool names,
  and asserted schema ids are updated to the new `core/*` values. pg-testkit /
  TCP PG.
- New: `derive` writes an embeddings row in the same tx (the behavioral delta).
- Registration test updated (list + count = 29). Full workspace build + test
  green; `grep proxima_agent_memory` → none anywhere (crates/, apps/, tools/, docs/).

## Resolved decisions (2026-06-15)

1. **Approach — Option B (engine-verb abstraction)**, chosen for long-term
   cleanliness (greenfield; effort is not the metric). Tools call Engine verbs,
   not storage-pg; storage swappable behind the Engine; no cycle.
2. **Namespace — `core/*`** (supersedes the earlier `proxima_*` pick, which
   only made sense while these were a separate flavor palette). One uniform
   prefix, no special-casing.
3. **A/P authoring — keep both roles:** `core/derive` = `ExternalAgent` MCP
   authoring (embeds in-tx via the new Engine verb); `core/emit_*` = internal
   consolidation authoring. Subsumes Spec B's embed-on-derive.
4. **Framework model — no marketplace.** Agent-memory is substrate; Proxima is
   a framework (core + flavor crates composed into an app); no marketplace, no
   runtime registration. Supersedes [[project_flavor_marketplace]] /
   [[project_tool_marketplace]] (RETIRED). Domain flavors (Code, invoice) stay
   flavors; generic agent memory is core.
