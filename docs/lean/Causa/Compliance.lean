/-
Causa — Compliance

Hard delete is abandonment, the forgotten path, or a flavor-scoped erase:

  wipeable := abandoned ∨ (forgotten ∧ unreferenced ∧ policy) ∨ flavorScoped

Forget sets a flag on the row and keeps it (#488); erase wipes abandoned owners; a flavor-scoped erase wipes whole
series of one flavor's own schemas under one live owner, on Admin or the
host's system authority (renegotiated in writing, docs/13 §Flavor-scoped
erase). An owner-to-owner transfer carries
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

/-- No stored row, live or forgotten, still pins this `t`. -/
def unreferenced (memories : Set Memory) (id : MemoryId) : Prop :=
  ∀ m : Memory, m ∈ memories →
    id ∉ memory_origins m ∧ id ∉ memory_refs m

/-- No remaining admission names this Content. A forgotten row is still an
    admission and keeps its payload; Content GC is admission-set emptiness. -/
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

/-- The row `id` is stored and flagged forgotten. -/
def forgotten (memories : Set Memory) (id : MemoryId) : Prop :=
  ∃ m : Memory, m ∈ memories ∧ memory_t m = id ∧ memory_forgotten m = true

/-- Forgetting a stored row makes it forgotten. -/
theorem forgotten_after_forget
    (memories : Set Memory) (m : Memory) (hm : m ∈ memories) :
    forgotten (forgetIn memories (memory_t m)) (memory_t m) :=
  ⟨m.forget, (forgetIn_marks memories (memory_t m) m hm rfl).1, rfl, rfl⟩

-- ============================================================
-- Flavor-scoped erase (docs/13 §Flavor-scoped erase)
-- ============================================================

/-- Who issues a flavor-scoped erase: a user, or the host's system authority
    (a retention schedule runs with nobody in the loop). -/
inductive EraseAuthority where
  | user (u : User)
  | system

/-- Admin on the owner — the Goal write ceiling, the gate a transfer takes —
    or the host's system authority. -/
def EraseAuthority.admits : EraseAuthority → Owner → Prop
  | .user u, o => may_write u o .goal
  | .system, _ => True

/-- One flavor-scoped erase: whole series (`handles`), one live owner, and
    only the named flavor's own schemas. The flavor's vocabulary is a
    PARAMETER (`ownSchemas`), as in `Causa.Flavor`; a series carries one
    schema (`seriesSchema`: a new shape is a new handle, Identity). Nothing
    here requires the owner to be abandoned. -/
structure FlavorScopedErase (memories : Set Memory) where
  owner        : Owner
  ownSchemas   : SchemaRef → Prop
  seriesSchema : Handle → SchemaRef
  handles      : Set Handle
  authority    : EraseAuthority
  authorized   : authority.admits owner
  ownSchema    : ∀ h : Handle, h ∈ handles → ownSchemas (seriesSchema h)
  /-- Every version of a selected series, live or forgotten, is the owner's. -/
  inOwner      : ∀ m : Memory, m ∈ memories → memory_handle m ∈ handles →
    memory_owner m = owner

/-- A `t` the erase selects: any version, live or forgotten, of a selected
    series. -/
def FlavorScopedErase.selects {memories : Set Memory}
    (e : FlavorScopedErase memories) (id : MemoryId) : Prop :=
  ∃ m : Memory, m ∈ memories ∧ memory_t m = id ∧ memory_handle m ∈ e.handles

/-- CO-7 / ST-13: abandonment OR (forgotten ∧ unreferenced ∧ policy) OR
    selected by a flavor-scoped erase of that owner. -/
def wipeable (o : Owner) (memories : Set Memory)
    (id : MemoryId) (policy : Prop) : Prop :=
  abandoned o ∨ (forgotten memories id ∧ unreferenced memories id ∧ policy) ∨
    ∃ e : FlavorScopedErase memories, e.owner = o ∧ e.selects id

theorem wipeable_when_abandoned
    (o : Owner) (memories : Set Memory)
    (id : MemoryId) (policy : Prop) (h : abandoned o) :
    wipeable o memories id policy :=
  Or.inl h

theorem wipeable_when_forgotten_unreferenced_policy
    (o : Owner) (memories : Set Memory)
    (id : MemoryId) (policy : Prop)
    (hc : forgotten memories id) (hu : unreferenced memories id) (hp : policy) :
    wipeable o memories id policy :=
  Or.inr (Or.inl ⟨hc, hu, hp⟩)

theorem wipeable_when_flavor_scoped
    (o : Owner) (memories : Set Memory)
    (id : MemoryId) (policy : Prop)
    (e : FlavorScopedErase memories) (ho : e.owner = o) (hs : e.selects id) :
    wipeable o memories id policy :=
  Or.inr (Or.inr ⟨e, ho, hs⟩)

/-- The erase never prunes a series' history: every version of a selected
    series, live or forgotten, is selected. -/
theorem flavor_erase_takes_whole_series
    {memories : Set Memory}
    (e : FlavorScopedErase memories) (m : Memory)
    (hm : m ∈ memories) (hh : memory_handle m ∈ e.handles) :
    e.selects (memory_t m) :=
  ⟨m, hm, rfl, hh⟩

/-- … forgotten versions included: a flavor-scoped erase keeps no forgotten
    copy. -/
theorem flavor_erase_takes_forgotten_versions
    {memories : Set Memory}
    (e : FlavorScopedErase memories) (m : Memory)
    (hm : m ∈ memories) (_hf : memory_forgotten m = true)
    (hh : memory_handle m ∈ e.handles) :
    e.selects (memory_t m) :=
  ⟨m, hm, rfl, hh⟩

/-- One owner: no version of a selected series, live or forgotten, belongs
    to anyone else. -/
theorem flavor_erase_stays_in_owner
    {memories : Set Memory}
    (e : FlavorScopedErase memories) :
    ∀ m : Memory, m ∈ memories → memory_handle m ∈ e.handles →
      memory_owner m = e.owner :=
  e.inOwner

/-- Only the flavor's own vocabulary. -/
theorem flavor_erase_stays_in_flavor
    {memories : Set Memory}
    (e : FlavorScopedErase memories) (h : Handle) (hh : h ∈ e.handles) :
    e.ownSchemas (e.seriesSchema h) :=
  e.ownSchema h hh

/-- Below Admin, no user authorizes a flavor-scoped erase: a write ceiling
    that stops short of Goals stops short of erasing. -/
theorem below_admin_cannot_authorize_flavor_erase
    (u : User) (o : Owner) (x : Role) (hx : o u = some x) (hw : x.write ≤ 3) :
    ¬ (EraseAuthority.user u).admits o := by
  rintro ⟨y, hy, hwy⟩
  rw [hx] at hy
  injection hy with hxy
  subst hxy
  simp only [Role.mayWrite, AccessKind.rank] at hwy
  omega

/-- The Ingest preset in particular: a principal that may add Facts may not
    erase them. -/
theorem ingest_cannot_authorize_flavor_erase
    (u : User) (o : Owner) (hx : o u = some Role.ingest) :
    ¬ (EraseAuthority.user u).admits o :=
  below_admin_cannot_authorize_flavor_erase u o Role.ingest hx (by decide)

/-- Target render: live / forgotten / unavailable. A pin to a forgotten `t`
    renders forgotten, not a missing origin. -/
def pin_render_live (memories : Set Memory) (id : MemoryId) : Prop :=
  ∃ m : Memory, m ∈ memories ∧ memory_t m = id ∧ memory_forgotten m = false

def pin_render_forgotten (memories : Set Memory) (id : MemoryId) : Prop :=
  forgotten memories id

def pin_render_unavailable (memories : Set Memory) (id : MemoryId) : Prop :=
  ¬ pinExists memories id

/-- With unique `t`, a stored pin target renders exactly one of live or
    forgotten, and never unavailable. -/
theorem pin_render_stored
    (memories : Set Memory) (id : MemoryId) (h : pinExists memories id) :
    (pin_render_live memories id ∨ pin_render_forgotten memories id) ∧
      ¬ pin_render_unavailable memories id := by
  obtain ⟨m, hm, ht⟩ := h
  refine ⟨?_, fun hu => hu ⟨m, hm, ht⟩⟩
  cases hf : memory_forgotten m with
  | false => exact Or.inl ⟨m, hm, ht, hf⟩
  | true => exact Or.inr ⟨m, hm, ht, hf⟩

theorem pin_render_live_not_forgotten
    (memories : Set Memory) (hunique : MemoryIdUnique memories) (id : MemoryId)
    (hl : pin_render_live memories id) : ¬ pin_render_forgotten memories id := by
  obtain ⟨m, hm, ht, hf⟩ := hl
  rintro ⟨m', hm', ht', hf'⟩
  have heq := hunique m m' hm hm' (ht.trans ht'.symm)
  subst heq
  simp [hf] at hf'

/-- Forget of a pinned `t` deletes neither the pinning row nor its target:
    forget is a flag on the target row (#488). -/
theorem pin_survives_target_forget
    (memories : Set Memory) (m : Memory) (id : MemoryId)
    (hm : m ∈ memories)
    (_hpin : id ∈ memory_origins m ∨ id ∈ memory_refs m)
    (htarget : pinExists memories id) :
    pinExists (forgetIn memories id) id ∧
      ∃ m' : Memory, m' ∈ forgetIn memories id ∧ memory_t m' = memory_t m ∧
        memory_origins m' = memory_origins m ∧ memory_refs m' = memory_refs m := by
  refine ⟨pinExists_forgetIn memories id id htarget, ?_⟩
  by_cases h : memory_t m = id
  · exact ⟨m.forget, ⟨m, hm, Or.inl ⟨h, rfl⟩⟩, rfl, rfl, rfl⟩
  · exact ⟨m, ⟨m, hm, Or.inr ⟨h, rfl⟩⟩, rfl, rfl, rfl⟩

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
    (memories : Set Memory)
    (goals : Set Goal) (targets : Set ErasedPinTarget)
    (target : Memory) (memories' : Set Memory)
    (targets' : Set ErasedPinTarget) : Prop where
  targetInMemories : target ∈ memories
  memoryIdsUnique : MemoryIdUnique memories
  witnessIdsUnique : ErasedPinTargetIdUnique targets
  witnessMemoryDisjoint :
    ErasedTargetsDisjointMemories targets (hardEraseMemoryRows memories target)
  witnessGoalDisjoint : ErasedTargetsDisjointGoals targets goals
  targetWitnessCompatible :
    ErasedPinTargetKindCompatible targets (memory_t target)
      (memory_kind target).pinTargetKind
  targetNotGoal : ∀ g : Goal, g ∈ goals → memory_t target ≠ goal_t g
  memoriesEq : memories' = hardEraseMemoryRows memories target
  targetsEq : targets' = hardEraseMemoryWitnesses targets target

theorem hard_erase_memory_records_kind
    (memories : Set Memory) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Memory)
    (memories' : Set Memory) (targets' : Set ErasedPinTarget)
    (htransition : HardEraseMemoryTransition
      memories goals targets target memories' targets') :
    recordedMemoryTargetKind
      targets' (memory_t target) (memory_kind target) := by
  rw [htransition.targetsEq]
  refine ⟨{ t := memory_t target, kind := (memory_kind target).pinTargetKind }, ?_, rfl, rfl⟩
  exact Or.inr ⟨rfl, rfl⟩

theorem hard_erase_memory_rejects_wrong_kind
    (memories : Set Memory) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Memory)
    (e : ErasedPinTarget) (he : e ∈ targets)
    (hid : erased_pin_target_t e = memory_t target)
    (hkind : erased_pin_target_kind e ≠ (memory_kind target).pinTargetKind) :
    ¬ HardEraseMemoryTransition
      memories goals targets target
        (hardEraseMemoryRows memories target)
        (hardEraseMemoryWitnesses targets target) := by
  intro htransition
  exact hkind (htransition.targetWitnessCompatible e he hid)

theorem hard_erase_memory_preserves_source
    (memories : Set Memory)
    (goals : Set Goal) (targets : Set ErasedPinTarget)
    (target source : Memory) (memories' : Set Memory)
    (targets' : Set ErasedPinTarget)
    (htransition : HardEraseMemoryTransition
      memories goals targets target memories' targets')
    (hs : source ∈ memories) (hsource : memory_t source ≠ memory_t target) :
    ∃ post : Memory,
      post ∈ memories' ∧ post = source ∧
      memory_origins post = memory_origins source ∧
      memory_refs post = memory_refs source := by
  rw [htransition.memoriesEq]
  exact ⟨source, ⟨hs, hsource⟩, rfl, rfl, rfl⟩

theorem hard_erase_memory_result_well_formed
    (memories : Set Memory) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Memory)
    (memories' : Set Memory) (targets' : Set ErasedPinTarget)
    (htransition : HardEraseMemoryTransition
      memories goals targets target memories' targets') :
    ErasedPinTargetIdUnique targets' ∧
    ErasedTargetsDisjointMemories targets' memories' ∧
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
  · intro e he g hg
    change e ∈ targets ∨
      (erased_pin_target_t e = memory_t target ∧
        erased_pin_target_kind e = (memory_kind target).pinTargetKind) at he
    cases he with
    | inl hold => exact htransition.witnessGoalDisjoint e hold g hg
    | inr hnew =>
        exact fun same => htransition.targetNotGoal g hg (hnew.1.symm.trans same)

/-- Forget sets the flag on the target row (#488): every row stays, the
    payload and pins with it, and the hard-erase witness set is untouched. -/
structure ForgetMemoryTransition
    (memories : Set Memory)
    (targets : Set ErasedPinTarget) (target : Memory)
    (memories' : Set Memory)
    (targets' : Set ErasedPinTarget) : Prop where
  targetInMemories : target ∈ memories
  witnessIdsUnique : ErasedPinTargetIdUnique targets
  witnessMemoryDisjoint : ErasedTargetsDisjointMemories targets memories
  memoriesEq : memories' = forgetIn memories (memory_t target)
  targetsEq : targets' = targets

theorem forget_memory_marks_target
    (memories : Set Memory)
    (targets : Set ErasedPinTarget) (target : Memory)
    (memories' : Set Memory)
    (targets' : Set ErasedPinTarget)
    (htransition : ForgetMemoryTransition
      memories targets target memories' targets') :
    target.forget ∈ memories' ∧ forgotten memories' (memory_t target) := by
  rw [htransition.memoriesEq]
  exact ⟨(forgetIn_marks memories (memory_t target) target
      htransition.targetInMemories rfl).1,
    forgotten_after_forget memories target htransition.targetInMemories⟩

theorem forget_memory_leaves_witnesses
    (memories : Set Memory)
    (targets : Set ErasedPinTarget) (target : Memory)
    (memories' : Set Memory)
    (targets' : Set ErasedPinTarget)
    (htransition : ForgetMemoryTransition
      memories targets target memories' targets') :
    targets' = targets :=
  htransition.targetsEq

theorem forget_memory_no_witness_for_target
    (memories : Set Memory)
    (targets : Set ErasedPinTarget) (target : Memory)
    (memories' : Set Memory)
    (targets' : Set ErasedPinTarget)
    (htransition : ForgetMemoryTransition
      memories targets target memories' targets') :
    ¬ erasedPinTargetExists targets' (memory_t target) := by
  intro hexists
  obtain ⟨e, he, hid⟩ := hexists
  rw [htransition.targetsEq] at he
  exact (htransition.witnessMemoryDisjoint e he target htransition.targetInMemories) hid

theorem forget_memory_preserves_source
    (memories : Set Memory)
    (targets : Set ErasedPinTarget) (target source : Memory)
    (memories' : Set Memory)
    (targets' : Set ErasedPinTarget)
    (htransition : ForgetMemoryTransition
      memories targets target memories' targets')
    (hs : source ∈ memories) (hsource : memory_t source ≠ memory_t target) :
    ∃ post : Memory,
      post ∈ memories' ∧ post = source ∧
      memory_origins post = memory_origins source ∧
      memory_refs post = memory_refs source := by
  rw [htransition.memoriesEq]
  exact ⟨source, ⟨source, hs, Or.inr ⟨hsource, rfl⟩⟩, rfl, rfl, rfl⟩

/-- Forget deletes no row: the target and every other row survive with
    their pins, so a forgotten row is erased later by the ordinary Memory
    hard erase, not by a separate stub erase. -/
theorem forget_memory_keeps_every_row
    (memories : Set Memory)
    (targets : Set ErasedPinTarget) (target : Memory)
    (memories' : Set Memory)
    (targets' : Set ErasedPinTarget)
    (htransition : ForgetMemoryTransition
      memories targets target memories' targets')
    (m : Memory) (hm : m ∈ memories) :
    ∃ m' : Memory, m' ∈ memories' ∧ memory_t m' = memory_t m ∧
      memory_kind m' = memory_kind m ∧ memory_owner m' = memory_owner m := by
  rw [htransition.memoriesEq]
  exact forgetIn_keeps_row memories (memory_t target) m hm

/-- A Goal erase records a Goal witness while leaving the Memory set intact. -/
def hardEraseGoalRows (goals : Set Goal) (target : Goal) : Set Goal :=
  fun g => g ∈ goals ∧ goal_t g ≠ goal_t target

def hardEraseGoalWitnesses
    (targets : Set ErasedPinTarget) (target : Goal) : Set ErasedPinTarget :=
  fun e => e ∈ targets ∨
    (erased_pin_target_t e = goal_t target ∧ erased_pin_target_kind e = .Goal)

structure HardEraseGoalTransition
    (memories : Set Memory) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Goal)
    (memories' : Set Memory) (goals' : Set Goal)
    (targets' : Set ErasedPinTarget) : Prop where
  targetInGoals : target ∈ goals
  witnessIdsUnique : ErasedPinTargetIdUnique targets
  witnessMemoryDisjoint : ErasedTargetsDisjointMemories targets memories
  witnessGoalDisjoint :
    ErasedTargetsDisjointGoals targets (hardEraseGoalRows goals target)
  targetWitnessCompatible :
    ErasedPinTargetKindCompatible targets (goal_t target) .Goal
  targetNotMemory : ∀ m : Memory, m ∈ memories → goal_t target ≠ memory_t m
  memoriesEq : memories' = memories
  goalsEq : goals' = hardEraseGoalRows goals target
  targetsEq : targets' = hardEraseGoalWitnesses targets target

theorem hard_erase_goal_records_kind
    (memories : Set Memory) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Goal)
    (memories' : Set Memory) (goals' : Set Goal)
    (targets' : Set ErasedPinTarget)
    (htransition : HardEraseGoalTransition
      memories goals targets target memories' goals' targets') :
    recordedGoalTargetExists targets' (goal_t target) := by
  rw [htransition.targetsEq]
  refine ⟨{ t := goal_t target, kind := .Goal }, ?_, rfl, rfl⟩
  exact Or.inr ⟨rfl, rfl⟩

theorem hard_erase_goal_rejects_wrong_kind
    (memories : Set Memory) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Goal)
    (e : ErasedPinTarget) (he : e ∈ targets)
    (hid : erased_pin_target_t e = goal_t target)
    (hkind : erased_pin_target_kind e ≠ .Goal) :
    ¬ HardEraseGoalTransition
      memories goals targets target memories
        (hardEraseGoalRows goals target)
        (hardEraseGoalWitnesses targets target) := by
  intro htransition
  exact hkind (htransition.targetWitnessCompatible e he hid)

theorem hard_erase_goal_result_well_formed
    (memories : Set Memory) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Goal)
    (memories' : Set Memory) (goals' : Set Goal)
    (targets' : Set ErasedPinTarget)
    (htransition : HardEraseGoalTransition
      memories goals targets target memories' goals' targets') :
    ErasedPinTargetIdUnique targets' ∧
    ErasedTargetsDisjointMemories targets' memories' ∧
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
  · intro e he g hg
    change e ∈ targets ∨
      (erased_pin_target_t e = goal_t target ∧ erased_pin_target_kind e = .Goal) at he
    change g ∈ goals ∧ goal_t g ≠ goal_t target at hg
    cases he with
    | inl hold =>
        exact htransition.witnessGoalDisjoint e hold g ⟨hg.1, hg.2⟩
    | inr hnew => exact fun same => hg.2 (same.symm.trans hnew.1)

theorem hard_erase_goal_preserves_memory_source
    (memories : Set Memory) (goals : Set Goal)
    (targets : Set ErasedPinTarget) (target : Goal)
    (memories' : Set Memory) (goals' : Set Goal)
    (targets' : Set ErasedPinTarget)
    (htransition : HardEraseGoalTransition
      memories goals targets target memories' goals' targets')
    (source : Memory) (hs : source ∈ memories) :
    ∃ post : Memory,
      post ∈ memories' ∧ post = source ∧
      memory_origins post = memory_origins source ∧
      memory_refs post = memory_refs source := by
  rw [htransition.memoriesEq]
  exact ⟨source, hs, rfl, rfl, rfl⟩

end Causa
