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
  goal_refs := []
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
  goal_refs := []
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
  goal_refs := []
  blob_id := none
  content_id := none
  tick := tick
  fact_origins_empty := fun h => nomatch h
  perspective_never_cites := fun _ => rfl
  blob_fa_only := fun h => (h rfl).elim

/-- An owner-to-owner TRANSFER: the same series, the same `t`, the same
    cognitive content, under a new owning group. There is no publish and no
    universal reader — a transferred memory is exactly as readable as `dest`. -/
def transferred (handle : Handle) (dest : Owner) (id : MemoryId) (tick : Instant) : Memory :=
  fact handle dest id tick

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
    derivePins m = (memory_origins m, memory_refs m, memory_goal_refs m) :=
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

/-- A transfer is an owner SUBSTITUTION, not a copy: handle, `t`, kind and the
    cognitive pins are the source fact's, so the series identity survives the
    move. THEOREM (definitional). -/
theorem transferred_preserves_series_identity
    (handle : Handle) (source dest : Owner) (id : MemoryId) (tick : Instant) :
    (transferred handle dest id tick).handle = (fact handle source id tick).handle ∧
      (transferred handle dest id tick).t = (fact handle source id tick).t ∧
      memory_kind (transferred handle dest id tick)
        = memory_kind (fact handle source id tick) ∧
      memory_origins (transferred handle dest id tick)
        = memory_origins (fact handle source id tick) ∧
      memory_refs (transferred handle dest id tick)
        = memory_refs (fact handle source id tick) :=
  ⟨rfl, rfl, rfl, rfl, rfl⟩

/-- Access to a transferred memory is exactly the destination group's role map:
    the entity carries no visibility flag of its own (invariant #5). This is the
    replacement for the deleted `published_readable` — readability is a fact
    about the OWNER now, not about a reserved universal group. -/
theorem transferred_readable_by_destination_member
    (handle : Handle) (dest : Owner) (id : MemoryId) (tick : Instant)
    (r : User) (x : Role) (hmem : dest r = some x) (hread : x.mayRead .fact) :
    may_read r (transferred handle dest id tick).owner .fact :=
  ⟨x, hmem, hread⟩

/-- The replacement for the deleted `published_read_only`: nothing is readable
    by everyone. A requester with no role in the destination can neither read
    nor write the transferred memory — in particular the PRIOR owner, once the
    transfer has moved the row out of its group, is such a requester. -/
theorem transferred_denies_non_members
    (handle : Handle) (dest : Owner) (id : MemoryId) (tick : Instant)
    (r : User) (k : AccessKind) (hout : dest r = none) :
    ¬ may_read r (transferred handle dest id tick).owner k ∧
      ¬ may_write r (transferred handle dest id tick).owner k :=
  non_member_denied r dest k hout

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
#print axioms transferred_preserves_series_identity
#print axioms transferred_readable_by_destination_member
#print axioms transferred_denies_non_members
#print axioms wipeable_when_abandoned

end Causa.Flavor
