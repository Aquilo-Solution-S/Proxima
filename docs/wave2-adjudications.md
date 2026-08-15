# SQL sweep adjudications (v0.0.8 storage wave)

Contradiction rulings, complexity deltas for every rewrite, and findings
deliberately left alone. Finding IDs (S-numbers) refer to the sweep's
findings document; evidence lives there, by pointer only.

## Everything here ships unconditionally

An earlier draft of this work put three of the rewrites behind
environment variables so the measurement loop could A/B them. That is
gone. Settings that select between two SQL shapes rot: the losing arm
stops being tested the moment nobody sets the variable, and the
statement a deployment actually runs stops being knowable from the
source. So each rewrite was decided one way, the loser deleted, and the
survivor is the only text that exists.

The bar that decision had to clear is not the same for every change.
Two classes ship here:

**Provably equivalent** — the rewrite selects the same rows as what it
replaced, for every input, by an argument written down in this document
and pinned by a test. No measurement is needed to justify these; a plan
improvement is a bonus, not the warrant.

- the S26 de-union sites, including the lineage walk's
  `readable_memories` / `start_memory_visible` / `load_nodes` prelude
  (fail-closed removal of a UUID-collision-only arm — see S26),
- the S6/S7 INDF-to-`=` sites (`change_event`, `source_cursors`,
  compliance-erase binds — column- or bind-never-NULL proofs),
- the S4/S6 owner-scope arm split over `memories`/`goals` (disjoint
  arms, per-arm ORDER/LIMIT, outer re-limit),
- the seq high-water read-owner spelling and its per-owner LATERAL
  probe (max-of-per-owner-maxima = global max over the same row set).

**Measured bets** — the row set is provably unchanged, but the plan
shape is a trade the planner could get wrong on a corpus we have not
seen. These carry a benchmark number in their doc comment and are
revertible as a unit.

- the S2 claim arm split (3,090 buffers / 37.6 ms → 6 buffers /
  0.040 ms to claim one job on a 200k-row queue),
- the S5 edge-id prefilter, whose lineage arm is complexity-*shifting*
  rather than flattening (see the honesty note under S5).

None of the shipped statements is byte-identical to the wave-1 base
(`21f8cc0b`). Anything comparing against that base must use this
section, not a per-site "unchanged by default" claim.

## S2 — embedding-job claim (arm split)

Ruling: the claim's logical shape (`FOR UPDATE SKIP LOCKED` queue
claim) is textbook and both status arms of it are kept verbatim; the
*plan* was independently defective (no model-scoped index, one
whole-backlog sort across a two-arm status `OR`), and the caller's
one-row claim loop was a separate multiplicative defect. The shipped
statement gives each status arm its own ordered scan over its own
partial index (`idx_embedding_jobs_pending_claim` /
`idx_embedding_jobs_processing_reclaim`, migration 0018), merges them
with `UNION ALL`, and re-limits. `FOR UPDATE` sits inside each arm
sub-select — PostgreSQL rejects a locking clause applied to the union
itself.

Open questions, deliberately not resolved here:

- **Over-locking:** each arm locks up to `$3` rows, so a claim may
  transiently lock up to `2 x limit` rows; the losing arm's locks
  release with the statement's transaction. Fairness effect under
  contention is unmeasured.
- **Batch reclaim window:** the inline drain claims one whole batch
  instead of claim-1-per-iteration, which widens the window in which a
  crashed worker's `processing` rows wait for the stale timeout.
  Measured trade, not a correctness issue.
- **Duplicate release semantics:** the batch drain releases
  already-embedded duplicates with an explicit note string; whether the
  note should be an enum-typed reason column is open.
- **Reclaim index vs the claim ORDER BY (adversarial review):**
  `idx_embedding_jobs_processing_reclaim` is `(model_id, updated_at)
  WHERE status = 'processing'` — it serves the staleness cutoff as an
  index range condition but NOT the reclaim arm's
  `ORDER BY enqueued_at, owner_kind, ...`, so that arm still top-N
  sorts the stale-processing rows it finds. The pending arm alone gets
  the sort-free scan (its index carries the full ORDER BY). Open
  design question: should the reclaim index instead carry the ORDER BY
  columns `(model_id, enqueued_at, owner_kind, owner_id, entity_kind,
  entity_id, embedding_version)` with `updated_at` left residual? The
  stale-processing set is normally tiny (crashed workers only), so the
  sort may be cheaper than the wider index — measure, don't guess. The
  0018 comment states the asymmetry instead of overclaiming.

## S26 / de-union — memory-keyed readers now probe `memories` directly

Adjudication (external review, applied here): keep the
`entity_owner_union()` helper for the genuinely polymorphic sites
(edge visibility in `query/edges.rs`, `consolidate/events.rs`,
`change_event.rs` — an edge endpoint or change-event target can be a
goal), but stop routing memory-keyed owner probes through it.

Equivalence argument, shared by every site below: for a probe keyed by
a memory id, the union's memories arm is exactly the probed row (same
PK), and the goals arm can match only if a `goal_id` collides with a
`memory_id`. Both are independently generated v4 UUID primary keys; a
collision match would be a bug rewarded, not a feature — removal is
fail-closed. The union carries no extra filters (no tombstone
predicate), so no semantics ride on it. Operator spelling (`=` vs
`IS NOT DISTINCT FROM`) is preserved byte-for-byte per site. Prior art:
`query/search.rs` `push_read_owner_scope` already reads the memory
row's own owner columns for this exact reason (wave-1, identical result
sets measured).

Complexity deltas (nesting = subquery depth of the owner probe; scans =
same-statement scans of `memories`+`goals` attributable to the probe):

| Site | Nesting | Probe scans | Semantics note |
| --- | --- | --- | --- |
| `fact_embeddings/text.rs` load_fact_text | 2 → 0 (EXISTS deleted) | 2 → 0 | `=` kept; row's own columns |
| `fact_embeddings/text.rs` load_embedding_text | 2 → 0 | 2 → 0 | same |
| `fact_embeddings/text.rs` load_fact_text_in_tx | 2 → 0 | 2 → 0 | same |
| `fact_embeddings/jobs.rs` list_facts_missing_embedding | 2 → 0 | 2 → 0 | `=` kept |
| `fact_embeddings/jobs.rs` enqueue_missing_embedding_jobs | 2 → 0 | 2 → 0 | `=` kept |
| `mcp_call_history.rs` read_mcp_call_history | 2 → 0 | 2 → 0 | INDF kept verbatim |
| `derive_append.rs` validate_supersedes_in_owner | 2 → 0 | 2 → 0 | `=` kept |
| `consolidate/memories.rs` load_batch_facts_by_id | 2 → 0 | 2 → 0 | `=` kept |
| `consolidate/memories.rs` load_abstraction_heads | 2 → 0 | 2 → 0 | `=` kept |
| `consolidate/memories.rs` load_memory_by_id | join deleted | 2 → 0 | LEFT JOIN projected owner columns the caller discarded; row set unchanged (join could only duplicate on PK collision) |
| `consolidate/memories.rs` load_memories_by_ids | 3 → 2 | 2 → 0 | read-set unnest EXISTS anchors on `m`; INDF via `read_owner_predicate` kept |
| `citations.rs` facts_citing_object | 3 → 2 | 2 → 0 | same shape as above |
| `citations.rs` citation_of_entity_head | 3 → 3 | 2 → 1 | probe key is `fe.current_memory_id` (not in scope); probes `memories` by PK instead of the union |
| `lineage.rs` start_memory_visible | 3 → 2 | 2 → 0 | read-set EXISTS anchors on `m` |
| `lineage.rs` load_nodes | 3 → 2 | 2 → 0 | same |
| `active_goals.rs` perspective probe (`$3`) | 3 → 3 | 2 → 1 | memory-keyed; probes `memories` by PK; INDF kept |
| `active_goals.rs` goal probes (g0/child/g/newer) | 3 → 2 each | 2 → 0 each | goal-keyed; the probed id is the in-scope goal row's own PK, so the probe reads that row's owner columns; INDF kept |

The lineage walk's `readable_memories` CTE was de-unioned on the same
argument and then deleted outright by S5, which restructured the
prelude away entirely.

Not de-unioned (still polymorphic, union retained by adjudication):
`query/edges.rs` read_edges + `weo` world probe (edge endpoints can be
goals), `consolidate/events.rs` edge visibility, `change_event.rs`
edge-target visibility, and `query/rows.rs` / keyset pagination
`read_owner_predicate` sites that anchor on their own table already.

## S5 — edge-id prefilter

Three read paths filtered edges on RESOLVED endpoint ids
(`COALESCE(fe.current_memory_id, e.source_id)`), which no btree over the
base columns can serve — every edge resolves before any filter applies.
The shipped statements prefilter on the raw endpoint columns plus
`head_probe` (the fact-entity ids currently heading each requested
memory, riding `idx_fact_entities_current_memory` from migration 0017)
and keep the original resolved-column predicate verbatim as the
residual, so the prefilter can only ever admit a superset of what the
residual then filters. Exact-superset argument in each statement's doc
comment.

Complexity deltas:

| Site | Shape delta | Scan delta |
| --- | --- | --- |
| `ports/memory.rs` neighbor window | +1 CTE (`head_probe`), predicate split into indexable prefilter + verbatim residual | edges: full scan → bitmap over `idx_edges_source`+`idx_edges_target` (plan-pinned under default costing with an 8k crowd) |
| `query/edges.rs` snapshot closure | same | edges: full scan → endpoint-index bitmap (planner picks the cheaper side of the AND) |
| `query/lineage.rs` walk (both directions) | −3 materialized CTEs (`readable_memories`, `edge_endpoints`, `edge_heads`); +1 per-step LATERAL probe and 3 per-row EXISTS probes | was: scan all of `memories` and all origin `edges` before the recursion starts; now: touch only rows reachable from the walked ids |

Honesty note: the lineage rewrite is complexity-*shifting*, not strictly
flattening — it trades three whole-table CTE materializations for
per-step probes, and the per-row EXISTS probes repeat per edge. On a
small graph inside a large corpus that is a clear win; on a walk that
touches a large fraction of `memories` it can lose. The row sets are
pinned by `edge_prefilter_pg.rs` (including the Fact-entity-head cases
and the World-source redaction case below), so a revert is a shape
decision, never a correctness one.

The walk's world-visibility probe reads `memories` directly rather than
the entity-owner union: the walk filters Goal endpoints out before the
probe, and a dangling head id matches neither the union nor `memories`
— the same fail-closed argument as the de-union sites.

The redaction rule the prefilter must not disturb: the walk drops an
edge when `source_world_visible AND NOT target_readable`. The prefilter
runs *before* that residual, so losing the predicate would return rows
rather than drop them — a disclosure, not a miss.
`a_world_sourced_edge_is_redacted_when_its_target_is_unreadable` pins
it, with a second World-to-World edge as the positive control.

Two things that fixture had to get right, both worth recording:

- **A World memory acquires outgoing edges only by publication.**
  `validate_edge_invariants` requires an edge's owner to equal its
  SOURCE endpoint's owner, and `edges_world_not_write_owner_chk`
  forbids a World edge owner — so no edge can ever be written out of a
  World-owned memory. The reachable state is the one
  `publish_to_world` creates: the edges are written while the memory is
  still personally owned, and the owner move happens afterwards. That
  is exactly the corpus the redaction rule exists for, and a fixture
  that seeds a World-owned source directly does not compile past the
  trigger.
- **The rule protects existence, not identity.** With the predicate
  neutered (verified by mutation: force the world-visibility probe to
  FALSE and the test fails 2 ≠ 1), the extra edge still comes back with
  its target projected as `Redacted`. What leaks is that a published
  memory HAS an unreadable ancestor. So the assertion is that no such
  edge is returned at all — a check for a visible secret target would
  have passed the mutant.

## S6/S7 — INDF → `=` where equivalence is provable

Two proof shapes, both making the spelling change semantics-free while
restoring the `(owner_kind, owner_id, ...)` index prefixes INDF defeats
(PostgreSQL has no index strategy for `DistinctExpr`):

1. **Column never NULL** (`change_event` via its
   `owner_ref_shape_chk` + `world_not_write_owner_chk`;
   `source_cursors.owner_id` is NOT NULL): `col = x` and
   `col IS NOT DISTINCT FROM x` select identical rows for every `x`,
   including a NULL read-set id (the World member), which matches nothing
   under either spelling. Sites: `query/rows.rs` `read_seq_high_water`
   (now via the new `read_owner_equality_predicate` helper),
   `change_history.rs`, `consolidate/events.rs` (2), `source_cursors.rs`
   (2 reads).
2. **Bind never NULL** (`compliance_erase.rs`, 16 sites): every erase
   entry point constructs its owner as `OwnerRef::Group`/`Personal` from
   a typed id — World is not representable on this path — so the
   `owner_id` bind is non-NULL and INDF ≡ `=` regardless of column
   nullability (a NULL column row fails both).

Complexity delta: zero shape change — operator spelling only; per-site
nesting and scan counts unchanged.

Deliberately NOT swapped (journaled per S7's "keep two-arm only on
genuinely World-tolerant read paths"): `fact_embeddings/write.rs`,
`ops.rs`, `close_batch.rs` residual-filter INDF sites (pkey-anchored or
residual after an indexed anchor — no plan to win), `mcp_call_history.rs`
(single-owner read that may legitimately be handed a World owner), and
every `read_owner_predicate` site over `memories`/`goals` (both tables
are World-tolerant since 0008_v005; those move under the S4/S6 arm
split below instead).

Deferred from S6: joining `query_memories`'s sequential round trips
(memories page, goals page, edges, high-water) with `tokio::try_join!` —
a Rust-side change with no SQL shape delta.

## S4/S6 — Query owner scope (arm split)

The Query verb's owner scope reads two World-tolerant tables:
`memories` and `goals` dropped their `*_world_not_write_owner_chk`
constraints in `0008_v005`, so a World row carries `owner_id IS NULL`
and plain `=` against a NULL read-set id silently drops it. The INDF
spelling there was load-bearing, not accidental — which is exactly why
it could not simply be swapped.

The shipped shape respells the read-owner join as
`owner_kind = s.kind AND owner_id = s.id` and appends the page body once
more as a constant `owner_kind = 'world' AND owner_id IS NULL` arm, but
only when World is actually in the read set. Prior art:
`search.rs::push_read_owner_scope` already ships this split in-tree, and
it is the S4 shape the sweep measured at 74–239x. Arms are disjoint (an
equality join never matches the NULL-id World member; the World arm's
rows match no non-World member), each arm keeps its own ORDER/LIMIT, and
both arms are parenthesized so those clauses bind to their branch rather
than the union (PostgreSQL docs §7.4 "Combining Queries") — with the
outer LIMIT re-applied over the merged result.

| site | replaced | shipped | complexity delta |
| --- | --- | --- | --- |
| `query/memories.rs` page | 1 INDF lateral, nesting 3 | equality lateral + optional World arm, nesting 4 | +1 nesting, +1 same-table scan (World in read set only); statement text duplicates the page body once |
| `query/goals.rs` page | same | same | same |
| `query/rows.rs` `read_seq_high_water` | whole-table `EXISTS` walk (post-S6 `=` spelling), 1 `change_event` scan | per-owner `LATERAL ... ORDER BY seq DESC LIMIT 1` probes, 1 probe per read-set member | nesting +1; max-of-per-owner-maxima = global max over the same row set, so semantics unchanged |

Test coverage in `tests/integration/owner_scope_pg.rs`: the three
shipped statements are byte-pinned (`the_memory_page_sql_is_pinned`,
`the_goal_page_sql_is_pinned`, `the_high_water_sql_is_pinned`); the
World arm is asserted to appear only for a World-containing read set;
a published row is asserted reachable exactly when World is in the read
set; the high-water probe is asserted to reach every read-set member;
a mixed Personal+World corpus where each arm alone can fill the page
pins the global top-N (the case that catches dropping the outer LIMIT
after `UNION ALL`); and the page is plan-pinned to ride an
`(owner_kind, owner_id, ...)` memories index under DEFAULT costing with
a 20k-row crowd (the S36 trap: a one-row fixture with seqscan disabled
proves capability, not the plan the corpus gets).

## Migrations 0017 / 0018 — blocking index builds

Both are plain `CREATE INDEX`, so each build takes a `ShareLock` and
blocks writes to its table for the duration. sqlx runs each migration
file inside a transaction and `CREATE INDEX CONCURRENTLY` cannot run in
one, so a concurrent build is not expressible in this lane today.
Operators upgrading a large `memories` or `change_event` should expect a
write pause. Both file headers say so; making the lane able to express
an out-of-transaction migration is an open item recorded in
`docs/how-to/migrations.md`.

Per that same policy these are drafts under the v0.0.8 cycle: they get
squashed into one frozen file under a fresh version number at release
prep, and a version number is never reused (a checksum mismatch surfaces
as `VersionMismatch(N)` and nothing suppresses it).

## `text.rs` owner gate and the World owner (open question, no change)

`load_fact_text` / `load_embedding_text` / `load_fact_text_in_tx` gate
on `owner_id = $3` (plain `=`). `owner_binds` emits a NULL id exactly
for `OwnerRef::World`, and `NULL = NULL` is not TRUE — so a
World-owned row can never be read through these functions, before or
after the de-union (the rewrite preserved `=` byte-for-byte). Open
question for the callers' owners: does any embedding path ever run with
the World owner? If yes, the gate should become the two-arm World-aware
shape `push_read_owner_scope` uses; if no, a comment pinning the
invariant would close this. Not changed here — semantics preservation
was binding.

## S23 — `idx_embeddings_owner`

Adjudicated: do **not** drop. Nothing measured here shows the index
costing anything, and dropping an index is the one move in this sweep
that cannot be walked back without a rebuild on a live cluster. It
stays until something measures it as dead weight.

## S3 — the two RI-only indexes in 0017

`idx_memories_citation_mapping` and `idx_memories_source_batch` were
challenged as serving no SELECT on this branch. They stay: `compliance/
code_repo_erase.rs` deletes from both `citation_mappings` and
`source_batches`, so PostgreSQL's referential-integrity checks are live
readers of exactly these FK-referencing columns. That is S3's stated
purpose, and it is a real code path, not a hypothetical one.

## S32 — `memories_one_fact_per_receipt`

Adjudicated: **keep as-is**; skip the partial-index variant. The
constraint doubles as a race signal: `fact_ingest.rs::is_receipt_race`
matches unique violations by this constraint's name, and a partial
unique *index* raises a differently-shaped violation surface than the
table constraint that matcher was written against.

## Cosmetic — enum literal spelling (zero performance claim)

`fact_embeddings/{jobs,ops,reconcile}.rs` now spell enum literals bare
(`'pending'`, `'failed'`, `IN ('Abstraction', 'Perspective')`) wherever
the column context determines the type, keeping explicit
`::proxima_core.*` casts only in untyped contexts (UNION-arm
projections, CASE/COALESCE mixing). Every rewritten statement was
PREPARE-verified on the scratch cluster. This is a spelling
unification, not a performance change, and must not be sold as one.

## Standing rule — no stringly-typed state

Audited: zero TEXT closed-vocabulary columns exist in the core schema
(`sql_enums_pg::core_closed_vocab_columns_use_sql_enums` pins this).
Any future finding that tempts a TEXT state column becomes an entry
here proposing a native enum — noting the `ALTER TYPE ... ADD VALUE`
constraints (no transactional add-then-use in one migration; no value
removal) — never an inline fix.

## Dynamic-SQL ratchet

`scripts/check-sql-policy.py` passes at 66 sites, down from 74 at the
wave-1 base. The sweep removed 12 (five de-unions, three
`text.rs` INDF sites, two `query/goals.rs` `push_str` sites, two
`jobs.rs` `format!` sites) and added 4, all test-only plan guards —
zero new production dynamic-SQL sites. The two wave-1 sites this
document previously listed as pre-existing failures
(`fact_ingest_batch.rs` free-form proof comment, `query/search.rs`
`push_str` without adjacent proof) are settled in-tree; the checker is
green with no suppressions.

## What the adversarial review left open

An external adversarial review found no row-set divergence in any
rewrite, confirmed the de-union loses nothing, and rated the
INDF-to-`=` proofs airtight. Its contract and documentation findings are
remediated above. Of its four design findings, three are now closed —
the flags-vs-unconditional split (the flags are gone; see the top
section), the transactional `ShareLock` on 0017/0018 (disclosed in both
migration headers and above), and the RI-only indexes (S3 above, kept
on evidence). One stands:

**Lineage walk density.** The rewrite trades three whole-table CTE
materializations for per-step LATERAL probes plus four per-row EXISTS
fragments. It is the one shape in this wave whose win depends on the
corpus rather than on an argument. If a benchmark on a walk-heavy graph
shows it losing, the revert is `query/lineage.rs` alone and the pinned
row sets make the revert safe.

## Lexical GIN-first — migration 0019 and the split lexical branch

The largest single finding of the wave, and the last to land. It is
recorded here rather than as an `S`-number because it is not a sweep
item: it is a decision two earlier migrations made deliberately, on a
premise that stopped being true.

### What was wrong

`memories`, `agent_note_v1` and `agent_derivation_v1` have carried a
`STORED` generated `search_tsv` column since migration 0011, with no
index on any of them. That was on purpose. `0009_v006.sql` had dropped
the v0.0.6 GIN indexes because the read path matched `c.search_tsv`
against an owner-scoped `candidates` CTE and no index on a base table
can serve a predicate applied to a CTE result. `0011_v007.sql` repeated
the reasoning and added a second argument:

> owner-first enumeration already reduces a search to a few hundred rows
> before any text predicate runs

The first argument was correct and remains correct. The second is false
for a single-owner deployment, where the owner scope *is* the table —
and that is the default shape of a personal memory system. Measured
there, every lexical search read the whole owner scope and spilled a
tsvector per candidate row to disk.

### Why the index alone is worthless, and the pushdown almost so

Two measurements decided the design, both on a ~10^5-row single-owner
corpus of conversational text, relative to the shipped statement:

| | lexical mode | hybrid mode (product default) |
|---|---|---|
| GIN index added, statement unchanged | 1.00 | 1.00 |
| predicate pushed to the base tables, all three arms in one gate | 0.88 | 0.82 |
| predicate pushed down, tsquery arms only | 0.22 | **0.0004** |

The first row is what 0009 and 0011 already said. The second is the one
that decided the shape: `LIKE '%…%'` has no index in core, and a planner
facing `search_tsv @@ q OR lower(text) LIKE '%…%'` must scan the owner
scope for the second arm, so it never chooses the GIN path for the
first. Splitting the substring arm into its own statement is not a
tidiness preference — it is the entire difference between the index
being live and being write amplification.

Pushdown on its own is still worth something: the branch set stops
materialising a tsvector per candidate row, and a 100+ MB tuplestore
spill disappears. That is the 0.88/0.82 row, and it is all the index
would have bought without the split.

### What the substring arm is actually for

Counted per query class on the same corpus — rows the substring
predicate finds that neither tsquery does:

| query class | rows only `LIKE` finds |
|---|---|
| natural multi-word ("thrift store clothing haul", …) | 0, in every case measured |
| single word | 8 of 313 |
| all-stopword ("what is the") | 1,145 — the only arm that fires |
| partial word ("ustainab") | 4,551 — the only arm that fires |

So it is not a general recall contributor; it is the fallback for
queries the tsquery cannot express, exactly as the builder's own
comment says. Both of the cases where it carries the whole search share
a property: the tsquery arms return few or no rows. That is what makes
the arm skippable without a recall trade — the skip condition is a
faithful statement of what the arm is for, not a heuristic.

### The rule

A substring-only row scores a flat `0.25` and nothing else, so its final
score is `SUBSTRING_BAND` in lexical mode and
`(1 - semantic_weight) * SUBSTRING_BAND` in hybrid. If the candidates in
hand already hold `fetch_target` rows scoring *strictly* above that, no
row the substring statement could return can reach the page, and the
statement is skippable with no row changing. Three conditions are
checked rather than assumed, and each was a real hole in the first draft
of this design:

- **Recency order breaks it entirely.** The argument is about score
  order; `SearchOrder::Recency` pages by `created_at`, so the newest
  substring-only row outranks every tsquery hit. Never skipped there.
- **Width is `fetch_target`,** the page plus its has-more probe plus a
  relevance cursor's already-emitted rows — not `req.limit`.
- **Hybrid counts fused scores,** which is what lets it skip at all: a
  corpus with embeddings fills the page from the semantic leg well above
  `0.4 × 0.25`. Hybrid's lexical leg has no rescue arm, so on its own it
  usually cannot.

The band also has to reach fusion for rows the semantic leg returned and
the tsquery gate excluded. That is a column on the rank-first semantic
statement, evaluated over rows it was reading anyway, OR-ed per branch
with a window function because `eligible_entities` collapses a memory's
branches to one row and would otherwise lose a base-text match to a
sidecar projection that has none. The escape-hatch semantic statement is
frozen and cannot carry it, so a hybrid search under
`PROXIMA_PG_SEMANTIC_INDEX_FIRST=off` always reads the substring
statement instead.

### What it costs

A second round trip under its own snapshot, whenever the fallback fires.
Hybrid has read under two snapshots since its legs became concurrent and
the anomaly has the same shape — every row returned existed during the
search, and the page is re-ranked from the union in one place. It is not
the same guarantee `rank_first_semantic_branch_sql` documents for
keeping its scan and eligibility check in one statement; that one is
about a scan and its own filter disagreeing about liveness, which two
independent candidate sets merged by id cannot do.

When the fallback does fire, the search costs roughly what it used to.
The trade is that it fires on the queries that were always going to need
a scan, and not on the rest.

### What is not indexed, and why

`interpretation_v1` declares no `tsv_column`, so the builder tokenises
its projection inline and no index on the raw table could match that
expression — the brittleness 0009 deleted. Its `UNION ALL` arm scans
that one sidecar; the arms are planned separately, so it does not cost
the `memories` and note arms their indexes. Giving it a stored column is
a separate change with a table rewrite attached.

### The one thing holding it in place

`search_pg::plans::the_lexical_gate_is_served_by_the_search_tsv_index`.
Both spellings of the gate return identical rows, so moving it back
above the branch set is silent: the whole suite stays green and the
default mode loses three orders of magnitude. The guard's corpus gives
the owner 20,000 rows and the query term five of them, so the two plans
are not close — the lesson the sibling rank-first guard records after a
small fixture let a planner tie-break flap between CI runs.
