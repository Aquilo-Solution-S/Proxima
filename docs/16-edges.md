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
| **Reference** — points-at | schema-declared `references()` | `memory.refs[]` |
| **Interpretation** — a claim | an interpretation Perspective | ordinary `refs` on that node |

Supersession is a later `t` on the same `handle`, not a pin. Authorship is a
row column when present.

```
proxima_core.memory (
    origins uuid[],   -- Origin pins (empty on Facts)
    refs    uuid[],   -- Reference pins
    ...
)
```

- Target is always a `t`. Never a handle. No follow-at-read.
- No pin id, payload, sidecar, citation, or status.
- Ten call sites A→B are one `refs` entry and ten payload sites.
- Rebuildable: re-derive from node content, same set.

Two kinds only. A third kind fails the node-home test.

Source of a pin is the declaring row. Layering: `ℓ(source) ≥ ℓ(target)` for
memory endpoints. Goals sit outside the layer comparison. Facts cannot have
origins. Facts cannot be interpretation sources.

Write admitted iff write on source and read on target at write time.
Unreadable targets redact independently.

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
| neighbors | walks `origins` / `refs` |

In-tree code flavor: call sites live on `CodeChunkV1.calls`; work assignment
is a Perspective whose payload names worker and request; request
`depends_on_memory_ids` are payload refs.

## Kernel

`docs/lean/Causa/Edges.lean` (E1–E7 in `docs/lean/COVERAGE.md`).

- **E1** both endpoints exist (or cooled stub).
- **E2** source-owned.
- **E3** layering.
- **E4** kind follows operation. Zero origins is legal (interpretation).
- **E5** identity is the pin itself.
- **E6** no content on the pin.
- **E7** rebuildable from node content.
