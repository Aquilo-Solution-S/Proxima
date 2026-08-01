/-
Causa — Flavor (the openness proof)

The kernel models NO flavor: no `FlavorId`, runtime vocabulary contribution,
namespacing, or registration. All of that is build-time/engine mechanics
(Composition.lean was deleted, D16). Yet the substrate is OPEN — any application
integrates as a flavor with ZERO kernel change.

This file is the constructive WITNESS of that openness, and it adds NO axiom.
A flavor's "vocabulary" is just inhabitants of the kernel's existing opaque type
`SchemaRef` — its schema tag — taken as a PARAMETER here, never an axiom.
Optional sidecars are flavor-owned wrappers around ordinary `Memory` and `Goal`
rows; the kernel only sees the projected core row. We build concrete flavor rows
and discharge the kernel's universal invariants on them using only pre-existing
theorems: a flavor Fact is a Fact, a flavor Abstraction grounds in Facts (N1), a
flavor Perspective rests on an admitted memory when admitted, flavor rows are
access-controlled and compliance-governed — none of it flavor-specific.

The guarantee is `#print axioms` (below): every flavor theorem rests on no Causa
axioms — never one named `flavor`, because none exists. That ABSENCE,
machine-checked, IS the openness. A flavor adds vocabulary; it never adds a
rule, and never adds a trusted assumption. Compliance is derived, not
axiomatized.

Goals extend the same way: a flavor Goal is a `Goal` carrying the flavor's
schema. EDGES DO NOT EXTEND AT ALL, and that is the deliberate v0.0.8 loss
(doc 16 §What This Removes): there is no `RelationId` for a flavor to mint, no
edge sidecar to type, and the kind vocabulary is closed at two. A flavor that
wants a novel traversable link expresses it as an interpretation node — the
escape valve is total, and the question is never whether something can be
expressed, only where it lives. Optional Memory/Goal sidecar payloads can still
refine those rows without changing kernel validity.
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
  ⟨id, .Fact, owner, schema, none, none, none, none, t, none, none, fun _ => rfl⟩

/-- A concrete flavor Abstraction. -/
def abstraction (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant) : Memory :=
  ⟨id, .Abstraction, owner, schema, none, none, none, none, t, none, none, fun h => nomatch h⟩

/-- A concrete flavor Perspective — useful for app-owned policy/current-state
    views. -/
def perspective (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant) : Memory :=
  ⟨id, .Perspective, owner, schema, none, none, none, none, t, none, none, fun h => nomatch h⟩

/-- A flavor Fact published to World (the universal read-only group). -/
def published (schema : SchemaRef) (id : MemoryId) (t : Instant) : Memory :=
  ⟨id, .Fact, world, schema, none, none, none, none, t, none, none, fun _ => rfl⟩

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

/- There is deliberately NO `OptionalEdgeSidecar`. Typed edge sidecars are
   gone with the relation layer (doc 16 §What This Removes): an index row
   carries no payload at all (E6), and content that wanted a sidecar belongs in
   a node's payload. -/

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

/-- Changing receipt payload does not change the admitted Fact. -/
theorem fact_receipt_payload_irrelevant
    {Payload : Type} (fact : Fact) (payload₁ payload₂ : Payload) :
    (OptionalFactReceipt.mk fact payload₁).fact =
      (OptionalFactReceipt.mk fact payload₂).fact := rfl

/-- Memory invariants apply to the projected `Memory`, independently of the
    optional sidecar payload, once that row belongs to an admitted graph. -/
theorem memory_sidecar_grounded
    (memories : Set Memory) (goals : Set Goal)
    (factEntities : Set FactEntity) (edges : Set Edge)
    (hgraph : MemoryGraphValid memories goals factEntities edges)
    {Payload : Type} (row : OptionalMemorySidecar Payload)
    (hm : row.memory ∈ memories) :
    GroundsInFact edges row.memory :=
  memory_grounds_in_facts memories goals factEntities edges hgraph row.memory hm

/-- Goal invariants apply to the projected `Goal`, independently of the optional
    sidecar payload. This is intentionally just projection: sidecars add no new
    Goal lifecycle rule. -/
theorem goal_sidecar_state_projection
    {Payload : Type} (row : OptionalGoalSidecar Payload) :
    goal_state row.goal = row.goal.state := rfl

/-- A flavor's index rows are ordinary rows: they come from the flavor node's
    own declaration, and a legal declaration derives valid rows by the same
    theorem that governs core (`declared_edges_valid`). A flavor adds no
    typing gate, because there is none left to add to. -/
theorem flavor_declared_edges_valid
    (d : NodeDeclaration) (hd : NodeDeclarationValid d) :
    ∀ e : Edge, e ∈ d.edges → EdgeValid e :=
  declared_edges_valid d hd

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
    (memories : Set Memory) (goals : Set Goal)
    (factEntities : Set FactEntity) (edges : Set Edge)
    (hgraph : MemoryGraphValid memories goals factEntities edges)
    (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant)
    (hm : abstraction schema owner id t ∈ memories) :
    GroundsInFact edges (abstraction schema owner id t) :=
  memory_grounds_in_facts memories goals factEntities edges hgraph _ hm

/-- The flavor's Perspective inherits the universal Perspective rule: once
    admitted to a valid graph it must rest on an admitted memory row — its
    origins if it derived, its subjects if it interprets. -/
theorem flavor_perspective_has_provenance
    (memories : Set Memory) (goals : Set Goal)
    (factEntities : Set FactEntity) (edges : Set Edge)
    (hgraph : MemoryGraphValid memories goals factEntities edges)
    (schema : SchemaRef) (owner : Owner) (id : MemoryId) (t : Instant)
    (hm : perspective schema owner id t ∈ memories) :
    ∃ e : Edge, e ∈ edges ∧
      edge_source e = .memory (perspective schema owner id t) ∧
      (∃ mt : Memory, mt ∈ memories ∧ edge_target e = .memory mt) :=
  Causa.perspective_has_provenance memories goals factEntities edges hgraph _ hm rfl

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
#print axioms fact_receipt_payload_irrelevant
#print axioms memory_sidecar_grounded
#print axioms goal_sidecar_state_projection
#print axioms flavor_declared_edges_valid
#print axioms fact_receipt_is_fact
#print axioms fact_is_fact
#print axioms perspective_is_perspective
#print axioms abstraction_grounded
#print axioms flavor_perspective_has_provenance
#print axioms published_readable
#print axioms published_read_only
#print axioms wipeable_when_abandoned

end Causa.Flavor
