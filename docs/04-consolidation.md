# 04 — Consolidation

Consolidation = operator runtime above Fact ingest.

Core owns scheduling, idempotency, owner isolation, registry validation, and
append-only writes. Flavors own operators, prompts, payload schemas, wake
policy, and retrieval policy.

## Shape

```
source receipt metadata
  -> receipt-backed Fact + structural Edge
  -> closed source batch
  -> Abstraction + provenance Edge

change_event(F/A/P/Goal/Edge)
  -> external harness pull
  -> armed Active Goal wake match
  -> harness decision
  -> typed A/P/Goal/Edge writes
  -> change_event(...)
```

Phase split:

| Phase | Input | Output | Runtime |
|---|---|---|---|
| FactIngest | external observation + receipt metadata | receipt-backed Fact + structural Edge | 01 / 03 / 05 |
| F->A | Fact set, source batch | Abstraction + provenance Edge | flavor-operator discipline |
| A->P | Abstraction set, active Perspective context | Perspective + provenance Edge | operator / harness |
| A->Goal | Abstraction set, active Perspective context | Goal + evidence Edge | operator / harness |

## Source-batch lifecycle

`source_batches` is core source-ingest lifecycle, not a domain payload.

| Column family | Rule |
|---|---|
| `id` | UUIDv7 declared by the source; unique within `(source_id, owner)` |
| `source_id` | source identity |
| owner | `owner_kind`, `owner_id` |
| `opened_at`, `closed_at` | open vs closed lifecycle |

Domain metadata belongs on `CitedObject` / `CitationMapping` sidecars, not on
`source_batches`.

## Phase 2 — Perspective-context embedding

Operator rules:

| Operator | Signature | Rule |
|---|---|---|
| F->A | `2^F x Pi -> A` | Facts become one typed Abstraction. |
| A->P | `2^A x Pi -> P` | Abstractions become one typed Perspective. |
| A->Goal | `2^A x Pi -> Goal` | Abstractions may propose or supersede Goals. |

`Pi` = active Perspective context plus registered flavor behavior.

F->A exclusivity:

- Exclusive per `(input contract, operator id, output Abstraction schema)`.
- Multiple F->A operators may read the same Fact schema when they emit distinct
  Abstraction schemas.
- The same operator may emit a new row only when the input contract or output
  Abstraction schema differs.
- v0.0.1 ships no core F->A tracking table; this is flavor-operator discipline
  until a core mechanism lands.

Cross-domain synthesis:

```
F(D1) + F(D2) -> A(D1,D2)
```

The join object is a typed Abstraction with provenance to every input Fact.
Direct semantic / causal Fact-to-Fact edges remain forbidden (see 02 §Edges).

A->P, A->Goal, and mechanical Edge writes are intentionally plural: different
Perspective contexts can frame the same Abstractions differently.

## Prompt locality

Prompts live with the flavor operators that use them.

| Layer | Owns |
|---|---|
| Core | dispatcher, template interface, registry validation, write protocol |
| Flavor | prompt text, operator code, retrieval policy, write allow-list |
| Runtime config | host-injected embedding client (retrieval), Postgres connection |

Core stores prompt version references on outputs. Core does not ship
domain prompts and does not accept runtime prompt registration.

## Execution model and isolation

Wake matching is per Owner and per armed Active Goal. Proxima is a
passive brain hub: it runs no in-process dispatcher. External harnesses
drive the wake loop and own their own cursor position; core serves the
loop through pull verbs and validates every admitted candidate/write.

Goal-owned wake config:

| Carrier | Key | Function |
|---|---|---|
| `Goal.wake` side table | `(Owner, goal_id)` | trigger selector, prompt, hard-memory context, canonical tool/action allow-list |

Harness wake loop (driven externally, served by core pull verbs):

1. Pull owner `change_event` rows after the harness-held cursor
   (`list_change_events_after`).
2. Match only armed Active Goal heads; Paused/Achieved/Abandoned Goals do not fire.
3. Check trigger readability against the trigger Fact's actual owner.
4. Check each hard-memory context row against that memory's actual owner/kind.
5. Admit only configured tool/action ids within actor tool scope intersected
   with the deployment tool-surface profile (`ToolScope::Palette` when narrowed).
6. Execute externally; core does not run a scheduler, plugin host, or tool executor.
7. Validate every write through schema and relation registries.
8. Commit output rows and emitted `change_event` rows atomically; any emitted Fact
   must receive a same-transaction `core/wake-motivated-by` Goal-context edge.
9. Advance the harness cursor after consideration, independent of output count.

Fired-wake idempotency is the harness's responsibility; core does not keep a
server-side invocation ledger.

Isolation:

- Owner is the access boundary.
- Cross-owner reads and edges are invalid.
- Server-resolved owner roles govern cross-owner reads.
- External harness cursors and policy terminate wake cycles.

## Output protocol

Operators write ordinary typed entities only:

| Output | Required |
|---|---|
| Abstraction | memory row, typed sidecar, text, operator provenance |
| Perspective | memory row, typed sidecar, text, operator provenance |
| Goal | goal row, typed sidecar, authorship, optional supersession |
| Edge | registered relation, legal endpoint kinds, owner match |

No Dream entity. No Dream relation class. No Core dream pipeline.

Dreaming is flavor policy expressed as ordinary F->A / A->P / A->Goal
operators under the same registry and edge invariants as every other
consolidation pass.

Partial persistence is invalid: either all outputs from one invocation commit
with their change events, or none do.

## Idempotence and reproducibility

Idempotence keys:

| Path | Key |
|---|---|
| Fact ingest | `receipt_id` |
| GoalWrite | client `request_id` |

Reproducibility metadata:

| Row | Columns |
|---|---|
| Abstraction / Perspective | operator kind, model id, prompt version, declared input/provenance context |
| Goal | authorship, schema id/version, supersession lineage |
| Edge | relation id, authoring path, endpoint ids |

Bibliographic citation remains Fact-only (see 11). Operator reproducibility is
inline provenance, not citation.

Retries append only through the same idempotency boundary. A changed prompt,
model binding, operator version, Perspective context, or input contract is a
new derivation, not mutation of the old row.

## Invariants

- F/A/P layering and edge direction: 02 §Edges.
- Typed A/P sidecars: 03 §Sidecar tables.
- Append-only identity and supersession: 07 §Append-only.
- Build-time schemas, relations, prompts, tools: 08 §Registration mechanism.
- Fact-only bibliographic citation: 11 §Three-layer model.
- Source-batch lifecycle is core; domain metadata is citation-sidecar data.
- F->A exclusivity is per output Abstraction schema and operator.
