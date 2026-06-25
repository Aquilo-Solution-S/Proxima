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

axiom Goal : Type
axiom goal_id         : Goal → GoalId
axiom goal_owner      : Goal → Owner
axiom goal_schema     : Goal → SchemaRef
axiom goal_state      : Goal → GoalState
axiom goal_supersedes : Goal → Option Goal
axiom goal_parents    : Goal → Set Goal
axiom goal_authorship : Goal → GoalAuthorship

/-- Core retrieval/render text (doc 06 Goal row fields: "`title`,
    `text` | core retrieval/render text"). Total — every Goal carries
    both; payload stays opaque. -/
axiom goal_title : Goal → Text
axiom goal_text  : Goal → Text

/-- The personality instance whose A→Goal operator authored this Goal
    row, if operator-authored (doc 04: A→Goal runs under Π). `none`
    for direct User/External writes. Needed because the read-scope
    matrix gates GOALS too (doc 02 §Read-scope Matrix: "self may read
    other's A/P/Goals"). -/
axiom goal_authoring_personality : Goal → Option PersonalityInstance

axiom goal_authoring_personality_owner :
  ∀ (g : Goal) (p : PersonalityInstance),
    goal_authoring_personality g = some p →
    personality_owner p = goal_owner g

axiom goal_id_injective :
  ∀ g1 g2 : Goal, goal_id g1 = goal_id g2 → g1 = g2

instance : AppendOnly Goal := ⟨⟩
noncomputable instance : Supersedable Goal := ⟨goal_supersedes⟩

-- ============================================================
-- Supersession constraints (doc 06 §Goal-Write API)
-- ============================================================

/-- GO-1 + GO-2 — the doc-06 §Goal-Write-API supersession-constraints
    table, one axiom (merged, minimization pass): prior and new Goal
    share Owner ("Same Owner"), and the prior→new state pair is
    admitted ("Valid transition"). Every transition writes a new Goal
    row (GO-5); no in-place mutation; compliance erasure is the only
    delete path. -/
axiom goal_supersession_constraints :
  ∀ g g' : Goal, goal_supersedes g = some g' →
    goal_owner g = goal_owner g' ∧
    goalTransitionAdmitted (goal_state g') (goal_state g)

/-- GO-1 in its original shape — projection theorem. -/
theorem goal_supersession_same_owner :
    ∀ g g' : Goal, goal_supersedes g = some g' →
      goal_owner g = goal_owner g' :=
  fun g g' h => (goal_supersession_constraints g g' h).1

/-- GO-2 in its original shape — projection theorem. -/
theorem goal_supersession_admitted :
    ∀ g g' : Goal, goal_supersedes g = some g' →
      goalTransitionAdmitted (goal_state g') (goal_state g) :=
  fun g g' h => (goal_supersession_constraints g g' h).2

/-- GO-2b — "Current head: stale prior cannot be lifecycle head"
    (doc 06 §Goal-Write API). Timeless face: a Goal has at most one
    successor — two rows superseding the same prior would mean one of
    them superseded a stale (non-head) row. -/
axiom goal_supersession_prior_is_head :
  ∀ g1 g2 g' : Goal,
    goal_supersedes g1 = some g' → goal_supersedes g2 = some g' →
    g1 = g2

-- ============================================================
-- DAG (doc 06: "DAG position"; goal_parents)
-- ============================================================

/-- GO-4 — DAG parents stay within one Owner. Grounded on doc 04
    §Isolation ("Owner is the access boundary. Cross-owner reads and
    edges are invalid") — the parent relation is stored edge-like.
    NOTE: doc 06 §Scoping does not list `parent_goal_ids`; kept by
    decision `2026-06-11-goal-parents-owner-scope.md`. -/
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

/-- Closing a Goal is itself an act. This includes laying it down
    (`Abandoned`), not only reaching it (`Achieved`). By principle 3,
    an act that touches the world emits a Fact, so terminal Goals
    require a close-Fact reference. Whether that Fact justifies the
    close is measurement/decider responsibility, not a kernel claim. -/
axiom goal_close_fact : Goal → Option Memory

axiom terminal_goal_closes_with_fact :
  ∀ g : Goal, (goal_state g).terminal = true →
    ∃ m : Memory, goal_close_fact g = some m ∧ memory_kind m = .Fact

/-- A lifecycle head: no later row supersedes it. "Stale prior cannot
    be lifecycle head" (GoalWrite supersession constraint) is the
    contrapositive — superseded rows are not heads. -/
def goalIsHead (g : Goal) : Prop :=
  ¬ ∃ g' : Goal, goal_supersedes g' = some g

/-- GO-8 — G_active(owner) = current Goal heads where state = Active
    (doc 06 §Goal Entity, verbatim). A query, not an entity.

    NOTE the deliberate duality (decision
    `docs/domain/decisions/2026-06-11-active-goals-two-queries.md`):
    doc 06 also defines an INSTANCE-scoped `active_goals(instance)`
    (§Goal Assignment) that filters by `core/inspires` assignment to
    the current Self-Perspective. That query needs the named
    `core/inspires` relation constant; the kernel models relation
    CLASSES and shapes, not named relation ids, so the instance query
    stays engine-level. The two queries are different scopes of the
    same head/Active filter — not a contradiction. -/
def activeGoals (o : Owner) : Set Goal :=
  fun g => goal_owner g = o ∧ goal_state g = .Active ∧ goalIsHead g

/-- ME-7 for Goals — the read-scope matrix gates Goal retrieval
    exactly as it gates A/P (doc 02 §Read-scope Matrix). Facts have
    no Goal analogue; operator-authored Goals are gated against their
    authoring instance, direct writes are substrate-visible. -/
def personality_may_read_goal (p : PersonalityInstance) (g : Goal) : Prop :=
  personality_owner p = goal_owner g ∧
  (match goal_authoring_personality g with
   | some author => read_scope (goal_owner g) p author
   | none        => True)

end Proxima
