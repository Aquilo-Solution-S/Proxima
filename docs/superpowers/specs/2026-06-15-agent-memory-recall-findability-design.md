# 2026-06-15 — Agent-memory recall findability (passive primitives) — Spec B

- **Status:** design — Spec **B**, depends on Spec A
  (`2026-06-15-memory-surface-into-core-design.md`). Rebased 2026-06-15 onto the
  canonical core memory surface after the cdx review. Pending implementation
  plan (sequenced after Spec A lands).
- **Branch:** `agent-memory-recall-findability` (off `road-to-v1`)
- **Scope owner:** Heinrich
- **Related:** `docs/02-memory.md`, `docs/04-consolidation.md`,
  `docs/07-storage.md`, `docs/14-protocol-surface.md`, memories
  `project-retrieval-surface-state`, `project_brain_hub_contraction`.

## What changed vs the first draft (cdx review, verified against source)

- **Lexical is already GIN-indexed** (`idx_agent_note_v1_search`,
  `idx_agent_derivation_v1_search`, `…baseline.sql:87,101`). "Add GIN" dropped.
  The **only scanning path left is the vector similarity** (`real[]` cosine via
  `unnest`).
- **Embed-on-derive moved to Spec A.** Unification makes `proxima_derive` reuse
  the transactional emit/consolidate embed path
  (`…/consolidate/memories.rs:534`), so A/P findability is fixed there. B no
  longer owns embed-on-derive.
- **Search filters / lineage partly pre-exist.** Spec A's canonical search
  absorbs `core/search_memories` (`kind`/`schema`/`reader`) and converges
  `core/walk_memory_lineage`. B owns only the **genuinely-new** retrieval:
  tags / time / order filters, the read-scope fix, recency lever.
- **Embedding write is an upsert/mutate** (`fact_embeddings.rs:96`) vs
  `docs/07` "new row"; `embedding_version` hardcoded `1`. B reconciles this.

## Problem (post-Spec-A baseline)

With Spec A landed, the canonical core memory loop is one handle-based palette,
A/P embed transactionally on author, and lexical recall is GIN-indexed. Three
findability gaps remain for an LLM building long-term memory:

1. **The vector path still scans and is hand-rolled.** `embeddings.vec` is
   `real[]` with no ANN index; semantic similarity is a `unnest`-based cosine
   with zero-vector/zero-denominator clamps
   (`crates/storage-pg/src/verbs/query/search.rs:249,261`). Correctness is
   fragile and it does not scale past ~10k embeddings/owner.
2. **The async Fact embedding path is best-effort and silent.** Fact ingest
   (`proxima_remember`, `record_utterance`, `event_ingest`) embeds *post-commit*
   with warn-on-failure, no retry, no durable obligation
   (`remember.rs:173-185`, `record_utterance.rs:121`, `engine/ingest.rs:33`).
   A model blip → a permanently unsearchable Fact. (Derive is already
   transactional via Spec A; this is only the inherently-async ingest paths.)
3. **Recall lacks the levers an agent expects.** Even post-A, there is no
   `tags` / `since` / `until` / `order` filter, and the agent-facing search does
   not honor the **read-scope matrix** (it passed `reader = None` / used direct
   SQL pre-A — must be threaded through on the canonical surface).

## Decisions (settled)

- **Binding constraint: recall findability.** **Philosophy: passive, sharpen
  primitives** — reliable embedding + sharper retrieval only; no decision model
  in the loop. The host-injected embedding client is sanctioned retrieval config
  (`docs/04`), not a loop model. Consistent with `project_brain_hub_contraction`.
- **Embedding posture (resolved):** transactional where possible (derive, via
  Spec A) **+ a durable embedding-jobs obligation as the safety net for the
  async Fact ingest paths** + backfill.
- **pgvector now (resolved):** real `vector(D)` + HNSW + `<=>` cosine; assume one
  active embedding model (multi-dim → partial HNSW per `(model_id, dim)`).
- **Read-scope: fix now (resolved):** thread reader-personality context through
  the canonical search.

## Goal & non-goals

**Goal:** anything an agent remembers or derives is reliably and selectively
findable later, at scale, with the substrate running no decision model.

**Non-goals (harness concerns under the passive decision):** dedup/"similar
memory?" hints, auto-derivation/compaction, forgetting *policy* (recency is
exposed as a lever, not a baked decay curve), re-introducing wake operators.

## Design

All tool-arg additions are optional and backward compatible. Two slices.

### Slice B1 — vector correctness + durable Fact embedding

**B1a. pgvector / HNSW / `<=>`.** Install the `pgvector` extension; migrate
`proxima_core.embeddings.vec` `real[]` → `vector(D)`; add an HNSW index; replace
the hand-rolled `unnest` cosine with the `<=>` cosine-distance operator
(ranking = `1 - distance`; define zero-vector behavior to match today's clamp).
Single active model assumed → one `vector(D)` column; multi-model/multi-dim
would use a partial HNSW index per `(model_id, dim)`. Equivalence test: `<=>`
ranking matches the current brute-force cosine within tolerance on a fixture.
Reconcile `docs/07` to the shipped pgvector reality.

**B1b. Durable embedding obligation (async Fact paths).** New operational
side-table `proxima_core.embedding_jobs`, keyed
`(owner_kind, owner_id, owner_org_id, entity_kind, entity_id, model_id,
embedding_version, dim)` — note `embedding_version` + `dim` are in the key
(cdx). Columns: SQL-enum `status` (`pending|processing|done|failed`),
`attempts`, `last_error`, `enqueued_at`, `updated_at`.

- The job is enqueued **in the same transaction** as the Fact write, **only on a
  genuinely new insert** (idempotent replay must not re-enqueue —
  `remember` currently embeds even on replay; `derive` returns early on
  duplicate before sidecar/change_event, `derive_append.rs:74`).
- A host-invoked drainer (same posture as the retention sweep / change_event
  poll — *not* a model loop) claims `pending` rows with `FOR UPDATE SKIP
  LOCKED`, calls the host embedding client, writes the `embeddings` row, marks
  `done`; on failure bumps `attempts` / records `last_error` for retry with
  backoff and a max-attempt cap.
- **Mutate-vs-append:** the existing embedding write upserts the
  `(entity,version,model)` row (`fact_embeddings.rs:96`), contradicting
  `docs/07` "new row". Resolve by **treating embeddings as a derived cache and
  fixing `docs/07` to say upsert** (simplest; embeddings are re-derivable, not
  cognitive graph). Re-embed trigger = `model_id`/`embedding_version` change →
  new obligation key → new/updated row.
- **GC:** A/P now carry embeddings (Spec A). Extend Fact cleanup
  (`fact_cleanup.rs`) so tombstoning/erasing a memory also removes its
  embeddings **and** any open `embedding_jobs`.
- **Backfill:** batched by `(owner, kind, model_id, version)` — any memory
  lacking a current-model embedding gets a `pending` job. Explicitly *not* an
  unbounded O(all memories) scan; bounded per batch.

Error handling shifts from silent warn → observable `embedding_jobs.status`.

### Slice B2 — selective recall

**B2a. New filters on the canonical search** (optional; `kind`/`reader` already
present from Spec A's absorption of `core/search_memories`):

- `tags: Vec<String>` + `tag_match: any|all` (default `any`).
- `since` / `until` — range on `created_at`.
- `order: relevance|recency` (default `relevance`); return `created_at`
  alongside `score`. Recency is a **lever**, not a decay policy.

**B2b. Read-scope fix.** Thread reader-personality context through both the
lexical and semantic canonical-search paths so cross-personality visibility is
enforced by the read-scope matrix (today the agent-facing search passes
`reader = None`). Single-personality owners are unaffected; multi-personality /
company-shared owners get correct gating.

**B2c. `(owner, created_at)` index** on `memories` to back the time filters and
`order=recency`.

## Invariants preserved

- Append-only memory rows; `embedding_jobs` + `embeddings` are derived
  operational/cache tables, never memory edits.
- Owner scoping on every read/write; jobs and all filters owner-keyed.
- F/A/P layering unchanged; the read-scope fix makes search *more* compliant.
- No decision model in the substrate; only the host-injected embedding client.
- All tool-arg additions optional and backward compatible.

## Testing strategy

- **B1a:** `<=>` ranking matches brute-force cosine within tolerance on a
  fixture; HNSW used by the planner (`EXPLAIN`); zero-vector behavior matches
  prior clamp. (pg-testkit, TCP PG.)
- **B1b:** Fact write enqueues a job transactionally; **replay does not
  re-enqueue**; forced-failure path leaves no committed Fact without a job;
  drainer pending→done, failure→retry→cap, `SKIP LOCKED` under concurrency;
  model/version change → new obligation; backfill is batched; tombstone removes
  embeddings + jobs.
- **B2:** each filter (tags any/all, time range, order=recency) returns the
  expected subset and composes with all modes; read-scope — a reader excluded by
  the matrix does not see another personality's A/P in results.

## Slice boundaries

1. **B1** — pgvector/HNSW/`<=>` + equivalence tests; `embedding_jobs` for the
   async Fact paths + drainer + backfill; embedding GC/tombstone; `docs/07`
   reconcile. (Vector path stops scanning; no Fact silently unfindable.)
2. **B2** — tags/time/order filters + recency lever + read-scope reader
   threading + `(owner, created_at)` index.

Both slices presuppose Spec A's canonical surface and its transactional
embed-on-derive.
