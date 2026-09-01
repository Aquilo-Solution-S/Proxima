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
  /-- Since the Goal-reference split each reference column resolves against exactly one
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

/-- The retained graph after hard erase.  A witness preserves only the
    identity and closed kind needed to validate an old pin; it deliberately
    carries neither owner nor payload and therefore cannot support the live
    grounding claim below. -/
structure RetainedMemoryGraphValid
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) : Prop where
  memoryIdUnique : MemoryIdUnique memories
  goalIdUnique : GoalIdUnique goals
  cooledIdUnique : CooledIdUnique cooled
  erasedPinIdUnique : ErasedPinTargetIdUnique targets
  headAligned : MemoryHeadAligned memories heads
  pinTargetsExist :
    ∀ m : Memory, m ∈ memories →
      (∀ id : MemoryId, id ∈ memory_origins m →
        retainedOriginTargetExists memories cooled targets id) ∧
      (∀ id : MemoryId, id ∈ memory_refs m →
        retainedMemoryReferenceTargetExists memories cooled targets id) ∧
      (∀ id : GoalId, id ∈ memory_goal_refs m →
        retainedGoalReferenceTargetExists goals targets id)
  originKind :
    ∀ m : Memory, m ∈ memories →
      RetainedOriginKindValid memories cooled targets m
  /-- Memory `t` and Goal `t` do not collide (both globally UNIQUE). -/
  memoryGoalIdsDisjoint :
    ∀ (m : Memory) (g : Goal), m ∈ memories → g ∈ goals → memory_t m ≠ goal_t g
  erasedMemoryDisjoint : ErasedTargetsDisjointMemories targets memories
  erasedCooledDisjoint : ErasedTargetsDisjointCooled targets cooled
  erasedGoalDisjoint : ErasedTargetsDisjointGoals targets goals
  /-- Every Abstraction has nonempty origins (F→A or A→A). -/
  abstractionHasOrigins :
    ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
      memory_origins m ≠ []
  /-- CN-6 — every non-Fact names at least one pin (origins or refs). -/
  derivedProvenance :
    ∀ m : Memory, m ∈ memories → memory_kind m ≠ .Fact →
      memory_origins m ≠ [] ∨ memory_refs m ≠ []
  /-- Every hot pin target is strictly earlier. -/
  pinTimeStrict :
    ∀ m tgt : Memory, m ∈ memories → tgt ∈ memories →
      pinFrom m tgt → memory_tick tgt < memory_tick m

/-- A retained committed store includes the graph shape and the Goal-row
    carriers whose historical targets may now be witnesses. -/
structure RetainedCommittedStoreValid
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead) (cooled : Set Cooled)
    (targets : Set ErasedPinTarget) : Prop where
  graph : RetainedMemoryGraphValid memories goals heads cooled targets
  assignments : RetainedGoalAssignmentValid goals memories cooled targets
  evidence : RetainedGoalEvidenceValid goals memories cooled targets
  terminalClose : RetainedGoalTerminalCloseFactValid goals memories cooled targets

theorem memory_graph_valid_retained_empty
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead) (cooled : Set Cooled)
    (hgraph : MemoryGraphValid memories goals heads cooled) :
    RetainedMemoryGraphValid memories goals heads cooled (fun _ => False) := by
  refine {
    memoryIdUnique := hgraph.memoryIdUnique
    goalIdUnique := hgraph.goalIdUnique
    cooledIdUnique := hgraph.cooledIdUnique
    erasedPinIdUnique := ?_
    headAligned := hgraph.headAligned
    pinTargetsExist := ?_
    originKind := ?_
    memoryGoalIdsDisjoint := hgraph.memoryGoalIdsDisjoint
    erasedMemoryDisjoint := ?_
    erasedCooledDisjoint := ?_
    erasedGoalDisjoint := ?_
    abstractionHasOrigins := hgraph.abstractionHasOrigins
    derivedProvenance := hgraph.derivedProvenance
    pinTimeStrict := hgraph.pinTimeStrict
  }
  · intro e1 e2 he1 _ _
    exact False.elim he1
  · intro m hm
    constructor
    · intro id hid
      exact Or.inl ((hgraph.pinTargetsExist m hm).1 id hid)
    constructor
    · intro id hid
      exact Or.inl ((hgraph.pinTargetsExist m hm).2.1 id hid)
    · intro id hid
      exact Or.inl ((hgraph.pinTargetsExist m hm).2.2 id hid)
  · intro m hm
    let h := hgraph.originKind m hm
    refine {
      factEmpty := h.factEmpty
      absFactOrAbs := ?_
      perspAbsOrEmpty := ?_
    }
    · intro habs id hid
      cases h.absFactOrAbs habs id hid with
      | inl hfact => exact Or.inl (Or.inl hfact)
      | inr habs => exact Or.inr (Or.inl habs)
    · intro hp
      cases h.perspAbsOrEmpty hp with
      | inl hempty => exact Or.inl hempty
      | inr hall =>
          exact Or.inr (fun id hid => Or.inl (hall id hid))
  · intro e he _m _hm
    exact False.elim he
  · intro e he _c _hc
    exact False.elim he
  · intro e he _g _hg
    exact False.elim he

theorem memory_graph_and_goals_valid_retained_empty
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead) (cooled : Set Cooled)
    (hgraph : MemoryGraphValid memories goals heads cooled)
    (hassignment : GoalAssignmentValid memories goals)
    (hevidence : GoalEvidenceValid goals memories cooled)
    (hclose : GoalTerminalCloseFactValid goals memories cooled) :
    RetainedCommittedStoreValid memories goals heads cooled (fun _ => False) := by
  exact {
    graph := memory_graph_valid_retained_empty memories goals heads cooled hgraph
    assignments := goal_assignment_valid_retained_empty goals memories cooled hassignment
    evidence := goal_evidence_valid_retained_empty goals memories cooled hevidence
    terminalClose := goal_terminal_close_valid_retained_empty goals memories cooled hclose
  }

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

/-- After the Goal-reference split `refs` is the Memory spine, so a Goal `t` is never in it. It
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
