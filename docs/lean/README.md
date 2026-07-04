# Causa — the Proxima Lean kernel

The domainless invariants of Proxima — the F/A/P memory ontology, operators,
edges, owner scoping, goals, citations, compliance, and composition — as a
spec-mode Lean 4 kernel. **This kernel is the source of truth.** The prose
docs (`docs/*.md`) are rationale and commentary; where docs or code disagree
with the kernel, the kernel wins until renegotiated in writing.

## Build

```
cd docs/lean && lake build
```

Toolchain: `leanprover/lean4:v4.13.0` (elan-managed). No Mathlib. Spec-mode:
tokens are `opaque` definitions or `inductive` vocabularies; redundant
invariants are `theorem`s proved from the core or table-validity witnesses (the
minimization discipline) — a failing build is drift.

## Files (load order)

| File | Carries |
|---|---|
| `Causa/Prelude.lean` | minimal `Set`, `Instant`, `Text` |
| `Causa/Owner.lean` | resolved Owner/group model and role ladder (doc 01) |
| `Causa/Identity.lean` | ids, stable `OwnerRef`, source batches, append-only/immutable classes, vector-store independence (docs 01, 07) |
| `Causa/Memory.lean` | F/A/P kinds, layer order, row fields, Fact/FactEntity typing (doc 02) |
| `Causa/Knowledge.lean` | text-bearing knowledge artifacts and interpreter-class recoverability |
| `Causa/Goals.lean` | Goal states, lifecycle, supersession heads, active set, Self query projection (doc 06) |
| `Causa/Edges.lean` | relation classes, directionality matrix, source-owner scope, masks, memory supersession/head queries (doc 02) |
| `Causa/Authorization.lean` | owner-role read/write ceilings, owner-state resolution, world/personal/group access theorems |
| `Causa/EdgeAuthorization.lean` | source-owned edge reads and descriptor-selected target write gates |
| `Causa/Operators.lean` | F→A / A→P / A→Goal shapes, no downward writes, provenance obligations, batch gate (docs 02, 04) |
| `Causa/Provenance.lean` | admitted graph validity, grounding, table-scoped provenance/uniqueness witnesses |
| `Causa/Wake.lean` | Goal-armed wake firing, no-escalation, tool bounds, autonomy/termination theorems |
| `Causa/Citations.lean` | Fact-only bibliography, 1:1 mapping, owner match (doc 11) |
| `Causa/Compliance.lean` | abandonment-gated erasure, source cascade, target projection redaction (doc 13) |
| `Causa/Principles.lean` | named principle rollups over lower-level theorems |
| `Causa/Flavor.lean` | core independence, namespace discipline, optional sidecars/receipts, payload opacity (docs 03, 08) |

## Coverage

`COVERAGE.md` maps every invariant extracted from the source docs to its
kernel carrier (opaque def / inductive / theorem / structural shape /
commentary) or to an explicit exclusion with reason. Nothing is silently
dropped.

## The domainless boundary

Payloads are opaque (`SchemaRef`): the kernel never sees what a schema
contains, only that everything is schema-typed and namespaced. Applications
(working-hero, neko, …) attach as flavors with zero kernel change. Core
independence is witnessed constructively in `Causa.Flavor`; there is no
`FlavorId`, runtime registry, or flavor-specific axiom in the kernel.
