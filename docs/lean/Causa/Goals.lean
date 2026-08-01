/-
Causa — Goals

Goal: intended direction — desired future state, lifecycle head
(doc 06 §Contract). A core entity on its own axis: Goals are NEVER
Memory (doc 06 §Goal Entity, "Not Memory"). The two distinct Lean
Types carry GO-6 structurally.

Goal↔Goal decomposition/dependency, assignment, and evidence are declared ON
THE GOAL ROW (`dependency_goal_ids`, `assignment_perspective_id`,
`evidence_memory_ids`; doc 16 §Flavor Migration). The index rows are DERIVED
from those columns, which is what makes the goal side of the edge table
rebuildable (E7) — the Goal row is the home of the statement, the index only
records that it was made. The kernel keeps no `goal_parents` DAG primitive.

GO-7 — there is no Self primitive in this kernel, BY DESIGN:
"There is no Self row" (doc 06 §Self). Self(instance) is a query —
readable Perspective selector rows + active Goal heads. Head-aware Perspective
queries read the memory row's lineage pointer and live in `Causa.Memory`
(`perspectiveHeads`). Self must never be cached as a Memory row, a Goal row,
Personality instance, or materialized causal chain ("cache would become
authority"). The absence of a `Self` axiom here is the invariant;
COVERAGE.md records it explicitly.

Wake (2026-06-28): a Goal may carry optional `WakeConfig`. When present
the Goal is ARMED — it reacts to matching Facts (the Mail→Fact→Wake
loop). "Goals are wake entries when configured to wake": an action with
no goal-context is useless, so wake rides on the Goal that gives it
direction, NOT a separate entity kind. The Goal row carries only the
CONFIG; the firing semantics (no-escalation, time-grounding, motivation)
are proved in `Causa.Wake`.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity
import Causa.Memory

namespace Causa

-- ============================================================
-- States and lifecycle (doc 06 §Goal Entity)
-- ============================================================

inductive GoalState where
  | Active     -- live direction
  | Paused     -- suspended direction
  | Achieved   -- positive close (terminal)
  | Abandoned  -- post-active negative close (terminal)
  deriving DecidableEq, Repr

def GoalState.terminal : GoalState → Bool
  | .Achieved | .Abandoned => true
  | _ => false

/-- The admitted supersession transitions, transcribed from the
    doc 06 lifecycle diagram:

      (none) -> Active
      Active -> Active       # modification
      Active -> Paused
      Active -> Achieved
      Active -> Abandoned
      Paused -> Active

    Achieved/Abandoned close out Active. Terminal states admit nothing.
    `(none) -> Active` is creation, not a transition, so it does not
    appear in this relation. -/
def goalTransitionAdmitted : GoalState → GoalState → Prop
  | .Active,   .Active   => True   -- modification
  | .Active,   .Paused   => True
  | .Active,   .Achieved => True
  | .Active,   .Abandoned => True
  | .Paused,   .Active   => True
  | _, _ => False

-- ============================================================
-- The Goal entity (doc 06 §Goal Entity)
-- ============================================================

inductive GoalAuthorship where
  | User            -- direct user Goal writes
  | External        -- outside-agent Goal writes
  | SystemTool      -- tool-authored lifecycle close
  | SystemOperator  -- A→Goal operator output
  deriving DecidableEq, Repr

-- ============================================================
-- Wake configuration (2026-06-28) — what arms a Goal to react
-- ============================================================

/-- A tool the agent may invoke during a wake. A thin descriptor: a name and
    the schema of its arguments. The kernel needs only the tool's identity;
    what it DOES (its World side effect) is flavor/engine mechanics, and the
    record it leaves is an ordinary Fact (the ToolCall abstraction returns a
    Fact). -/
structure Action where
  name      : Text
  signature : SchemaRef

/-- Wake configuration carried by a Goal. `trigger` is the (opaque,
    flavor-supplied) Fact matcher; `toolset` the permitted Actions ("World
    Actions"); `prompt` the standing instruction; `hard_memories` the
    always-injected context. A Goal with `wake = some _` is ARMED; `none` is a
    passive standing intent. This is the new kernel STRUCTURE (point 1, "it
    holds the configuration") — but it rides on the existing `Goal` kind
    (point 6, "Goals are wake entries when configured to wake"), never a
    separate entity. -/
structure WakeConfig where
  trigger       : Set Fact
  toolset       : Set Action
  prompt        : Text
  hard_memories : Set MemoryId

/-- Goal row shape. Supersession stores the prior Goal id, not a recursive
    object pointer, and the three topology columns store ids for the same
    reason. `wake` arms the Goal (D-A); `none` = a passive standing intent. -/
structure Goal where
  id          : GoalId
  owner       : Owner
  schema      : SchemaRef
  title       : Text
  text        : Text
  state       : GoalState
  supersedes  : Option GoalId
  authorship  : GoalAuthorship
  close_fact  : Option Memory
  wake        : Option WakeConfig
  /-- GO-12 — the self Perspective this Goal inspires
      (`goals.assignment_perspective_id`, where `core/inspires` went). -/
  assignment  : Option MemoryId
  /-- GO-3/GO-4 — the Goals this one waits on (`goals.dependency_goal_ids`,
      where `core/depends-on` went). -/
  dependencies : List GoalId
  /-- GO-14 — the memories this Goal rests on (`goals.evidence_memory_ids`,
      where `core/motivated-by` / `core/wake-motivated-by` went). -/
  evidence    : List MemoryId
  terminal_close_fact :
    state.terminal = true →
      ∃ m : Memory, close_fact = some m ∧ memory_kind m = .Fact

/-- Compatibility accessor for prose/Rust vocabulary. -/
def goal_id : Goal → GoalId := Goal.id

/-- Compatibility accessor for prose/Rust vocabulary. -/
def goal_owner : Goal → Owner := Goal.owner

/-- Compatibility accessor for prose/Rust vocabulary. -/
def goal_schema : Goal → SchemaRef := Goal.schema

/-- Compatibility accessor for prose/Rust vocabulary. -/
def goal_title : Goal → Text := Goal.title

/-- Compatibility accessor for prose/Rust vocabulary. -/
def goal_text : Goal → Text := Goal.text

/-- Compatibility accessor for prose/Rust vocabulary. -/
def goal_state : Goal → GoalState := Goal.state

/-- Compatibility accessor for prose/Rust vocabulary. -/
def goal_supersedes : Goal → Option GoalId := Goal.supersedes

/-- Compatibility accessor for prose/Rust vocabulary. -/
def goal_authorship : Goal → GoalAuthorship := Goal.authorship

/-- Compatibility accessor for prose/Rust vocabulary. -/
def goal_close_fact : Goal → Option Memory := Goal.close_fact

/-- Compatibility accessor for prose/Rust vocabulary. -/
def goal_wake : Goal → Option WakeConfig := Goal.wake

/-- Compatibility accessor for prose/Rust vocabulary. -/
def goal_assignment : Goal → Option MemoryId := Goal.assignment

/-- Compatibility accessor for prose/Rust vocabulary. -/
def goal_dependencies : Goal → List GoalId := Goal.dependencies

/-- Compatibility accessor for prose/Rust vocabulary. -/
def goal_evidence : Goal → List MemoryId := Goal.evidence

/-- E7, Goal side — every id a Goal row declares about another node, in the
    order the write asserts them: the Perspective it inspires, the Goals it
    waits on, the memories it rests on. Exactly one `reference` index row per
    entry (`goal_topology_edge_count` in storage), and nothing else: a Goal
    declares no derivation, so it sources no `origin` row. -/
def goalDeclaredTargetIds (g : Goal) : List Id :=
  (goal_assignment g).toList ++ goal_dependencies g ++ goal_evidence g

/-- A Goal is ARMED when it carries wake configuration — only then does it
    react to Facts. The passive case (`none`) is a standing intent. -/
def goalArmed (g : Goal) : Prop := (goal_wake g).isSome = true

/-- Goal id uniqueness is a table/store invariant, not a global property of
    raw structure values. -/
def GoalIdUnique (goals : Set Goal) : Prop :=
  ∀ g1 g2 : Goal,
    g1 ∈ goals →
    g2 ∈ goals →
    goal_id g1 = goal_id g2 →
    g1 = g2

/-- GO-9 in its legacy name — projection theorem over a valid Goal table. -/
theorem goal_id_injective :
    ∀ (goals : Set Goal),
      GoalIdUnique goals →
      ∀ g1 g2 : Goal,
        g1 ∈ goals →
        g2 ∈ goals →
        goal_id g1 = goal_id g2 → g1 = g2 := by
  intro goals huniq g1 g2 hg1 hg2 hid
  exact huniq g1 g2 hg1 hg2 hid

instance : AppendOnly Goal := ⟨⟩

-- ============================================================
-- Supersession constraints (doc 06 §Goal-Write API)
-- ============================================================

/-- Supersession ids resolve to rows in the actual Goal table. -/
def GoalSupersessionResolved (goals : Set Goal) : Prop :=
  ∀ (g : Goal) (prior_id : GoalId),
    g ∈ goals →
    goal_supersedes g = some prior_id →
    ∃ prior : Goal, prior ∈ goals ∧ goal_id prior = prior_id

/-- GO-1 + GO-2 — table-scoped supersession validity: prior and new Goal
    share Owner ("Same Owner"), and the prior→new state pair is admitted
    ("Valid transition"). Every transition writes a new Goal row (GO-5);
    no in-place mutation; compliance erasure is the only delete path. -/
def GoalSupersessionValid (goals : Set Goal) : Prop :=
  ∀ g g' : Goal,
    g ∈ goals →
    g' ∈ goals →
    goal_supersedes g = some (goal_id g') →
    goal_owner g = goal_owner g' ∧
    goalTransitionAdmitted (goal_state g') (goal_state g)

/-- GO-1 in its original shape — projection theorem. -/
theorem goal_supersession_same_owner :
    ∀ (goals : Set Goal),
      GoalSupersessionValid goals →
      ∀ g g' : Goal,
        g ∈ goals →
        g' ∈ goals →
        goal_supersedes g = some (goal_id g') →
        goal_owner g = goal_owner g' := by
  intro goals hvalid g g' hg hg' hsup
  exact (hvalid g g' hg hg' hsup).1

/-- GO-2 in its original shape — projection theorem. -/
theorem goal_supersession_admitted :
    ∀ (goals : Set Goal),
      GoalSupersessionValid goals →
      ∀ g g' : Goal,
        g ∈ goals →
        g' ∈ goals →
        goal_supersedes g = some (goal_id g') →
        goalTransitionAdmitted (goal_state g') (goal_state g) := by
  intro goals hvalid g g' hg hg' hsup
  exact (hvalid g g' hg hg' hsup).2

/-- GO-2b write-admission constraint: at most one successor row may name
    the same prior Goal id in a valid Goal table. -/
def GoalSuccessorUnique (goals : Set Goal) : Prop :=
  ∀ (g1 g2 : Goal) (prior_id : GoalId),
    g1 ∈ goals →
    g2 ∈ goals →
    goal_supersedes g1 = some prior_id →
    goal_supersedes g2 = some prior_id →
    g1 = g2

/-- GO-2b in its r1 name — projection theorem over a valid Goal table. -/
theorem goal_supersession_prior_is_head :
    ∀ (goals : Set Goal),
      GoalSuccessorUnique goals →
      ∀ (g1 g2 : Goal) (prior_id : GoalId),
        g1 ∈ goals →
        g2 ∈ goals →
        goal_supersedes g1 = some prior_id →
        goal_supersedes g2 = some prior_id →
        g1 = g2 := by
  intro goals huniq g1 g2 prior_id hg1 hg2 h1 h2
  exact huniq g1 g2 prior_id hg1 hg2 h1 h2

/-- GO-17 — root Goal rows (no `supersedes`) are creations, and creation is
    only `(none) -> Active` (doc 06 lifecycle). Paused/terminal roots would be
    lifecycle conclusions without a prior Goal. -/
def GoalRootValid (goals : Set Goal) : Prop :=
  ∀ g : Goal, g ∈ goals → goal_supersedes g = none → goal_state g = .Active

/-- Projection: a root Goal in a valid Goal table is Active. -/
theorem goal_root_active :
    ∀ (goals : Set Goal),
      GoalRootValid goals →
      ∀ g : Goal,
        g ∈ goals →
        goal_supersedes g = none →
        goal_state g = .Active := by
  intro goals hvalid g hg hroot
  exact hvalid g hg hroot

-- ============================================================
-- Goal-to-Goal topology (doc 16 §Flavor Migration)
-- ============================================================

/- GO-3/GO-4: no `goal_parents` accessor and no Goal-local DAG entity.
   Decomposition/dependency is the `dependency_goal_ids` column above, and the
   index rows it implies are ordinary `reference` entries derived from it.
   Relation-specific acyclicity, if needed, is engine validation, not a core
   Goal-row invariant. -/

-- ============================================================
-- Heads and the active set (doc 06 §Goal Entity)
-- ============================================================

/-- Closing a Goal is itself an act. This includes laying it down
    (`Abandoned`), not only reaching it (`Achieved`). By principle 3,
    an act that touches the world emits a Fact, so terminal Goals
    require a close-Fact reference. Whether that Fact justifies the
    close is measurement/decider responsibility, not a kernel claim. -/
theorem terminal_goal_closes_with_fact :
    ∀ g : Goal, (goal_state g).terminal = true →
      ∃ m : Memory, goal_close_fact g = some m ∧ memory_kind m = .Fact := by
  intro g h
  exact g.terminal_close_fact h

/-- GO-18 — table-scoped strengthening for close Facts: a terminal Goal's
    close Fact resolves to an actual Fact row owned by the same Owner as the
    Goal. The row-local `terminal_close_fact` field only proves the shape; this
    predicate is the storage/table validity face. -/
def GoalTerminalCloseFactValid (goals : Set Goal) (memories : Set Memory) : Prop :=
  ∀ g : Goal, g ∈ goals → (goal_state g).terminal = true →
    ∃ m : Memory,
      m ∈ memories ∧
      goal_close_fact g = some m ∧
      memory_kind m = .Fact ∧
      memory_owner m = goal_owner g

/-- Projection: terminal close Facts are table rows. -/
theorem terminal_goal_close_fact_member :
    ∀ (goals : Set Goal) (memories : Set Memory),
      GoalTerminalCloseFactValid goals memories →
      ∀ g : Goal,
        g ∈ goals →
        (goal_state g).terminal = true →
        ∃ m : Memory, m ∈ memories ∧ goal_close_fact g = some m := by
  intro goals memories hvalid g hg hterminal
  obtain ⟨m, hm, hclose, _, _⟩ := hvalid g hg hterminal
  exact ⟨m, hm, hclose⟩

/-- Projection: terminal close Facts are same-owner Facts. -/
theorem terminal_goal_close_fact_same_owner_fact :
    ∀ (goals : Set Goal) (memories : Set Memory),
      GoalTerminalCloseFactValid goals memories →
      ∀ (g : Goal) (m : Memory),
        g ∈ goals →
        (goal_state g).terminal = true →
        goal_close_fact g = some m →
        memory_kind m = .Fact ∧ memory_owner m = goal_owner g := by
  intro goals memories hvalid g m hg hterminal hclose
  obtain ⟨stored, _, hstored, hkind, howner⟩ := hvalid g hg hterminal
  rw [hclose] at hstored
  injection hstored with heq
  rw [heq]
  exact ⟨hkind, howner⟩

/-- A lifecycle head in the actual Goal table: no later row in the same
    table supersedes this row's id. -/
def goalIsHead (goals : Set Goal) (g : Goal) : Prop :=
  g ∈ goals ∧
  ¬ ∃ g' : Goal, g' ∈ goals ∧ goal_supersedes g' = some (goal_id g)

/-- Superseded rows are not lifecycle heads. -/
theorem goal_superseded_not_head :
    ∀ (goals : Set Goal) (g g' : Goal),
      g' ∈ goals →
      goal_supersedes g' = some (goal_id g) →
      ¬ goalIsHead goals g := by
  intro goals g g' hg' hsup hhead
  exact hhead.2 ⟨g', hg', hsup⟩

/-- Supersession reachability within one Goal table, from an originally
    assigned/source Goal row to any later row in its supersession lineage. -/
inductive GoalSupersessionReachable (goals : Set Goal) : Goal → Goal → Prop where
  | refl {g : Goal} :
      g ∈ goals → GoalSupersessionReachable goals g g
  | step {source mid next : Goal} :
      GoalSupersessionReachable goals source mid →
      next ∈ goals →
      goal_supersedes next = some (goal_id mid) →
      GoalSupersessionReachable goals source next

/-- A current active head reached from an assigned/source Goal row. -/
def activeGoalHeadFrom (goals : Set Goal) (source head : Goal) : Prop :=
  GoalSupersessionReachable goals source head ∧
  goal_state head = .Active ∧
  goalIsHead goals head

/-- Projection: a reached active Goal head is Active. -/
theorem active_goal_head_from_active :
    ∀ (goals : Set Goal) (source head : Goal),
      activeGoalHeadFrom goals source head → goal_state head = .Active := by
  intro goals source head h
  exact h.2.1

/-- Projection: a reached active Goal head is a lifecycle head. -/
theorem active_goal_head_from_head :
    ∀ (goals : Set Goal) (source head : Goal),
      activeGoalHeadFrom goals source head → goalIsHead goals head := by
  intro goals source head h
  exact h.2.2

/-- GO-8 — G_active(owner) = current Goal heads where state = Active
    (doc 06 §Goal Entity, verbatim). A query, not an entity.

    NOTE the deliberate duality: this Owner query is the substrate head/Active
    filter over the actual Goal table. The assignment-scoped Self query over
    Goal→Perspective inspiration edges is defined in `Causa.Edges`, where Edge
    rows and registered descriptors are available without introducing a Self
    entity or a named relation-id axiom. -/
def activeGoals (goals : Set Goal) (o : Owner) : Set Goal :=
  fun g => goal_owner g = o ∧ goal_state g = .Active ∧ goalIsHead goals g

-- ============================================================
-- Self as query, not entity (doc 06 §Self)
-- ============================================================

/-- GO-7 — the Goal half of Self(owner): active Goal heads. This is only an
    alias for a query over existing Goal rows, deliberately not a `Self` row. -/
def selfGoals (goals : Set Goal) (o : Owner) : Set Goal :=
  activeGoals goals o

/-- GO-7 — the Perspective half of Self(owner): existing Perspective rows owned
    by the same resolved owner. No personality instance, read-scope matrix, or
    cached Self row is introduced. -/
def selfPerspectives (memories : Set Memory) (o : Owner) : Set Memory :=
  fun m => m ∈ memories ∧ memory_owner m = o ∧ memory_kind m = .Perspective

/-- Self goals are exactly active goals; the name adds no new entity. -/
theorem self_goals_are_active_goals :
    ∀ (goals : Set Goal) (o : Owner), selfGoals goals o = activeGoals goals o := by
  intro goals o
  rfl

/-- Projection: every Self goal belongs to the requested owner. -/
theorem self_goal_owner :
    ∀ (goals : Set Goal) (o : Owner) (g : Goal),
      g ∈ selfGoals goals o → goal_owner g = o := by
  intro goals o g h
  exact h.1

/-- Projection: every Self goal is active. -/
theorem self_goal_active :
    ∀ (goals : Set Goal) (o : Owner) (g : Goal),
      g ∈ selfGoals goals o → goal_state g = .Active := by
  intro goals o g h
  exact h.2.1

/-- Projection: every Self goal is a lifecycle head in the source Goal table. -/
theorem self_goal_head :
    ∀ (goals : Set Goal) (o : Owner) (g : Goal),
      g ∈ selfGoals goals o → goalIsHead goals g := by
  intro goals o g h
  exact h.2.2

/-- Projection: every Self perspective is drawn from the source Memory table. -/
theorem self_perspective_member :
    ∀ (memories : Set Memory) (o : Owner) (m : Memory),
      m ∈ selfPerspectives memories o → m ∈ memories := by
  intro memories o m h
  exact h.1

/-- Projection: every Self perspective belongs to the requested owner. -/
theorem self_perspective_owner :
    ∀ (memories : Set Memory) (o : Owner) (m : Memory),
      m ∈ selfPerspectives memories o → memory_owner m = o := by
  intro memories o m h
  exact h.2.1

/-- Projection: Self's memory component contains only Perspectives. -/
theorem self_perspective_kind :
    ∀ (memories : Set Memory) (o : Owner) (m : Memory),
      m ∈ selfPerspectives memories o → memory_kind m = .Perspective := by
  intro memories o m h
  exact h.2.2

-- ============================================================
-- Goal assignment and evidence (doc 06 §Goal Assignment; doc 16)
-- ============================================================

/-- GO-12 — Goal assignment to a Self-Perspective, read off the Goal row's
    own `assignment_perspective_id`. No Self entity, no relation id, and no
    edge: the Goal knows the Perspective it inspires, and the `reference`
    index row is derived from that column. -/
def goalAssignedToPerspective (memories : Set Memory) (goal : Goal) (self : Memory) : Prop :=
  self ∈ memories ∧
  memory_kind self = .Perspective ∧
  goal_assignment goal = some (memory_id self)

/-- Projection: an assignment target is a Perspective row. -/
theorem goal_assignment_target_perspective :
    ∀ memories goal self,
      goalAssignedToPerspective memories goal self → memory_kind self = .Perspective := by
  intro _ _ _ h
  exact h.2.1

/-- GO-12 — active goals for a queried Self-Perspective: begin at assigned
    Goal rows, follow Goal supersession inside the Goal table, and return only
    current Active heads. A query over Goals + Memories, not a Self row. -/
def activeGoalsForSelf
    (goals : Set Goal) (memories : Set Memory) (self : Memory) : Set Goal :=
  fun head =>
    ∃ source : Goal,
      source ∈ goals ∧
      goalAssignedToPerspective memories source self ∧
      activeGoalHeadFrom goals source head

/-- Projection: every Self-assigned active Goal is Active. -/
theorem active_goal_for_self_active :
    ∀ goals memories self head,
      head ∈ activeGoalsForSelf goals memories self → goal_state head = .Active := by
  intro goals memories self head h
  rcases h with ⟨source, _, _, hhead⟩
  exact active_goal_head_from_active goals source head hhead

/-- Projection: every Self-assigned active Goal is a lifecycle head. -/
theorem active_goal_for_self_head :
    ∀ goals memories self head,
      head ∈ activeGoalsForSelf goals memories self → goalIsHead goals head := by
  intro goals memories self head h
  rcases h with ⟨source, _, _, hhead⟩
  exact active_goal_head_from_head goals source head hhead

/-- Projection: Self-assigned active Goals come from Perspective-targeted
    assignment, not from an owner-only active-goal scan. -/
theorem active_goal_for_self_has_assignment :
    ∀ goals memories self head,
      head ∈ activeGoalsForSelf goals memories self →
        ∃ source : Goal,
          source ∈ goals ∧
          goalAssignedToPerspective memories source self ∧
          activeGoalHeadFrom goals source head := by
  intro goals memories self head h
  exact h

/-- GO-14/GO-16 — table-scoped evidence validity for Goal rows. Every declared
    evidence id resolves to an admitted non-Perspective memory (the write path
    admits Fact and Abstraction only), and an operator-authored Goal must rest
    on at least one. User/External Goals may be intent without evidence. -/
structure GoalEvidenceValid (goals : Set Goal) (memories : Set Memory) : Prop where
  resolved : ∀ g : Goal, g ∈ goals → ∀ i : MemoryId, i ∈ goal_evidence g →
    ∃ m : Memory, m ∈ memories ∧ memory_id m = i ∧ memory_kind m ≠ .Perspective
  operatorGrounded : ∀ g : Goal, g ∈ goals →
    goal_authorship g = .SystemOperator → goal_evidence g ≠ []

/-- Projection: every SystemOperator Goal has table-resolved evidence. -/
theorem system_operator_goal_has_evidence :
    ∀ goals memories,
      GoalEvidenceValid goals memories →
      ∀ g : Goal, g ∈ goals → goal_authorship g = .SystemOperator →
        ∃ m : Memory, m ∈ memories ∧ memory_id m ∈ goal_evidence g ∧
          memory_kind m ≠ .Perspective := by
  intro goals memories hvalid g hg hauth
  have hne := hvalid.operatorGrounded g hg hauth
  obtain ⟨i, hi⟩ : ∃ i : MemoryId, i ∈ goal_evidence g := by
    cases hlist : goal_evidence g with
    | nil => exact absurd hlist hne
    | cons a rest => exact ⟨a, List.mem_cons_self _ _⟩
  obtain ⟨m, hm, hid, hkind⟩ := hvalid.resolved g hg i hi
  exact ⟨m, hm, by rw [hid]; exact hi, hkind⟩

/-- Projection: Goal evidence never points at a Perspective. Every declared
    evidence id resolves to an admitted Fact or Abstraction — a Goal rests on
    what was observed or derived, never on a judgment. -/
theorem goal_evidence_not_perspective :
    ∀ goals memories,
      GoalEvidenceValid goals memories →
      ∀ (g : Goal) (i : MemoryId), g ∈ goals → i ∈ goal_evidence g →
        ∃ m : Memory, m ∈ memories ∧ memory_id m = i ∧ memory_kind m ≠ .Perspective := by
  intro goals memories hvalid g i hg hi
  exact hvalid.resolved g hg i hi

end Causa
