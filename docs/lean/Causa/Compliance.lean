/-
Causa — Compliance

Hard delete is abandonment-only, plus the v0.0.8 cold path:

  wipeable := abandoned ∨ (cold ∧ unreferenced ∧ policy)

Forget cools; erase wipes abandoned owners. An owner-to-owner transfer carries
the entity's erase reach with it: after the move it is the DESTINATION owner
whose abandonment wipes the row. There is no edge cascade — pins live on the
declaring row.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Memory
import Causa.Authorization
import Causa.EdgeAuthorization

namespace Causa

def abandoned (o : Owner) : Prop := ∀ u : User, o u = none

theorem drop_personal_abandoned (u : User) :
    abandoned ((Owner.ofUser u).drop u) := by
  intro v
  by_cases h : v = u <;> simp [Group.drop, Owner.ofUser, h]

/-- No hot row still pins this `t`. -/
def unreferenced (memories : Set Memory) (id : MemoryId) : Prop :=
  ∀ m : Memory, m ∈ memories →
    id ∉ memory_origins m ∧ id ∉ memory_refs m

/-- No remaining admission names this Content. Cooled stubs do not keep
    payload identity; Content GC is admission-set emptiness. -/
def contentUnreferenced (memories : Set Memory) (id : ContentId) : Prop :=
  ∀ m : Memory, m ∈ memories → memory_content_id m ≠ some id

def contentWipeable (o : Owner) (memories : Set Memory) (id : ContentId) : Prop :=
  abandoned o ∨ contentUnreferenced memories id

theorem content_wipeable_when_abandoned
    (o : Owner) (memories : Set Memory) (id : ContentId) (h : abandoned o) :
    contentWipeable o memories id :=
  Or.inl h

theorem content_wipeable_when_unreferenced
    (o : Owner) (memories : Set Memory) (id : ContentId)
    (h : contentUnreferenced memories id) :
    contentWipeable o memories id :=
  Or.inr h

def cold (stubs : Set Cooled) (id : MemoryId) : Prop :=
  ∃ c : Cooled, c ∈ stubs ∧ cooled_t c = id

/-- CO-7 / ST-13 rebase: abandonment OR (cold ∧ unreferenced ∧ policy). -/
def wipeable (o : Owner) (memories : Set Memory) (stubs : Set Cooled)
    (id : MemoryId) (policy : Prop) : Prop :=
  abandoned o ∨ (cold stubs id ∧ unreferenced memories id ∧ policy)

theorem wipeable_when_abandoned
    (o : Owner) (memories : Set Memory) (stubs : Set Cooled)
    (id : MemoryId) (policy : Prop) (h : abandoned o) :
    wipeable o memories stubs id policy :=
  Or.inl h

theorem wipeable_when_cold_unreferenced_policy
    (o : Owner) (memories : Set Memory) (stubs : Set Cooled)
    (id : MemoryId) (policy : Prop)
    (hc : cold stubs id) (hu : unreferenced memories id) (hp : policy) :
    wipeable o memories stubs id policy :=
  Or.inr ⟨hc, hu, hp⟩

/-- A pin to a cooled `t` renders Cold, not a missing origin. -/
def pin_target_cold (stubs : Set Cooled) (id : MemoryId) : Prop :=
  cold stubs id

/-- Target render: hot / Cold / Unavailable (UML §5c). -/
def pin_render_hot (memories : Set Memory) (id : MemoryId) : Prop :=
  ∃ m : Memory, m ∈ memories ∧ memory_t m = id

def pin_render_cold (memories : Set Memory) (stubs : Set Cooled) (id : MemoryId) : Prop :=
  ¬ pin_render_hot memories id ∧ cold stubs id

def pin_render_unavailable (memories : Set Memory) (stubs : Set Cooled) (id : MemoryId) : Prop :=
  ¬ pin_render_hot memories id ∧ ¬ cold stubs id

/-- Forget of a pinned `t` does not delete the pinning row. -/
theorem pin_survives_target_cool
    (memories : Set Memory) (stubs : Set Cooled) (m : Memory) (id : MemoryId)
    (hm : m ∈ memories)
    (_hpin : id ∈ memory_origins m ∨ id ∈ memory_refs m)
    (hc : cold stubs id) :
    pinExists memories stubs id ∧ m ∈ memories :=
  ⟨Or.inr hc, hm⟩

-- ============================================================
-- Modeled hard-erase transitions
-- ============================================================

/-- Remove one Memory admission by its immutable `t`.  The model does not
    encode SQL lock order or trigger sequencing; it only describes the
    committed set transform. -/
def hardEraseMemoryRows (memories : Set Memory) (target : Memory) : Set Memory :=
  fun m => m ∈ memories ∧ memory_t m ≠ memory_t target

/-- Add the closed kind witness emitted when a Memory row is erased. -/
def hardEraseMemoryWitnesses
    (targets : Set ErasedPinTarget) (target : Memory) :
    Set ErasedPinTarget :=
  fun e => e ∈ targets ∨
    (erased_pin_target_t e = memory_t target ∧
      erased_pin_target_kind e = (memory_kind target).pinTargetKind)

/-- Pure state relation for a Memory hard erase.  The preconditions model the
    database's uniqueness/disjointness fences, including rejection of an
    already-recorded target with a conflicting kind. -/
structure HardEraseMemoryTransition
    (memories : Set Memory) (cooled : Set Cooled)
    (goals : Set Goal) (targets : Set ErasedPinTarget)
    (target : Memory) (memories' : Set Memory)
    (targets' : Set ErasedPinTarget) : Prop where
  targetInMemories : target ∈ memories
  memoryIdsUnique : MemoryIdUnique memories
  witnessIdsUnique : ErasedPinTargetIdUnique targets
  witnessMemoryDisjoint :
    ErasedTargetsDisjointMemories targets (hardEraseMemoryRows memories target)
  witnessCooledDisjoint : ErasedTargetsDisjointCooled targets cooled
  witnessGoalDisjoint : ErasedTargetsDisjointGoals targets goals
  targetWitnessCompatible :
    ErasedPinTargetKindCompatible targets (memory_t target)
      (memory_kind target).pinTargetKind
  targetNotCooled : ∀ c : Cooled, c ∈ cooled → memory_t target ≠ cooled_t c
  targetNotGoal : ∀ g : Goal, g ∈ goals → memory_t target ≠ goal_t g
  memoriesEq : memories' = hardEraseMemoryRows memories target
  targetsEq : targets' = hardEraseMemoryWitnesses targets target

theorem hard_erase_memory_records_kind
    (memories : Set Memory) (cooled : Set Cooled) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Memory)
    (memories' : Set Memory) (targets' : Set ErasedPinTarget)
    (htransition : HardEraseMemoryTransition
      memories cooled goals targets target memories' targets') :
    recordedMemoryTargetKind
      targets' (memory_t target) (memory_kind target) := by
  rw [htransition.targetsEq]
  refine ⟨{ t := memory_t target, kind := (memory_kind target).pinTargetKind }, ?_, rfl, rfl⟩
  exact Or.inr ⟨rfl, rfl⟩

theorem hard_erase_memory_rejects_wrong_kind
    (memories : Set Memory) (cooled : Set Cooled) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Memory)
    (e : ErasedPinTarget) (he : e ∈ targets)
    (hid : erased_pin_target_t e = memory_t target)
    (hkind : erased_pin_target_kind e ≠ (memory_kind target).pinTargetKind) :
    ¬ HardEraseMemoryTransition
      memories cooled goals targets target
        (hardEraseMemoryRows memories target)
        (hardEraseMemoryWitnesses targets target) := by
  intro htransition
  exact hkind (htransition.targetWitnessCompatible e he hid)

theorem hard_erase_memory_preserves_source
    (memories : Set Memory) (cooled : Set Cooled)
    (goals : Set Goal) (targets : Set ErasedPinTarget)
    (target source : Memory) (memories' : Set Memory)
    (targets' : Set ErasedPinTarget)
    (htransition : HardEraseMemoryTransition
      memories cooled goals targets target memories' targets')
    (hs : source ∈ memories) (hsource : memory_t source ≠ memory_t target) :
    ∃ post : Memory,
      post ∈ memories' ∧ post = source ∧
      memory_origins post = memory_origins source ∧
      memory_refs post = memory_refs source := by
  rw [htransition.memoriesEq]
  exact ⟨source, ⟨hs, hsource⟩, rfl, rfl, rfl⟩

theorem hard_erase_memory_result_well_formed
    (memories : Set Memory) (cooled : Set Cooled) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Memory)
    (memories' : Set Memory) (targets' : Set ErasedPinTarget)
    (htransition : HardEraseMemoryTransition
      memories cooled goals targets target memories' targets') :
    ErasedPinTargetIdUnique targets' ∧
    ErasedTargetsDisjointMemories targets' memories' ∧
    ErasedTargetsDisjointCooled targets' cooled ∧
    ErasedTargetsDisjointGoals targets' goals := by
  rw [htransition.targetsEq, htransition.memoriesEq]
  constructor
  · intro e1 e2 h1 h2 hid
    change e1 ∈ targets ∨
      (erased_pin_target_t e1 = memory_t target ∧
        erased_pin_target_kind e1 = (memory_kind target).pinTargetKind) at h1
    change e2 ∈ targets ∨
      (erased_pin_target_t e2 = memory_t target ∧
        erased_pin_target_kind e2 = (memory_kind target).pinTargetKind) at h2
    cases h1 with
    | inl h1old =>
        cases h2 with
        | inl h2old => exact htransition.witnessIdsUnique e1 e2 h1old h2old hid
        | inr h2new =>
            exact erased_pin_target_ext e1 e2 hid
              ((htransition.targetWitnessCompatible e1 h1old (hid.trans h2new.1)).trans
                h2new.2.symm)
    | inr h1new =>
        cases h2 with
        | inl h2old =>
            exact erased_pin_target_ext e1 e2 hid
              (h1new.2.trans
                (htransition.targetWitnessCompatible e2 h2old
                  (hid.symm.trans h1new.1)).symm)
        | inr h2new =>
            exact erased_pin_target_ext e1 e2 (h1new.1.trans h2new.1.symm)
              (h1new.2.trans h2new.2.symm)
  constructor
  · intro e he m hm
    change e ∈ targets ∨
      (erased_pin_target_t e = memory_t target ∧
        erased_pin_target_kind e = (memory_kind target).pinTargetKind) at he
    change m ∈ memories ∧ memory_t m ≠ memory_t target at hm
    cases he with
    | inl hold =>
        exact htransition.witnessMemoryDisjoint e hold m ⟨hm.1, hm.2⟩
    | inr hnew =>
        intro same
        exact hm.2 ((hnew.1.symm.trans same).symm)
  constructor
  · intro e he c hc
    change e ∈ targets ∨
      (erased_pin_target_t e = memory_t target ∧
        erased_pin_target_kind e = (memory_kind target).pinTargetKind) at he
    cases he with
    | inl hold => exact htransition.witnessCooledDisjoint e hold c hc
    | inr hnew =>
        exact fun same => htransition.targetNotCooled c hc (hnew.1.symm.trans same)
  · intro e he g hg
    change e ∈ targets ∨
      (erased_pin_target_t e = memory_t target ∧
        erased_pin_target_kind e = (memory_kind target).pinTargetKind) at he
    cases he with
    | inl hold => exact htransition.witnessGoalDisjoint e hold g hg
    | inr hnew =>
        exact fun same => htransition.targetNotGoal g hg (hnew.1.symm.trans same)

/-- A cooled-only erase has the same witness contract as hot deletion, but
    removes the cooled locator rather than a hot Memory row. -/
def hardEraseCooledRows (cooled : Set Cooled) (target : Cooled) : Set Cooled :=
  fun c => c ∈ cooled ∧ cooled_t c ≠ cooled_t target

def hardEraseCooledWitnesses
    (targets : Set ErasedPinTarget) (target : Cooled) : Set ErasedPinTarget :=
  fun e => e ∈ targets ∨
    (erased_pin_target_t e = cooled_t target ∧
      erased_pin_target_kind e = (cooled_kind target).pinTargetKind)

structure HardEraseCooledTransition
    (memories : Set Memory) (cooled : Set Cooled)
    (goals : Set Goal) (targets : Set ErasedPinTarget)
    (target : Cooled) (memories' : Set Memory) (cooled' : Set Cooled)
    (targets' : Set ErasedPinTarget) : Prop where
  targetInCooled : target ∈ cooled
  cooledIdsUnique : CooledIdUnique cooled
  witnessIdsUnique : ErasedPinTargetIdUnique targets
  witnessMemoryDisjoint : ErasedTargetsDisjointMemories targets memories
  witnessCooledDisjoint :
    ErasedTargetsDisjointCooled targets (hardEraseCooledRows cooled target)
  witnessGoalDisjoint : ErasedTargetsDisjointGoals targets goals
  targetWitnessCompatible :
    ErasedPinTargetKindCompatible targets (cooled_t target)
      (cooled_kind target).pinTargetKind
  targetNotMemory : ∀ m : Memory, m ∈ memories → cooled_t target ≠ memory_t m
  targetNotGoal : ∀ g : Goal, g ∈ goals → cooled_t target ≠ goal_t g
  memoriesEq : memories' = memories
  cooledEq : cooled' = hardEraseCooledRows cooled target
  targetsEq : targets' = hardEraseCooledWitnesses targets target

theorem hard_erase_cooled_records_kind
    (memories : Set Memory) (cooled : Set Cooled) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Cooled)
    (memories' : Set Memory) (cooled' : Set Cooled)
    (targets' : Set ErasedPinTarget)
    (htransition : HardEraseCooledTransition
      memories cooled goals targets target memories' cooled' targets') :
    recordedMemoryTargetKind
      targets' (cooled_t target) (cooled_kind target) := by
  rw [htransition.targetsEq]
  refine ⟨{ t := cooled_t target, kind := (cooled_kind target).pinTargetKind }, ?_, rfl, rfl⟩
  exact Or.inr ⟨rfl, rfl⟩

theorem hard_erase_cooled_rejects_wrong_kind
    (memories : Set Memory) (cooled : Set Cooled) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Cooled)
    (e : ErasedPinTarget) (he : e ∈ targets)
    (hid : erased_pin_target_t e = cooled_t target)
    (hkind : erased_pin_target_kind e ≠ (cooled_kind target).pinTargetKind) :
    ¬ HardEraseCooledTransition
      memories cooled goals targets target
        memories
        (hardEraseCooledRows cooled target)
        (hardEraseCooledWitnesses targets target) := by
  intro htransition
  exact hkind (htransition.targetWitnessCompatible e he hid)

theorem hard_erase_cooled_result_well_formed
    (memories : Set Memory) (cooled : Set Cooled) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Cooled)
    (memories' : Set Memory) (cooled' : Set Cooled)
    (targets' : Set ErasedPinTarget)
    (htransition : HardEraseCooledTransition
      memories cooled goals targets target memories' cooled' targets') :
    ErasedPinTargetIdUnique targets' ∧
    ErasedTargetsDisjointMemories targets' memories' ∧
    ErasedTargetsDisjointCooled targets' cooled' ∧
    ErasedTargetsDisjointGoals targets' goals := by
  rw [htransition.targetsEq, htransition.memoriesEq, htransition.cooledEq]
  constructor
  · intro e1 e2 h1 h2 hid
    change e1 ∈ targets ∨
      (erased_pin_target_t e1 = cooled_t target ∧
        erased_pin_target_kind e1 = (cooled_kind target).pinTargetKind) at h1
    change e2 ∈ targets ∨
      (erased_pin_target_t e2 = cooled_t target ∧
        erased_pin_target_kind e2 = (cooled_kind target).pinTargetKind) at h2
    cases h1 with
    | inl h1old =>
        cases h2 with
        | inl h2old => exact htransition.witnessIdsUnique e1 e2 h1old h2old hid
        | inr h2new =>
            exact erased_pin_target_ext e1 e2 hid
              ((htransition.targetWitnessCompatible e1 h1old (hid.trans h2new.1)).trans
                h2new.2.symm)
    | inr h1new =>
        cases h2 with
        | inl h2old =>
            exact erased_pin_target_ext e1 e2 hid
              (h1new.2.trans
                (htransition.targetWitnessCompatible e2 h2old
                  (hid.symm.trans h1new.1)).symm)
        | inr h2new =>
            exact erased_pin_target_ext e1 e2 (h1new.1.trans h2new.1.symm)
              (h1new.2.trans h2new.2.symm)
  constructor
  · intro e he m hm
    change e ∈ targets ∨
      (erased_pin_target_t e = cooled_t target ∧
        erased_pin_target_kind e = (cooled_kind target).pinTargetKind) at he
    cases he with
    | inl hold => exact htransition.witnessMemoryDisjoint e hold m hm
    | inr hnew =>
        exact fun same => htransition.targetNotMemory m hm (hnew.1.symm.trans same)
  constructor
  · intro e he c hc
    change e ∈ targets ∨
      (erased_pin_target_t e = cooled_t target ∧
        erased_pin_target_kind e = (cooled_kind target).pinTargetKind) at he
    change c ∈ cooled ∧ cooled_t c ≠ cooled_t target at hc
    cases he with
    | inl hold =>
        exact htransition.witnessCooledDisjoint e hold c ⟨hc.1, hc.2⟩
    | inr hnew => exact fun same => hc.2 (same.symm.trans hnew.1)
  · intro e he g hg
    change e ∈ targets ∨
      (erased_pin_target_t e = cooled_t target ∧
        erased_pin_target_kind e = (cooled_kind target).pinTargetKind) at he
    cases he with
    | inl hold => exact htransition.witnessGoalDisjoint e hold g hg
    | inr hnew =>
        exact fun same => htransition.targetNotGoal g hg (hnew.1.symm.trans same)

theorem hard_erase_cooled_preserves_memory_source
    (memories : Set Memory) (cooled : Set Cooled) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Cooled)
    (memories' : Set Memory) (cooled' : Set Cooled)
    (targets' : Set ErasedPinTarget)
    (htransition : HardEraseCooledTransition
      memories cooled goals targets target memories' cooled' targets')
    (source : Memory) (hs : source ∈ memories) :
    ∃ post : Memory,
      post ∈ memories' ∧ post = source ∧
      memory_origins post = memory_origins source ∧
      memory_refs post = memory_refs source := by
  rw [htransition.memoriesEq]
  exact ⟨source, hs, rfl, rfl, rfl⟩

/-- The reversible forget transform removes a hot row, adds its matching
    cooled locator, and leaves the hard-erase witness set untouched. -/
def cooledSnapshot (target : Memory) : Cooled := {
  t := memory_t target
  handle := memory_handle target
  owner := memory_owner target
  kind := memory_kind target
}

def forgetMemoryCooledRows
    (cooled : Set Cooled) (target : Memory) : Set Cooled :=
  fun c => c ∈ cooled ∨ c = cooledSnapshot target

structure ForgetMemoryTransition
    (memories : Set Memory) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (target : Memory)
    (memories' : Set Memory) (cooled' : Set Cooled)
    (targets' : Set ErasedPinTarget) : Prop where
  targetInMemories : target ∈ memories
  witnessIdsUnique : ErasedPinTargetIdUnique targets
  witnessMemoryDisjoint : ErasedTargetsDisjointMemories targets memories
  noExistingCooledTarget :
    ∀ c : Cooled, c ∈ cooled → cooled_t c ≠ memory_t target
  memoriesEq : memories' = hardEraseMemoryRows memories target
  cooledEq : cooled' = forgetMemoryCooledRows cooled target
  targetsEq : targets' = targets

theorem forget_memory_creates_matching_cooled
    (memories : Set Memory) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (target : Memory)
    (memories' : Set Memory) (cooled' : Set Cooled)
    (targets' : Set ErasedPinTarget)
    (htransition : ForgetMemoryTransition
      memories cooled targets target memories' cooled' targets') :
    cooledSnapshot target ∈ cooled' := by
  rw [htransition.cooledEq]
  exact Or.inr rfl

theorem forget_memory_leaves_witnesses
    (memories : Set Memory) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (target : Memory)
    (memories' : Set Memory) (cooled' : Set Cooled)
    (targets' : Set ErasedPinTarget)
    (htransition : ForgetMemoryTransition
      memories cooled targets target memories' cooled' targets') :
    targets' = targets :=
  htransition.targetsEq

theorem forget_memory_no_witness_for_target
    (memories : Set Memory) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (target : Memory)
    (memories' : Set Memory) (cooled' : Set Cooled)
    (targets' : Set ErasedPinTarget)
    (htransition : ForgetMemoryTransition
      memories cooled targets target memories' cooled' targets') :
    ¬ erasedPinTargetExists targets' (memory_t target) := by
  intro hexists
  obtain ⟨e, he, hid⟩ := hexists
  rw [htransition.targetsEq] at he
  exact (htransition.witnessMemoryDisjoint e he target htransition.targetInMemories) hid

theorem forget_memory_preserves_source
    (memories : Set Memory) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (target source : Memory)
    (memories' : Set Memory) (cooled' : Set Cooled)
    (targets' : Set ErasedPinTarget)
    (htransition : ForgetMemoryTransition
      memories cooled targets target memories' cooled' targets')
    (hs : source ∈ memories) (hsource : memory_t source ≠ memory_t target) :
    ∃ post : Memory,
      post ∈ memories' ∧ post = source ∧
      memory_origins post = memory_origins source ∧
      memory_refs post = memory_refs source := by
  rw [htransition.memoriesEq]
  exact ⟨source, ⟨hs, hsource⟩, rfl, rfl, rfl⟩

/-- A Goal erase records a Goal witness while leaving the Memory set intact. -/
def hardEraseGoalRows (goals : Set Goal) (target : Goal) : Set Goal :=
  fun g => g ∈ goals ∧ goal_t g ≠ goal_t target

def hardEraseGoalWitnesses
    (targets : Set ErasedPinTarget) (target : Goal) : Set ErasedPinTarget :=
  fun e => e ∈ targets ∨
    (erased_pin_target_t e = goal_t target ∧ erased_pin_target_kind e = .Goal)

structure HardEraseGoalTransition
    (memories : Set Memory) (goals : Set Goal) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (target : Goal)
    (memories' : Set Memory) (goals' : Set Goal)
    (targets' : Set ErasedPinTarget) : Prop where
  targetInGoals : target ∈ goals
  witnessIdsUnique : ErasedPinTargetIdUnique targets
  witnessMemoryDisjoint : ErasedTargetsDisjointMemories targets memories
  witnessCooledDisjoint : ErasedTargetsDisjointCooled targets cooled
  witnessGoalDisjoint :
    ErasedTargetsDisjointGoals targets (hardEraseGoalRows goals target)
  targetWitnessCompatible :
    ErasedPinTargetKindCompatible targets (goal_t target) .Goal
  targetNotMemory : ∀ m : Memory, m ∈ memories → goal_t target ≠ memory_t m
  targetNotCooled : ∀ c : Cooled, c ∈ cooled → goal_t target ≠ cooled_t c
  memoriesEq : memories' = memories
  goalsEq : goals' = hardEraseGoalRows goals target
  targetsEq : targets' = hardEraseGoalWitnesses targets target

theorem hard_erase_goal_records_kind
    (memories : Set Memory) (goals : Set Goal) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (target : Goal)
    (memories' : Set Memory) (goals' : Set Goal)
    (targets' : Set ErasedPinTarget)
    (htransition : HardEraseGoalTransition
      memories goals cooled targets target memories' goals' targets') :
    recordedGoalTargetExists targets' (goal_t target) := by
  rw [htransition.targetsEq]
  refine ⟨{ t := goal_t target, kind := .Goal }, ?_, rfl, rfl⟩
  exact Or.inr ⟨rfl, rfl⟩

theorem hard_erase_goal_rejects_wrong_kind
    (memories : Set Memory) (goals : Set Goal) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (target : Goal)
    (e : ErasedPinTarget) (he : e ∈ targets)
    (hid : erased_pin_target_t e = goal_t target)
    (hkind : erased_pin_target_kind e ≠ .Goal) :
    ¬ HardEraseGoalTransition
      memories goals cooled targets target memories
        (hardEraseGoalRows goals target)
        (hardEraseGoalWitnesses targets target) := by
  intro htransition
  exact hkind (htransition.targetWitnessCompatible e he hid)

theorem hard_erase_goal_result_well_formed
    (memories : Set Memory) (goals : Set Goal) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (target : Goal)
    (memories' : Set Memory) (goals' : Set Goal)
    (targets' : Set ErasedPinTarget)
    (htransition : HardEraseGoalTransition
      memories goals cooled targets target memories' goals' targets') :
    ErasedPinTargetIdUnique targets' ∧
    ErasedTargetsDisjointMemories targets' memories' ∧
    ErasedTargetsDisjointCooled targets' cooled ∧
    ErasedTargetsDisjointGoals targets' goals' := by
  rw [htransition.targetsEq, htransition.memoriesEq, htransition.goalsEq]
  constructor
  · intro e1 e2 h1 h2 hid
    change e1 ∈ targets ∨
      (erased_pin_target_t e1 = goal_t target ∧ erased_pin_target_kind e1 = .Goal) at h1
    change e2 ∈ targets ∨
      (erased_pin_target_t e2 = goal_t target ∧ erased_pin_target_kind e2 = .Goal) at h2
    cases h1 with
    | inl h1old =>
        cases h2 with
        | inl h2old => exact htransition.witnessIdsUnique e1 e2 h1old h2old hid
        | inr h2new =>
            exact erased_pin_target_ext e1 e2 hid
              ((htransition.targetWitnessCompatible e1 h1old (hid.trans h2new.1)).trans
                h2new.2.symm)
    | inr h1new =>
        cases h2 with
        | inl h2old =>
            exact erased_pin_target_ext e1 e2 hid
              (h1new.2.trans
                (htransition.targetWitnessCompatible e2 h2old
                  (hid.symm.trans h1new.1)).symm)
        | inr h2new =>
            exact erased_pin_target_ext e1 e2 (h1new.1.trans h2new.1.symm)
              (h1new.2.trans h2new.2.symm)
  constructor
  · intro e he m hm
    change e ∈ targets ∨
      (erased_pin_target_t e = goal_t target ∧ erased_pin_target_kind e = .Goal) at he
    cases he with
    | inl hold => exact htransition.witnessMemoryDisjoint e hold m hm
    | inr hnew =>
        exact fun same => htransition.targetNotMemory m hm (hnew.1.symm.trans same)
  constructor
  · intro e he c hc
    change e ∈ targets ∨
      (erased_pin_target_t e = goal_t target ∧ erased_pin_target_kind e = .Goal) at he
    cases he with
    | inl hold => exact htransition.witnessCooledDisjoint e hold c hc
    | inr hnew =>
        exact fun same => htransition.targetNotCooled c hc (hnew.1.symm.trans same)
  · intro e he g hg
    change e ∈ targets ∨
      (erased_pin_target_t e = goal_t target ∧ erased_pin_target_kind e = .Goal) at he
    change g ∈ goals ∧ goal_t g ≠ goal_t target at hg
    cases he with
    | inl hold =>
        exact htransition.witnessGoalDisjoint e hold g ⟨hg.1, hg.2⟩
    | inr hnew => exact fun same => hg.2 (same.symm.trans hnew.1)

theorem hard_erase_goal_preserves_memory_source
    (memories : Set Memory) (goals : Set Goal) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) (target : Goal)
    (memories' : Set Memory) (goals' : Set Goal)
    (targets' : Set ErasedPinTarget)
    (htransition : HardEraseGoalTransition
      memories goals cooled targets target memories' goals' targets')
    (source : Memory) (hs : source ∈ memories) :
    ∃ post : Memory,
      post ∈ memories' ∧ post = source ∧
      memory_origins post = memory_origins source ∧
      memory_refs post = memory_refs source := by
  rw [htransition.memoriesEq]
  exact ⟨source, hs, rfl, rfl, rfl⟩

end Causa
