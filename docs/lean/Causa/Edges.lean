/-
Causa — Pins (no Edge table)

v0.0.8: there is no index table. `origins[]` and `refs[]` live on the
Memory row (UML §5). The two closed kinds remain:

  origin    = made-from   (origins)
  reference = points-at   (refs onto Memory, goal_refs onto Goal)

Rebuildability is identity: the pin set IS node content. Goal pins live on
the Goal row (`assignment_t`, `dependency_t`, `evidence_t`, `close_fact_t`,
`write_act_t`) and never appear in `memory.refs`.

E1–E7 rebase:
E1 existence   — live origin and refs t are a hot Memory.t or a cooled stub;
                goal_refs resolves against the Goal spine. Retained historical
                pins may also resolve through a kinded erase witness.
  E2 ownership   — the pin is on the declaring row; no separate owner column
  E3 layering    — OriginKindValid (UML CHECKs)
  E4 kind follows — origins vs refs; no verb writes a pin
  E5 / E6        — there is no pin row, so no pin id and no pin payload
  E7 rebuild     — derivePins m = (m.origins, m.refs, m.goal_refs)
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity
import Causa.Memory
import Causa.Goals

namespace Causa

inductive EdgeKind where
  | origin
  | reference
  deriving DecidableEq, Repr

/-- Pins extracted from a node — THE derivation, and it is identity.
    Three columns since v0.0.11: the Goal spine is its own list. -/
def derivePins (m : Memory) :
    List MemoryId × List MemoryId × List GoalId :=
  (memory_origins m, memory_refs m, memory_goal_refs m)

/- A legacy/live reference carrier can still name either spine. The storage
   split is reflected by `goalReferenceTargetExists`; this union remains the
   target-existence predicate for the pre-split/live graph relation. -/
def referenceTargetExists
    (memories : Set Memory) (goals : Set Goal) (cooled : Set Cooled)
    (id : Id) : Prop :=
  pinExists memories cooled id ∨
  ∃ g : Goal, g ∈ goals ∧ goal_t g = id

/- A reference may point at a Goal, but no longer from the same column as a
   Memory reference. `refs` resolves against the Memory spine (`pinExists`)
   and `goal_refs` against the Goal spine; the column IS the target's kind,
   so nothing downstream has to re-derive it. Goal-DECLARED pins remain on
   the Goal row and are a separate thing again. -/
def goalReferenceTargetExists (goals : Set Goal) (id : GoalId) : Prop :=
  ∃ g : Goal, g ∈ goals ∧ goal_t g = id

/-- Historical reference existence: a live/cooled Memory, a live Goal, or a
    database-only erase witness. The mixed predicate is legacy-only; the
    typed predicates below are the retained graph contract after the split. -/
def retainedReferenceTargetExists
    (memories : Set Memory) (goals : Set Goal) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (id : Id) : Prop :=
  referenceTargetExists memories goals cooled id ∨
  erasedPinTargetExists targets id

def retainedMemoryReferenceTargetExists
    (memories : Set Memory) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (id : MemoryId) : Prop :=
  pinExists memories cooled id ∨ recordedMemoryTargetExists targets id

def retainedGoalReferenceTargetExists
    (goals : Set Goal) (targets : Set ErasedPinTarget) (id : GoalId) : Prop :=
  goalReferenceTargetExists goals id ∨ recordedGoalTargetExists targets id

def retainedOriginTargetExists
    (memories : Set Memory) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (id : MemoryId) : Prop :=
  pinExists memories cooled id ∨ recordedMemoryTargetExists targets id

theorem goal_is_reference_target
    (goals : Set Goal) (g : Goal) (hg : g ∈ goals) :
    goalReferenceTargetExists goals (goal_t g) :=
  ⟨g, hg, rfl⟩

/-- Read-side resolution (v0.0.11). `goal_refs` is the STORED typing; what
    a given reader may see is a separate question, so before projection the
    Goals this reader cannot read are moved back into the untyped `refs`
    carrier, where they redact exactly as an unreadable Memory does. The
    stored discriminant must not survive into the projection: knowing that
    a withheld target is a Goal is itself disclosure. -/
def resolveVisibleGoalRefs (readable : GoalId → Bool) (m : Memory) :
    List MemoryId × List GoalId :=
  (memory_refs m ++ (memory_goal_refs m).filter (fun g => !readable g),
   (memory_goal_refs m).filter readable)

/-- The non-disclosure invariant. A row carrying an unreadable Goal
    reference resolves to exactly the carrier of a row that had that id in
    `refs` all along — so no reader downstream can tell the two apart, and
    the split buys its typing without widening what a reader learns. -/
theorem unreadable_goal_reference_resolves_like_a_memory_reference
    (readable : GoalId → Bool) (m n : Memory) (g : GoalId)
    (hmg : memory_goal_refs m = [g]) (hng : memory_goal_refs n = [])
    (hrefs : memory_refs n = memory_refs m ++ [g])
    (hg : readable g = false) :
    resolveVisibleGoalRefs readable m = resolveVisibleGoalRefs readable n := by
  simp [resolveVisibleGoalRefs, hmg, hng, hrefs, hg]

/-- A readable Goal stays typed: resolution is a filter, not a demotion. -/
theorem readable_goal_reference_survives_resolution
    (readable : GoalId → Bool) (m : Memory) (g : GoalId)
    (hmg : memory_goal_refs m = [g]) (hg : readable g = true) :
    resolveVisibleGoalRefs readable m = (memory_refs m, [g]) := by
  simp [resolveVisibleGoalRefs, hmg, hg]

/-- The read-cost claim. A row with no Goal reference resolves to itself,
    so nothing about the Goal spine has to be consulted to project it.
    Before the split this was undecidable from the row: `refs` could not
    say whether any of its ids were Goals, so every read had to ask. -/
theorem a_row_without_goal_refs_resolves_to_itself
    (readable : GoalId → Bool) (m : Memory) (h : memory_goal_refs m = []) :
    resolveVisibleGoalRefs readable m = (memory_refs m, []) := by
  simp [resolveVisibleGoalRefs, h]

theorem retained_goal_is_reference_target
    (_memories : Set Memory) (goals : Set Goal) (_cooled : Set Cooled)
    (targets : Set ErasedPinTarget)
    (g : Goal) (hg : g ∈ goals) :
    retainedGoalReferenceTargetExists goals targets (goal_t g) := by
  exact Or.inl (goal_is_reference_target goals g hg)

theorem erased_goal_is_retained_goal_reference
    (goals : Set Goal) (targets : Set ErasedPinTarget)
    (e : ErasedPinTarget) (he : e ∈ targets)
    (hk : erased_pin_target_kind e = .Goal) :
    retainedGoalReferenceTargetExists goals targets (erased_pin_target_t e) := by
  exact Or.inr ⟨e, he, rfl, hk⟩

theorem erased_memory_is_retained_memory_reference
    (memories : Set Memory) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget)
    (e : ErasedPinTarget) (he : e ∈ targets)
    (hk : erased_pin_target_kind e ≠ .Goal) :
    retainedMemoryReferenceTargetExists memories cooled targets
      (erased_pin_target_t e) := by
  cases hkind : erased_pin_target_kind e with
  | Fact => exact Or.inr ⟨.Fact, e, he, rfl, hkind⟩
  | Abstraction => exact Or.inr ⟨.Abstraction, e, he, rfl, hkind⟩
  | Perspective => exact Or.inr ⟨.Perspective, e, he, rfl, hkind⟩
  | Goal => exact False.elim (hk hkind)

/-- Legacy-only mixed carrier: a witness kind is not checked against the
    caller's typed column. New retained graph proofs use the two predicates
    above instead. -/
theorem erased_is_retained_reference_target
    (memories : Set Memory) (goals : Set Goal) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget)
    (e : ErasedPinTarget) (he : e ∈ targets) :
    retainedReferenceTargetExists memories goals cooled targets
      (erased_pin_target_t e) := by
  exact Or.inr ⟨e, he, rfl⟩

/-- Legacy-only mixed live carrier; typed retained proofs do not use it. -/
theorem live_reference_target_is_retained
    (memories : Set Memory) (goals : Set Goal) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (id : Id)
    (h : referenceTargetExists memories goals cooled id) :
    retainedReferenceTargetExists memories goals cooled targets id :=
  Or.inl h

theorem live_origin_target_is_retained
    (memories : Set Memory) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (id : MemoryId)
    (h : pinExists memories cooled id) :
    retainedOriginTargetExists memories cooled targets id :=
  Or.inl h

theorem pins_are_node_content (m : Memory) :
    derivePins m = (m.origins, m.refs, m.goal_refs) := rfl

theorem derived_table_rebuildable (m : Memory) :
    derivePins m = (memory_origins m, memory_refs m, memory_goal_refs m) :=
  pins_are_node_content m

/-- E4z — a write with empty origins is legal (interpretation Perspective). -/
theorem declaration_without_origins_writes_no_origin_pins (m : Memory)
    (h : memory_origins m = []) :
    ∀ id : MemoryId, id ∈ memory_origins m → False := by
  intro id hin
  rw [h] at hin
  cases hin

/-- Fact sources declare no origins, so they never originate from A/P. -/
theorem fact_source_reaches_only_facts (m : Memory)
    (hk : memory_kind m = .Fact) :
    memory_origins m = [] :=
  m.fact_origins_empty hk

/-- Layer rule on origins: A origins are Facts or Abstractions; P origins
    are empty or A's. Targets may be hot or cooled. -/
theorem origin_layer_rule
    (memories : Set Memory) (cooled : Set Cooled) (m : Memory)
    (hv : OriginKindValid memories cooled m) :
    (memory_kind m = .Abstraction →
      ∀ id : MemoryId, id ∈ memory_origins m →
        pinKindFactOrAbstraction memories cooled id) ∧
    (memory_kind m = .Perspective →
      memory_origins m = [] ∨
      ∀ id : MemoryId, id ∈ memory_origins m →
        pinKindIs memories cooled id .Abstraction ∧
          MemoryKind.layer .Abstraction ≤ (memory_kind m).layer) := by
  constructor
  · intro habs id hid
    exact hv.absFactOrAbs habs id hid
  · intro hp
    cases hv.perspAbsOrEmpty hp with
    | inl hempty => exact Or.inl hempty
    | inr hall =>
      refine Or.inr ?_
      intro id hid
      refine ⟨hall id hid, ?_⟩
      rw [hp]
      exact Nat.le_succ 1

theorem abstraction_origin_fact_or_abstraction
    (memories : Set Memory) (cooled : Set Cooled) (m : Memory) (id : MemoryId)
    (hv : OriginKindValid memories cooled m)
    (habs : memory_kind m = .Abstraction)
    (hid : id ∈ memory_origins m) :
    pinKindFactOrAbstraction memories cooled id :=
  hv.absFactOrAbs habs id hid

/-- Interpretation is a Perspective with empty origins that refs a subject. -/
def interpretationOf (p : Memory) (subject : Memory) : Prop :=
  memory_kind p = .Perspective ∧
  memory_origins p = [] ∧
  memory_t subject ∈ memory_refs p

theorem interpretation_is_never_a_fact
    (p subject : Memory) (h : interpretationOf p subject) :
    memory_kind p ≠ .Fact := by
  intro hf
  rw [h.1] at hf
  exact (nomatch hf)

theorem interpretation_rows_are_references
    (p subject : Memory) (h : interpretationOf p subject) :
    memory_t subject ∈ memory_refs p :=
  h.2.2

/-- Goal pins are the Goal-row columns, never `Memory.refs`. -/
theorem goal_declared_rows_are_references (g : Goal) :
    goalDeclaredTargetIds g =
      (goal_assignment g).toList ++ goal_dependencies g ++ goal_evidence g ++
        (goal_close_fact_t g).toList ++ (goal_write_act_t g).toList :=
  rfl

theorem goal_declared_row_count (g : Goal) :
    (goalDeclaredTargetIds g).length =
      ((goal_assignment g).toList ++ goal_dependencies g ++ goal_evidence g ++
        (goal_close_fact_t g).toList ++ (goal_write_act_t g).toList).length := rfl

/-- Closed vocabulary: two kinds. -/
theorem principle_epistemic_edge_kinds_are_exactly_two_aux (k : EdgeKind) :
    k = .origin ∨ k = .reference := by
  cases k with
  | origin => exact Or.inl rfl
  | reference => exact Or.inr rfl

/-- info: 'Causa.pins_are_node_content' does not depend on any axioms -/
#guard_msgs in
#print axioms pins_are_node_content

/-- info: 'Causa.fact_source_reaches_only_facts' does not depend on any axioms -/
#guard_msgs in
#print axioms fact_source_reaches_only_facts

/-- info: 'Causa.interpretation_is_never_a_fact' does not depend on any axioms -/
#guard_msgs in
#print axioms interpretation_is_never_a_fact

/-- info: 'Causa.declaration_without_origins_writes_no_origin_pins' does not depend on any axioms -/
#guard_msgs in
#print axioms declaration_without_origins_writes_no_origin_pins

/-- info: 'Causa.origin_layer_rule' does not depend on any axioms -/
#guard_msgs in
#print axioms origin_layer_rule

/-- info: 'Causa.abstraction_origin_fact_or_abstraction' does not depend on any axioms -/
#guard_msgs in
#print axioms abstraction_origin_fact_or_abstraction

/-- info: 'Causa.unreadable_goal_reference_resolves_like_a_memory_reference' depends on axioms: [propext] -/
#guard_msgs in
#print axioms unreadable_goal_reference_resolves_like_a_memory_reference

/-- info: 'Causa.readable_goal_reference_survives_resolution' depends on axioms: [propext] -/
#guard_msgs in
#print axioms readable_goal_reference_survives_resolution

/-- info: 'Causa.a_row_without_goal_refs_resolves_to_itself' depends on axioms: [propext] -/
#guard_msgs in
#print axioms a_row_without_goal_refs_resolves_to_itself

end Causa
