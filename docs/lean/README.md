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
primitives are `axiom`/`inductive`; redundant invariants are `theorem`s proved
from the core (the minimization discipline) — a failing build is drift.

## Files (load order)

| File | Carries |
|---|---|
| `Causa/Prelude.lean` | minimal `Set`, `Instant`, `Text` |
| `Causa/Owner.lean` | Principal (User/Group), Owner, visibility rule (doc 01) |
| `Causa/Identity.lean` | ids, Event, append-only/immutable/supersedable classes, vector-store independence (docs 01, 07) |
| `Causa/Memory.lean` | F/A/P kinds, ℓ, fact↔event, text rule, supersession, personality, read-scope matrix (doc 02) |
| `Causa/Goals.lean` | Goal states, lifecycle, DAG acyclicity, active set (doc 06) |
| `Causa/Edges.lean` | relation classes, directionality matrix, single-owner scope, masks (doc 02) |
| `Causa/Operators.lean` | F→A / A→P / A→Goal shapes, no downward writes, provenance obligations, batch gate (docs 02, 04) |
| `Causa/Citations.lean` | Fact-only bibliography, 1:1 mapping, owner match (doc 11) |
| `Causa/Compliance.lean` | erasure scopes, suppression guard, pause (doc 13) |
| `Causa/Composition.lean` | core independence, namespace discipline, frozen registry, payload opacity (docs 03, 08) |

## Coverage

`COVERAGE.md` maps every invariant extracted from the source docs to its
kernel carrier (axiom / def / structural shape / commentary) or to an explicit
exclusion with reason. Nothing is silently dropped.

## The domainless boundary

Payloads are opaque (`SchemaRef`): the kernel never sees what a schema
contains, only that everything is schema-typed and namespaced. Applications
(working-hero, neko, …) attach as flavors with zero kernel change — the core
is total without any flavor (`core_always_present`, `registry_determined`).
