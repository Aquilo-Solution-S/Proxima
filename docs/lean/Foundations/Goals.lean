/-
Proxima Foundations — Goals

Goal: intended direction — desired future state, DAG position,
lifecycle head (doc 06 §Contract). A core entity on its own axis:
Goals are NEVER Memory (doc 06 §Goal Entity, "Not Memory"). The two
distinct Lean Types carry GO-6 structurally.

GO-7 — there is no Self primitive in this kernel, BY DESIGN:
"There is no Self row" (doc 06 §Self). Self(instance) is a query —
current root Perspective + active perspective heads + active goals.
Self must never be cached as a Memory row, a Goal row, or a
materialized causal chain ("cache would become authority"). The
absence of a `Self` axiom here is the invariant; COVERAGE.md records
it explicitly.
-/

import Foundations.Prelude
import Foundations.Owner
import Foundations.Identity
import Foundations.Memory

namespace Proxima

-- ============================================================
-- States and lifecycle (doc 06 §Goal Entity)
-- ============================================================

inductive GoalState where
  | Proposed   -- awaiting gate
  | Active     -- live direction
  | Paused     -- suspended direction
  | Achieved   -- positive close (terminal)
  | Abandoned  -- post-active negative close (terminal)
  | Rejected   -- gate-time decline (terminal)
  deriving DecidableEq, Repr

def GoalState.terminal : GoalState → Bool
  | .Achieved | .Abandoned | .Rejected => true
  | _ => false

/-- The admitted supersession transitions, transcribed from the
    doc 06 lifecycle diagram:

      (none) -> Proposed -> Active -> Paused -> Active
                          \-> Rejected
                          \-> Achieved
                          \-> Abandoned
      (none) -> Active
      Active -> Active       # modification

    Reading the diagram literally: Rejected branches off Proposed
    (gate-time decline — its state-table meaning), Achieved/Abandoned
    close out Active ("post-active negative close"). Terminal states
    admit nothing. `(none) -> Proposed | Active` are creations, not
    transitions, so they don't appear in this relation. -/
def goalTransitionAdmitted : GoalState → GoalState → Prop
  | .Proposed, .Active   => True
  | .Proposed, .Rejected => True
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
  | User            -- direct user Goal writes and gates
  | External        -- outside-agent proposals
  | SystemTool      -- tool-authored lifecycle close
  | SystemOperator  -- A→Goal operator output
  deriving DecidableEq, Repr

axiom Goal : Type
axiom goal_id         : Goal → GoalId
axiom goal_owner      : Goal → Owner
axiom goal_schema     : Goal → SchemaRef
axiom goal_state      : Goal → GoalState
axiom goal_supersedes : Goal → Option Goal
axiom goal_parents    : Goal → Set Goal
axiom goal_authorship : Goal → GoalAuthorship

axiom goal_id_injective :
  ∀ g1 g2 : Goal, goal_id g1 = goal_id g2 → g1 = g2

instance : AppendOnly Goal := ⟨⟩
noncomputable instance : Supersedable Goal := ⟨goal_supersedes⟩

-- ============================================================
-- Supersession constraints (doc 06 §Goal-Write API)
-- ============================================================

/-- GO-1 — prior and new Goal share Owner. -/
axiom goal_supersession_same_owner :
  ∀ g g' : Goal, goal_supersedes g = some g' →
    goal_owner g = goal_owner g'

/-- GO-2 — "Valid transition: prior state and new state pair is
    admitted." Every transition writes a new Goal row (GO-5); no
    in-place mutation; compliance erasure is the only delete path. -/
axiom goal_supersession_admitted :
  ∀ g g' : Goal, goal_supersedes g = some g' →
    goalTransitionAdmitted (goal_state g') (goal_state g)

-- ============================================================
-- DAG (doc 06: "DAG position"; goal_parents)
-- ============================================================

/-- GO-4 — DAG parents stay within one Owner ("Cross-owner Goal
    assignment and cross-owner evidence are rejected", §Scoping). -/
axiom goal_parents_same_owner :
  ∀ g p : Goal, p ∈ goal_parents g → goal_owner p = goal_owner g

/-- Ancestry over `goal_parents`. -/
inductive goalAncestor : Goal → Goal → Prop where
  | parent  {g p : Goal}   : p ∈ goal_parents g → goalAncestor g p
  | trans   {g p q : Goal} : goalAncestor g p → goalAncestor p q → goalAncestor g q

/-- GO-3 — the Goal graph is acyclic: no Goal is its own ancestor. -/
axiom goal_parents_acyclic : ∀ g : Goal, ¬ goalAncestor g g

-- ============================================================
-- Heads and the active set (doc 06 §Goal Entity)
-- ============================================================

/-- A lifecycle head: no later row supersedes it. "Stale prior cannot
    be lifecycle head" (GoalWrite supersession constraint) is the
    contrapositive — superseded rows are not heads. -/
def goalIsHead (g : Goal) : Prop :=
  ¬ ∃ g' : Goal, goal_supersedes g' = some g

/-- GO-8 — G_active(owner) = current Goal heads where state = Active
    (doc 06, verbatim). A query, not an entity. -/
def activeGoals (o : Owner) : Set Goal :=
  fun g => goal_owner g = o ∧ goal_state g = .Active ∧ goalIsHead g

end Proxima
