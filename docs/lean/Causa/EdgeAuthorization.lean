/-
Causa — Pin authorization

Writer can read each pinned `t` (UML §2 `read`). No edge row. Source-owned
means the pin lives on the declaring Memory/Goal. Target render is a
separate gate (live / forgotten / unavailable).
-/

import Causa.Edges
import Causa.Authorization

namespace Causa

def MemoryKind.accessKind : MemoryKind → AccessKind
  | .Fact => .fact
  | .Abstraction => .abstraction
  | .Perspective => .perspective

/-- Seeing the declaring row is source-local. -/
def pin_source_read_admitted (requester : User) (source : Memory) : Prop :=
  may_read requester (memory_owner source) (memory_kind source).accessKind

/-- Target projection: the pinned row may be shown only if readable. -/
def pin_target_readable (requester : User) (target : Memory) : Prop :=
  may_read requester (memory_owner target) (memory_kind target).accessKind

/-- Uniform write admission: source write + target read. -/
def pin_write_admitted (requester : User) (source target : Memory) : Prop :=
  may_write requester (memory_owner source) (memory_kind source).accessKind ∧
  may_read requester (memory_owner target) (memory_kind target).accessKind

theorem pin_write_admitted_source_write :
    ∀ requester source target,
      pin_write_admitted requester source target →
      may_write requester (memory_owner source) (memory_kind source).accessKind := by
  intro _ _ _ h
  exact h.1

theorem pin_write_admitted_target_read :
    ∀ requester source target,
      pin_write_admitted requester source target →
      may_read requester (memory_owner target) (memory_kind target).accessKind := by
  intro _ _ _ h
  exact h.2

/-- Cross-owner targets are admitted when readable. -/
theorem cross_owner_target_admitted :
    ∀ requester source target,
      pin_write_admitted requester source target →
      may_read requester (memory_owner target) (memory_kind target).accessKind :=
  pin_write_admitted_target_read

/-- Goal pin: writer writes the Goal owner, reads the named Memory. -/
def goal_pin_write_admitted (requester : User) (g : Goal) (target : Memory) : Prop :=
  may_write requester (goal_owner g) .goal ∧
  may_read requester (memory_owner target) (memory_kind target).accessKind

/- ================================================================
   Goal collaboration authority

   These predicates are deliberately separate from `may_read` and
   `may_write`. A Goal audience is an additional, exact-version route; it is
   never inserted into an `Owner` or used to manufacture owner authority.
   ================================================================ -/

structure GoalControllerEpoch where
  goal_handle : Handle
  owner : Owner
  generation : Nat

structure GoalPublication where
  goal : GoalControllerEpoch
  memory_t : MemoryId
  source_owner : Owner
  source_generation : Nat
  controller_generation : Nat

structure GoalAudienceGrant where
  goal : GoalControllerEpoch
  subject : User
  role : Role
  controller_generation : Nat

structure GoalWorkBinding where
  goal : GoalControllerEpoch
  memory_handle : Handle
  source_owner : Owner
  source_generation : Nat
  controller_generation : Nat

/-- The audience route reads one exact admission.  A Goal's terminal state is
    intentionally absent: completion does not revoke a publication. -/
def goal_publication_read_admitted
    (requester : User) (publication : GoalPublication)
    (grant : Option GoalAudienceGrant) (current_controller_generation : Nat)
    (memory_owner_generation : Nat) (memory : Memory) : Prop :=
  (may_read requester publication.goal.owner memory.kind.accessKind ∨
    ∃ guest, grant = some guest ∧ guest.goal = publication.goal ∧
      guest.subject = requester ∧ guest.role.mayRead memory.kind.accessKind ∧
      guest.controller_generation = current_controller_generation) ∧
  publication.memory_t = memory_t memory ∧
  publication.source_owner = memory_owner memory ∧
  publication.goal.generation = current_controller_generation ∧
  publication.controller_generation = current_controller_generation ∧
  publication.source_generation = memory_owner_generation

/-- Editing an existing Goal's title or progress is separate from creating
    Goals or changing their decomposition. A guest needs Goal-level write
    for this exact stable Goal and its current controller generation. -/
def goal_edit_admitted (requester : User) (controller : GoalControllerEpoch)
    (grant : Option GoalAudienceGrant) (goal : Goal)
    (current_controller_generation : Nat) : Prop :=
  controller.goal_handle = goal_handle goal ∧
  controller.owner = goal_owner goal ∧
  controller.generation = current_controller_generation ∧
  (may_write requester (goal_owner goal) .goal ∨
    ∃ guest, grant = some guest ∧ guest.goal = controller ∧
      guest.subject = requester ∧ guest.role.mayWrite .goal ∧
      guest.controller_generation = current_controller_generation)

theorem goal_edit_requires_exact_goal (requester : User)
    (controller : GoalControllerEpoch) (grant : Option GoalAudienceGrant)
    (goal : Goal) (generation : Nat) :
    goal_edit_admitted requester controller grant goal generation →
      controller.goal_handle = goal_handle goal := by
  intro h
  exact h.1

theorem goal_edit_stale_controller_denied (requester : User)
    (controller : GoalControllerEpoch) (grant : Option GoalAudienceGrant)
    (goal : Goal) (generation : Nat)
    (h : controller.generation ≠ generation) :
    ¬ goal_edit_admitted requester controller grant goal generation := by
  intro admitted
  exact h admitted.2.2.1

/-- A work route is same-owner and write-ceiling bounded.  It grants no Goal
    administration, transfer, erase, or owner-membership capability. -/
def goal_work_write_admitted
    (requester : User) (binding : GoalWorkBinding)
    (grant : Option GoalAudienceGrant) (goal : Goal)
    (current_controller_generation memory_owner_generation : Nat)
    (memory : Memory) : Prop :=
  (may_write requester (goal_owner goal) memory.kind.accessKind ∨
    ∃ guest, grant = some guest ∧ guest.goal = binding.goal ∧
      guest.subject = requester ∧ guest.role.mayWrite memory.kind.accessKind ∧
      guest.controller_generation = current_controller_generation) ∧
  binding.goal.goal_handle = goal_handle goal ∧
  binding.source_owner = goal_owner goal ∧
  binding.memory_handle = memory_handle memory ∧
  binding.source_owner = memory_owner memory ∧
  binding.goal.generation = current_controller_generation ∧
  binding.controller_generation = current_controller_generation ∧
  binding.source_generation = memory_owner_generation ∧
  goal_state goal = .Active

/-- Working-draft reads use the same exact binding but do not require the Goal
    to remain Active; completing a Goal is not an implicit access revocation. -/
def goal_work_read_admitted
    (requester : User) (binding : GoalWorkBinding)
    (grant : Option GoalAudienceGrant) (goal : Goal)
    (current_controller_generation memory_owner_generation : Nat)
    (memory : Memory) : Prop :=
  (may_read requester (goal_owner goal) memory.kind.accessKind ∨
    ∃ guest, grant = some guest ∧ guest.goal = binding.goal ∧
      guest.subject = requester ∧ guest.role.mayRead memory.kind.accessKind ∧
      guest.controller_generation = current_controller_generation) ∧
  binding.goal.goal_handle = goal_handle goal ∧
  binding.source_owner = goal_owner goal ∧
  binding.memory_handle = memory_handle memory ∧
  binding.source_owner = memory_owner memory ∧
  binding.goal.generation = current_controller_generation ∧
  binding.controller_generation = current_controller_generation ∧
  binding.source_generation = memory_owner_generation

theorem goal_work_write_implies_read
    (requester : User) (binding : GoalWorkBinding)
    (grant : Option GoalAudienceGrant) (goal : Goal)
    (controller_generation memory_generation : Nat) (memory : Memory) :
    goal_work_write_admitted requester binding grant goal
      controller_generation memory_generation memory →
    goal_work_read_admitted requester binding grant goal
      controller_generation memory_generation memory := by
  intro h
  rcases h with ⟨hroute, hgoal, howner, hhandle, hmemoryowner,
    hgoalgen, hbindctrl, hsource, _active⟩
  have hread :
      may_read requester (goal_owner goal) memory.kind.accessKind ∨
        ∃ guest, grant = some guest ∧ guest.goal = binding.goal ∧
          guest.subject = requester ∧ guest.role.mayRead memory.kind.accessKind ∧
          guest.controller_generation = controller_generation := by
    cases hroute with
    | inl owner => exact Or.inl (may_write_implies_read _ _ _ owner)
    | inr guest =>
        rcases guest with ⟨g, hsome, hgoal', hsubject, hwrite, hctrl⟩
        exact Or.inr ⟨g, hsome, hgoal', hsubject,
          Nat.lt_of_lt_of_le hwrite g.role.write_le_read, hctrl⟩
  exact ⟨hread, hgoal, howner, hhandle, hmemoryowner,
    hgoalgen, hbindctrl, hsource⟩

theorem goal_controller_generation_mismatch_denies_publication
    (requester : User) (p : GoalPublication) (g : GoalAudienceGrant)
    (current_controller_generation memory_generation : Nat) (memory : Memory)
    (howner : ¬ may_read requester p.goal.owner memory.kind.accessKind)
    (h : g.controller_generation ≠ current_controller_generation) :
    ¬ goal_publication_read_admitted requester p (some g)
      current_controller_generation memory_generation memory := by
  intro admitted
  rcases admitted with ⟨route, _, _, _, _, _, _, _⟩
  rcases route with owner | guest
  · exact False.elim (howner owner)
  · rcases guest with ⟨guest, hsome, _, _, _, hguest⟩
    injection hsome with hguest_eq
    subst guest
    exact h hguest

theorem goal_source_generation_mismatch_denies_publication
    (requester : User) (p : GoalPublication) (g : Option GoalAudienceGrant)
    (controller_generation memory_generation : Nat) (memory : Memory)
    (h : p.source_generation ≠ memory_generation) :
    ¬ goal_publication_read_admitted requester p g
      controller_generation memory_generation memory := by
  intro admitted
  rcases admitted with ⟨_, _, _, _, _, hsource⟩
  exact h hsource

theorem goal_work_requires_same_owner
    (requester : User) (binding : GoalWorkBinding)
    (grant : Option GoalAudienceGrant) (goal : Goal)
    (controller_generation memory_generation : Nat) (memory : Memory)
    (h : binding.source_owner ≠ goal_owner goal) :
    ¬ goal_work_write_admitted requester binding grant goal
      controller_generation memory_generation memory := by
  intro admitted
  rcases admitted with ⟨_, _, howner, _, _, _, _, _, _⟩
  exact h howner

theorem goal_work_memory_handle_mismatch_denies
    (requester : User) (binding : GoalWorkBinding)
    (grant : Option GoalAudienceGrant) (goal : Goal)
    (controller_generation memory_generation : Nat) (memory : Memory)
    (h : binding.memory_handle ≠ memory_handle memory) :
    ¬ goal_work_write_admitted requester binding grant goal
      controller_generation memory_generation memory := by
  intro admitted
  rcases admitted with ⟨_, _, _, hhandle, _, _, _, _, _⟩
  exact h hhandle

theorem goal_work_memory_owner_mismatch_denies
    (requester : User) (binding : GoalWorkBinding)
    (grant : Option GoalAudienceGrant) (goal : Goal)
    (controller_generation memory_generation : Nat) (memory : Memory)
    (h : binding.source_owner ≠ memory_owner memory) :
    ¬ goal_work_write_admitted requester binding grant goal
      controller_generation memory_generation memory := by
  intro admitted
  rcases admitted with ⟨_, _, _, _, howner, _, _, _, _⟩
  exact h howner

theorem goal_work_controller_generation_mismatch_denies
    (requester : User) (binding : GoalWorkBinding)
    (grant : Option GoalAudienceGrant) (goal : Goal)
    (controller_generation memory_generation : Nat) (memory : Memory)
    (howner : ¬ may_write requester (goal_owner goal) memory.kind.accessKind)
    (h : binding.controller_generation ≠ controller_generation) :
    ¬ goal_work_write_admitted requester binding grant goal
      controller_generation memory_generation memory := by
  intro admitted
  rcases admitted with ⟨route, _, _, _, _, _, hbinding, _, _⟩
  rcases route with owner | guest
  · exact False.elim (howner owner)
  · rcases guest with ⟨_, _, _, _, _⟩
    exact h hbinding

structure ForkSourcePermission where
  requester : User
  source : Memory
  destination_owner : Owner
  source_generation : Nat
  destination_generation : Nat
  source_use : User → Prop
  destination_write : User → Prop

/-- An independent adaptation is a fresh A/P series anchored at one exact
    source version. Facts remain immutable admissions; Goals are not Memory
    fork targets. -/
def fork_admitted (permission : ForkSourcePermission)
    (source target : Memory)
    (current_source_generation current_destination_generation : Nat) : Prop :=
  memory_t source = memory_t permission.source ∧
  (memory_kind target = .Abstraction ∨ memory_kind target = .Perspective) ∧
  memory_kind source = memory_kind target ∧
  memory_handle source ≠ memory_handle target ∧
  memory_owner target = permission.destination_owner ∧
  permission.source_generation = current_source_generation ∧
  permission.destination_generation = current_destination_generation ∧
  memory_forked_from target = some (memory_t source) ∧
  memory_origins target = memory_origins source ∧
  memory_refs target = memory_refs source ∧
  memory_goal_refs target = memory_goal_refs source ∧
  memory_guided_by target = [] ∧
  permission.source_use permission.requester ∧
  permission.destination_write permission.requester

theorem fork_never_admits_fact_target
    (permission : ForkSourcePermission) (source target : Memory)
    (h : memory_kind target = .Fact) :
    ∀ source_generation destination_generation,
      ¬ fork_admitted permission source target source_generation destination_generation := by
  intro source_generation destination_generation
  intro admitted
  rcases admitted with ⟨_, htarget, _, _, _, _, _, _, _, _, _, _, _⟩
  cases htarget with
  | inl h_abs => simp_all
  | inr h_persp => simp_all

theorem fork_requires_fresh_series
    (permission : ForkSourcePermission) (source target : Memory)
    (h : memory_handle source = memory_handle target) :
    ∀ source_generation destination_generation,
      ¬ fork_admitted permission source target source_generation destination_generation := by
  intro source_generation destination_generation
  intro admitted
  rcases admitted with ⟨_, _, _, hfresh, _, _, _, _, _, _, _, _, _⟩
  exact hfresh h

theorem fork_requires_exact_anchor_and_copied_pins
    (permission : ForkSourcePermission) (source target : Memory)
    (source_generation destination_generation : Nat)
    (hadmitted : fork_admitted permission source target
      source_generation destination_generation) :
    memory_forked_from target = some (memory_t source) ∧
      memory_origins target = memory_origins source ∧
      memory_refs target = memory_refs source ∧
      memory_goal_refs target = memory_goal_refs source := by
  rcases hadmitted with
    ⟨_, _, _, _, _, _, _, hanchor, horigins, hrefs, hgoalrefs, _, _, _⟩
  exact ⟨hanchor, horigins, hrefs, hgoalrefs⟩

def fork_source_ancestor_read_admitted (requester : User) (source : Memory) : Prop :=
  may_read requester (memory_owner source) source.kind.accessKind

theorem fork_admission_preserves_separate_ancestor_read_gate
    (permission : ForkSourcePermission) (source target : Memory)
    (source_generation destination_generation : Nat)
    (hadmitted : fork_admitted permission source target
      source_generation destination_generation)
    (hancestor_unreadable :
      ¬ fork_source_ancestor_read_admitted permission.requester source) :
    fork_admitted permission source target source_generation destination_generation ∧
      ¬ fork_source_ancestor_read_admitted permission.requester source :=
  ⟨hadmitted, hancestor_unreadable⟩

/-- A forgotten pin target keeps its owner and kind, so the write admission
    onto it is the same rule as onto a live one (#488). -/
theorem pin_write_admitted_forgotten_target
    (requester : User) (source target : Memory) :
    pin_write_admitted requester source target.forget ↔
      pin_write_admitted requester source target :=
  Iff.rfl

end Causa
