/-
Proxima Foundations — Principles

Named principle surface over existing kernel content. This file adds no
trusted assumptions; each theorem below is discharged by definitions or
existing axioms/theorems from Memory, Edges, and Operators.
-/

import Foundations.Operators
import Foundations.Personality
import Foundations.Provenance

namespace Proxima

/-- P1 — Facts sit below Perspective/read-scope: within one Owner, a
    Fact is readable unconditionally. -/
theorem principle_1_facts_below_perspective :
    ∀ (p : PersonalityInstance) (m : Memory),
      personality_owner p = memory_owner m →
      memory_kind m = .Fact →
      personality_may_read p m := by
  intro p m ho hk
  unfold personality_may_read
  exact ⟨ho, Or.inl hk⟩

/-- P2 (weakened) — operator-derived Goals carry evidence by an
    A→Goal Structural edge from the Goal to a non-Perspective Memory.
    This does NOT say every Goal carries evidence: User/External Goals
    need none here. Whether the evidence satisfies the Goal is a
    measurement/decider judgment, not a universal kernel rule. -/
theorem principle_2_operator_goals_carry_evidence :
    ∀ e : Edge, edge_authorship e = .OperatorAtoGoal →
      relation_class (edge_relation e) = .Structural ∧
      (∃ g : Goal, edge_source e = .goal g) ∧
      (∃ mt : Memory, edge_target e = .memory mt ∧ memory_kind mt ≠ .Perspective) := by
  intro e ha
  have h := operator_edges_shaped e
  rw [ha] at h
  exact h

/-- P3 — Goals/operators never author Facts: Facts have no authoring
    personality. Discharged by CN-5 `facts_only_from_sources`. -/
theorem principle_3_goals_never_author_facts :
    ∀ m : Memory,
      memory_kind m = .Fact →
      memory_authoring_personality m = none :=
  facts_only_from_sources

/-- P3b — closing a Goal is an act, and the close-act emits a Fact. -/
theorem principle_3b_goal_close_is_an_act :
    ∀ g : Goal, (goal_state g).terminal = true →
      ∃ m : Memory, goal_close_fact g = some m ∧ memory_kind m = .Fact :=
  terminal_goal_closes_with_fact

/-- P3c — the loop's causal closure is perspectival: a goal and a
    fact may be related causally ONLY by a perspective-authored
    claim, never a structural/EventSource/user edge. -/
theorem principle_3c_causal_closure_is_perspectival :
    ∀ e : Edge, relation_class (edge_relation e) = .Causal →
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

/-- P5 — every memory is grounded in Facts: a well-founded derivation/
    supersession descent (incl. higher-order A→A provenance) bottoms out
    at Facts. Names the Provenance.lean grounding theorem. -/
theorem principle_5_memories_grounded_in_facts :
    ∀ m : Memory, GroundsInFact m :=
  memory_grounds_in_facts

/-- P6a — derivation/provenance edges obey the layer directionality
    law: for memory→memory edges, ℓ(source) ≥ ℓ(target). This names
    existing theorem ME-10 `edge_layer_rule`. -/
theorem principle_6a_derivation_provenance_strictly_upward :
    ∀ (e : Edge) (ms mt : Memory),
      edge_source e = .memory ms →
      edge_target e = .memory mt →
      (memory_kind mt).layer ≤ (memory_kind ms).layer :=
  edge_layer_rule

/-- P6b — for authored non-Fact memories, `personality_may_read` is
    governed by the read-scope matrix entry for the authoring
    personality. -/
theorem principle_6b_read_scope_governs_authored_derived_reads :
    ∀ (p author : PersonalityInstance) (m : Memory),
      personality_owner p = memory_owner m →
      memory_kind m ≠ .Fact →
      memory_authoring_personality m = some author →
      (personality_may_read p m ↔ read_scope (memory_owner m) p author) := by
  intro p author m ho hk ha
  unfold personality_may_read
  rw [ho, ha]
  simp [hk]

/-- P6b append-only compatibility note: the stronger claim "matrix
    changes affect future reads only" has no separate theorem-shaped
    statement in the current kernel because there is no matrix-version
    or matrix-event state accessor. -/
def principle_6b_append_only_compatibility_note : String :=
  "read_scope has no matrix-version/event state accessor"

/-- P7 — personality character supervenes on the active Perspective
    head set. The aggregation semantics stay opaque in `character_of`. -/
theorem principle_7_personality_is_aggregate_of_perspectives :
    ∀ p q : PersonalityInstance, activePerspectiveHeads p = activePerspectiveHeads q →
      personality_character p = personality_character q :=
  fun _ _ h => congrArg character_of h

end Proxima
