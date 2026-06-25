/-
Proxima Foundations — Principles

Named principle surface over existing kernel content. This file adds no
trusted assumptions; each theorem below is discharged by definitions or
existing axioms/theorems from Memory, Edges, and Operators.
-/

import Foundations.Operators

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

/-- P3 — Goals/operators never author Facts: Facts have no authoring
    personality. Discharged by CN-5 `facts_only_from_sources`. -/
theorem principle_3_goals_never_author_facts :
    ∀ m : Memory,
      memory_kind m = .Fact →
      memory_authoring_personality m = none :=
  facts_only_from_sources

/-- P4 — direct Fact→Fact relations are non-interpretive: the matrix
    permits exactly Structural or Provenance for Fact→Fact. -/
theorem principle_4_facts_connect_non_interpretively :
    ∀ c : RelationClass,
      c ∈ legalClasses .Fact .Fact ↔ c = .Structural ∨ c = .Provenance := by
  intro c
  rfl

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

end Proxima
