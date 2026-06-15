# 2026-06-15 — Memory surface into Core (promote + unify) — Spec A

- **Status:** design — decisions resolved 2026-06-15; pending implementation plan
- **Branch:** `agent-memory-recall-findability` (off `road-to-v1`)
- **Scope owner:** Heinrich
- **Spec B (follow-on, blocked on this):**
  `2026-06-15-agent-memory-recall-findability-design.md` — recall findability,
  rebased onto the canonical core surface this spec produces.
- **Related:** `docs/02-memory.md`, `docs/08-core-and-flavors.md`,
  `docs/14-protocol-surface.md`, memories `project_brain_hub_contraction`,
  `project_substrate_first_goal`, `project_flavor_marketplace`,
  `project_no_uuids_in_model_context`, `project_strassberger_migration_strategy`.

## Problem

There are **two overlapping agent-memory surfaces**:

- **Core primitives** (already in `crates/core`): `core/emit_abstraction`,
  `core/emit_perspective`, `core/search_memories`, `core/walk_memory_lineage`,
  `core/facts_citing_object`, `Engine::ensure_fact_embedding`
  (`crates/core/src/flavor.rs:791-794`, `crates/core/src/mcp/core_tools/`).
- **The `agent-memory` flavor** (`flavors/agent-memory/`, separate crate
  `proxima-agent-memory`, separate schema `proxima_agent_memory`): a parallel
  ergonomic layer — `proxima_remember`, `proxima_derive`, `proxima_link`,
  `proxima_search_graph`, `proxima_open`, `record_utterance`.

These do the same job two ways (e.g. `proxima_search_graph` vs
`core/search_memories`; `proxima_derive` vs `core/emit_*`), with subtle
behavioral divergence (derive doesn't embed; emit does — see Spec B). The
flavor is **mis-filed**: generic agent long-term memory is *substrate*, not a
domain flavor. The brain-hub contraction defines Proxima as exactly this —
memory + per-agent history + personality config. Domain flavors (Code, the
invoice flavor) are the real flavors; agent-memory is core.

This spec promotes the agent-memory surface into core and collapses the two
surfaces into **one canonical core memory palette**. It is a
relocation + dedup + naming change — **no new retrieval features** (those are
Spec B).

## Goal & non-goals

**Goal:** one canonical, handle-based core memory tool surface; the
`proxima_agent_memory` schema and tools live in core; the substrate/flavor line
is sharpened (substrate = core memory; flavors = domain). Behavior-preserving
except where two surfaces are deduped to one.

**Non-goals (Spec B, or out entirely):**

- Embed-on-derive, pgvector/HNSW, durable embedding jobs, read-scope-matrix
  fix, new search filters (tags/time/order), recency lever. All Spec B.
- Any change to graph semantics, F/A/P layering, citation model.
- New domain flavors.

## Decisions settled (at brainstorm)

- **Agent-memory → Core**, as substrate (not a reference flavor).
- **Two specs**, this one first.
- **Handle-based I/O is canonical** for the unified surface (no UUIDs in model
  context, per `project_no_uuids_in_model_context`). Where a core/* tool is
  UUID-based today, the canonical tool is handle-based.

## Design

### Crate & module

Fold `flavors/agent-memory/src/*` into `crates/core` (alongside the existing
`crates/core/src/mcp/core_tools/`), and **delete the `proxima-agent-memory`
crate**. Core already hosts the equivalent tools, so a sibling crate adds no
isolation value and forces the awkward flavor-composition dance for what is
substrate.

→ *Open decision #3: fold directly into `crates/core` (recommended) vs. a new
core-owned `crates/memory` crate that core depends on (more separable, more
churn).*

### Schema relocation

Move `proxima_agent_memory.{agent_note_v1, agent_derivation_v1}` and their GIN
search indexes (`idx_agent_note_v1_search`, `idx_agent_derivation_v1_search`)
into `proxima_core`, folded into `crates/storage-pg/migrations/0001_init.sql`;
drop `flavors/agent-memory/migrations/`. **Window-sensitive and cheap right
now:** migrations were just squashed to a single `0001`, and the Strassberger
rollout is a fresh-DB cutover (no data migration; SharePoint is SoR, PG is a
rebuildable mirror — `project_strassberger_migration_strategy`). Relocating
before any prod data exists avoids a data migration later.

### Tool-surface unification

Collapse to one canonical palette. Proposed mapping (old → canonical):

| Capability | Today (two ways) | Canonical | Note |
|---|---|---|---|
| Append agent Fact | `proxima_remember` | keep (`remember`) | No core equivalent; `core/emit_*` are A/P only. |
| Chat utterance Fact | `record_utterance` | keep | Agent-memory specific; stays. |
| Author A/P | `proxima_derive` **and** `core/emit_abstraction`/`emit_perspective` | **one ergonomic handle-based author** | The overlap. See open decision #1. |
| Search memories | `proxima_search_graph` **and** `core/search_memories` | `proxima_search_graph` absorbs it | Richer (hybrid + neighbor edges + sidecar-aware); inherits `core/search_memories`'s `kind`/`schema`/`reader` filters. `core/search_memories` removed. |
| Lineage | `core/walk_memory_lineage` | one handle-based lineage tool | Converge; Spec B's `proxima_trace` is just this, renamed/handle-fronted. |
| Open by handle | `proxima_open` | keep | No core equivalent. |
| Author edge | `proxima_link` | keep | Ergonomic wrapper over edge append. |
| Citation lookup | `core/facts_citing_object` | keep (handle-fronted) | |

→ *Open decision #1 (the crux): how to dedup A/P authoring.* `proxima_derive`
is `ExternalAgent`-authored, handle-based, takes `source_handles` + `model_id`;
`core/emit_*` are personality-operator-authored and embed transactionally
(`crates/storage-pg/src/verbs/consolidate/memories.rs:534`). Options: (a) one
unified author tool with an authorship param; (b) keep both but make their
roles explicit and non-overlapping (derive = external-agent MCP authoring;
emit = internal consolidation authoring) and ensure derive reuses emit's
transactional embed path (this hands Spec B its fix for free). **Recommend
(b).**

→ *Open decision #2: tool namespace.* Pick one — `proxima_*` (current
agent-memory style, product-branded) or `core/*` (current core style). Mixed is
the status quo we're removing. **Recommend `proxima_*`** for the
agent-facing palette (it's the product surface an external harness sees) with
internal/admin tools staying `core/*`.

### Handle-based I/O

The canonical tools take and return handles (`F:`/`A:`/`P:`/`G:`/`E:` prefixed
ids or session handles), not raw UUIDs. Where unification pulls a UUID-based
`core/*` tool into the palette (search_memories filters, walk_memory_lineage),
its inputs/outputs convert to handles via the existing `HandleTable` /
prefixed-id machinery (`crates/core/src/mcp/handles.rs`).

## Invariants preserved

- No change to graph semantics, F/A/P layering, append-only, owner scope,
  citation model, read-scope matrix behavior (Spec B changes the last).
- This is structural: same writes, same reads, fewer/renamed tools, relocated
  storage. Behavior-preserving except the intentional surface dedup.

## Consumer impact

Strassberger (`Apps/backend`) deps on the `proxima-agent-memory` crate
(`backend/Cargo.toml`) but uses no agent-memory retrieval in production (its
reads go through its own invoice-flavor sidecar views). Blast radius: a
dependency rename (`proxima-agent-memory` → core) + any direct type imports.
Pre-v1, near-zero functional impact. Flag in the rollout checklist.

## Rollout

1. Schema fold into `0001_init.sql`; drop the flavor migration.
2. Move tool modules into core; delete the crate; rewire composing binaries
   (`crates/proxima`, `crates/mcp-server`) and tests.
3. Apply the surface dedup (open decisions #1/#2) and handle-fronting.
4. Update Strassberger's dependency.

## Testing strategy

- The existing agent-memory tests move into core and **stay green unchanged**
  (behavior-preserving is the bar). pg-testkit / TCP PG.
- Tool-surface tests assert the canonical palette (the dedup removed the
  duplicates; remaining tools resolve and round-trip handles).
- Full workspace build + test suite green; no orphaned references to
  `proxima_agent_memory` / `proxima-agent-memory`.

## Resolved decisions (2026-06-15)

1. **A/P authoring dedup — keep both with non-overlapping roles.**
   `proxima_derive` = external-agent MCP authoring (`ExternalAgent`,
   handle-based, `source_handles`); `core/emit_*` = internal consolidation
   authoring. **`proxima_derive` reuses emit's transactional embed path**
   (`crates/storage-pg/src/verbs/consolidate/memories.rs:534`) — which is
   exactly Spec B's embed-on-derive fix, so it lands here as a side effect of
   unification.
2. **Tool namespace — `proxima_*`** for the agent-facing memory palette (it's
   the product surface a harness sees); `core/*` reserved for internal/admin.
3. **Crate placement — fold directly into `crates/core`** (delete the
   `proxima-agent-memory` crate); core already hosts the equivalents.
4. **Framework model — no marketplace.** Agent-memory is **substrate**, not a
   reference flavor. Proxima is a framework: `core` + flavor *crates* composed
   into an app; there is no flavor/tool marketplace and no runtime registration.
   This supersedes the old marketplace framing (memories
   [[project_flavor_marketplace]] / [[project_tool_marketplace]] updated to
   RETIRED). Domain flavors (Code, invoice) stay flavors; generic agent memory
   is core.
