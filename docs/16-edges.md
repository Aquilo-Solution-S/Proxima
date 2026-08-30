# 16. Edges

Pins between nodes. There is no edge table.

## The Thesis

> An edge carries no information beyond its existence: endpoints, direction,
> creation time, kind. All content lives in nodes.

1. **Kind follows operation.** The kind is a consequence of the write, never
   a writer-chosen vocabulary.
2. **The node-home test.** If no node owns the statement, the model is
   missing a node, not a kind.

## The Model

| Way | Statement lives in | Stored as |
|---|---|---|
| **Origin** — made-from | the derived write (`derived_from`) | `memory.origins[]` |
| **Reference** — points-at a Memory | schema-declared `references()` | `memory.refs[]` |
| **Reference** — points-at a Goal | schema-declared `references()` | `memory.goal_refs[]` |
| **Interpretation** — a claim | an interpretation Perspective | ordinary `refs` on that node |

Supersession is a later `t` on the same `handle`, not a pin. Authorship is a
row column when present.

```
proxima_core.memory (
    origins   uuid[],   -- Origin pins (empty on Facts)
    refs      uuid[],   -- Reference pins onto the Memory spine
    goal_refs uuid[],   -- Reference pins onto the Goal spine
    ...
)
```

- Target is always a `t`. Never a handle. Origins target a hot or cooled
  Memory. References target a hot or cooled Memory from `refs`, or a Goal
  from `goal_refs`. Exact historical restoration may resolve a target through
  the internal kinded erase witness, but that witness is not a public edge and
  is never follow-at-read.
- No pin id, payload, sidecar, citation, or status.
- Ten call sites A→B are one `refs` entry and ten payload sites.
- Rebuildable: re-derive from node content, same set.

### One kind, two columns

Reference is still one kind. The split is by **spine**, not by kind: `refs`
is Memory-only and `goal_refs` is Goal-only, so the column a target sits in
IS its kind. That is what the untyped `uuid[]` could not say, and what every
reader, the writer, and `home_owner` each had to re-derive against the Goal
spine — a round trip per read page and a second kind load per write target.

Both columns are append-only, both are GIN-indexed, and the write path locks
each spine once, in sorted `t` order, so the deadlock ordering is unchanged.
Grounding deliberately does not see `goal_refs`: a non-Fact must still pin a
Memory, and a Goal reference has never counted as provenance.

**Non-disclosure.** The stored discriminant must not reach projection. On
read, `goal_refs` is filtered to the Goals this reader may see; every other
Goal id falls back into `refs`, where it redacts exactly as an unreadable
Memory does. Knowing that a withheld target is a Goal would itself be
disclosure, so a redacted target says nothing about which spine it was on.

**Migrating.** `refs` written before the split still mixes the two. Migration
`0004_v011_goal_refs.sql` partitions hot and cooled rows by Goal-spine
membership; cold objects below format version 5 are partitioned the same way
on hydrate, since the object itself cannot say which ids were Goals.

Two kinds only. A third kind fails the node-home test.

Source of a pin is the declaring row. Layering: `ℓ(source) ≥ ℓ(target)` for
memory endpoints. Goals sit outside the layer comparison. Facts cannot have
origins. Facts cannot be interpretation sources.

Write admitted iff write on source and read on target at write time.
Unreadable targets redact independently.

Hard erase does not repair the declaring row: its `origins[]` and `refs[]`
remain byte-for-byte, with no cascade or nulling. It appends the permanent
owner-free `(t, closed kind)` witness needed only for exact sealed cooled
restoration. Ordinary writes still require live targets and cannot use or
reuse that witness. Legacy cooled rows whose pin arrays are `NULL` use the
ordinary live-target path. The witness is excluded from export, transfer,
forget, and all public Edge/PinNode/MCP/REST projections; public missing
targets keep the existing redacted/missing behavior rather than gaining an
`Unavailable` state.

Memory admission, hydration, forget, and single-entity erase use two advisory
namespaces: a sorted/deduplicated `proxima-memory-handle:<handle>` set first,
then a sorted/deduplicated `proxima-forget:<t>` lifecycle set. Head, row, blob,
and content persistence follow those locks. The namespaces stay distinct
because a caller-supplied handle may equal a caller-supplied `t`; the values
are not the same identity. Series erase captures all seed handles, locks that
set once, expands hot + cooled versions under those locks, then locks the
complete version set once. The pre-lock version set witnesses each handle's
series generation: a captured seed that disappears while siblings remain is
still expanded through its handle, while a non-empty disjoint post-lock set
means the handle was fully erased and reused, so the stale erase retries
instead of deleting the replacement. A handle/t set cannot grow after its
lifecycle lock is taken: a late version is included by expansion or the
operation returns a retryable conflict.

Goal writes that mint lifecycle Facts include those Facts' reserved Memory
handles in the same sorted handle union before locking Goal targets. Owner,
source, and repository sweeps, plus transfer's broader multi-round footprint,
remain outside this complete-set guarantee.

## Computed Scores Are Abstractions

A persisted score is an Abstraction: payload holds value and method,
`refs` point at inputs, optional `blob_id` cites the computation record.
Query-time similarity is not persisted and authors no pin.

Citation is `blob_id` 0..1 on Fact ∪ Abstraction. Perspectives never cite
(11).

## Current surface

No edge verb. No `E:` wire prefix. No `proxima://edges`.

| Tool | Pins |
|---|---|
| `core_interpret` | interpretation Perspective; subjects are `refs` |
| `core_derive` | `origins` from `derived_from` |
| lineage | walks `origins` |
| neighbors | current-head inbound sample (cap 200) plus the hit's own `origins` / `refs` |

In-tree code flavor: call sites live on `CodeChunkV1.calls`; work assignment
is a Perspective whose payload names worker and request; request
`depends_on_memory_ids` are payload refs.

## Kernel

`docs/lean/Causa/Edges.lean` (E1–E7, E-SPINE, E-NODISC, E-READ in
`docs/lean/COVERAGE.md`).

- **E1** live origins and `refs` resolve to a hot Memory or cooled stub;
  `goal_refs` resolves to a Goal. Retained historical references may
  additionally resolve to a kinded erase witness; Goal witnesses are never
  Memory-origin kinds.
- **E2** source-owned.
- **E3** layering.
- **E4** kind follows operation. Zero origins is legal (interpretation).
- **E5** identity is the pin itself.
- **E6** no content on the pin.
- **E7** rebuildable from node content.
- **E-SPINE** the reference column is the target's spine: `refs` never holds
  a Goal.
- **E-NODISC** an unreadable Goal reference is indistinguishable from an
  unreadable Memory reference.
- **E-READ** a row with no Goal reference needs no Goal-spine read.
