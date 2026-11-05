/-
Causa — Provenance grounding

N1: persisted memories are grounded by well-founded descent inside an
admitted memory graph. Descent runs along `origins[]` and `refs[]` (no
Edge table). `tick` (UUIDv8 order) makes the descent well-founded.
-/

import Causa.Operators

namespace Causa

def pinFrom (m tgt : Memory) : Prop :=
  memory_t tgt ∈ memory_origins m ∨ memory_t tgt ∈ memory_refs m ∨
    memory_forked_from m = some (memory_t tgt)

def pinFromInTable (memories : Set Memory) (m tgt : Memory) : Prop :=
  m ∈ memories ∧ tgt ∈ memories ∧ pinFrom m tgt

/-- P5 — one admitted memory graph. No FactEntity. No Edge table. -/
structure MemoryGraphValid
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead) : Prop where
  memoryIdUnique : MemoryIdUnique memories
  goalIdUnique : GoalIdUnique goals
  headAligned : MemoryHeadAligned memories heads
  forkAnchorAligned : ForkAnchorAligned memories
  /-- Since the Goal-reference split each reference column resolves against exactly one
      spine: `refs` against Memory (live or forgotten) and `goal_refs` against
      Goal. Before the split `refs` had to admit either, which is what
      forced every reader to re-derive the target's kind. -/
  pinTargetsExist :
    ∀ m : Memory, m ∈ memories →
      (∀ id : MemoryId, id ∈ memory_origins m → pinExists memories id) ∧
      (∀ id : MemoryId, id ∈ memory_refs m → pinExists memories id) ∧
      (∀ id : MemoryId, memory_forked_from m = some id → pinExists memories id) ∧
      (∀ id : GoalId, id ∈ memory_goal_refs m →
        goalReferenceTargetExists goals id)
  motivationsValid :
    ∀ g : Goal, g ∈ goals → goal_motivations_valid memories g
  guidanceValid :
    ∀ m : Memory, m ∈ memories → perspective_guidance_valid memories goals m
  originKind : ∀ m : Memory, m ∈ memories → OriginKindValid memories m
  /-- Memory `t` and Goal `t` do not collide (both globally UNIQUE). -/
  memoryGoalIdsDisjoint :
    ∀ (m : Memory) (g : Goal), m ∈ memories → g ∈ goals → memory_t m ≠ goal_t g
  /-- Every Abstraction has nonempty origins (F→A or A→A). -/
  abstractionHasOrigins :
    ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
      memory_origins m ≠ []
  /-- CN-6 — every non-Fact names at least one pin (origins or refs). -/
  derivedProvenance :
    ∀ m : Memory, m ∈ memories → memory_kind m ≠ .Fact →
      memory_origins m ≠ [] ∨ memory_refs m ≠ []
  /-- B2 — every non-Fact pins a stored row (live or forgotten: forget keeps
      the row, so it still grounds). -/
  groundingSupport :
    ∀ m : Memory, m ∈ memories → memory_kind m ≠ .Fact →
      ∃ id : MemoryId,
        (id ∈ memory_origins m ∨ id ∈ memory_refs m ∨
          memory_forked_from m = some id) ∧
        (∃ tgt : Memory, tgt ∈ memories ∧ memory_t tgt = id)
  /-- Every stored pin target is strictly earlier. -/
  pinTimeStrict :
    ∀ m tgt : Memory, m ∈ memories → tgt ∈ memories →
      pinFrom m tgt → memory_tick tgt < memory_tick m

/-- The retained graph after hard erase.  A witness preserves only the
    identity and closed kind needed to validate an old pin; it deliberately
    carries neither owner nor payload and therefore cannot support the live
    grounding claim below. -/
structure RetainedMemoryGraphValid
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead)
    (targets : Set ErasedPinTarget) : Prop where
  memoryIdUnique : MemoryIdUnique memories
  goalIdUnique : GoalIdUnique goals
  erasedPinIdUnique : ErasedPinTargetIdUnique targets
  headAligned : MemoryHeadAligned memories heads
  forkAnchorAligned : ForkAnchorAligned memories
  pinTargetsExist :
    ∀ m : Memory, m ∈ memories →
      (∀ id : MemoryId, id ∈ memory_origins m →
        retainedOriginTargetExists memories targets id) ∧
      (∀ id : MemoryId, id ∈ memory_refs m →
        retainedMemoryReferenceTargetExists memories targets id) ∧
      (∀ id : MemoryId, memory_forked_from m = some id →
        retainedMemoryReferenceTargetExists memories targets id) ∧
      (∀ id : GoalId, id ∈ memory_goal_refs m →
        retainedGoalReferenceTargetExists goals targets id)
  motivationsValid :
    ∀ g : Goal, g ∈ goals → goal_motivations_retained_valid memories goals targets g
  guidanceValid :
    ∀ m : Memory, m ∈ memories → perspective_guidance_retained_valid memories goals targets m
  originKind :
    ∀ m : Memory, m ∈ memories →
      RetainedOriginKindValid memories targets m
  /-- Memory `t` and Goal `t` do not collide (both globally UNIQUE). -/
  memoryGoalIdsDisjoint :
    ∀ (m : Memory) (g : Goal), m ∈ memories → g ∈ goals → memory_t m ≠ goal_t g
  erasedMemoryDisjoint : ErasedTargetsDisjointMemories targets memories
  erasedGoalDisjoint : ErasedTargetsDisjointGoals targets goals
  /-- Every Abstraction has nonempty origins (F→A or A→A). -/
  abstractionHasOrigins :
    ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
      memory_origins m ≠ []
  /-- CN-6 — every non-Fact names at least one pin (origins or refs). -/
  derivedProvenance :
    ∀ m : Memory, m ∈ memories → memory_kind m ≠ .Fact →
      memory_origins m ≠ [] ∨ memory_refs m ≠ []
  /-- Every stored pin target is strictly earlier. -/
  pinTimeStrict :
    ∀ m tgt : Memory, m ∈ memories → tgt ∈ memories →
      pinFrom m tgt → memory_tick tgt < memory_tick m

/-- A retained committed store includes the graph shape and the Goal-row
    carriers whose historical targets may now be witnesses. -/
structure RetainedCommittedStoreValid
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead)
    (targets : Set ErasedPinTarget) : Prop where
  graph : RetainedMemoryGraphValid memories goals heads targets
  assignments : RetainedGoalAssignmentValid goals memories targets
  evidence : RetainedGoalEvidenceValid goals memories targets
  terminalClose : RetainedGoalTerminalCloseFactValid goals memories targets

theorem memory_graph_valid_retained_empty
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead)
    (hgraph : MemoryGraphValid memories goals heads) :
    RetainedMemoryGraphValid memories goals heads (fun _ => False) := by
  refine {
    memoryIdUnique := hgraph.memoryIdUnique
    goalIdUnique := hgraph.goalIdUnique
    erasedPinIdUnique := ?_
    headAligned := hgraph.headAligned
    forkAnchorAligned := hgraph.forkAnchorAligned
    pinTargetsExist := ?_
    motivationsValid := by
      intro g hg id hid
      exact Or.inl (hgraph.motivationsValid g hg id hid)
    guidanceValid := by
      intro m hm hp id hid
      exact Or.inl (hgraph.guidanceValid m hm hp id hid)
    originKind := ?_
    memoryGoalIdsDisjoint := hgraph.memoryGoalIdsDisjoint
    erasedMemoryDisjoint := ?_
    erasedGoalDisjoint := ?_
    abstractionHasOrigins := hgraph.abstractionHasOrigins
    derivedProvenance := hgraph.derivedProvenance
    pinTimeStrict := hgraph.pinTimeStrict
  }
  · intro e1 e2 he1 _ _
    exact False.elim he1
  · intro m hm
    refine ⟨?_, ?_, ?_, ?_⟩
    · intro id hid
      exact Or.inl ((hgraph.pinTargetsExist m hm).1 id hid)
    · intro id hid
      exact Or.inl ((hgraph.pinTargetsExist m hm).2.1 id hid)
    · intro id hid
      exact Or.inl ((hgraph.pinTargetsExist m hm).2.2.1 id hid)
    · intro id hid
      exact Or.inl ((hgraph.pinTargetsExist m hm).2.2.2 id hid)
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
  · intro e he _g _hg
    exact False.elim he

theorem memory_graph_and_goals_valid_retained_empty
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead)
    (hgraph : MemoryGraphValid memories goals heads)
    (hassignment : GoalAssignmentValid memories goals)
    (hevidence : GoalEvidenceValid goals memories)
    (hclose : GoalTerminalCloseFactValid goals memories) :
    RetainedCommittedStoreValid memories goals heads (fun _ => False) := by
  exact {
    graph := memory_graph_valid_retained_empty memories goals heads hgraph
    assignments := goal_assignment_valid_retained_empty goals memories hassignment
    evidence := goal_evidence_valid_retained_empty goals memories hevidence
    terminalClose := goal_terminal_close_valid_retained_empty goals memories hclose
  }

theorem memory_graph_origin_kind :
    ∀ memories goals heads,
      MemoryGraphValid memories goals heads →
      ∀ m : Memory, m ∈ memories → OriginKindValid memories m := by
  intro _ _ _ hgraph
  exact hgraph.originKind

theorem grounding_wf
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead)
    (hgraph : MemoryGraphValid memories goals heads) :
    WellFounded (fun lo hi => pinFromInTable memories hi lo) :=
  Subrelation.wf
    (fun {a b} h => hgraph.pinTimeStrict b a h.1 h.2.1 h.2.2)
    (invImage memory_tick Nat.lt_wfRel).wf

inductive GroundsInFact (memories : Set Memory) : Memory → Prop where
  | fact {m} : memory_kind m = .Fact → GroundsInFact memories m
  | step {m tgt} : pinFrom m tgt →
      GroundsInFact memories tgt → GroundsInFact memories m

theorem memory_grounds_in_facts
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead)
    (hgraph : MemoryGraphValid memories goals heads) :
    ∀ m : Memory, m ∈ memories → GroundsInFact memories m := by
  intro m
  refine (grounding_wf memories goals heads hgraph).induction
    (C := fun m => m ∈ memories → GroundsInFact memories m) m ?_
  intro m ih hm
  by_cases hfact : memory_kind m = .Fact
  · exact GroundsInFact.fact hfact
  · obtain ⟨id, hpin⟩ := hgraph.groundingSupport m hm hfact
    obtain ⟨hid, tgt, htgt, ht⟩ := hpin
    have hfrom : pinFrom m tgt := by
      cases hid with
      | inl ho => exact Or.inl (by rw [ht]; exact ho)
      | inr hr =>
          cases hr with
          | inl hr => exact Or.inr (Or.inl (by rw [ht]; exact hr))
          | inr hfork => exact Or.inr (Or.inr (by rw [ht]; exact hfork))
    have htable : pinFromInTable memories m tgt := ⟨hm, htgt, hfrom⟩
    exact GroundsInFact.step hfrom (ih tgt htable htgt)

theorem abstraction_has_provenance :
    ∀ memories goals heads,
      MemoryGraphValid memories goals heads →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
        memory_origins m ≠ [] := by
  intro memories goals heads hgraph m hm hk
  exact hgraph.abstractionHasOrigins m hm hk

theorem perspective_has_provenance :
    ∀ memories goals heads,
      MemoryGraphValid memories goals heads →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Perspective →
        memory_origins m ≠ [] ∨ memory_refs m ≠ [] := by
  intro memories goals heads hgraph m hm hk
  have hne : memory_kind m ≠ .Fact := by rw [hk]; intro h; exact (nomatch h)
  exact hgraph.derivedProvenance m hm hne

theorem guidance_does_not_ground
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead)
    (hgraph : MemoryGraphValid memories goals heads)
    (p : Memory) (hp : p ∈ memories)
    (hk : memory_kind p = .Perspective)
    (goal_id : GoalId) (hguided : goal_id ∈ memory_guided_by p) :
    goal_id ∉ memory_origins p := by
  intro horigin
  obtain ⟨g, hg, hgt, _⟩ :=
    hgraph.guidanceValid p hp hk goal_id hguided
  obtain ⟨source, hsource, hst⟩ :=
    (hgraph.pinTargetsExist p hp).1 goal_id horigin
  exact hgraph.memoryGoalIdsDisjoint source g hsource hg (hst.trans hgt.symm)

/-- After the Goal-reference split `refs` is the Memory spine, so a Goal `t` is never in it. It
    is a theorem now rather than a case each reader had to rule out at read
    time: the column carries the target's kind, and the two id spaces are
    disjoint, so nothing has to be probed to know this. -/
theorem a_goal_is_never_a_memory_reference
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead)
    (hgraph : MemoryGraphValid memories goals heads)
    (m : Memory) (hm : m ∈ memories) (g : Goal) (hg : g ∈ goals) :
    goal_t g ∉ memory_refs m := by
  intro hin
  obtain ⟨m', hm', ht⟩ := (hgraph.pinTargetsExist m hm).2.1 (goal_t g) hin
  exact hgraph.memoryGoalIdsDisjoint m' g hm' hg ht

/-- The other half: a Goal reference that IS declared resolves on the Goal
    spine, never on the Memory one. -/
theorem a_goal_reference_resolves_on_the_goal_spine
    (memories : Set Memory) (goals : Set Goal)
    (heads : Set MemoryHead)
    (hgraph : MemoryGraphValid memories goals heads)
    (m : Memory) (hm : m ∈ memories) (id : GoalId)
    (hin : id ∈ memory_goal_refs m) :
    goalReferenceTargetExists goals id :=
  (hgraph.pinTargetsExist m hm).2.2.2 id hin

theorem abstraction_grounds_in_facts :
    ∀ memories goals heads,
      MemoryGraphValid memories goals heads →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
        GroundsInFact memories m := by
  intro memories goals heads hgraph m hm _
  exact memory_grounds_in_facts memories goals heads hgraph m hm

end Causa
