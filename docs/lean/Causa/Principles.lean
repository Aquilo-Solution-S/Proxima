/-
Causa — Principles

Named principle surface over existing kernel content. This file adds no
trusted assumptions; each theorem below is discharged by definitions or
existing axioms/theorems from Memory, Edges, and Operators.
-/

import Causa.Operators
import Causa.Provenance

namespace Causa

/-- P1 — Facts sit below Perspectives in the F/A/P layer order. -/
theorem principle_1_facts_below_perspective :
    MemoryKind.layer .Fact < MemoryKind.layer .Perspective := by
  simp [MemoryKind.layer]

/-- P2 (weakened) — operator-derived Goals carry evidence by an
    A→Goal Structural edge from the Goal to a non-Perspective Memory.
    This does NOT say every Goal carries evidence: User/External Goals
    need none here. Whether the evidence satisfies the Goal is a
    measurement/decider judgment, not a universal kernel rule. -/
theorem principle_2_operator_goals_carry_evidence :
    ∀ registry (e : Edge), EdgeOperatorShapeValid registry e →
      edge_authorship e = .OperatorAtoGoal →
      EdgeHasClass registry e .Structural ∧
      (∃ g : Goal, edge_source e = .goal g) ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt ≠ .Perspective) := by
  intro registry e hshape ha
  have h := operator_edges_shaped registry e hshape
  rw [ha] at h
  exact h

/-- P3 — operator memory outputs are never Facts. Discharged by
    CN-5 `operator_memory_output_not_fact`. -/
theorem principle_3_operators_never_output_facts :
    ∀ registry (e : Edge) (m : Memory),
      EdgeOperatorShapeValid registry e →
      (edge_authorship e = .OperatorFtoA ∨
       edge_authorship e = .OperatorAtoA ∨
       edge_authorship e = .OperatorAtoP) →
      edge_source e = .memory m →
      memory_kind m ≠ .Fact :=
  operator_memory_output_not_fact

/-- P3b — closing a Goal is an act, and the close-act emits a Fact. -/
theorem principle_3b_goal_close_is_an_act :
    ∀ g : Goal, (goal_state g).terminal = true →
      ∃ m : Memory, goal_close_fact g = some m ∧ memory_kind m = .Fact :=
  terminal_goal_closes_with_fact

/-- P3c — the loop's causal closure is perspectival: a goal and a
    fact may be related causally ONLY by a perspective-authored
    claim, never a structural/source-ingest/user edge. -/
theorem principle_3c_causal_closure_is_perspectival :
    ∀ registry e, EdgeHasClass registry e .Causal →
      ((∃ g : Goal, edge_source e = .goal g) ∨
        (∃ g : Goal, edge_target e = .goal g)) →
      edge_authorship e = .PerspectiveGoalLink :=
  causal_goal_edge_perspectival

/-- P4 — direct Fact→Fact relations are non-interpretive: the matrix
    permits exactly Structural or Provenance for Fact→Fact. -/
theorem principle_4_facts_connect_non_interpretively :
    ∀ c : RelationClass,
      c ∈ legalClasses .Fact .Fact ↔ c = .Structural ∨ c = .Provenance := by
  intro c
  rfl

/-- P5 — every admitted memory is grounded in Facts: a well-founded derivation/
    supersession descent (incl. higher-order A→A provenance) bottoms out
    at Facts inside the admitted memory graph. Names the Provenance.lean
    table-scoped grounding theorem. -/
theorem principle_5_memories_grounded_in_facts :
    ∀ registry memories goals factEntities edges,
      MemoryGraphValid registry memories goals factEntities edges →
      ∀ m : Memory, m ∈ memories → GroundsInFact registry edges m :=
  memory_grounds_in_facts

/-- P6a — derivation/provenance edges obey the layer directionality
    law: for memory→memory edges, ℓ(source) ≥ ℓ(target). This names
    existing theorem ME-10 `edge_layer_rule`. -/
theorem principle_6a_derivation_provenance_strictly_upward :
    ∀ registry (e : Edge), EdgeCoreValid registry e → ∀ (ms mt : Memory),
      edge_source e = .memory ms →
      edge_target e = .memory mt →
      (memory_kind mt).layer ≤ (memory_kind ms).layer :=
  edge_layer_rule

/-- P6b — materialized personality read-scope was removed: there is no
    kernel `PersonalityInstance`, `read_scope`, or authored-personality
    accessor. Wake/read context is modeled from Goals, Facts, access roles,
    and Perspective-context edges, not a materialized personality scope. -/
def principle_6b_personality_read_scope_removed : String :=
  "no kernel PersonalityInstance/read_scope; wake context is not materialized"

/-- P7 — personality is not an entity. It emerges from Perspective and
    wake context; the kernel carries this by structural absence. -/
def principle_7_personality_is_not_entity : String :=
  "personality has no kernel row/type/instance; it emerges from Perspective context"

end Causa
