/-
Causa — Principles

Named principle surface over the timeseries kernel. No trusted assumptions.
-/

import Causa.Operators
import Causa.Provenance
import Causa.Knowledge
import Causa.Goals

namespace Causa

theorem principle_1_facts_below_perspective :
    MemoryKind.layer .Fact < MemoryKind.layer .Perspective := by
  simp [MemoryKind.layer]

/-- P2 rebase: declared evidence is never a Perspective. Authorship-gated
    "operator goals carry evidence" retired with the authorship blob. -/
theorem principle_2_goal_evidence_not_perspective :
    ∀ goals memories cooled,
      GoalEvidenceValid goals memories cooled →
      ∀ (g : Goal) (i : MemoryId), g ∈ goals → i ∈ goal_evidence g →
        (∃ m : Memory, m ∈ memories ∧ memory_t m = i ∧
          memory_kind m ≠ .Perspective) ∨
        (∃ c : Cooled, c ∈ cooled ∧ cooled_t c = i ∧
          cooled_kind c ≠ .Perspective) :=
  goal_evidence_not_perspective

theorem principle_3_operators_never_output_facts :
    ∀ (inv : OperatorInvocation) (m : Memory),
      InvocationShapeValid inv → m ∈ inv.outputMemories → memory_kind m ≠ .Fact :=
  operator_memory_output_not_fact

theorem principle_3b_goal_close_is_an_act :
    ∀ g : Goal, (goal_state g).terminal = true →
      (goal_close_fact_t g).isSome = true :=
  terminal_goal_closes_with_fact

/-- P3c — a Goal never writes Memory.refs; its pins live on the Goal row. -/
theorem principle_3c_causal_closure_is_perspectival :
    ∀ (g : Goal),
      goalDeclaredTargetIds g =
        (goal_assignment g).toList ++ goal_dependencies g ++ goal_evidence g ++
          (goal_close_fact_t g).toList ++ (goal_write_act_t g).toList :=
  goal_declared_rows_are_references

/-- P4 — a Fact declares no origins, so it never originates from A/P. -/
theorem principle_4_facts_connect_non_interpretively :
    ∀ m : Memory, memory_kind m = .Fact → memory_origins m = [] :=
  fun m hk => m.fact_origins_empty hk

theorem principle_epistemic_edge_kinds_are_exactly_two (k : EdgeKind) :
    k = .origin ∨ k = .reference :=
  principle_epistemic_edge_kinds_are_exactly_two_aux k

theorem principle_epistemic_fact_never_interprets
    (p subject : Memory) (h : interpretationOf p subject) :
    memory_kind p ≠ .Fact :=
  interpretation_is_never_a_fact p subject h

theorem principle_epistemic_operator_output_not_fact :
    ∀ (inv : OperatorInvocation) (output : Memory),
      InvocationShapeValid inv → output ∈ inv.outputMemories →
        memory_kind output ≠ .Fact :=
  operator_memory_output_not_fact

/-- P4 epistemic supersession RETIRED: later `t` on a Fact handle is a new
    observation, not a revision. -/

theorem principle_5_memories_grounded_in_facts :
    ∀ memories goals heads cooled,
      MemoryGraphValid memories goals heads cooled →
      ∀ m : Memory, m ∈ memories → GroundsInFact memories cooled m :=
  memory_grounds_in_facts

theorem principle_epistemic_abstraction_grounded_in_facts :
    ∀ memories goals heads cooled,
      MemoryGraphValid memories goals heads cooled →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
        GroundsInFact memories cooled m :=
  abstraction_grounds_in_facts

theorem principle_epistemic_perspective_is_no_view_from_nowhere :
    ∀ memories goals heads cooled,
      MemoryGraphValid memories goals heads cooled →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Perspective →
        memory_origins m ≠ [] ∨ memory_refs m ≠ [] :=
  perspective_has_provenance

theorem principle_6a_derivation_provenance_strictly_upward :
    ∀ memories cooled out id,
      OriginKindValid memories cooled out →
      memory_kind out = .Abstraction →
      id ∈ memory_origins out →
      pinKindFactOrAbstraction memories cooled id :=
  operator_origin_row_not_upward

def principle_6b_personality_read_scope_removed : String :=
  "no kernel PersonalityInstance/read_scope; wake context is not materialized"

def principle_7_personality_is_not_entity : String :=
  "personality has no kernel row/type/instance; it emerges from Perspective context"

theorem principle_8_knowledge_artifact_model_independent :
    ∀ a : KnowledgeArtifact,
      ∃ c : InterpreterClass,
        c ∈ a.interpreterClasses ∧ c.canRecover a.text a.content :=
  knowledge_artifact_model_independent

theorem principle_8b_long_term_knowledge_artifact_has_text_memory :
    ∀ memories (a : KnowledgeArtifact),
      KnowledgeArtifactIn memories a →
        a.carrier ∈ memories ∧ ∃ text : Text, text = a.text :=
  long_term_knowledge_artifact_has_text_memory

/-- P9 — the pin set IS node content. No Edge table to rebuild. -/
theorem principle_9_index_is_a_function_of_node_content :
    ∀ m : Memory, derivePins m = (memory_origins m, memory_refs m, memory_goal_refs m) :=
  derived_table_rebuildable

/-- H2 — shared payload does not collapse admission identity. -/
theorem principle_content_share_preserves_t
    (m1 m2 : Memory) (h : contentShared m1 m2) :
    memory_t m1 ≠ memory_t m2 :=
  shared_content_preserves_distinct_admissions m1 m2 h

/-- H8 — Self is cue-indexed; the owner-wide P set is not Self. -/
theorem principle_situated_self_touches_cue :
    ∀ memories heads o cue m,
      m ∈ situatedSelf memories heads o cue → cueTouches m cue :=
  situated_self_touches_cue

end Causa
