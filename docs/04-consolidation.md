# 04 — Consolidation Operators

When a real-world event reaches Proxima, including its Facts into the
agent's internal world happens in **two distinct phases**:

| Phase | What it does | Cost | Cadence |
|---|---|---|---|
| **1. Graph embedding** | Materialize fact memories + structural edges from the event payload | Cheap, deterministic | Synchronous on event ingestion |
| **2. Personality embedding** | Build Abstractions and Perspectives via F→A and A→P operators | Expensive, LLM-driven | Eager per batch / dream cycle |

Phase 1 is a precondition for Phase 2. A Fact must exist in the graph before
consolidation operators can find it. The two phases use disjoint code paths, run on
different schedules, and have different failure semantics.

## Phase 1 — Graph embedding

The event arrives, conforming to a registered schema. The engine:

1. Validates the payload against the schema.
2. Inserts the shared `memory` row + the typed sidecar row.
3. Asks the EventSource to declare any **structural edges** the payload
   implies, given knowledge of its own structure.
4. Persists those edges with `Authorship::EventSource(source_id)`.

The EventSource is the only thing that can author Phase-1 edges, because
the structural relations are payload-encoded:

| Source | Structural edges authored (relation ids) |
|---|---|
| Forgejo commit   | `code/parent-commit`, `code/commit-author`, `code/touches-file` (one per changed file) |
| Telegram message | `chat/in-thread`, `chat/replies-to`, `chat/message-author` |
| Email            | `mail/in-thread`, `mail/in-reply-to`, `mail/sender` |
| PDF chunk        | `doc/prev-chunk`, `doc/next-chunk`, `doc/in-document` |
| Court ruling     | `law/in-case`, `law/cites-ruling` |

These are not interpretations. The previous chunk *is* chunk N-1 by
construction; the parent commit *is* the SHA in the payload's parent
field. Phase 1 only encodes what the payload directly states.

Every relation id in the table above is a registered
`RelationDescriptor` of class `Structural`, owned by the source's
flavor (see [02 §Relation registry](docs/02-memory.md#relation-registry), [08 §Registration](docs/08-core-and-flavors.md#registration-mechanism)). The engine
rejects an EventSource attempting to author an edge with an
unregistered relation id — there is no ad-hoc-string path.

What Phase 1 does **not** do:

- No similarity edges. Vector proximity is a query-time computation, never
  a stored edge (per the rule we already established: similarity is recall,
  not causation).
- No LLM calls. No interpretation. No abstraction.
- No cross-source inference. If a commit message says "fixes #1234", that
  text-pattern-driven link is consolidation operators' job, not Phase 1's. (A
  payload field that explicitly references an issue ID *can* be a
  structural edge — the line is "is the link in the payload by
  construction" vs. "do we have to interpret".)

### Forward-reference targets and cycles

A structural edge may target a memory that doesn't exist yet — and the
target may *itself* later produce an edge back. The engine resolves both
cases through a single mechanism: **edge buffering by deterministic
target ID**, resolved on every memory insert.

Mechanism:

- When an edge is authored with a target whose deterministic ID has no
  matching memory yet, the edge is stored in `pending_edge(source_id,
  relation, target_deterministic_id, owner, ..)`.
- On every memory insert, the engine scans `pending_edge` for entries
  targeting the new memory's deterministic ID **within the same Owner**
  and resolves them into real edges.
- The resolution is by ID match; no LLM, no heuristics, no ambiguity.
- Cross-owner pending edges never resolve and surface in the queryable
  view as a permanent gap.

This makes ingestion **order-independent**, which matters because:

1. **Out-of-order arrival is normal.** A repo crawler may not visit
   commits chronologically; an email importer may receive replies
   before originals.
2. **Cycles fall out for free.** Function A calls B; function B calls A.
   Whichever fact arrives first leaves its outbound edge buffered. When
   the second arrives, both edges resolve in one transaction. No special
   cycle code path; no SCC collapsing; no "first one wins". Both edges
   land as ordinary structural edges and the cycle is just two facts
   with mutual references.

Worked example (mutual recursion):

```
t=0  Forgejo source emits fact for function A, payload lists calls=[B].
     - Memory A inserted.
     - Edge (A → calls → B) attempted; B has no memory. Buffered.

t=1  Forgejo source emits fact for function B, payload lists calls=[A].
     - Memory B inserted.
     - Insert hook scans pending_edge for target=det_id(B): finds (A,
       calls, B). Resolves to real edge.
     - B's own payload-declared edges processed: (B, calls, A). A exists.
       Edge created directly.

Result: both edges (A→calls→B) and (B→calls→A) exist. The cycle is data.
```

Buffered edges that never resolve (e.g., a commit references a parent
from a repo we never crawl) stay buffered. They are surfaced via a
queryable view (`pending_edge` is a real table) so an operator can decide
to either re-attempt the source or accept the gap. The engine never
silently drops them.

### Properties

- **Idempotent.** Re-receiving an event produces the same `event_id`;
  insert is a no-op.
- **Atomic per event.** The fact + its structural edges land in one
  transaction. No half-ingested events.
- **Boring.** No prompts, no model versions, no creativity.

## Source-batch lifecycle

A source batch is a UUIDv7 the source declares opaquely at emit (Q6,
01). Its lifecycle row in `source_batches` is the F→A trigger gate
and the only place batch state is persisted:

```
source_batches(
    id          pk UUIDv7,             -- == source_batch_id on events / facts
    source_id,
    owner_*,
    opened_at   NOT NULL,
    closed_at   nullable,                -- NOT NULL once source signals batch-complete
)

source_batch_f2a(
    batch_id               pk UUIDv7,       -- FK source_batches.id
    operator_id            pk OperatorId,    -- the F→A operator that ran
    prompt_version         pk PromptVersion,
    model_id               pk ModelId,
    personality_id         pk PersonalityId,
    personality_state_hash pk Hash32,
    head_memory_id         nullable,         -- latest output row for this invocation
    run_at                 NOT NULL,
    PRIMARY KEY (
        batch_id,
        operator_id,
        prompt_version,
        model_id,
        personality_id,
        personality_state_hash
    )
)
```

Rules:

- **Open on first event.** Inserting an event whose `source_batch_id`
  has no row creates the row with `opened_at = now`.
- **Closed by source signal.** EventSource calls
  `engine.close_batch(source_batch_id)`; engine sets
  `closed_at = now`. Idempotent — repeat calls are no-ops.
- **F→A trigger.** Engine enqueues each registered F→A operator
  independently for batches with `closed_at IS NOT NULL` and no
  matching row in `source_batch_f2a`. On run completion, a row is
  inserted into `source_batch_f2a`. Resumption / dedup is per full
  invocation key, not just per operator.
- **Aggregates derived, not stored.** Batch start / end / count /
  cited-object linkage are derived from the constituent events and
  facts on demand. The row carries lifecycle only.
- **Not flavor-typed.** `source_batches` and `source_batch_f2a` have
  fixed shapes. Per-flavor metadata (scraper config, OCR prompt
  version, ingestion intent, etc.) belongs on a `CitedObject` the
  batch's Facts cite (11), not on the batch rows.

`source_batch_id` and `cited_object_id` are conceptually distinct —
the batch is the F→A consolidation episode, the cited object is the
artefact in Reality. They often coincide (one PDF → one batch → one
Document), but for streams (one ChatSession → many batches over time)
they don't, and the consolidation phase cadence depends on the batch, not
the artefact.

With multiple F→A operators per Fact schema, per-operator tracking
replaces the single `f2a_run_at` column. This generalises naturally to
A→Goal and Edge operators if batch-level lifecycle is ever needed for
them, and the append-only-per-operator pattern matches the discipline
elsewhere in the system.

## Phase 2 — Personality embedding

**Operators are flavor-shipped against core contracts.** Core ships
the dispatcher (which finds candidate inputs, snapshots personality,
calls the operator, validates output, persists in one transaction)
and the operator traits (`F2AOperator`, `A2POperator`,
`EdgeOperator`). Flavors ship the operator implementations and
their cadence policies. There is no separate "dream flavor" kind —
any flavor may ship operators alongside (or instead of) schemas,
sources, and tools (08, 13).

Operator scope is one of two:

| Scope | Inputs | Authored by |
|---|---|---|
| **Intra-flavor** | Restricted to schemas owned by the operator's flavor | The flavor that owns the Fact / Abstraction schemas it consumes |
| **Cross-flavor** | Spans the full A/P set of all linked flavors | A flavor that ships only operators (no schemas of its own — though it may), explicitly registering itself as cross-flavor |

Four operator classes:

- **F→A** — always intra (forced by the source-batch invariant: a
  batch belongs to one source). At most one F→A per (Fact schema,
  Abstraction schema) pair; **multiple F→A operators may coexist** in
  one binary over the same Fact schema, each producing a different
  typed Abstraction. This mirrors A→P: parallel cognitive frames over
  the same Reality, with each operator's outputs forming their own
  supersession lineage.
- **A→P** — intra **or** cross. **Multiple A→P operators may
  coexist** in one binary, run in parallel against the same A set,
  and produce different typed Perspectives. This is the architectural
  affordance for parallel cognitive frames: assigning different goals
  / tasks / chat sessions to different A→P operators is a
  composition decision, not a runtime branch.
- **A→Goal** — intra **or** cross. Synthesizes new Goals from an A-set
  retrieved under Π — the agent-discovered-goal capability. Output
  Goals carry typed payloads (06 §Typed payloads) and provenance edges
  back to the input Abstractions. Multiple A→Goal operators may
  coexist, same as A→P; each may register against a different
  `GoalPayload` schema. Specified below.
- **Edge** — cross-flavor refinement that authors `Causal` /
  `Interpretive` edges over existing A/P **without** authoring new
  memories. Optional. Multiple edge operators may coexist; edges are
  additive.

### F→A — Fact to Abstraction (intra, intra-source-batch)

**Trigger:** a source batch is complete. "Complete" is per-source:

| Source | Batch boundary |
|---|---|
| PDF | All chunks of one document have been ingested |
| Chat session | Conversational segment ends (timeout, explicit end, or agent decides) |
| Email thread | Configurable: per-message, or close-of-thread |
| Repo crawl | Crawl pass finishes |
| Single message / single email / single ruling | The single event itself |

The EventSource decides the boundary and signals batch-complete to the
engine. Closure is **persisted** in `source_batches.closed_at`; F→A is
enqueued only for closed batches, and per-(batch, operator) tracking in
`source_batch_f2a` de-duplicates enqueues across restarts (see
§Source-batch lifecycle above).

**Per-personality fan-out.** Each registered F→A operator runs once
per (batch, active-personality) pair. The dispatcher captures a
`PersonalitySnapshot` for each active personality on the batch's owner
and invokes the operator under that snapshot; outputs are tagged with
the producing `personality_id`. Parallel personalities yield parallel
Abstraction lineages over the same batch.

**The operator:**

1. Loads every Fact in the batch, with typed payload.
2. Renders each via its schema renderer (on-demand text — see [03](docs/03-schema-registry.md)).
3. Captures a **personality state snapshot** (see below).
4. Constructs a prompt: rendered facts + personality + structured output
   schema.
5. Calls the LLM.
6. Parses the structured output into zero-or-more `Abstraction` memories,
   each with provenance edges back to the source Facts it references.
7. Computes embeddings on the new Abstractions and stores them.

A single batch may produce zero, one, or many Abstractions — chunking a
book might produce a tree (chapters → themes → "what this book is about"),
or just a single top-level summary. The operator decides granularity.

**Cadence:** eager-per-batch by default. Throttled for high-volume
sources. Re-runnable during dream cycles when personality has shifted.

### A→P — Abstraction to Perspective (intra or cross-flavor)

**Scope:**

- **Intra-flavor A→P** consumes only Abstractions whose `schema_id`
  is owned by the operator's flavor and produces Perspectives whose
  `schema_id` is also owned by that flavor. Useful when a single
  flavor has its own internal synthesis pattern (e.g. "what these
  Forgejo abstractions tell me about repo X").
- **Typed cross-flavor A→P** declares its inputs by name across
  multiple flavors — e.g. `inputs: [code::BugFixClusterV1,
  learning::LectureNoteV1]` — by taking explicit Cargo deps on
  the input flavor crates. The compiler checks the schemas exist
  and have the expected payload shape; type discipline is
  preserved across the flavor boundary. Symmetric to F→A's
  ability to target other flavors' Abstraction schemas as
  outputs.
- **Polymorphic cross-flavor A→P** declares `inputs:
  AnyAbstraction` and consumes the union of all registered
  Abstractions at composite time. Inputs are typed-erased
  (`dyn AbstractionPayload`) — the operator has no compile-time
  knowledge of what schemas it will see. This is an explicit
  opt-out of payload typing, reserved for cognition that is
  deliberately schema-agnostic: self-model formation,
  goal-coverage analysis, generic personality synthesis. The
  schemas the operator sees depend on the composite, not on the
  operator's source. Personality-formation lives here: a
  Perspective derived from "everything I have abstracted" is the
  architectural unit of selfhood.

**Default to typed.** Polymorphic cross-flavor is an explicit
opt-in to runtime payload erasure for the narrow class of
operators that genuinely cannot enumerate their inputs. Most
cross-flavor operators have a known input set and should declare
it.

**Multiple A→P operators run in parallel.** Each is identified by
its `prompt_version` and produces its own typed
`PerspectivePayload`. A composite that links three A→P operators
(say: code-internal, learning-internal, cross-flavor-self) gets
three concurrent cognitive frames over the same A pool. No special
machinery — separate operators with separate Perspective schemas
collide nowhere.

**Personality scope** is an orthogonal axis to flavor scope. An
`A2POperator` declares:

```rust
const FLAVOR_SCOPE:      FlavorScope;        // Intra | Cross
const PERSONALITY_SCOPE: PersonalityScope;   // OwnPersonality | AllPersonalities
```

`OwnPersonality` (default) restricts retrieval to A authored under
the runner's `personality_id`. `AllPersonalities` opens retrieval to
the union scoped by `M[self][*]` — the per-Owner read-scope matrix
(see [02 §Read-scope matrix](docs/02-memory.md#read-scope-matrix)).
A "Synthesist" personality whose job is to integrate other
personalities' outputs declares `AllPersonalities`; default operators
stay `OwnPersonality`. The diagonal `M[self][self] = 1` is hardcoded,
so retrieval always includes the runner's own A even under
`AllPersonalities`.

**Per-personality fan-out** mirrors F→A: each A→P operator runs once
per active personality on the Owner, with that personality's snapshot,
producing Perspectives tagged by `personality_id`. Multiple
personalities × multiple A→P operators = a matrix of parallel
cognitive frames over the same A pool, all addressable, all preserved
as parallel lineages.

**Trigger:** each A→P operator declares its own cadence predicate
(scheduled dream cycle, goal-write hook, abstraction-volume
threshold, explicit user request). Engine evaluates predicates and
dispatches.

A→P always operates against a **consolidation context** — a target
question or topic the operator is integrating around. It never just
"runs over everything"; that would have no semantic frame.

**The operator:**

1. Selects (or accepts) a consolidation context: an active goal, a recent
   abstraction cluster, a scheduled topic from the dream plan.
2. Retrieves Abstractions by vector similarity to the context, optionally
   filtered by goal-relevance. Cross-source.
3. Captures a personality state snapshot.
4. Constructs a prompt: retrieved abstractions + personality + context +
   structured output schema.
5. Calls the LLM.
6. Parses output into zero-or-more `Perspective` memories with provenance
   edges to input Abstractions.
7. May also emit `PerspectiveLink` edges between Facts the operator
   identifies as causally related under the new Perspective.
8. Computes embeddings on new Perspectives.

A→P may legitimately produce zero output: not every consolidation context
yields a defensible Perspective. The operator does not force it.

**Cadence:** operator-declared. Lazy. Expensive. Should be observable.

### Edge operators — cross-flavor refinement (no new memories)

Optional. A flavor may ship an `EdgeOperator` that authors `Causal`
or `Interpretive` edges over the existing A/P set without producing
new memories. Useful for cross-flavor interpretive linking ("this
Code abstraction and that Learning abstraction are about the same
underlying tension") that doesn't warrant a new Perspective.

**Scope:** typically cross-flavor. Edge operators may also be
intra-flavor (within one flavor's A/P), but the value is mostly in
cross-cutting refinement.

**Output:** edges only. Each edge resolves to a registered
`RelationDescriptor` ([02 §Relation registry](docs/02-memory.md#relation-registry)). The engine validates
authorship and directionality ([02 §Edges](docs/02-memory.md#edges)) before persistence.

**Cadence:** operator-declared, same as A→P.

Multiple edge operators may coexist; edges are additive (no
operator overwrites another's edges). Edge operators may **not**
emit `Provenance` or `Supersedes` edges — those are reserved for
memory-authoring operators and the engine.

### Prompt locality

Prompts live with their operators in **flavor crates** (08), not in core.

| Operator | Prompt scope | Registered where |
|---|---|---|
| F→A | Per `(Fact schema → Abstraction schema)` | The flavor that ships the F→A operator and owns the Abstraction output schema. The owner of the Fact schema and the owner of the F→A operator no longer have to be the same flavor. |
| A→P intra-flavor | Per `(input AbstractionPayload set → output PerspectivePayload)` | The flavor that ships the A→P operator and owns the input/output schemas. |
| A→P cross-flavor | Per cross-flavor operator | The flavor that ships the cross-flavor operator. The flavor owns the output `PerspectivePayload` schema; inputs are the union A pool. |
| Edge operator | Per cross-flavor edge operator | The flavor that ships the operator. |

Core supplies the dispatcher — batch loading, personality snapshot,
LLM call, output parsing, edge emission, transactional persistence
— and the prompt-template *interface* (what variables it must bind:
`facts`, `abstractions`, `personality`, `context`, `output_schema`).
The flavor supplies the operator code, the actual template strings,
and the cadence predicate.

Prompts version under the same migration discipline as schemas
([03](docs/03-schema-registry.md)). `prompt_version` is one of the inline operator-invocation
columns on the A/P memory row ([02](docs/02-memory.md)) and enters the F→A / A→P
invocation key (see §Idempotence and reproducibility below) so
re-running with a new prompt produces a new Abstraction /
Perspective rather than overwriting.

### Personality state

Both operators capture a snapshot of personality at run start. The
snapshot is produced by the **active `PersonalityFlavor`** for the
running (Owner, personality_id) pair (see
[02 §Personality](docs/02-memory.md#personality),
[08 §Registration mechanism](docs/08-core-and-flavors.md#registration-mechanism)):

```rust
struct PersonalitySnapshot {
    personality_id:  PersonalityId,           // which flavor produced this
    perspective_ids: Vec<MemoryId>,            // P_active per this flavor's rules
    goal_ids:        Vec<GoalId>,              // G_active per this flavor's rules
    state_hash:      Hash,                      // deterministic; persisted on memory.personality_state_hash
    captured_at:     Timestamp,
}
```

`personality_id` and `state_hash` are both persisted inline on the
resulting A/P / Goal memory ([02](docs/02-memory.md)). Re-running an
operator with the same inputs and the same `(personality_id, state_hash)`
produces the same output (modulo LLM noise). Different personality
states under the **same** `personality_id` produce supersedes-linked
outputs in that personality's lineage; different `personality_id`s
produce parallel lineages, never colliding on supersession.

Selection of `perspective_ids` / `goal_ids` is **flavor-defined**.
Common primitives (recency, topical similarity, goal-relevant filter,
identity-Perspective inclusion) are the building blocks each
`PersonalityFlavor` composes; the substrate does not legislate one
canonical mix. The matrix row `M[personality_id][*]` (per
[02 §Read-scope matrix](docs/02-memory.md#read-scope-matrix)) is
hashed into `state_hash` at snapshot time so a matrix toggle producing
new admissible sources is a different invocation key.

### Execution model and isolation

Operators run inside the substrate's **dispatcher** — built into
core ([08 §Bare core](docs/08-core-and-flavors.md#bare-core)), not
flavor surface. Two tiers of concurrency, structurally separated:

**Phase 1 (event ingestion) is parallel-by-source.** Each
`EventSource` impl owns its own pump (webhook listener, poller,
manual feed) and runs independently of every other source. The
per-event tx (validate + insert Fact + author structural edges)
shares only Postgres write throughput with other sources; no
operator queue is involved. A slow Forgejo webhook does not slow
Telegram intake; an over-eager scraper rate-limits itself without
affecting other sources. This tier needs no isolation machinery
beyond per-source rate limits configured in
[10 §Operator concurrency](docs/10-configuration.md#operator-concurrency).

**Phase 2 (operators) is parallel-by-operator.** Every registered
operator (F→A / A→P / A→Goal / Edge) gets:

- a bounded MPSC work queue;
- a dedicated worker pool with a configurable concurrency cap
  (10 §Operator concurrency);
- per-(Owner, `personality_id`) fairness scheduling within its
  queue — round-robin or deficit-weighted so one noisy Owner /
  personality cannot starve quiet ones on a shared operator.

The structural property that makes this clean: by composite
discipline ([08 §Composite discipline](docs/08-core-and-flavors.md#composite-discipline)),
every operator's outputs go into **disjoint sidecars**. F→A is
at-most-one per (Fact, Abstraction) pair; each A→P operator owns
a distinct `PerspectivePayload`; cross-flavor writes into another
flavor's sidecar are forbidden. Disjoint outputs ⇒ zero row
contention across operators ⇒ **a slow operator backs up only its
own queue.** Cross-operator backpressure is impossible by
construction, not by careful runtime engineering.

Three corollaries:

- **Sluggish flavor X never sluggishes flavor Y.** The "why is
  one flavor making the whole engine slow" failure mode is ruled
  out structurally. The slow operator's queue grows; everyone
  else stays liquid.
- **Owner-level fairness is the runtime knob.** An Owner producing
  a high event rate gets interleaved with quiet Owners under the
  default deficit policy — no Owner can starve another on shared
  operators.
- **Global cost cap is a separate axis.** A binary-wide LLM
  concurrency / token-budget guard sits **above** per-operator
  workers; the dispatcher gates worker dispatch on the global
  semaphore. Per-operator backpressure handles internal fairness;
  the cost cap handles total-spend protection. See
  [10 §Operator concurrency](docs/10-configuration.md#operator-concurrency).

What this is **not**:

- **Not per-flavor process isolation.** One binary, one tokio
  runtime, queues in-memory.
- **Not cross-binary RPC.** Backpressure is bounded-channel
  pressure inside the process.
- **Not eventual consistency.** Each operator commits its memory /
  edge / change_event rows in one tx
  ([14 §Consistency](docs/14-protocol-surface.md#consistency-strong-write-to-stream-via-outbox)).

The operator graph is a **build-time fact** (registration in
`proxima_flavor!`); the per-operator concurrency parameters are
**runtime config** (15). Composite-discipline invariants (disjoint
outputs, at-most-one F→A per pair) are checked at compile time, so
the structural isolation that makes per-operator backpressure work
is a property of the linker's output, not a runtime contract.

## Operators as set transforms

Both operators are best specified as **transformations on memory subsets**.
Memory is a typed universe; consolidation operators produce new subsets from old
ones. Putting this in set-theoretic notation gives us a contract that's
implementation-agnostic and provable against.

### Universe

```
M = F ⊎ A ⊎ P              -- disjoint union of Facts, Abstractions, Perspectives
E                            -- universe of edges (each carries source, target, relation, authorship)
G                            -- universe of goals
```

Every memory has a `layer` ∈ {0, 1, 2} corresponding to {F, A, P}.

### Personality

```
Π = (P_active ⊆ P, G_active ⊆ G)
```

A personality state is a pair of subsets: the perspectives currently in
scope and the goals currently active. A snapshot is taken at operator
invocation; its hash is recorded in the operator's citation.

### Source batches

```
B(b) = { f ∈ F | source_batch_id(f) = b }
```

A source batch is a subset of Facts sharing one source-batch identifier.

### F→A operator

```
F→A : Owner × 𝒫(F) × Π → 𝒫(A) × 𝒫(E)

(ω, B, Π, v) ↦ (A_new, E_new)
```

Pre-conditions:

- B = B(b) for some batch identifier b — input is a coherent batch, not
  an arbitrary union of facts.
- For every f ∈ B: `f.owner == ω`. Π is drawn from ω's active
  perspectives + goals.
- Every f ∈ B has `schema_id ≠ None` and a registered renderer.
- At most one F→A operator per (input Fact schema, output
  Abstraction schema) pair — multiple operators may run over the
  same Fact schema, each producing a distinct typed Abstraction.

Post-conditions:

- `A_new ∩ A_existing = ∅` — only new abstractions; never replaces.
- For every a ∈ A_new: `a.owner == ω`.
- For every a ∈ A_new: `provenance(a) ⊆ B` — the set of memories `a`
  was synthesised from is contained in this batch, never beyond.
  (`provenance(m)` is the input set carried by the operator's
  provenance edges; not the bibliographic citation, which is
  Fact-only — see [11](docs/11-citations.md)).
- For every a ∈ A_new: `a.schema_id` resolves to a registered
  `AbstractionPayload` and the typed sidecar row is written alongside
  the shared `memory` row ([03](docs/03-schema-registry.md)).
- For every e ∈ E_new: `e.authorship = OperatorFtoA(a)` for some
  a ∈ A_new — i.e. owned by the source-side Abstraction (top-down).
- For every e ∈ E_new: `e.source.kind = Abstraction ∧ e.target.kind =
  Fact` — F→A emits **provenance edges A→F only**. Inter-A edges are
  out of scope for this operator (within-A structural edges are
  authored by EventSources or future Phase-3 consolidation operators, not
  here).
- For every e ∈ E_new: `e.source.owner == e.target.owner == ω` —
  scope invariant from 02 holds.

Determinism: given fixed (ω, B, Π, v, model_id, prompt_version),
F→A returns the same output. LLM nondeterminism is acknowledged.

Each F→A operator's Abstractions form their own supersession lineage,
identified by the operator's `(operator_kind, prompt_version, model_id,
output_schema)`. Re-running operator X produces a new X-Abstraction
superseding the old X-Abstraction, untouched by operator Y's parallel
output.

### A→P operator

```
A→P : Owner × 𝒫(A) × Π × Context → 𝒫(P) × 𝒫(E)

(ω, A_ctx, Π, c) ↦ (P_new, E_new)
```

All inputs and outputs share `Owner ω` — A→P never crosses owners.
Π carries `personality_id`; output P_new is tagged with that id.

Where `Context` is a structured value naming the consolidation context
— typically an active Goal, a topical cluster, or a scheduled
dream-cycle subject. A→P never operates without a context.

Pre-conditions:

- A_ctx is a retrieved subset (via vector similarity + goal relevance to
  c). It may span multiple source batches by construction. Retrieval
  is gated by the operator's `PERSONALITY_SCOPE`: under
  `OwnPersonality`, A_ctx ⊆ {a ∈ A | a.personality_id == Π.personality_id};
  under `AllPersonalities`, A_ctx ⊆ {a ∈ A | M[Π.personality_id][a.personality_id]}
  with the matrix per [02 §Read-scope matrix](docs/02-memory.md#read-scope-matrix).
- c is a non-null context.

Post-conditions:

- `P_new ∩ P_existing = ∅`.
- For every p ∈ P_new: `provenance(p) ⊆ A_ctx` (input-memory set,
  not bibliographic citation; see [11](docs/11-citations.md)).
- For every p ∈ P_new: `p.schema_id` resolves to a registered
  `PerspectivePayload`; sidecar row written alongside `memory` ([03](docs/03-schema-registry.md)).
- For every e ∈ E_new: authorship is `OperatorAtoP(p)` (provenance
  P→A, top-down, owned by the source Perspective) or
  `PerspectiveLink(p)` (within-F interpretive edge owned by the
  Perspective).
- Provenance edges have `source.kind = Perspective ∧ target.kind =
  Abstraction` and target ⊆ A_ctx; `PerspectiveLink` edges have both
  endpoints in F.
- All edges respect the layer rule.

Output cardinality: A→P may return `P_new = ∅`. Not every context
yields a defensible Perspective; the operator does not force it.

### A→Goal operator

```
A→Goal : Owner × 𝒫(A) × Π × Context → 𝒫(Goal) × 𝒫(E)

(ω, A_ctx, Π, c) ↦ (G_new, E_new)
```

All inputs and outputs share `Owner ω` — A→Goal never crosses owners.
`Context` is a structured value naming the synthesis target (a
parent goal under which to elaborate sub-goals, a topical cluster
the agent should respond to, a scheduled introspection prompt). The
operator does not run without a context.

Pre-conditions:

- A_ctx is a retrieved subset (vector similarity + goal relevance to c).
- c is a non-null context.

Post-conditions:

- `G_new ∩ G_existing = ∅` — only new goals; never replaces.
- For every g ∈ G_new: `g.owner == ω`.
- For every g ∈ G_new: `g.schema_id` resolves to a registered
  `GoalPayload` and the typed sidecar row is written alongside the
  shared `goal` row (06 §Typed payloads).
- For every g ∈ G_new: `g.authorship = System { Operator { operator_id } }`
  and operator-invocation columns (`operator_kind`, `model_id`,
  `prompt_version`, `personality_id`, `personality_state_hash`) are NOT NULL.
- For every e ∈ E_new: `e.authorship = OperatorAtoGoal(g)` for some
  g ∈ G_new — i.e. owned by the produced Goal (top-down).
- For every e ∈ E_new: `e.source` is the Goal `g`, `e.target` is an
  Abstraction in `A_ctx`, `e.relation = "core/derived-from"` (class
  `Provenance`).
- For every e ∈ E_new: `e.source.owner == e.target.owner == ω`.

Output cardinality: A→Goal may return `G_new = ∅`. Not every context
yields a defensible new goal; the operator does not force one.

Determinism: given fixed (ω, A_ctx, Π, c, model_id, prompt_version),
A→Goal returns the same output (modulo LLM nondeterminism). Π carries
both `personality_id` and `state_hash`, so two personalities producing
identical text by coincidence still hold disjoint invocation keys.

User-confirmation pattern: agent-discovered Goals land with
`authorship = System { Operator … }`. UI may surface them for user
confirmation; user acceptance writes a supersession with
`authorship = User`. The original operator-authored Goal stays in
the supersession chain as audit. Cross-personality supersession (a
new active personality replacing a retired personality's Goal) also
routes through `authorship = User` per
[06 §Goal-write API](docs/06-goals-and-self.md#goal-write-api).

### Composition and idempotence

Both operators are non-destructive: `M' = M ∪ A_new ∪ P_new`. The
universe only grows. No memory is mutated, deleted, or rewritten by an
operator. Supersession links are added between new and prior outputs
**within a personality lineage**; cross-personality outputs are
parallel, never superseded by operator action.

Idempotence is a property of the **invocation key** (scoped to Owner):

```
key(F→A,    ω, B,    personality_id, state_hash, model, prompt_v)        = hash(...)
key(A→P,    ω, c, A_ctx, personality_id, state_hash, model, prompt_v)    = hash(...)
key(A→Goal, ω, c, A_ctx, personality_id, state_hash, model, prompt_v)    = hash(...)
```

`personality_id` is part of every key — parallel personalities never
collide on dedup. Two invocations with identical keys are coalesced;
only the first runs. A re-run with a different `state_hash`
(personality drift within the same flavor) is a different key, produces
fresh output, and supersedes within that personality's lineage. A
re-run under a different `personality_id` is a different key and
produces a parallel output, not a supersession.

### Why this notation matters

Stating the operators as set transforms with explicit pre/post-conditions
gives us a few concrete things:

- **A test contract.** The post-conditions are checkable invariants;
  every operator output gets validated against them before persistence.
- **A reasoning surface.** Questions like "can A→P produce an edge with a
  Fact source?" have unambiguous answers (no — provenance edges target
  Abstractions, PerspectiveLink endpoints are both Facts; no allowed
  edge has a Fact as source authored by A→P).
- **A migration anchor.** When operators evolve (new prompt strategies,
  different retrieval modes), the set-transform signature stays
  constant. Implementation may shift; contract holds.

## Output protocol

All memory-producing operators emit a uniform structured output,
validated by the engine. A→Goal uses a parallel goal-output shape.

```rust
struct OperatorOutput {
    memories: Vec<NewMemory>,                  // F→A, A→P
    goals:    Vec<NewGoal>,                    // A→Goal
    edges:    Vec<NewEdge>,
}

struct NewMemory {
    kind:           DerivedKind,              // Abstraction (F→A) or Perspective (A→P)
    text:           String,                    // operator-authored narrative; required for A/P
    typed_payload:  TypedPayload,              // required — every A/P has a registered schema ([03](docs/03-schema-registry.md))
    provenance:     Vec<MemoryId>,             // input memories this synthesises from (not bibliographic — see [11](docs/11-citations.md))
}

struct NewGoal {
    text:           String,                    // operator-authored statement of intent
    state:          GoalState,                 // typically Active for synthesised goals
    parent_goal_ids: Vec<GoalId>,              // optional placement under existing goals
    typed_payload:  TypedGoalPayload,          // required — every Goal has a registered GoalPayload (06)
    provenance:     Vec<MemoryId>,             // Abstractions this goal was derived from
}

struct NewEdge {
    source:   EntityId,                         // MemoryId | GoalId
    target:   EntityId,
    relation: Relation,                         // must resolve to a registered RelationDescriptor
    // authorship is set by engine: OperatorFtoA / OperatorAtoP / OperatorAtoGoal / PerspectiveLink
}
```

`TypedPayload` wraps the registered `AbstractionPayload` or
`PerspectivePayload` ([03](docs/03-schema-registry.md)); `TypedGoalPayload` wraps the registered
`GoalPayload` ([06](docs/06-goals-and-self.md)). The engine writes the sidecar row alongside the
shared `memory` / `goal` row in the same transaction; an A / P / Goal
with no typed payload is rejected before persistence.

Engine validation rejects:

- Wrong-kind output (F→A producing Perspectives, A→P producing Goals,
  A→Goal producing memories, etc.).
- `provenance` entries naming memories that don't exist or aren't in
  the operator's input scope.
- Edges violating the directionality rule (component 02).
- Edges with a `relation.id` that doesn't resolve to a registered
  `RelationDescriptor`, or whose descriptor forbids the actual
  `(source.kind, target.kind, authorship)` triple.
- An F→A edge that is not `A → F` provenance (inter-A edges from F→A
  are out of scope; see post-conditions above).
- An A→Goal edge that is not `Goal → A` provenance under `core/derived-from`.
- A `typed_payload` whose schema doesn't match `kind` (e.g. an
  `AbstractionPayload` on a `Perspective` output, a non-`GoalPayload`
  on a Goal output) or isn't registered, or is missing entirely.

A failed validation aborts the run; nothing is persisted; the failure is
recorded for observation.

## Supersession across runs

A re-run of either operator produces a new output, never an in-place
update. The new memory carries a `supersedes` edge pointing to the prior
output it replaces (when one exists). Consumers asking for "current
state" walk supersession chains to the head.

This is the trauma-resolution mechanic at the storage layer:

- Old Abstraction was produced under a Perspective that didn't fit the
  Facts.
- Personality shifts (new goal, updated perspective).
- F→A re-runs against the same source batch; new Abstraction emerges
  under the updated personality.
- New Abstraction supersedes old. Both queryable.

The engine never decides on its own that supersession should happen.
Re-runs are explicit (operator invocation triggered by external policy).
The supersession edge is the audit trail for "we updated our view here."

## Idempotence and reproducibility

A run is identified by a deterministic key:

```
F→A:  hash(operator=F→A, batch_id, personality_state_hash, model_id, prompt_version)
A→P:  hash(operator=A→P, context_hash, personality_state_hash, model_id, prompt_version)
```

Two runs with the same key are coalesced — re-invocation returns the
prior result, no LLM call. This makes operators safe to retry on
infrastructure failure.

LLM nondeterminism is acknowledged: same key may produce different text.

The four inputs to the F→A / A→P key — `operator_kind`, `model_id`,
`prompt_version`, `personality_state_hash` — are the inline
operator-invocation columns on the memory row ([02](docs/02-memory.md)). They are
reproducibility metadata, not citations; the bibliographic concept of
citation applies only to Facts ([11](docs/11-citations.md)).

## What this does not include

- **Actuator dispatch.** Goals → Actions → Reality is component 05.
- **Self updates.** Self is a pure query (06), read from existing
  memories, not produced by a separate operator.
- **Cadence policies' content.** Each operator declares its own cadence
  predicate; the engine evaluates predicates and dispatches. The set of
  *legitimate* predicates (batch-close, goal-write, schedule, abstraction
  threshold, …) is a flavor authoring concern, not a core invariant.
- **Conflict resolution between concurrent runs.** If two F→A invocations
  race on the same batch, the one that lands first wins; the loser's
  output is discarded by the idempotence key check. Concurrent A→P
  operators against the same A pool **do not conflict** — each produces
  its own typed Perspective; that is the design.

## Summary

Phase 1 is the postal service: deliver every event, faithfully, into the
graph with correct addressing. Phase 2 is the mind: read the mail, make
sense of it, update the worldview. The two never run in the same code
path, share no failure modes, and require radically different
operational treatment.

## Anchors

- `phase-1-graph-embedding`
- `forward-reference-targets-and-cycles`
- `properties`
- `source-batch-lifecycle`
- `phase-2-personality-embedding`
- `f-to-a-fact-to-abstraction`
- `a-to-p-abstraction-to-perspective`
- `edge-operators-cross-flavor-refinement`
- `prompt-locality`
- `personality-state`
- `execution-model-and-isolation`
- `operators-as-set-transforms`
- `universe`
- `personality`
- `source-batches`
- `f-to-a-operator`
- `a-to-p-operator`
- `a-to-goal-operator`
- `composition-and-idempotence`
- `why-this-notation-matters`
- `output-protocol`
- `supersession-across-runs`
- `idempotence-and-reproducibility`
- `what-this-does-not-include`
- `summary`
