/-
Causa — Flavor (the openness proof)

A flavor's vocabulary is inhabitants of `SchemaRef`. Optional sidecars wrap
ordinary Memory/Goal rows. There is no Edge type to extend.
-/

import Causa.Provenance
import Causa.Authorization
import Causa.Compliance

namespace Causa.Flavor

def fact (handle : Handle) (owner : Owner) (id : MemoryId) (tick : Instant) : Memory where
  handle := handle
  t := id
  kind := .Fact
  owner := owner
  origins := []
  refs := []
  blob_id := none
  content_id := none
  tick := tick
  fact_origins_empty := fun _ => rfl
  perspective_never_cites := fun h => nomatch h
  blob_fa_only := fun h => (h rfl).elim

def abstraction (handle : Handle) (owner : Owner) (id : MemoryId) (tick : Instant) : Memory where
  handle := handle
  t := id
  kind := .Abstraction
  owner := owner
  origins := []
  refs := []
  blob_id := none
  content_id := none
  tick := tick
  fact_origins_empty := fun h => nomatch h
  perspective_never_cites := fun h => nomatch h
  blob_fa_only := fun h => (h rfl).elim

def perspective (handle : Handle) (owner : Owner) (id : MemoryId) (tick : Instant) : Memory where
  handle := handle
  t := id
  kind := .Perspective
  owner := owner
  origins := []
  refs := []
  blob_id := none
  content_id := none
  tick := tick
  fact_origins_empty := fun h => nomatch h
  perspective_never_cites := fun _ => rfl
  blob_fa_only := fun h => (h rfl).elim

def published (handle : Handle) (id : MemoryId) (tick : Instant) : Memory :=
  fact handle world id tick

structure OptionalMemorySidecar (Payload : Type) where
  memory  : Memory
  payload : Payload

structure OptionalGoalSidecar (Payload : Type) where
  goal    : Goal
  payload : Payload

structure OptionalFactReceipt (Payload : Type) where
  fact    : Fact
  payload : Payload

theorem memory_sidecar_payload_irrelevant
    {Payload : Type} (memory : Memory) (payload₁ payload₂ : Payload) :
    (OptionalMemorySidecar.mk memory payload₁).memory =
      (OptionalMemorySidecar.mk memory payload₂).memory := rfl

theorem goal_sidecar_payload_irrelevant
    {Payload : Type} (goal : Goal) (payload₁ payload₂ : Payload) :
    (OptionalGoalSidecar.mk goal payload₁).goal =
      (OptionalGoalSidecar.mk goal payload₂).goal := rfl

theorem fact_receipt_payload_irrelevant
    {Payload : Type} (fact : Fact) (payload₁ payload₂ : Payload) :
    (OptionalFactReceipt.mk fact payload₁).fact =
      (OptionalFactReceipt.mk fact payload₂).fact := rfl

theorem memory_sidecar_grounded
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead) (cooled : Set Cooled)
    (hgraph : MemoryGraphValid memories goals heads cooled)
    {Payload : Type} (row : OptionalMemorySidecar Payload)
    (hm : row.memory ∈ memories) :
    GroundsInFact memories cooled row.memory :=
  memory_grounds_in_facts memories goals heads cooled hgraph row.memory hm

theorem goal_sidecar_state_projection
    {Payload : Type} (row : OptionalGoalSidecar Payload) :
    goal_state row.goal = row.goal.state := rfl

/-- Flavor pins are the node's own arrays. Rebuildability is identity. -/
theorem flavor_declared_pins_are_node_content (m : Memory) :
    derivePins m = (memory_origins m, memory_refs m) :=
  pins_are_node_content m

theorem fact_receipt_is_fact
    {Payload : Type} (receipt : OptionalFactReceipt Payload) :
    memory_kind receipt.fact.memory = .Fact :=
  fact_memory_kind receipt.fact

theorem fact_is_fact (handle : Handle) (owner : Owner) (id : MemoryId) (tick : Instant) :
    memory_kind (fact handle owner id tick) = .Fact := by
  simp [fact, memory_kind]

theorem perspective_is_perspective
    (handle : Handle) (owner : Owner) (id : MemoryId) (tick : Instant) :
    memory_kind (perspective handle owner id tick) = .Perspective := by
  simp [perspective, memory_kind]

theorem abstraction_grounded
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead) (cooled : Set Cooled)
    (hgraph : MemoryGraphValid memories goals heads cooled)
    (handle : Handle) (owner : Owner) (id : MemoryId) (tick : Instant)
    (hm : abstraction handle owner id tick ∈ memories) :
    GroundsInFact memories cooled (abstraction handle owner id tick) :=
  memory_grounds_in_facts memories goals heads cooled hgraph _ hm

theorem flavor_perspective_has_provenance
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead) (cooled : Set Cooled)
    (hgraph : MemoryGraphValid memories goals heads cooled)
    (handle : Handle) (owner : Owner) (id : MemoryId) (tick : Instant)
    (hm : perspective handle owner id tick ∈ memories) :
    memory_origins (perspective handle owner id tick) ≠ [] ∨
      memory_refs (perspective handle owner id tick) ≠ [] :=
  Causa.perspective_has_provenance memories goals heads cooled hgraph _ hm
    (by simp [perspective, memory_kind])

theorem published_readable (handle : Handle) (id : MemoryId) (tick : Instant) (r : User) :
    may_read r (published handle id tick).owner .fact := by
  simp [published, fact]
  exact world_universally_readable r .fact

theorem published_read_only (handle : Handle) (id : MemoryId) (tick : Instant)
    (r : User) (k : AccessKind) :
    ¬ may_write r (published handle id tick).owner k := by
  simp [published, fact]
  exact world_read_only r k

theorem wipeable_when_abandoned (handle : Handle) (owner : Owner) (id : MemoryId) (tick : Instant)
    (h : abandoned (fact handle owner id tick).owner) (r : User) :
    (fact handle owner id tick).owner r = none :=
  h r

#print axioms memory_sidecar_payload_irrelevant
#print axioms goal_sidecar_payload_irrelevant
#print axioms fact_receipt_payload_irrelevant
#print axioms memory_sidecar_grounded
#print axioms goal_sidecar_state_projection
#print axioms flavor_declared_pins_are_node_content
#print axioms fact_receipt_is_fact
#print axioms fact_is_fact
#print axioms perspective_is_perspective
#print axioms abstraction_grounded
#print axioms flavor_perspective_has_provenance
#print axioms published_readable
#print axioms published_read_only
#print axioms wipeable_when_abandoned

end Causa.Flavor
