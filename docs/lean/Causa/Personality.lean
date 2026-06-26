/-
Causa — Personality Character

N5 — character is a query over active Perspective heads authored by
one personality. It is not stored as Memory, Goal, or Self state.
-/

import Causa.Memory

namespace Causa

/-- Opaque personality character. No constructors: flavor weighting
    stays inside `character_of`. -/
axiom Character : Type

/-- Opaque aggregator over the active Perspective head set. -/
axiom character_of : Set Memory → Character

/-- A memory lifecycle head: no later row supersedes it. Memory
    analogue of `goalIsHead`. -/
def memoryIsHead (m : Memory) : Prop :=
  ¬ ∃ m' : Memory, memory_supersedes m' = some m

/-- Active Perspective heads authored by personality `p`. -/
def activePerspectiveHeads (p : PersonalityInstance) : Set Memory :=
  fun m =>
    memory_kind m = .Perspective ∧
    memory_authoring_personality m = some p ∧
    memoryIsHead m

/-- Personality character as a derived query, not a stored row. -/
noncomputable def personality_character (p : PersonalityInstance) : Character :=
  character_of (activePerspectiveHeads p)

end Causa
