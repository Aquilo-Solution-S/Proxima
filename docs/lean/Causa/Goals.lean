/-
Causa — Goals

Goal: intended direction — desired future state, lifecycle head
(doc 06 §Contract). A core entity on its own axis: Goals are NEVER
Memory (doc 06 §Goal Entity, "Not Memory"). The two distinct Lean
Types carry GO-6 structurally.

Goal-to-Goal decomposition / dependency / inspiration is ordinary Edge
topology, not a `Goal` row field. Decisions about Goal↔Goal relations
live with Edge rows and relation descriptors; the kernel keeps no
`goal_parents` DAG primitive.

GO-7 — there is no Self primitive in this kernel, BY DESIGN:
"There is no Self row" (doc 06 §Self). Self(instance) is a query —
current root Perspective + active perspective heads + active goals.
Self must never be cached as a Memory row, a Goal row, Personality
instance, or materialized causal chain ("cache would become
authority"). The absence of a `Self` axiom here is the invariant;
COVERAGE.md records it explicitly.
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

/-- Goal row shape. Supersession stores the prior Goal id, not a recursive
    object pointer. Goal↔Goal DAG/topology is deliberately absent: those
    decisions are represented by ordinary Edge rows and relation descriptors. -/
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

-- ============================================================
-- Goal-to-Goal topology
-- ============================================================

/- GO-3/GO-4 retired from the Goal row: no `goal_parents` accessor,
   no Goal-local DAG. Goal decomposition/dependency/inspiration is
   Edge topology, with relation descriptor masks and edge ownership
   governing legality. Relation-specific acyclicity, if needed, is
   engine/relation validation, not a core Goal-row invariant. -/

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

/-- GO-8 — G_active(owner) = current Goal heads where state = Active
    (doc 06 §Goal Entity, verbatim). A query, not an entity.

    NOTE the deliberate duality (decision
    `docs/domain/decisions/2026-06-11-active-goals-two-queries.md`):
    doc 06 also sketches context-scoped active-goal queries. Those
    depend on wake context/Perspective selection and the named
    `core/inspires` relation constant; the kernel models relation
    CLASSES and shapes, not named relation ids, so the context query
    stays engine-level. The Owner query below is the substrate head/
    Active filter over the actual Goal table. -/
def activeGoals (goals : Set Goal) (o : Owner) : Set Goal :=
  fun g => goal_owner g = o ∧ goal_state g = .Active ∧ goalIsHead goals g

end Causa
