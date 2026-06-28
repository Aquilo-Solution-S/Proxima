/-
Causa — Principles

Named principle surface over existing kernel content. This file adds no
trusted assumptions; each theorem below is discharged by definitions or
existing definitions/theorems from Memory, Edges, Operators, Provenance, and Knowledge.
-/

import Causa.Operators
import Causa.Provenance
import Causa.Knowledge

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

/-- Epistemic corollary — a valid Fact→Fact edge cannot carry a Causal class.
    Causal claims must be represented above Facts / perspective-relatively. -/
theorem principle_epistemic_fact_to_fact_not_causal :
    ∀ registry (e : Edge) (source target : Memory),
      EdgeHasClass registry e .Causal →
      edge_source e = .memory source →
      edge_target e = .memory target →
      memory_kind source = .Fact →
      memory_kind target = .Fact →
      False := by
  intro registry e source target hclass hsource htarget hsourceFact htargetFact
  have hlegal := edge_class_legal registry e .Causal hclass source target hsource htarget
  rw [hsourceFact, htargetFact] at hlegal
  rcases hlegal with h | h <;> exact (nomatch h)

/-- Epistemic corollary — a valid Fact→Fact edge cannot carry an Interpretive
    class. Interpretation is not an observer-independent Fact edge. -/
theorem principle_epistemic_fact_to_fact_not_interpretive :
    ∀ registry (e : Edge) (source target : Memory),
      EdgeHasClass registry e .Interpretive →
      edge_source e = .memory source →
      edge_target e = .memory target →
      memory_kind source = .Fact →
      memory_kind target = .Fact →
      False := by
  intro registry e source target hclass hsource htarget hsourceFact htargetFact
  have hlegal := edge_class_legal registry e .Interpretive hclass source target hsource htarget
  rw [hsourceFact, htargetFact] at hlegal
  rcases hlegal with h | h <;> exact (nomatch h)

/-- Epistemic corollary — operator-emitted generalizations/interpretations cannot
    become new immutable Facts. This is a representation bound, not a solution to
    Hume's problem of induction. -/
theorem principle_epistemic_operator_output_not_fact :
    ∀ registry (e : Edge) (output : Memory),
      EdgeOperatorShapeValid registry e →
      (edge_authorship e = .OperatorFtoA ∨
       edge_authorship e = .OperatorAtoA ∨
       edge_authorship e = .OperatorAtoP) →
      edge_source e = .memory output →
      memory_kind output ≠ .Fact :=
  operator_memory_output_not_fact

/-- Epistemic corollary — valid Supersession cannot revise Fact identity:
    neither endpoint of a valid Supersession memory edge can be a Fact. -/
theorem principle_epistemic_supersession_cannot_touch_facts :
    ∀ registry (e : Edge) (source target : Memory),
      EdgeHasClass registry e .Supersession →
      edge_source e = .memory source →
      edge_target e = .memory target →
      memory_kind source ≠ .Fact ∧ memory_kind target ≠ .Fact :=
  facts_never_supersede

/-- P5 — every admitted memory is grounded in Facts: a well-founded derivation/
    supersession descent (incl. higher-order A→A provenance) bottoms out
    at Facts inside the admitted memory graph. Names the Provenance.lean
    table-scoped grounding theorem. -/
theorem principle_5_memories_grounded_in_facts :
    ∀ registry memories goals factEntities edges,
      MemoryGraphValid registry memories goals factEntities edges →
      ∀ m : Memory, m ∈ memories → GroundsInFact registry edges m :=
  memory_grounds_in_facts

/-- Epistemic corollary — every admitted Abstraction is empirically grounded:
    it has finite descent to Facts inside the valid memory graph. -/
theorem principle_epistemic_abstraction_grounded_in_facts :
    ∀ registry memories goals factEntities edges,
      MemoryGraphValid registry memories goals factEntities edges →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
        GroundsInFact registry edges m :=
  abstraction_grounds_in_facts

/-- Epistemic corollary — every admitted Perspective has persisted Provenance
    to an admitted Abstraction; no Perspective is a view from nowhere. -/
theorem principle_epistemic_perspective_has_abstraction_provenance :
    ∀ registry memories goals factEntities edges,
      MemoryGraphValid registry memories goals factEntities edges →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Perspective →
        ∃ e : Edge, e ∈ edges ∧ EdgeHasClass registry e .Provenance ∧
          edge_source e = .memory m ∧
          (∃ mt : Memory, mt ∈ memories ∧ edge_target e = .memory mt ∧
            memory_kind mt = .Abstraction) :=
  perspective_has_provenance

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

/-- P8 — knowledge artifacts are model-independent: recoverability is witnessed
    by an interpreter class, not one named LLM model or human. -/
theorem principle_8_knowledge_artifact_model_independent :
    ∀ a : KnowledgeArtifact,
      ∃ c : InterpreterClass,
        c ∈ a.interpreterClasses ∧ c.canRecover a.text a.content :=
  knowledge_artifact_model_independent

/-- P8b — long-term knowledge artifacts are admitted text-bearing Memory rows. -/
theorem principle_8b_long_term_knowledge_artifact_has_text_memory :
    ∀ memories (a : KnowledgeArtifact),
      KnowledgeArtifactIn memories a →
        a.carrier ∈ memories ∧ ∃ text : Text, memory_text a.carrier = some text :=
  long_term_knowledge_artifact_has_text_memory

end Causa
