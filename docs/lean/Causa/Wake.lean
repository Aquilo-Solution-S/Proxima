/-
Causa — Wake (the self-organizing organism)

Wake rides on an armed Goal: `goal.wake_id = some wake_id`. The config
row is reusable; the fire is a write-act Fact, not a pin to the Goal.
-/

import Causa.Goals
import Causa.Edges
import Causa.Authorization
import Causa.Provenance

namespace Causa.Wake

open Causa

def MemoryKind.access : MemoryKind → AccessKind
  | .Fact        => .fact
  | .Abstraction => .abstraction
  | .Perspective => .perspective

/-- W5 rebase: produced rows `ref` the write-act `t` (UML §5b). No session ⇒ none. -/
def refsWriteAct (m : Memory) (tr : MemoryId) : Prop :=
  tr ∈ memory_refs m

structure Firing where
  actor    : User
  goal     : Goal
  config   : WakeConfig
  trigger  : Fact
  emitted  : List Memory
  injected : List Memory
  invoked  : List Action
  wake_config        : goal_wake_id goal = some (wake_id config)
  armed              : goalArmed goal
  goal_active        : goal_state goal = GoalState.Active
  actor_member       : goal_owner goal actor ≠ none
  trigger_read       : may_read actor (memory_owner trigger.memory) .fact
  each_injected_read : ∀ m ∈ injected, may_read actor (memory_owner m) (MemoryKind.access (memory_kind m))
  each_fact          : ∀ m ∈ emitted, memory_kind m = .Fact
  each_later         : ∀ m ∈ emitted, memory_tick trigger.memory < memory_tick m
  each_authzd        : ∀ m ∈ emitted, may_write actor (memory_owner m) .fact
  /-- One write-act Fact per fire, or none (keyless / no session). -/
  write_act_t        : Option MemoryId
  each_refs_write_act :
    ∀ m ∈ emitted, ∀ tr : MemoryId, write_act_t = some tr → tr ∈ memory_refs m
  each_action_allowed : ∀ a ∈ invoked, a ∈ config.toolset

theorem wake_emits_facts (fr : Firing) :
    ∀ m ∈ fr.emitted, memory_kind m = .Fact := fr.each_fact

theorem wake_cannot_escalate (fr : Firing) :
    ∀ m ∈ fr.emitted, may_write fr.actor (memory_owner m) .fact := fr.each_authzd

theorem powerless_actor_noops (fr : Firing)
    (h : ∀ o : Owner, ¬ may_write fr.actor o .fact) : fr.emitted = [] := by
  cases hl : fr.emitted with
  | nil => rfl
  | cons m ms =>
    exact absurd (fr.each_authzd m (by rw [hl]; exact List.mem_cons_self m ms))
      (h (memory_owner m))

theorem wake_trigger_readable (fr : Firing) :
    may_read fr.actor (memory_owner fr.trigger.memory) .fact := fr.trigger_read

theorem wake_invoked_actions_allowed (fr : Firing) :
    ∀ a ∈ fr.invoked, a ∈ fr.config.toolset := fr.each_action_allowed

theorem wake_context_readable (fr : Firing) :
    ∀ m ∈ fr.injected, may_read fr.actor (memory_owner m) (MemoryKind.access (memory_kind m)) :=
  fr.each_injected_read

theorem wake_emission_refs_write_act (fr : Firing) :
    ∀ m ∈ fr.emitted, ∀ tr : MemoryId, fr.write_act_t = some tr → tr ∈ memory_refs m :=
  fr.each_refs_write_act

/-- W5 — a Goal is never in Memory.refs; write-act is a Memory `t`. -/
theorem wake_motivation_is_never_causal (fr : Firing) :
    goalDeclaredTargetIds fr.goal =
      (goal_assignment fr.goal).toList ++ goal_dependencies fr.goal ++
        goal_evidence fr.goal ++ (goal_close_fact_t fr.goal).toList ++
        (goal_write_act_t fr.goal).toList :=
  goal_declared_rows_are_references fr.goal

def fires (f g : Memory) : Prop :=
  ∃ fr : Firing, fr.trigger.memory = f ∧ g ∈ fr.emitted

theorem fires_advances_time {f g : Memory} (h : fires f g) :
    memory_tick f < memory_tick g := by
  obtain ⟨fr, htrig, hmem⟩ := h
  have hlt := fr.each_later g hmem
  rw [htrig] at hlt
  exact hlt

theorem organism_grounded : WellFounded fires :=
  Subrelation.wf
    (fun {_ _} h => fires_advances_time h)
    (invImage memory_tick Nat.lt_wfRel).wf

def noopFiring (actor : User) (goal : Goal) (config : WakeConfig) (trig : Fact)
    (hcfg : goal_wake_id goal = some (wake_id config))
    (harm : goalArmed goal)
    (hactive : goal_state goal = GoalState.Active)
    (hmem : goal_owner goal actor ≠ none)
    (hread : may_read actor (memory_owner trig.memory) .fact) : Firing where
  actor := actor
  goal := goal
  config := config
  trigger := trig
  emitted := []
  injected := []
  invoked := []
  wake_config := hcfg
  armed := harm
  goal_active := hactive
  actor_member := hmem
  trigger_read := hread
  each_injected_read := by intro m hm; simp at hm
  each_fact := by intro m hm; simp at hm
  each_later := by intro m hm; simp at hm
  each_authzd := by intro m hm; simp at hm
  write_act_t := none
  each_refs_write_act := by intro _m hm _tr _htr; cases hm
  each_action_allowed := by intro a ha; simp at ha

/-- New Goal version on the same handle that records one evidence `t`. -/
def recordEvidence (goal : Goal) (i : MemoryId) (newId : GoalId) (newTick : Instant) : Goal :=
  { goal with t := newId, tick := newTick, evidence_t := [i] }

theorem recordEvidence_wake (goal : Goal) (i : MemoryId) (newId : GoalId) (newTick : Instant) :
    goal_wake_id (recordEvidence goal i newId newTick) = goal_wake_id goal := rfl

theorem recordEvidence_state (goal : Goal) (i : MemoryId) (newId : GoalId) (newTick : Instant) :
    goal_state (recordEvidence goal i newId newTick) = goal_state goal := rfl

theorem recordEvidence_owner (goal : Goal) (i : MemoryId) (newId : GoalId) (newTick : Instant) :
    goal_owner (recordEvidence goal i newId newTick) = goal_owner goal := rfl

def mkFact (handle : Handle) (id : MemoryId) (o : Owner) (tick : Instant) : Memory where
  handle := handle
  t := id
  kind := .Fact
  owner := o
  origins := []
  refs := []
  goal_refs := []
  blob_id := none
  content_id := none
  tick := tick
  fact_origins_empty := fun _ => rfl
  perspective_never_cites := fun h => nomatch h
  blob_fa_only := fun h => (h rfl).elim

def oneShotFiring
    (actor : User) (goal : Goal) (config : WakeConfig) (trig : Fact)
    (hcfg : goal_wake_id goal = some (wake_id config))
    (harm : goalArmed goal)
    (hactive : goal_state goal = GoalState.Active)
    (hmem : goal_owner goal actor ≠ none)
    (hread : may_read actor (memory_owner trig.memory) .fact)
    (o : Owner) (gh : Handle) (gid : MemoryId) (tick : Instant)
    (hw : may_write actor o .fact)
    (hlate : memory_tick trig.memory < tick) : Firing where
  actor := actor
  goal := goal
  config := config
  trigger := trig
  emitted := [mkFact gh gid o tick]
  injected := []
  invoked := []
  wake_config := hcfg
  armed := harm
  goal_active := hactive
  actor_member := hmem
  trigger_read := hread
  each_injected_read := by intro m hm; simp at hm
  each_fact := by intro m hm; simp [mkFact] at hm; subst hm; rfl
  each_later := by intro m hm; simp [mkFact] at hm; subst hm; exact hlate
  each_authzd := by intro m hm; simp [mkFact] at hm; subst hm; exact hw
  write_act_t := none
  each_refs_write_act := by
    intro _m _hm _tr htr
    cases htr
  each_action_allowed := by intro a ha; simp at ha

theorem oneShot_fires
    (actor : User) (goal : Goal) (config : WakeConfig) (trig : Fact)
    (hcfg : goal_wake_id goal = some (wake_id config))
    (harm : goalArmed goal)
    (hactive : goal_state goal = GoalState.Active)
    (hmem : goal_owner goal actor ≠ none)
    (hread : may_read actor (memory_owner trig.memory) .fact)
    (o : Owner) (gh : Handle) (gid : MemoryId) (tick : Instant)
    (hw : may_write actor o .fact)
    (hlate : memory_tick trig.memory < tick) :
    fires trig.memory (mkFact gh gid o tick) :=
  ⟨oneShotFiring actor goal config trig hcfg harm hactive hmem hread o gh gid tick hw hlate,
    by rfl, by simp [oneShotFiring]⟩

theorem firing_requires_active (fr : Firing) : goal_state fr.goal = GoalState.Active :=
  fr.goal_active

theorem terminal_cannot_fire (g : Goal) (h : (goal_state g).terminal = true) :
    ¬ ∃ fr : Firing, fr.goal = g := by
  rintro ⟨fr, hfg⟩
  have ha := fr.goal_active
  rw [hfg] at ha
  rw [ha] at h
  exact absurd h (by decide)

def closeGoal (goal : Goal) (closeFact : Memory) (_hk : memory_kind closeFact = .Fact)
    (newId : GoalId) (newTick : Instant) : Goal where
  handle := goal_handle goal
  t := newId
  owner := goal_owner goal
  title := goal_title goal
  state := .Achieved
  request_id := goal_request_id goal
  close_fact_t := some (memory_t closeFact)
  assignment_t := goal_assignment goal
  dependency_t := goal_dependencies goal
  evidence_t := [memory_t closeFact]
  wake_id := none
  write_act_t := none
  tick := newTick
  terminal_close_fact := fun _ => rfl

theorem closeGoal_terminal (goal : Goal) (closeFact : Memory)
    (hk : memory_kind closeFact = .Fact) (newId : GoalId) (newTick : Instant) :
    (goal_state (closeGoal goal closeFact hk newId newTick)).terminal = true := rfl

theorem closeGoal_same_handle (goal : Goal) (closeFact : Memory)
    (hk : memory_kind closeFact = .Fact) (newId : GoalId) (newTick : Instant) :
    goal_handle (closeGoal goal closeFact hk newId newTick) = goal_handle goal := rfl

theorem closeGoal_same_owner (goal : Goal) (closeFact : Memory)
    (hk : memory_kind closeFact = .Fact) (newId : GoalId) (newTick : Instant) :
    goal_owner (closeGoal goal closeFact hk newId newTick) = goal_owner goal := rfl

theorem closeGoal_admitted (goal : Goal) (closeFact : Memory)
    (hk : memory_kind closeFact = .Fact) (newId : GoalId) (newTick : Instant)
    (hactive : goal_state goal = GoalState.Active) :
    goalTransitionAdmitted (goal_state goal)
      (goal_state (closeGoal goal closeFact hk newId newTick)) := by
  rw [hactive]; exact trivial

theorem closeGoal_halts_wake (goal : Goal) (closeFact : Memory)
    (hk : memory_kind closeFact = .Fact) (newId : GoalId) (newTick : Instant) :
    ¬ ∃ fr : Firing, fr.goal = closeGoal goal closeFact hk newId newTick :=
  terminal_cannot_fire _ (closeGoal_terminal goal closeFact hk newId newTick)

theorem agent_can_act
    (actor : User) (goal : Goal) (config : WakeConfig) (trig : Fact)
    (hcfg : goal_wake_id goal = some (wake_id config))
    (harm : goalArmed goal) (hactive : goal_state goal = GoalState.Active)
    (hmem : goal_owner goal actor ≠ none)
    (hread : may_read actor (memory_owner trig.memory) .fact)
    (o : Owner) (hw : may_write actor o .fact)
    (gh : Handle) (gid : MemoryId) :
    ∃ g : Memory, fires trig.memory g :=
  ⟨_, oneShot_fires actor goal config trig hcfg harm hactive hmem hread o gh gid
        (memory_tick trig.memory + 1) hw (Nat.lt_succ_self _)⟩

theorem act_iff_fact_write_authority
    (actor : User) (goal : Goal) (config : WakeConfig) (trig : Fact)
    (hcfg : goal_wake_id goal = some (wake_id config))
    (harm : goalArmed goal) (hactive : goal_state goal = GoalState.Active)
    (hmem : goal_owner goal actor ≠ none)
    (hread : may_read actor (memory_owner trig.memory) .fact)
    (gh : Handle) (gid : MemoryId) :
    (∃ o : Owner, may_write actor o .fact)
      ↔ (∃ fr : Firing, fr.actor = actor ∧ fr.goal = goal ∧ fr.emitted ≠ []) := by
  constructor
  · rintro ⟨o, hw⟩
    exact ⟨oneShotFiring actor goal config trig hcfg harm hactive hmem hread o gh gid
            (memory_tick trig.memory + 1) hw (Nat.lt_succ_self _),
           rfl, rfl, by simp [oneShotFiring]⟩
  · rintro ⟨fr, hact, _, hne⟩
    cases hl : fr.emitted with
    | nil => rw [hl] at hne; exact absurd rfl hne
    | cons m ms =>
      have hm : m ∈ fr.emitted := by rw [hl]; exact List.mem_cons_self m ms
      have hwm := fr.each_authzd m hm
      rw [hact] at hwm
      exact ⟨memory_owner m, hwm⟩

def autonomousRun (seed : Fact) (o : Owner) (gh : Handle) (ids : Nat → MemoryId) :
    Nat → Memory
  | 0 => seed.memory
  | (n+1) => mkFact gh (ids n) o (memory_tick seed.memory + (n+1))

theorem autonomousRun_fact (seed : Fact) (o : Owner) (gh : Handle) (ids : Nat → MemoryId) :
    ∀ n, memory_kind (autonomousRun seed o gh ids n) = .Fact
  | 0 => fact_memory_kind seed
  | (_+1) => by simp [autonomousRun, mkFact, memory_kind]

theorem autonomousRun_time (seed : Fact) (o : Owner) (gh : Handle) (ids : Nat → MemoryId) :
    ∀ n, memory_tick (autonomousRun seed o gh ids n)
        = memory_tick seed.memory + n
  | 0 => by simp [autonomousRun, memory_tick]
  | (_+1) => by simp [autonomousRun, mkFact, memory_tick]

theorem organism_autonomous
    (actor : User) (goal : Goal) (config : WakeConfig) (seed : Fact)
    (hcfg : goal_wake_id goal = some (wake_id config))
    (harm : goalArmed goal) (hactive : goal_state goal = GoalState.Active)
    (hmem : goal_owner goal actor ≠ none)
    (hread : may_read actor (memory_owner seed.memory) .fact)
    (o : Owner) (hw : may_write actor o .fact)
    (gh : Handle) (ids : Nat → MemoryId) :
    ∃ run : Nat → Memory, ∀ n : Nat, fires (run n) (run (n+1)) := by
  refine ⟨autonomousRun seed o gh ids, fun n => ?_⟩
  have hlate : memory_tick (autonomousRun seed o gh ids n)
      < memory_tick seed.memory + (n+1) := by
    rw [autonomousRun_time seed o gh ids n]; exact Nat.lt_succ_self _
  have hread_n : may_read actor (memory_owner (autonomousRun seed o gh ids n)) .fact := by
    cases n with
    | zero => exact hread
    | succ k =>
      have hown : memory_owner (autonomousRun seed o gh ids k.succ) = o := by
        simp [autonomousRun, mkFact, memory_owner]
      rw [hown]
      exact may_write_implies_read actor o .fact hw
  exact oneShot_fires actor goal config
    ⟨autonomousRun seed o gh ids n, autonomousRun_fact seed o gh ids n⟩
    hcfg harm hactive hmem
    hread_n o gh (ids n) (memory_tick seed.memory + (n+1))
    hw hlate

#print axioms organism_grounded
#print axioms wake_cannot_escalate
#print axioms powerless_actor_noops
#print axioms wake_emission_refs_write_act
#print axioms wake_invoked_actions_allowed
#print axioms wake_motivation_is_never_causal
#print axioms oneShot_fires
#print axioms terminal_cannot_fire
#print axioms closeGoal_halts_wake
#print axioms act_iff_fact_write_authority
#print axioms organism_autonomous

end Causa.Wake
