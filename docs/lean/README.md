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
tokens are `opaque` definitions or `inductive` vocabularies, except the
reviewed constructive `User` owner-algebra atom, whose representation is sealed
by a private constructor/field; redundant
invariants are `theorem`s proved from the core or table-validity witnesses (the
minimization discipline) — a failing build is drift.

## Files (load order)

| File | Carries |
|---|---|
| `Causa/Prelude.lean` | minimal `Set`, `Instant`, `Text` |
| `Causa/Owner.lean` | resolved Owner/group model and role ladder (doc 01) |
| `Causa/Identity.lean` | ids, stable `OwnerRef`, append-only/immutable classes, vector-store independence (docs 01, 07) |
| `Causa/Memory.lean` | F/A/P kinds, `(handle, t)` row, origins/refs, `Content` (owner-scoped payload), MemoryHead, Cooled stub (docs 02, UML v0.0.8) |
| `Causa/Knowledge.lean` | text-bearing knowledge artifacts and interpreter-class recoverability |
| `Causa/Goals.lean` | Goal `(handle, t)`, wake_id, GoalHead, transitions, evidence/assignment pins, `situatedSelf` cue-indexed query (docs 06) |
| `Causa/Edges.lean` | pins on the node (no Edge table): two closed kinds, OriginKindValid, derivePins identity, interpretation-as-node |
| `Causa/Authorization.lean` | owner-role read/write ceilings, owner-state resolution, personal/group access theorems |
| `Causa/EdgeAuthorization.lean` | source-owned index reads and the uniform source-write + target-read admission rule |
| `Causa/Operators.lean` | F→A / A→P / A→Goal phase contracts, no downward writes, invocation-ledger completeness (docs 02, 04) |
| `Causa/Provenance.lean` | admitted graph validity, grounding, table-scoped provenance/uniqueness witnesses |
| `Causa/Wake.lean` | Goal-armed wake firing, no-escalation, tool bounds, autonomy/termination theorems |
| `Causa/Citations.lean` | Fact ∪ Abstraction bibliography, 0..1 mapping, owner match (docs 11, 16) |
| `Causa/Compliance.lean` | abandonment-gated erasure, source cascade, target projection redaction (doc 13) |
| `Causa/Principles.lean` | named principle rollups over lower-level theorems |
| `Causa/Flavor.lean` | core independence, optional Memory/Goal sidecars and receipts, payload opacity (docs 03, 08) |

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
