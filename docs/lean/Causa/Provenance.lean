/-
Causa — Provenance grounding

N1: persisted memories are grounded by well-founded descent inside an
admitted memory graph. Descent runs along `origins[]` and `refs[]` (no
Edge table). `tick` (uuidv7 order) makes the descent well-founded.
-/

import Causa.Operators

namespace Causa

def pinFrom (m tgt : Memory) : Prop :=
  memory_t tgt ∈ memory_origins m ∨ memory_t tgt ∈ memory_refs m

def pinFromInTable (memories : Set Memory) (m tgt : Memory) : Prop :=
  m ∈ memories ∧ tgt ∈ memories ∧ pinFrom m tgt

/-- P5 — one admitted memory graph. No FactEntity. No Edge table. -/
structure MemoryGraphValid
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead) (cooled : Set Cooled) : Prop where
  memoryIdUnique : MemoryIdUnique memories
  goalIdUnique : GoalIdUnique goals
  cooledIdUnique : CooledIdUnique cooled
  headAligned : MemoryHeadAligned memories heads
  /-- Since v0.0.11 each reference column resolves against exactly one
      spine: `refs` against Memory (hot or cooled) and `goal_refs` against
      Goal. Before the split `refs` had to admit either, which is what
      forced every reader to re-derive the target's kind. -/
  pinTargetsExist :
    ∀ m : Memory, m ∈ memories →
      (∀ id : MemoryId, id ∈ memory_origins m → pinExists memories cooled id) ∧
      (∀ id : MemoryId, id ∈ memory_refs m → pinExists memories cooled id) ∧
      (∀ id : GoalId, id ∈ memory_goal_refs m →
        goalReferenceTargetExists goals id)
  originKind : ∀ m : Memory, m ∈ memories → OriginKindValid memories cooled m
  /-- Memory `t` and Goal `t` do not collide (both globally UNIQUE). -/
  memoryGoalIdsDisjoint :
    ∀ (m : Memory) (g : Goal), m ∈ memories → g ∈ goals → memory_t m ≠ goal_t g
  /-- A cooled stub keeps the Memory `t` it was cooled from, so it does not
      collide with a Goal `t` either. Needed to rule out the cooled half of
      `pinExists` when showing `refs` holds no Goal. -/
  cooledGoalIdsDisjoint :
    ∀ (c : Cooled) (g : Goal), c ∈ cooled → g ∈ goals → cooled_t c ≠ goal_t g
  /-- Every Abstraction has nonempty origins (F→A or A→A). -/
  abstractionHasOrigins :
    ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
      memory_origins m ≠ []
  /-- CN-6 — every non-Fact names at least one pin (origins or refs). -/
  derivedProvenance :
    ∀ m : Memory, m ∈ memories → memory_kind m ≠ .Fact →
      memory_origins m ≠ [] ∨ memory_refs m ≠ []
  /-- B2 — a cooled non-Fact stub is not a Fact-grounding leaf. -/
  groundingSupport :
    ∀ m : Memory, m ∈ memories → memory_kind m ≠ .Fact →
      ∃ id : MemoryId,
        (id ∈ memory_origins m ∨ id ∈ memory_refs m) ∧
        ((∃ tgt : Memory, tgt ∈ memories ∧ memory_t tgt = id) ∨
         (∃ c : Cooled, c ∈ cooled ∧ cooled_t c = id ∧ cooled_kind c = .Fact))
  /-- Every hot pin target is strictly earlier. -/
  pinTimeStrict :
    ∀ m tgt : Memory, m ∈ memories → tgt ∈ memories →
      pinFrom m tgt → memory_tick tgt < memory_tick m

theorem memory_graph_origin_kind :
    ∀ memories goals heads cooled,
      MemoryGraphValid memories goals heads cooled →
      ∀ m : Memory, m ∈ memories → OriginKindValid memories cooled m := by
  intro _ _ _ _ hgraph
  exact hgraph.originKind

theorem grounding_wf
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead) (cooled : Set Cooled)
    (hgraph : MemoryGraphValid memories goals heads cooled) :
    WellFounded (fun lo hi => pinFromInTable memories hi lo) :=
  Subrelation.wf
    (fun {a b} h => hgraph.pinTimeStrict b a h.1 h.2.1 h.2.2)
    (invImage memory_tick Nat.lt_wfRel).wf

inductive GroundsInFact (memories : Set Memory) (cooled : Set Cooled) : Memory → Prop where
  | fact {m} : memory_kind m = .Fact → GroundsInFact memories cooled m
  | cooled {m id} :
      (id ∈ memory_origins m ∨ id ∈ memory_refs m) →
      (∃ c : Cooled, c ∈ cooled ∧ cooled_t c = id ∧ cooled_kind c = .Fact) →
      GroundsInFact memories cooled m
  | step {m tgt} : pinFrom m tgt →
      GroundsInFact memories cooled tgt → GroundsInFact memories cooled m

theorem memory_grounds_in_facts
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead) (cooled : Set Cooled)
    (hgraph : MemoryGraphValid memories goals heads cooled) :
    ∀ m : Memory, m ∈ memories → GroundsInFact memories cooled m := by
  intro m
  refine (grounding_wf memories goals heads cooled hgraph).induction
    (C := fun m => m ∈ memories → GroundsInFact memories cooled m) m ?_
  intro m ih hm
  by_cases hfact : memory_kind m = .Fact
  · exact GroundsInFact.fact hfact
  · obtain ⟨id, hpin⟩ := hgraph.groundingSupport m hm hfact
    obtain ⟨hid, hsupport⟩ := hpin
    cases hsupport with
    | inl hhot =>
      obtain ⟨tgt, htgt, ht⟩ := hhot
      have hfrom : pinFrom m tgt := by
        cases hid with
        | inl ho => exact Or.inl (by rw [ht]; exact ho)
        | inr hr => exact Or.inr (by rw [ht]; exact hr)
      have htable : pinFromInTable memories m tgt := ⟨hm, htgt, hfrom⟩
      exact GroundsInFact.step hfrom (ih tgt htable htgt)
    | inr hcold =>
      exact GroundsInFact.cooled hid hcold

theorem abstraction_has_provenance :
    ∀ memories goals heads cooled,
      MemoryGraphValid memories goals heads cooled →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
        memory_origins m ≠ [] := by
  intro memories goals heads cooled hgraph m hm hk
  exact hgraph.abstractionHasOrigins m hm hk

theorem perspective_has_provenance :
    ∀ memories goals heads cooled,
      MemoryGraphValid memories goals heads cooled →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Perspective →
        memory_origins m ≠ [] ∨ memory_refs m ≠ [] := by
  intro memories goals heads cooled hgraph m hm hk
  have hne : memory_kind m ≠ .Fact := by rw [hk]; intro h; exact (nomatch h)
  exact hgraph.derivedProvenance m hm hne

/-- v0.0.11 — `refs` is the Memory spine, so a Goal `t` is never in it. It
    is a theorem now rather than a case each reader had to rule out at read
    time: the column carries the target's kind, and the two id spaces are
    disjoint, so nothing has to be probed to know this. -/
theorem a_goal_is_never_a_memory_reference
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead) (cooled : Set Cooled)
    (hgraph : MemoryGraphValid memories goals heads cooled)
    (m : Memory) (hm : m ∈ memories) (g : Goal) (hg : g ∈ goals) :
    goal_t g ∉ memory_refs m := by
  intro hin
  cases (hgraph.pinTargetsExist m hm).2.1 (goal_t g) hin with
  | inl hhot =>
    obtain ⟨m', hm', ht⟩ := hhot
    exact hgraph.memoryGoalIdsDisjoint m' g hm' hg ht
  | inr hcold =>
    obtain ⟨c, hc, ht⟩ := hcold
    exact hgraph.cooledGoalIdsDisjoint c g hc hg ht

/-- The other half: a Goal reference that IS declared resolves on the Goal
    spine, never on the Memory one. -/
theorem a_goal_reference_resolves_on_the_goal_spine
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead) (cooled : Set Cooled)
    (hgraph : MemoryGraphValid memories goals heads cooled)
    (m : Memory) (hm : m ∈ memories) (id : GoalId)
    (hin : id ∈ memory_goal_refs m) :
    goalReferenceTargetExists goals id :=
  (hgraph.pinTargetsExist m hm).2.2 id hin

theorem abstraction_grounds_in_facts :
    ∀ memories goals heads cooled,
      MemoryGraphValid memories goals heads cooled →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
        GroundsInFact memories cooled m := by
  intro memories goals heads cooled hgraph m hm _
  exact memory_grounds_in_facts memories goals heads cooled hgraph m hm

end Causa
