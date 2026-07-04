/-
Causa — Flavor (the openness proof)

The kernel models NO flavor: no `FlavorId`, runtime vocabulary contribution,
namespacing, or registration. All of that is build-time/engine mechanics
(Composition.lean was deleted, D16). Yet the substrate is OPEN — any application
integrates as a flavor with ZERO kernel change.

This file is the constructive WITNESS of that openness, and it adds NO axiom.
A flavor's "vocabulary" is just inhabitants of the kernel's existing opaque types
— a `SchemaRef` (its schema tag), a `RelationId` (its edge kinds). Those are
PARAMETERS here, never axioms. Optional sidecars are flavor-owned wrappers around
ordinary `Memory`, `Goal`, and `Edge` rows; the kernel only sees the projected
core row. We build concrete flavor rows and discharge the kernel's universal
invariants on them using only pre-existing theorems: a flavor Fact is a Fact, a
flavor Abstraction grounds in Facts (N1), a flavor Perspective has Abstraction
provenance when admitted, flavor rows are access-controlled and
compliance-governed — none of it flavor-specific.

The guarantee is `#print axioms` (below): every flavor theorem rests on no Causa
axioms — never one named `flavor`, because none exists. That ABSENCE,
machine-checked, IS the openness. A flavor adds vocabulary; it never adds a
rule, and never adds a trusted assumption. Compliance is derived, not
axiomatized.

Goals and edges extend the same way: a flavor Goal is a `Goal` carrying the
flavor's schema; a flavor edge is a valid `Edge` with the flavor's `RelationId`,
whose class is forced into the CLOSED `RelationClass` inductive — flavors add
relation ids, never classes (CF-F). Optional sidecar payloads can refine any of
these rows without changing kernel validity.
-/

import Causa.Provenance
import Causa.Authorization
import Causa.Compliance

namespace Causa.Flavor

-- A flavor's vocabulary: values of EXISTING kernel types, taken as PARAMETERS.
-- `schema` is the flavor's opaque schema tag (CF-G: the kernel never sees any
-- payload behind it); `owner` is the owning group of the row.

/-- A concrete flavor Fact — an ordinary `Memory` carrying the flavor's schema. -/
def fact (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant) : Memory :=
  ⟨id, .Fact, owner, schema, none, none, none, none, t⟩

/-- A concrete flavor Abstraction. -/
def abstraction (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant) : Memory :=
  ⟨id, .Abstraction, owner, schema, none, none, none, none, t⟩

/-- A concrete flavor Perspective — useful for app-owned policy/current-state
    views. -/
def perspective (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant) : Memory :=
  ⟨id, .Perspective, owner, schema, none, none, none, none, t⟩

/-- A flavor Fact published to World (the universal read-only group). -/
def published (schema : SchemaRef) (id : MemoryId) (t : Instant) : Memory :=
  ⟨id, .Fact, world, schema, none, none, none, none, t⟩

/-- Optional flavor-owned Memory sidecar. There is intentionally no theorem
    requiring such a wrapper for every memory row; sidecars are flavor/engine
    storage, not kernel ontology. -/
structure OptionalMemorySidecar (Payload : Type) where
  memory  : Memory
  payload : Payload

/-- Optional flavor-owned Goal sidecar. Goals already carry an opaque schema;
    additional typed payload remains flavor/engine storage. -/
structure OptionalGoalSidecar (Payload : Type) where
  goal    : Goal
  payload : Payload

/-- Optional flavor-owned Edge sidecar. Edge typing is still governed by the
    registered relation descriptor; additional payload is flavor/engine storage. -/
structure OptionalEdgeSidecar (Payload : Type) where
  edge    : Edge
  payload : Payload

/-- Optional event/receipt metadata for an admitted Fact. A receipt can represent
    any observed source: webhook, alert, sensor spike, API write, or a direct
    `store memory` call. The kernel proves only that the receipt is attached to
    a Fact row, not that the external world was "true". -/
structure OptionalFactReceipt (Payload : Type) where
  fact    : Fact
  payload : Payload

/-- Changing Memory-sidecar payload does not change the kernel-visible row. -/
theorem memory_sidecar_payload_irrelevant
    {Payload : Type} (memory : Memory) (payload₁ payload₂ : Payload) :
    (OptionalMemorySidecar.mk memory payload₁).memory =
      (OptionalMemorySidecar.mk memory payload₂).memory := rfl

/-- Changing Goal-sidecar payload does not change the kernel-visible row. -/
theorem goal_sidecar_payload_irrelevant
    {Payload : Type} (goal : Goal) (payload₁ payload₂ : Payload) :
    (OptionalGoalSidecar.mk goal payload₁).goal =
      (OptionalGoalSidecar.mk goal payload₂).goal := rfl

/-- Changing Edge-sidecar payload does not change the kernel-visible row. -/
theorem edge_sidecar_payload_irrelevant
    {Payload : Type} (edge : Edge) (payload₁ payload₂ : Payload) :
    (OptionalEdgeSidecar.mk edge payload₁).edge =
      (OptionalEdgeSidecar.mk edge payload₂).edge := rfl

/-- Changing receipt payload does not change the admitted Fact. -/
theorem fact_receipt_payload_irrelevant
    {Payload : Type} (fact : Fact) (payload₁ payload₂ : Payload) :
    (OptionalFactReceipt.mk fact payload₁).fact =
      (OptionalFactReceipt.mk fact payload₂).fact := rfl

/-- Memory invariants apply to the projected `Memory`, independently of the
    optional sidecar payload, once that row belongs to an admitted graph. -/
theorem memory_sidecar_grounded
    (registry : RelationRegistry) (memories : Set Memory) (goals : Set Goal)
    (factEntities : Set FactEntity) (edges : Set Edge)
    (hgraph : MemoryGraphValid registry memories goals factEntities edges)
    {Payload : Type} (row : OptionalMemorySidecar Payload)
    (hm : row.memory ∈ memories) :
    GroundsInFact registry edges row.memory :=
  memory_grounds_in_facts registry memories goals factEntities edges hgraph row.memory hm

/-- Goal invariants apply to the projected `Goal`, independently of the optional
    sidecar payload. This is intentionally just projection: sidecars add no new
    Goal lifecycle rule. -/
theorem goal_sidecar_state_projection
    {Payload : Type} (row : OptionalGoalSidecar Payload) :
    goal_state row.goal = row.goal.state := rfl

/-- Edge validity applies to the projected `Edge`, independently of the optional
    sidecar payload. Relation descriptors remain the only kernel typing gate. -/
theorem edge_sidecar_core_valid
    (registry : RelationRegistry) {Payload : Type} (row : OptionalEdgeSidecar Payload)
    (h : EdgeCoreValid registry row.edge) :
    EdgeCoreValid registry row.edge := h

/-- A receipt witnesses only admission of a Fact-shaped row. It does not certify
    external truth; source-specific trust belongs to flavor/engine policy. -/
theorem fact_receipt_is_fact
    {Payload : Type} (receipt : OptionalFactReceipt Payload) :
    memory_kind receipt.fact.memory = .Fact :=
  fact_memory_kind receipt.fact

/-- The flavor's row IS a Fact — the universal kind law governs it, no flavor
    clause. -/
theorem fact_is_fact (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant) :
    memory_kind (fact schema owner id t) = .Fact := rfl

/-- The flavor's row IS a Perspective — applications do not need to directly
    construct raw `Memory` rows for policy/current-state views. -/
theorem perspective_is_perspective
    (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant) :
    memory_kind (perspective schema owner id t) = .Perspective := rfl

/-- The flavor's Abstraction is grounded in Facts when admitted into a valid
    memory graph. The flavor inherits provenance grounding from the table bundle;
    no flavor-specific axiom is added. -/
theorem abstraction_grounded
    (registry : RelationRegistry) (memories : Set Memory) (goals : Set Goal)
    (factEntities : Set FactEntity) (edges : Set Edge)
    (hgraph : MemoryGraphValid registry memories goals factEntities edges)
    (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant)
    (hm : abstraction schema owner id t ∈ memories) :
    GroundsInFact registry edges (abstraction schema owner id t) :=
  memory_grounds_in_facts registry memories goals factEntities edges hgraph _ hm

/-- The flavor's Perspective inherits the universal Perspective provenance rule:
    once admitted to a valid graph, it must carry Provenance to an admitted
    Abstraction. -/
theorem perspective_has_abstraction_provenance
    (registry : RelationRegistry) (memories : Set Memory) (goals : Set Goal)
    (factEntities : Set FactEntity) (edges : Set Edge)
    (hgraph : MemoryGraphValid registry memories goals factEntities edges)
    (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant)
    (hm : perspective schema owner id t ∈ memories) :
    ∃ e : Edge, e ∈ edges ∧ EdgeHasClass registry e .Provenance ∧
      edge_source e = .memory (perspective schema owner id t) ∧
      (∃ mt : Memory, mt ∈ memories ∧ edge_target e = .memory mt ∧
        memory_kind mt = .Abstraction) :=
  Causa.perspective_has_provenance registry memories goals factEntities edges
    hgraph _ hm rfl

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

-- THE openness guarantee, machine-checked: each list below names no Causa
-- axioms. None is named `flavor`, because the kernel has none. Extensibility is
-- proven, not assumed.
#print axioms memory_sidecar_payload_irrelevant
#print axioms goal_sidecar_payload_irrelevant
#print axioms edge_sidecar_payload_irrelevant
#print axioms fact_receipt_payload_irrelevant
#print axioms memory_sidecar_grounded
#print axioms goal_sidecar_state_projection
#print axioms edge_sidecar_core_valid
#print axioms fact_receipt_is_fact
#print axioms fact_is_fact
#print axioms perspective_is_perspective
#print axioms abstraction_grounded
#print axioms perspective_has_abstraction_provenance
#print axioms published_readable
#print axioms published_read_only
#print axioms wipeable_when_abandoned

end Causa.Flavor
