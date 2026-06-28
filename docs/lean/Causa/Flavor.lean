/-
Causa — Flavor (the openness proof)

The kernel models NO flavor: no `FlavorId`, registry, vocabulary contribution,
namespacing, or registration. All of that is build-time/engine mechanics
(Composition.lean was deleted, D16). Yet the substrate is OPEN — any application
integrates as a flavor with ZERO kernel change.

This file is the constructive WITNESS of that openness, and it adds NO axiom.
A flavor's "vocabulary" is just inhabitants of the kernel's existing opaque types
— a `SchemaRef` (its sidecar schema), a `RelationId` (its edge kinds). Those are
PARAMETERS here, never axioms. We build a concrete flavor's `Memory` rows and
discharge the kernel's universal invariants on them using only pre-existing
theorems: a flavor Fact is a Fact, a flavor Abstraction grounds in Facts (N1),
flavor rows are access-controlled and compliance-governed — none of it
flavor-specific.

The guarantee is `#print axioms` (below): every flavor theorem rests ONLY on
axioms the kernel already trusts — never one named `flavor`, because none exists.
That ABSENCE, machine-checked, IS the openness. A flavor adds vocabulary; it never
adds a rule, and never adds a trusted assumption. Compliance is derived, not
axiomatized.

(Goals and edges extend the same way: a flavor Goal is a `Goal` carrying the
flavor's schema; a flavor edge is a valid `Edge` with the flavor's `RelationId`,
whose class is forced into the CLOSED `RelationClass` inductive — flavors add
relation ids, never classes (CF-F). Memory suffices to make the point.)
-/

import Causa.Provenance
import Causa.Authorization
import Causa.Compliance

namespace Causa.Flavor

-- A flavor's vocabulary: values of EXISTING kernel types, taken as PARAMETERS.
-- `schema` is the flavor's opaque sidecar schema tag (CF-G: the kernel never sees
-- the payload behind it); `owner` is the owning group of the row.

/-- A concrete flavor Fact — an ordinary `Memory` carrying the flavor's schema. -/
def fact (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant) : Memory :=
  ⟨id, .Fact, owner, schema, none, t⟩

/-- A concrete flavor Abstraction. -/
def abstraction (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant) : Memory :=
  ⟨id, .Abstraction, owner, schema, none, t⟩

/-- A flavor Fact published to World (the universal read-only group). -/
def published (schema : SchemaRef) (id : MemoryId) (t : Instant) : Memory :=
  ⟨id, .Fact, world, schema, none, t⟩

/-- The flavor's row IS a Fact — the universal kind law governs it, no flavor
    clause. -/
theorem fact_is_fact (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant) :
    memory_kind (fact schema owner id t) = .Fact := rfl

/-- The flavor's Abstraction is grounded in Facts — the universal N1 grounding
    theorem (`memory_grounds_in_facts`) applies directly. The flavor inherits
    provenance grounding for free. -/
theorem abstraction_grounded
    (registry : RelationRegistry) (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant) :
    GroundsInFact registry (abstraction schema owner id t) :=
  memory_grounds_in_facts registry _

/-- A flavor Fact published to World is readable by every requester — its access
    is its owner's, governed by the universal rule (`world_universally_readable`). -/
theorem published_readable (schema : SchemaRef) (id : MemoryId) (t : Instant) (r : User) :
    may_read r (published schema id t).owner .fact :=
  world_universally_readable r .fact

/-- …and cannot be written: publishing to World is read-only, by the same rule
    that governs every World-owned row. -/
theorem published_read_only (schema : SchemaRef) (id : MemoryId) (t : Instant)
    (r : User) (k : AccessKind) :
    ¬ may_write r (published schema id t).owner k :=
  world_read_only r k

/-- The flavor's row is subject to compliance: if its owning group empties, no one
    owns it — it is wipeable by the same abandonment rule as any row, no flavor
    clause. -/
theorem wipeable_when_abandoned (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant)
    (h : abandoned (fact schema owner id t).owner) (r : User) :
    (fact schema owner id t).owner r = none :=
  h r

-- THE openness guarantee, machine-checked: each list below names ONLY axioms the
-- kernel already trusts (`User`, `derived_has_provenance`,
-- `derivation_created_at_strict`, …). None is named `flavor`, because the kernel
-- has none. Extensibility is proven, not assumed.
#print axioms abstraction_grounded
#print axioms published_readable
#print axioms wipeable_when_abandoned

end Causa.Flavor
