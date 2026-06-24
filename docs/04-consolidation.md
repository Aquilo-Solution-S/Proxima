# 04 — Consolidation

Consolidation = operator runtime above EventSource Fact ingest.

Core owns scheduling, idempotency, owner isolation, registry validation, and
append-only writes. Flavors own operators, prompts, payload schemas, wake
policy, and retrieval policy.

## Shape

```
EventSource
  -> Event + Fact + structural Edge
  -> closed source batch
  -> Abstraction + provenance Edge

change_event(F/A/P/Goal/Edge)
  -> external harness pull
  -> wake entry detect match
  -> harness decision
  -> typed A/P/Goal/Edge writes
  -> change_event(...)
```

Phase split:

| Phase | Input | Output | Runtime |
|---|---|---|---|
| EventSource ingest | external event | Fact + structural Edge | 01 / 03 / 05 |
| F->A | Fact set, source batch | Abstraction + provenance Edge | flavor-operator discipline |
| A->P | Abstraction set, active personality instance | Perspective + provenance Edge | wake entry |
| A->Goal | Abstraction set, active personality instance | Goal + evidence Edge | wake entry |

## Source-batch lifecycle

`source_batches` is core EventSource lifecycle, not a domain payload.

| Column family | Rule |
|---|---|
| `id` | UUIDv7 declared by the source; unique within `(source_id, owner)` |
| `source_id` | source identity |
| owner | `owner_principal_kind`, `owner_principal_id` |
| `opened_at`, `closed_at` | open vs closed lifecycle |

Domain metadata belongs on `CitedObject` / `CitationMapping` sidecars, not on
`source_batches`.

## Phase 2 — Personality embedding

Operator rules:

| Operator | Signature | Rule |
|---|---|---|
| F->A | `2^F x Pi -> A` | Facts become one typed Abstraction. |
| A->P | `2^A x Pi -> P` | Abstractions become one typed Perspective. |
| A->Goal | `2^A x Pi -> Goal` | Abstractions may propose or supersede Goals. |

`Pi` = active personality instance and its registered flavor behavior.

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
personalities can frame the same Abstractions differently.

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

Wake execution is per Owner and per personality instance. Proxima is a
passive brain hub: it runs no in-process dispatcher. External harnesses
drive the wake loop and own their own cursor position; core serves the
loop through pull verbs and validates every write.

Runtime tables:

| Table | Key | Function |
|---|---|---|
| `personality_wake_entries` | `(Owner, personality_instance_id, wake_entry_id)` | trigger (kind+id), label, enabled, authored_by, probability_promille, goal_scope, instructions |

Harness wake loop (driven externally, served by core pull verbs):

1. Read active wake entries for `(Owner, personality_instance_id)`.
2. Pull owner `change_event` rows after the harness-held cursor
   (`list_change_events_after`).
3. Reject self-authored events (`change_event.entity_personality_instance_id`).
4. Reject events at or above the wake-chain depth bound
   (`change_event.wake_chain_depth`).
5. Match wake entry trigger against event kind / schema / relation.
6. Execute externally with tool scope from token capabilities intersected with
   the deployment tool-surface profile (`ToolScope::Palette` when narrowed).
7. Validate every write through schema and relation registries.
8. Commit output rows and emitted `change_event` rows atomically.
9. Advance the harness cursor after consideration, independent of output
   count.

Fired-wake idempotency is the harness's responsibility; core no longer
keeps a server-side invocation ledger.

Isolation:

- Owner is the access boundary.
- Cross-owner reads and edges are invalid.
- Read-scope matrix governs cross-personality reads (see 02 §Read-scope matrix).
- Depth bound terminates wake cycles.

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
| Event ingest | `event_id` |
| GoalWrite | client `request_id` |

Reproducibility metadata:

| Row | Columns |
|---|---|
| Abstraction / Perspective | operator kind, model id, prompt version, personality instance, wake depth |
| Goal | authorship, schema id/version, supersession lineage |
| Edge | relation id, authoring path, endpoint ids |

Bibliographic citation remains Fact-only (see 11). Operator reproducibility is
inline provenance, not citation.

Retries append only through the same idempotency boundary. A changed prompt,
model binding, operator version, personality instance, or input contract is a
new derivation, not mutation of the old row.

## Invariants

- F/A/P layering and edge direction: 02 §Edges.
- Typed A/P sidecars: 03 §Sidecar tables.
- Append-only identity and supersession: 07 §Append-only.
- Build-time schemas, relations, prompts, tools: 08 §Registration mechanism.
- Fact-only bibliographic citation: 11 §Three-layer model.
- Source-batch lifecycle is core; domain metadata is citation-sidecar data.
- F->A exclusivity is per output Abstraction schema and operator.
