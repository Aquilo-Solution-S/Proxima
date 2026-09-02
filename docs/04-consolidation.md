# 04 — Consolidation

Consolidation = operator runtime above Fact ingest.

Core owns scheduling, idempotency, owner isolation, registry validation, and
append-only writes. Flavors own operators, prompts, payload schemas, wake
policy, and retrieval policy.

## Shape

```
source receipt metadata
  -> receipt-backed Fact + its payload's reference entries
  -> Abstraction + origin entries

announce(F/A/P/Goal/Edge)
  -> external harness pull
  -> armed Active Goal wake match
  -> harness decision
  -> typed A/P/Goal writes (edges follow from what they declare)
  -> announce(...)
```

Phase split:

| Phase | Input | Output | Runtime |
|---|---|---|---|
| FactIngest | external observation + receipt metadata | receipt-backed Fact + its payload's `reference` entries | 01 / 03 / 05 |
| F->A | Fact set | Abstraction + `origin` entries | flavor-operator discipline |
| A->A | Abstraction set | Abstraction + `origin` entries | operator / harness |
| A->P | Abstraction set, active Perspective context | Perspective + `origin` entries | operator / harness |
| A->Goal | Abstraction set, active Perspective context | Goal + `reference` entries from its evidence column | operator / harness |

## F->A input admission

F->A takes any set of Facts, across any number of authoring calls and
any age. No grouping id, batch table, or close step exists or is
required: the `source_batches` table and `engine.close_batch` were
removed in v0.0.8, and `source_batch_id` after v0.0.9. The gate on
`core_derive` is exactly:

- one memory layer per operator invocation — every `source_handles`
  entry resolves to the same layer, else `InvalidArgument`;
- no Perspective from Facts and no derivation from Perspectives, else
  `LayeringViolation`.

Domain metadata belongs on `CitedObject` / `CitationMapping` sidecars.

## Phase 2 — Perspective-context embedding

Operator rules:

| Operator | Signature | Rule |
|---|---|---|
| F->A | `2^F x Pi -> A` | Facts become one typed Abstraction. |
| A->A | `2^A x Pi -> A` | Abstractions become one typed Abstraction. |
| A->P | `2^A x Pi -> P` | Abstractions become one typed Perspective. |
| A->Goal | `2^A x Pi -> Goal` | Abstractions may propose or advance Goals. |

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
A semantic or causal claim about two Facts is a node over them, never a
connection between them (see [02 §Edges](02-memory.md#edges)).

A->P and A->Goal are intentionally plural: different Perspective contexts can
frame the same Abstractions differently.

## Prompt locality

Prompts live with the flavor operators that use them.

| Layer | Owns |
|---|---|
| Core | dispatcher, template interface, registry validation, write protocol |
| Flavor | prompt text, operator code, retrieval policy, write allow-list |
| Runtime config | host-injected embedding client (retrieval), Postgres connection |

Core does not persist universal prompt/operator/model columns on Memory.
Schema-specific recipe metadata may live in typed Content. Core does not ship
domain prompts or accept runtime prompt registration.

## Execution model and isolation

Wake matching is per Owner and per armed Active Goal. Proxima is a
passive brain hub: it runs no in-process dispatcher. External harnesses
drive the wake loop and own their own cursor position; core serves the
loop through pull verbs and validates every admitted candidate/write.

Goal-owned wake config:

| Carrier | Key | Function |
|---|---|---|
| `wake_config` | `wake_id` referenced by Goal versions | trigger selector, prompt, hard-memory context, canonical tool/action allow-list |

Harness wake loop (driven externally, served by core pull verbs):

1. Pull owner `announce` rows after the harness-held cursor
   (`list_change_events_after`; MCP: `proxima://change-events{?since,limit}`).
2. Match only armed Active Goal heads; Paused/Achieved/Abandoned Goals do not fire.
3. Check trigger readability against the trigger Fact's actual owner.
4. Check each hard-memory context row against that memory's actual owner/kind.
5. Admit only configured tool/action ids within actor tool scope intersected
   with the deployment tool-surface profile (`ToolScope::Palette` when narrowed).
   Steps 2-5 are one admission read: `Engine::list_goal_wake_candidates`
   (MCP: `proxima://wake-candidates{?fact,limit}`).
6. Execute externally; core does not run a scheduler, plugin host, or tool executor.
7. Validate every write through the schema registry.
8. Commit output rows and emitted `announce` rows atomically; the Goal
   records its wake evidence in `Goal.evidence_t`, from which the
   `reference` entries are derived in that same transaction.
9. Advance the harness cursor after consideration, independent of output count.

Fired-wake idempotency is the harness's responsibility; core does not keep a
server-side invocation ledger.

Isolation:

- Owner is the access boundary.
- Cross-owner reads are governed by owner roles; an edge is always owned by
  its source.
- Server-resolved owner roles govern cross-owner reads.
- External harness cursors and policy terminate wake cycles.

## Output protocol

Operators write ordinary typed entities only:

| Output | Required |
|---|---|
| Abstraction | Memory version, typed Content, declared `origins` |
| Perspective | Memory version, typed Content, declared `origins` / `refs` |
| Goal | Goal version, typed sidecar, topology columns, optional `write_act_t` |

Edges are not in that table because operators do not write them. They follow
from what the nodes above declare: `derived_from` on the write, reference
fields on the payload.

No Dream entity. No Dream edge kind. No Core dream pipeline.

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

Persisted derivation evidence:

| Entity | Evidence |
|---|---|
| Abstraction / Perspective | declared input `t`s in `origins`; schema-specific recipe metadata may live in typed Content |
| Goal | `request_id`, version order on one `handle`, topology pins, optional write-act Fact in `write_act_t` |

The operator id, input contract, and model binding are admission/embedding
inputs, not universal Memory columns. The invocation manifest validates the
declared origins before admission. Bibliographic citation is Fact ∪
Abstraction (see 11), not operator provenance.

Retries use the same idempotency boundary. A changed recipe, context, or input
set is a new `t`, not mutation of the old row.

## Invariants

- F/A/P layering and edge direction: 02 §The Directionality Rule; the model
  itself: 16.
- Typed A/P sidecars: 03 §Sidecar tables.
- Append-only identity and head advancement: 07 §Append-only.
- Build-time schemas, prompts, tools: 08 §Registration mechanism.
- Bibliographic citation on Fact and Abstraction: 11 §Three-layer model.
- Source-batch lifecycle is core; domain metadata is citation-sidecar data.
- F->A exclusivity is per output Abstraction schema and operator.
