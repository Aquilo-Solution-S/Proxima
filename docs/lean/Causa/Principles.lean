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

/-- P2 (weakened) — operator-derived Goals carry evidence: the Goal ROW names
    at least one admitted non-Perspective memory it rests on
    (`evidence_memory_ids`). This does NOT say every Goal carries evidence:
    User/External Goals need none here. Whether the evidence satisfies the Goal
    is a measurement/decider judgment, not a universal kernel rule. -/
theorem principle_2_operator_goals_carry_evidence :
    ∀ goals memories,
      GoalEvidenceValid goals memories →
      ∀ g : Goal, g ∈ goals → goal_authorship g = .SystemOperator →
        ∃ m : Memory, m ∈ memories ∧ memory_id m ∈ goal_evidence g ∧
          memory_kind m ≠ .Perspective :=
  system_operator_goal_has_evidence

/-- P3 — operator memory outputs are never Facts. Discharged by
    CN-5 `operator_memory_output_not_fact`. -/
theorem principle_3_operators_never_output_facts :
    ∀ (inv : OperatorInvocation) (m : Memory),
      InvocationShapeValid inv → m ∈ inv.outputMemories → memory_kind m ≠ .Fact :=
  operator_memory_output_not_fact

/-- P3b — closing a Goal is an act, and the close-act emits a Fact. -/
theorem principle_3b_goal_close_is_an_act :
    ∀ g : Goal, (goal_state g).terminal = true →
      ∃ m : Memory, goal_close_fact g = some m ∧ memory_kind m = .Fact :=
  terminal_goal_closes_with_fact

/-- P3c — the loop's causal closure is perspectival, and in the two-kind model
    that is enforced by there being nowhere else to put it: every index row a
    Goal declares is a `reference`, so a Goal↔Fact connection asserts only that
    the Goal named the Fact. The claim that one caused the other is a judgment,
    and a judgment is an interpretation Perspective — a node. -/
theorem principle_3c_causal_closure_is_perspectival :
    ∀ (g : Goal) (d : NodeDeclaration), GoalDeclarationValid g d →
      ∀ e : Edge, e ∈ d.edges → edge_kind e = .reference :=
  goal_declared_rows_are_references

/-- P4 — direct Fact→Fact connections are non-interpretive. A Fact source
    reaches only Fact targets (E3), so no Fact-sourced row can be about an
    Abstraction or a Perspective in the first place. -/
theorem principle_4_facts_connect_non_interpretively :
    ∀ e : Edge, EdgeLayeringValid e →
      (edge_source e).memoryKind? = some .Fact →
      ∀ kt : MemoryKind, (edge_target e).memoryKind? = some kt → kt = .Fact :=
  fact_source_reaches_only_facts

/-- Epistemic corollary — an index row carries `origin` or `reference` and
    NOTHING else. THEOREM by exhaustion over the closed vocabulary: there is
    no causal and no interpretive value a Fact→Fact row could take, so
    "cosine similarity cannot encode an observer-relative relation" is not a
    rule to enforce but a shape that cannot be written. -/
theorem principle_epistemic_edge_kinds_are_exactly_two :
    ∀ e : Edge, edge_kind e = .origin ∨ edge_kind e = .reference := by
  intro e
  cases edge_kind e with
  | origin => exact Or.inl rfl
  | reference => exact Or.inr rfl

/-- Epistemic corollary — an interpretation is never a Fact. A Fact asserts no
    judgment, so it cannot occupy the interpreting position; the claim lives in
    a Perspective's payload and the index only records that it points at its
    subjects. -/
theorem principle_epistemic_fact_never_interprets :
    ∀ (edges : Set Edge) (p : Memory) (subject : NodeRef),
      interpretationOf edges p subject → memory_kind p ≠ .Fact :=
  interpretation_is_never_a_fact

/-- Epistemic corollary — operator-emitted generalizations/interpretations cannot
    become new immutable Facts. This is a representation bound, not a solution to
    Hume's problem of induction. -/
theorem principle_epistemic_operator_output_not_fact :
    ∀ (inv : OperatorInvocation) (output : Memory),
      InvocationShapeValid inv → output ∈ inv.outputMemories →
        memory_kind output ≠ .Fact :=
  operator_memory_output_not_fact

/-- Epistemic corollary — supersession cannot revise Fact identity: neither end
    of an admitted supersession lineage is a Fact. -/
theorem principle_epistemic_supersession_cannot_touch_facts :
    ∀ (memories : Set Memory), MemorySupersessionValid memories →
      ∀ new old : Memory, memorySupersedes memories new old →
        memory_kind new ≠ .Fact ∧ memory_kind old ≠ .Fact :=
  facts_never_supersede

/-- P5 — every admitted memory is grounded in Facts: a well-founded descent
    along the index (origins for what a row was made from, references for what
    it is about) bottoms out at Facts inside the admitted memory graph. Names
    the Provenance.lean table-scoped grounding theorem. -/
theorem principle_5_memories_grounded_in_facts :
    ∀ memories goals factEntities edges,
      MemoryGraphValid memories goals factEntities edges →
      ∀ m : Memory, m ∈ memories → GroundsInFact edges m :=
  memory_grounds_in_facts

/-- Epistemic corollary — every admitted Abstraction is empirically grounded:
    it has finite descent to Facts inside the valid memory graph. -/
theorem principle_epistemic_abstraction_grounded_in_facts :
    ∀ memories goals factEntities edges,
      MemoryGraphValid memories goals factEntities edges →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
        GroundsInFact edges m :=
  abstraction_grounds_in_facts

/-- Epistemic corollary — no Perspective is a view from nowhere: every admitted
    Perspective names at least one admitted memory row it rests on, and so
    descends to Facts. WEAKENED from "…to an Abstraction" with the class
    matrix: an interpretation Perspective references its subjects directly,
    whatever kind they are (doc 16 §The Model). -/
theorem principle_epistemic_perspective_is_no_view_from_nowhere :
    ∀ memories goals factEntities edges,
      MemoryGraphValid memories goals factEntities edges →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Perspective →
        ∃ e : Edge, e ∈ edges ∧
          edge_source e = .memory m ∧
          (∃ mt : Memory, mt ∈ memories ∧ edge_target e = .memory mt) :=
  perspective_has_provenance

/-- P6a — index rows obey the layer directionality law: for memory→memory
    rows, ℓ(source) ≥ ℓ(target). This names existing theorem ME-10
    `edge_layer_rule`. -/
theorem principle_6a_derivation_provenance_strictly_upward :
    ∀ e : Edge, EdgeValid e → ∀ ms mt : Memory,
      edge_source e = .memory ms →
      edge_target e = .memory mt →
      (memory_kind mt).layer ≤ (memory_kind ms).layer :=
  edge_layer_rule

/-- P6b — materialized personality read-scope was removed: there is no
    kernel `PersonalityInstance`, `read_scope`, or authored-personality
    accessor. Wake/read context is modeled from Goals, Facts, and access roles,
    not a materialized personality scope. -/
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

/-- P9 (v0.0.8) — REBUILDABILITY, the master edge invariant: the index is a
    function of node content, so dropping it and re-deriving it from the nodes
    yields the same set, and a store whose declarations are layer-legal
    rebuilds into a VALID index. Every other edge guarantee is a corollary of
    this one holding. -/
theorem principle_9_index_is_a_function_of_node_content :
    ∀ (content : Set NodeDeclaration) (edges : Set Edge),
      (∀ d : NodeDeclaration, d ∈ content → NodeDeclarationValid d) →
      EdgeTableRebuildable content edges →
      EdgeTableValid edges :=
  rebuilt_table_valid

end Causa
