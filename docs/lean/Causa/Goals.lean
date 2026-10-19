/-
Causa — Goals

Goal is its own timeseries (UML §3). Not Memory.

  series  = handle
  version = t
  later t on the same handle is the new head
  terminal state admits no later t

No `supersedes`. No authorship blob. Title stays. `text` is on the row: the
version's prompt, `none` when unknown, never blank (`goal_text_nonblank_chk`,
migration 0025); the rest of the body is the sidecar. A version that changes
the state keeps its predecessor's text (the Goal write verbs carry it).
`close_fact_t` pins an existing Fact `t`.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity
import Causa.Memory

namespace Causa

-- ============================================================
-- States and lifecycle
-- ============================================================

inductive GoalState where
  | Active
  | Paused
  | Achieved
  | Abandoned
  deriving DecidableEq, Repr

def GoalState.terminal : GoalState → Bool
  | .Achieved | .Abandoned => true
  | _ => false

/-- Admitted transitions between consecutive versions of one handle. -/
def goalTransitionAdmitted : GoalState → GoalState → Prop
  | .Active,   .Active    => True
  | .Active,   .Paused    => True
  | .Active,   .Achieved  => True
  | .Active,   .Abandoned => True
  | .Paused,   .Active    => True
  | _, _ => False

-- ============================================================
-- The Goal entity
-- ============================================================

/-- The characters `goal_text_nonblank_chk` trims: ASCII whitespace (space,
tab, line feed, carriage return, vertical tab, form feed). -/
def goalTextBlankChar (c : Char) : Bool :=
  c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\x0b' || c == '\x0c'

/-- `goal_text_nonblank_chk`: a stored text keeps a character the check does
not trim. -/
def GoalTextNonblank (s : Text) : Prop :=
  s.any (fun c => !goalTextBlankChar c) = true

structure Goal where
  handle        : Handle
  t             : GoalId
  owner         : Owner
  writer        : WriterAttribution
  title         : Text
  text          : Option Text
  state         : GoalState
  request_id    : Text
  close_fact_t  : Option MemoryId
  assignment_t  : Option MemoryId
  dependency_t  : List GoalId
  evidence_t    : List MemoryId
  write_act_t   : Option MemoryId
  /-- The parent's series (#514), set at decompose and fixed for the series. -/
  parent_handle : Option Handle
  tick          : Instant
  terminal_close_fact :
    state.terminal = true → close_fact_t.isSome = true
  text_nonblank :
    ∀ s, text = some s → GoalTextNonblank s

structure GoalHead where
  handle : Handle
  schema : SchemaRef
  owner  : Owner
  t      : GoalId

def goal_handle : Goal → Handle := Goal.handle
def goal_t : Goal → GoalId := Goal.t
def goal_id : Goal → GoalId := Goal.t
def goal_owner : Goal → Owner := Goal.owner
def goal_writer : Goal → WriterAttribution := Goal.writer
def goal_title : Goal → Text := Goal.title
def goal_text : Goal → Option Text := Goal.text
def goal_state : Goal → GoalState := Goal.state
def goal_request_id : Goal → Text := Goal.request_id
def goal_close_fact_t : Goal → Option MemoryId := Goal.close_fact_t
def goal_assignment : Goal → Option MemoryId := Goal.assignment_t
def goal_dependencies : Goal → List GoalId := Goal.dependency_t
def goal_evidence : Goal → List MemoryId := Goal.evidence_t
def goal_write_act_t : Goal → Option MemoryId := Goal.write_act_t
def goal_tick : Goal → Instant := Goal.tick
def goal_parent : Goal → Option Handle := Goal.parent_handle

def ErasedTargetsDisjointGoals
    (targets : Set ErasedPinTarget) (goals : Set Goal) : Prop :=
  ∀ e : ErasedPinTarget, e ∈ targets →
    ∀ g : Goal, g ∈ goals → erased_pin_target_t e ≠ goal_t g

theorem erased_target_not_goal
    (targets : Set ErasedPinTarget) (goals : Set Goal)
    (hdisjoint : ErasedTargetsDisjointGoals targets goals)
    (e : ErasedPinTarget) (he : e ∈ targets)
    (g : Goal) (hg : g ∈ goals) :
    erased_pin_target_t e ≠ goal_t g :=
  hdisjoint e he g hg

/-- Declared pins to other nodes (ids only). No edge table. -/
def goalDeclaredTargetIds (g : Goal) : List Id :=
  (goal_assignment g).toList ++ goal_dependencies g ++ goal_evidence g ++
    (goal_close_fact_t g).toList ++ (goal_write_act_t g).toList

def GoalIdUnique (goals : Set Goal) : Prop :=
  ∀ g1 g2 : Goal,
    g1 ∈ goals →
    g2 ∈ goals →
    goal_t g1 = goal_t g2 →
    g1 = g2

theorem goal_id_injective :
    ∀ (goals : Set Goal),
      GoalIdUnique goals →
      ∀ g1 g2 : Goal,
        g1 ∈ goals →
        g2 ∈ goals →
        goal_t g1 = goal_t g2 → g1 = g2 := by
  intro goals huniq g1 g2 hg1 hg2 hid
  exact huniq g1 g2 hg1 hg2 hid

/-- Each recorded Goal version keeps its own writer, independently of
    optional write-act Facts or later versions on the same handle. -/
theorem goal_version_writer_unique
    (goals : Set Goal) (hunique : GoalIdUnique goals)
    (g1 g2 : Goal) (hg1 : g1 ∈ goals) (hg2 : g2 ∈ goals)
    (ht : goal_t g1 = goal_t g2) :
    goal_writer g1 = goal_writer g2 :=
  congrArg goal_writer (hunique g1 g2 hg1 hg2 ht)

/-- Same `(owner, request_id)` is the same Goal version (UML §3). -/
def GoalRequestUnique (goals : Set Goal) : Prop :=
  ∀ g1 g2 : Goal,
    g1 ∈ goals →
    g2 ∈ goals →
    goal_owner g1 = goal_owner g2 →
    goal_request_id g1 = goal_request_id g2 →
    g1 = g2

instance : AppendOnly Goal := ⟨⟩

-- ============================================================
-- Version order on a handle (replaces supersedes)
-- ============================================================

def goalSameSeries (g g' : Goal) : Prop :=
  goal_handle g = goal_handle g'

def goalLater (new old : Goal) : Prop :=
  goalSameSeries new old ∧ goal_tick old < goal_tick new

/-- Adjacent later version: no mid tick on the same handle. -/
def goalImmediatelySucceeds (goals : Set Goal) (new old : Goal) : Prop :=
  new ∈ goals ∧ old ∈ goals ∧
  goalLater new old ∧
  ¬ ∃ mid : Goal, mid ∈ goals ∧ goalSameSeries mid old ∧
      goal_tick old < goal_tick mid ∧ goal_tick mid < goal_tick new

def GoalTransitionValid (goals : Set Goal) : Prop :=
  ∀ new old : Goal,
    goalImmediatelySucceeds goals new old →
    goal_owner new = goal_owner old ∧
    goalTransitionAdmitted (goal_state old) (goal_state new) ∧
    (goal_state new ≠ goal_state old → goal_text new = goal_text old)

theorem goal_transition_same_owner :
    ∀ (goals : Set Goal),
      GoalTransitionValid goals →
      ∀ new old : Goal,
        goalImmediatelySucceeds goals new old →
        goal_owner new = goal_owner old := by
  intro goals hvalid new old h
  exact (hvalid new old h).1

theorem goal_transition_admitted :
    ∀ (goals : Set Goal),
      GoalTransitionValid goals →
      ∀ new old : Goal,
        goalImmediatelySucceeds goals new old →
        goalTransitionAdmitted (goal_state old) (goal_state new) := by
  intro goals hvalid new old h
  exact (hvalid new old h).2.1

/-- Only a modify (Active to Active) gives a series a new text; a pause,
resume, abandon or achievement keeps the predecessor's, `none` included. -/
theorem goal_transition_keeps_text :
    ∀ (goals : Set Goal),
      GoalTransitionValid goals →
      ∀ new old : Goal,
        goalImmediatelySucceeds goals new old →
        goal_state new ≠ goal_state old →
        goal_text new = goal_text old := by
  intro goals hvalid new old h
  exact (hvalid new old h).2.2

/-- Root = least tick on the handle. Creation is Active only. -/
def GoalRootValid (goals : Set Goal) : Prop :=
  ∀ g : Goal, g ∈ goals →
    (∀ g' : Goal, g' ∈ goals → goalSameSeries g' g → goal_tick g ≤ goal_tick g') →
    goal_state g = .Active

theorem goal_root_active :
    ∀ (goals : Set Goal),
      GoalRootValid goals →
      ∀ g : Goal,
        g ∈ goals →
        (∀ g' : Goal, g' ∈ goals → goalSameSeries g' g → goal_tick g ≤ goal_tick g') →
        goal_state g = .Active := by
  intro goals hvalid g hg hroot
  exact hvalid g hg hroot

/-- Terminal admits no later `t` (UML §3). -/
def GoalTerminalClosed (goals : Set Goal) : Prop :=
  ∀ g g' : Goal,
    g ∈ goals → g' ∈ goals →
    goalSameSeries g' g →
    (goal_state g).terminal = true →
    goal_tick g' ≤ goal_tick g

-- ============================================================
-- Close Fact
-- ============================================================

theorem terminal_goal_closes_with_fact :
    ∀ g : Goal, (goal_state g).terminal = true →
      (goal_close_fact_t g).isSome = true := by
  intro g h
  exact g.terminal_close_fact h

def GoalTerminalCloseFactValid
    (goals : Set Goal) (memories : Set Memory) : Prop :=
  ∀ g : Goal, g ∈ goals → (goal_state g).terminal = true →
    (∃ m : Memory,
      m ∈ memories ∧
      goal_close_fact_t g = some (memory_t m) ∧
      memory_kind m = .Fact ∧
      memory_owner m = goal_owner g)

theorem terminal_goal_close_fact_member :
    ∀ (goals : Set Goal) (memories : Set Memory),
      GoalTerminalCloseFactValid goals memories →
      ∀ g : Goal,
        g ∈ goals →
        (goal_state g).terminal = true →
        (∃ m : Memory, m ∈ memories ∧ goal_close_fact_t g = some (memory_t m)) := by
  intro goals memories hvalid g hg hterminal
  obtain ⟨m, hm, hclose, _, _⟩ := hvalid g hg hterminal
  exact ⟨m, hm, hclose⟩

-- ============================================================
-- Heads and the active set
-- ============================================================

def goalIsHead (goals : Set Goal) (heads : Set GoalHead) (g : Goal) : Prop :=
  g ∈ goals ∧
  ∃ h : GoalHead, h ∈ heads ∧ h.handle = goal_handle g ∧ h.t = goal_t g

-- ============================================================
-- The goal tree (#514)
-- ============================================================

/-- `child` is the current head of a series decomposed under `parent`'s series. -/
def goalChildOf (goals : Set Goal) (heads : Set GoalHead) (child parent : Goal) : Prop :=
  goalIsHead goals heads child ∧ goal_parent child = some (goal_handle parent)

/-- Every version of a series names the same parent (the `goal_parent_fixed`
    trigger). -/
def GoalParentFixed (goals : Set Goal) : Prop :=
  ∀ g g' : Goal, g ∈ goals → g' ∈ goals →
    goalSameSeries g g' → goal_parent g = goal_parent g'

/-- An Achieved parent head has no open child head and, if it has children,
    at least one Achieved child (the post-lock achieve check). -/
def GoalTreeClosed (goals : Set Goal) (heads : Set GoalHead) : Prop :=
  ∀ p : Goal, goalIsHead goals heads p → goal_state p = .Achieved →
    (∀ c : Goal, goalChildOf goals heads c p →
      goal_state c = .Achieved ∨ goal_state c = .Abandoned) ∧
    ((∃ c : Goal, goalChildOf goals heads c p) →
      ∃ c : Goal, goalChildOf goals heads c p ∧ goal_state c = .Achieved)

theorem achieved_parent_child_closed :
    ∀ (goals : Set Goal) (heads : Set GoalHead),
      GoalTreeClosed goals heads →
      ∀ p c : Goal,
        goalIsHead goals heads p →
        goal_state p = .Achieved →
        goalChildOf goals heads c p →
        goal_state c = .Achieved ∨ goal_state c = .Abandoned := by
  intro goals heads hclosed p c hp hachieved hc
  exact (hclosed p hp hachieved).1 c hc

theorem achieved_parent_has_achieved_child :
    ∀ (goals : Set Goal) (heads : Set GoalHead),
      GoalTreeClosed goals heads →
      ∀ p : Goal,
        goalIsHead goals heads p →
        goal_state p = .Achieved →
        (∃ c : Goal, goalChildOf goals heads c p) →
        ∃ c : Goal, goalChildOf goals heads c p ∧ goal_state c = .Achieved := by
  intro goals heads hclosed p hp hachieved hsome
  exact (hclosed p hp hachieved).2 hsome

/-- Reachability along later ticks of the same handle. -/
inductive GoalSeriesReachable (goals : Set Goal) : Goal → Goal → Prop where
  | refl {g : Goal} :
      g ∈ goals → GoalSeriesReachable goals g g
  | step {source mid next : Goal} :
      GoalSeriesReachable goals source mid →
      next ∈ goals →
      goalLater next mid →
      GoalSeriesReachable goals source next

def activeGoalHeadFrom
    (goals : Set Goal) (heads : Set GoalHead) (source head : Goal) : Prop :=
  GoalSeriesReachable goals source head ∧
  goal_state head = .Active ∧
  goalIsHead goals heads head

theorem active_goal_head_from_active :
    ∀ (goals : Set Goal) (heads : Set GoalHead) (source head : Goal),
      activeGoalHeadFrom goals heads source head → goal_state head = .Active := by
  intro _ _ _ _ h
  exact h.2.1

theorem active_goal_head_from_head :
    ∀ (goals : Set Goal) (heads : Set GoalHead) (source head : Goal),
      activeGoalHeadFrom goals heads source head → goalIsHead goals heads head := by
  intro _ _ _ _ h
  exact h.2.2

def activeGoals (goals : Set Goal) (heads : Set GoalHead) (o : Owner) : Set Goal :=
  fun g => goal_owner g = o ∧ goal_state g = .Active ∧ goalIsHead goals heads g

-- ============================================================
-- Self as query, not entity (S3: cue-indexed, not parameterless)
-- ============================================================

/-- Candidate pool — owner's Perspectives. Not Self. -/
def ownerPerspectives (memories : Set Memory) (o : Owner) : Set Memory :=
  fun m => m ∈ memories ∧ memory_owner m = o ∧ memory_kind m = .Perspective

/-- Compatibility name. Not the Self query. -/
def selfPerspectives (memories : Set Memory) (o : Owner) : Set Memory :=
  ownerPerspectives memories o

def selfGoals (goals : Set Goal) (heads : Set GoalHead) (o : Owner) : Set Goal :=
  activeGoals goals heads o

/-- Situation touches this admission: the `t` is in the cue, or a pin is. -/
def cueTouches (m : Memory) (cue : Cue) : Prop :=
  memory_t m ∈ cue ∨
  (∃ id : MemoryId, id ∈ cue ∧ (id ∈ memory_origins m ∨ id ∈ memory_refs m))

/-- Self for a situation. Different cues, different Self. Question text is
    protocol; the kernel face of situation is `Cue`. -/
def situatedSelf
    (memories : Set Memory) (heads : Set MemoryHead)
    (o : Owner) (cue : Cue) : Set Memory :=
  fun m =>
    m ∈ ownerPerspectives memories o ∧
    memoryIsHead memories heads m ∧
    cueTouches m cue

theorem self_goals_are_active_goals :
    ∀ (goals : Set Goal) (heads : Set GoalHead) (o : Owner),
      selfGoals goals heads o = activeGoals goals heads o := by
  intro _ _ _
  rfl

theorem self_goal_owner :
    ∀ (goals : Set Goal) (heads : Set GoalHead) (o : Owner) (g : Goal),
      g ∈ selfGoals goals heads o → goal_owner g = o := by
  intro _ _ _ _ h
  exact h.1

theorem self_goal_active :
    ∀ (goals : Set Goal) (heads : Set GoalHead) (o : Owner) (g : Goal),
      g ∈ selfGoals goals heads o → goal_state g = .Active := by
  intro _ _ _ _ h
  exact h.2.1

theorem self_goal_head :
    ∀ (goals : Set Goal) (heads : Set GoalHead) (o : Owner) (g : Goal),
      g ∈ selfGoals goals heads o → goalIsHead goals heads g := by
  intro _ _ _ _ h
  exact h.2.2

theorem self_perspective_member :
    ∀ (memories : Set Memory) (o : Owner) (m : Memory),
      m ∈ selfPerspectives memories o → m ∈ memories := by
  intro _ _ _ h
  exact h.1

theorem self_perspective_owner :
    ∀ (memories : Set Memory) (o : Owner) (m : Memory),
      m ∈ selfPerspectives memories o → memory_owner m = o := by
  intro _ _ _ h
  exact h.2.1

theorem self_perspective_kind :
    ∀ (memories : Set Memory) (o : Owner) (m : Memory),
      m ∈ selfPerspectives memories o → memory_kind m = .Perspective := by
  intro _ _ _ h
  exact h.2.2

theorem situated_self_is_perspective :
    ∀ memories heads o cue m,
      m ∈ situatedSelf memories heads o cue → memory_kind m = .Perspective := by
  intro _ _ _ _ _ h
  exact h.1.2.2

theorem situated_self_owner :
    ∀ memories heads o cue m,
      m ∈ situatedSelf memories heads o cue → memory_owner m = o := by
  intro _ _ _ _ _ h
  exact h.1.2.1

theorem situated_self_is_head :
    ∀ memories heads o cue m,
      m ∈ situatedSelf memories heads o cue → memoryIsHead memories heads m := by
  intro _ _ _ _ _ h
  exact h.2.1

theorem situated_self_touches_cue :
    ∀ memories heads o cue m,
      m ∈ situatedSelf memories heads o cue → cueTouches m cue := by
  intro _ _ _ _ _ h
  exact h.2.2

/-- S3 — Self is cue-indexed. The candidate pool is not Self. -/
theorem situated_self_subset_owner_perspectives :
    ∀ memories heads o cue m,
      m ∈ situatedSelf memories heads o cue →
        m ∈ ownerPerspectives memories o := by
  intro _ _ _ _ _ h
  exact h.1

-- ============================================================
-- Goal assignment and evidence
-- ============================================================

def goalAssignedToPerspective (memories : Set Memory) (goal : Goal) (self : Memory) : Prop :=
  self ∈ memories ∧
  memory_kind self = .Perspective ∧
  memory_owner self = goal_owner goal ∧
  goal_assignment goal = some (memory_t self)

theorem goal_assignment_same_owner
    (memories : Set Memory) (goal : Goal) (self : Memory)
    (h : goalAssignedToPerspective memories goal self) :
    memory_owner self = goal_owner goal :=
  h.2.2.1

def GoalAssignmentValid (memories : Set Memory) (goals : Set Goal) : Prop :=
  ∀ g : Goal, g ∈ goals →
    goal_assignment g = none ∨
    ∃ self : Memory, goalAssignedToPerspective memories g self

def RetainedGoalAssignmentValid
    (goals : Set Goal) (memories : Set Memory)
  (targets : Set ErasedPinTarget) : Prop :=
  ∀ g : Goal, g ∈ goals →
    goal_assignment g = none ∨
    (∃ m : Memory,
      m ∈ memories ∧
      goal_assignment g = some (memory_t m) ∧
      memory_kind m = .Perspective ∧
      memory_owner m = goal_owner g) ∨
    (∃ e : ErasedPinTarget,
      e ∈ targets ∧
      ∃ id : MemoryId,
        goal_assignment g = some id ∧
        erased_pin_target_t e = id ∧
      erased_pin_target_kind e = .Perspective)

theorem goal_assignment_valid_retained_empty
    (goals : Set Goal) (memories : Set Memory)
    (hvalid : GoalAssignmentValid memories goals) :
    RetainedGoalAssignmentValid goals memories (fun _ => False) := by
  intro g hg
  cases hvalid g hg with
  | inl hnone => exact Or.inl hnone
  | inr hassigned =>
      obtain ⟨self, hself⟩ := hassigned
      exact Or.inr (Or.inl ⟨self, hself.1, hself.2.2.2, hself.2.1, hself.2.2.1⟩)

theorem goal_assignment_target_perspective :
    ∀ memories goal self,
      goalAssignedToPerspective memories goal self → memory_kind self = .Perspective := by
  intro _ _ _ h
  exact h.2.1

def activeGoalsForSelf
    (goals : Set Goal) (heads : Set GoalHead) (memories : Set Memory)
    (self : Memory) : Set Goal :=
  fun head =>
    ∃ source : Goal,
      source ∈ goals ∧
      goalAssignedToPerspective memories source self ∧
      activeGoalHeadFrom goals heads source head

theorem active_goal_for_self_active :
    ∀ goals heads memories self head,
      head ∈ activeGoalsForSelf goals heads memories self → goal_state head = .Active := by
  intro goals heads memories self head h
  rcases h with ⟨source, _, _, hhead⟩
  exact active_goal_head_from_active goals heads source head hhead

theorem active_goal_for_self_head :
    ∀ goals heads memories self head,
      head ∈ activeGoalsForSelf goals heads memories self →
        goalIsHead goals heads head := by
  intro goals heads memories self head h
  rcases h with ⟨source, _, _, hhead⟩
  exact active_goal_head_from_head goals heads source head hhead

theorem active_goal_for_self_has_assignment :
    ∀ goals heads memories self head,
      head ∈ activeGoalsForSelf goals heads memories self →
        ∃ source : Goal,
          source ∈ goals ∧
          goalAssignedToPerspective memories source self ∧
          activeGoalHeadFrom goals heads source head := by
  intro _ _ _ _ _ h
  exact h

/-- Evidence ids resolve to admitted Fact or Abstraction. No authorship
    field — operator-must-have-evidence is retired with the authorship blob. -/
structure GoalEvidenceValid
    (goals : Set Goal) (memories : Set Memory) : Prop where
  resolved : ∀ g : Goal, g ∈ goals → ∀ i : MemoryId, i ∈ goal_evidence g →
    (∃ m : Memory, m ∈ memories ∧ memory_t m = i ∧
      memory_kind m ≠ .Perspective)

theorem goal_evidence_not_perspective :
    ∀ goals memories,
      GoalEvidenceValid goals memories →
      ∀ (g : Goal) (i : MemoryId), g ∈ goals → i ∈ goal_evidence g →
        (∃ m : Memory, m ∈ memories ∧ memory_t m = i ∧
          memory_kind m ≠ .Perspective) := by
  intro goals memories hvalid g i hg hi
  exact hvalid.resolved g hg i hi

theorem memory_kind_ne_perspective_cases (kind : MemoryKind)
    (h : kind ≠ .Perspective) : kind = .Fact ∨ kind = .Abstraction := by
  cases kind <;> simp_all

def RetainedGoalEvidenceValid
    (goals : Set Goal) (memories : Set Memory)
    (targets : Set ErasedPinTarget) : Prop :=
  ∀ g : Goal, g ∈ goals → ∀ i : MemoryId, i ∈ goal_evidence g →
    (∃ m : Memory, m ∈ memories ∧ memory_t m = i ∧
      (memory_kind m = .Fact ∨ memory_kind m = .Abstraction)) ∨
    (∃ e : ErasedPinTarget, e ∈ targets ∧ erased_pin_target_t e = i ∧
      (erased_pin_target_kind e = .Fact ∨ erased_pin_target_kind e = .Abstraction))

def RetainedGoalTerminalCloseFactValid
    (goals : Set Goal) (memories : Set Memory)
    (targets : Set ErasedPinTarget) : Prop :=
  ∀ g : Goal, g ∈ goals → (goal_state g).terminal = true →
    (∃ m : Memory,
      m ∈ memories ∧
      goal_close_fact_t g = some (memory_t m) ∧
      memory_kind m = .Fact ∧
      memory_owner m = goal_owner g) ∨
    (∃ e : ErasedPinTarget,
      e ∈ targets ∧
      goal_close_fact_t g = some (erased_pin_target_t e) ∧
      erased_pin_target_kind e = .Fact)

theorem goal_evidence_valid_retained_empty
    (goals : Set Goal) (memories : Set Memory)
    (hvalid : GoalEvidenceValid goals memories) :
    RetainedGoalEvidenceValid goals memories (fun _ => False) := by
  intro g hg i hi
  obtain ⟨m, hm, hid, hk⟩ := hvalid.resolved g hg i hi
  exact Or.inl ⟨m, hm, hid, memory_kind_ne_perspective_cases _ hk⟩

theorem goal_terminal_close_valid_retained_empty
    (goals : Set Goal) (memories : Set Memory)
    (hvalid : GoalTerminalCloseFactValid goals memories) :
    RetainedGoalTerminalCloseFactValid goals memories (fun _ => False) := by
  intro g hg hterminal
  exact Or.inl (hvalid g hg hterminal)

end Causa
