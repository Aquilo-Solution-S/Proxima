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
  pinTargetsExist :
    ∀ m : Memory, m ∈ memories →
      (∀ id : MemoryId, id ∈ memory_origins m → pinExists memories cooled id) ∧
      (∀ id : MemoryId, id ∈ memory_refs m → pinExists memories cooled id)
  originKind : ∀ m : Memory, m ∈ memories → OriginKindValid memories m
  /-- UML: every Abstraction is made from at least one Fact `t`. -/
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

theorem memory_graph_origin_kind :
    ∀ memories goals heads cooled,
      MemoryGraphValid memories goals heads cooled →
      ∀ m : Memory, m ∈ memories → OriginKindValid memories m := by
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
      (∃ c : Cooled, c ∈ cooled ∧ cooled_t c = id) →
      GroundsInFact memories cooled m
  | step {m tgt} : pinFrom m tgt →
      GroundsInFact memories cooled tgt → GroundsInFact memories cooled m

/-- A nonempty list has a head element. -/
theorem list_ne_nil_mem {α : Type} (xs : List α) (h : xs ≠ []) :
    ∃ x : α, x ∈ xs := by
  cases xs with
  | nil => exact absurd rfl h
  | cons a rest => exact ⟨a, List.mem_cons_self a rest⟩

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
  · have hpin := hgraph.derivedProvenance m hm hfact
    have hexists := hgraph.pinTargetsExist m hm
    cases hpin with
    | inl horig =>
      obtain ⟨id, hid⟩ := list_ne_nil_mem (memory_origins m) horig
      have htarget := hexists.1 id hid
      cases htarget with
      | inl hhot =>
        obtain ⟨tgt, htgt, ht⟩ := hhot
        have hfrom : pinFrom m tgt := Or.inl (by rw [ht]; exact hid)
        have htable : pinFromInTable memories m tgt := ⟨hm, htgt, hfrom⟩
        exact GroundsInFact.step hfrom (ih tgt htable htgt)
      | inr hcold =>
        exact GroundsInFact.cooled (Or.inl hid) hcold
    | inr hrefs =>
      obtain ⟨id, hid⟩ := list_ne_nil_mem (memory_refs m) hrefs
      have htarget := hexists.2 id hid
      cases htarget with
      | inl hhot =>
        obtain ⟨tgt, htgt, ht⟩ := hhot
        have hfrom : pinFrom m tgt := Or.inr (by rw [ht]; exact hid)
        have htable : pinFromInTable memories m tgt := ⟨hm, htgt, hfrom⟩
        exact GroundsInFact.step hfrom (ih tgt htable htgt)
      | inr hcold =>
        exact GroundsInFact.cooled (Or.inr hid) hcold

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

theorem abstraction_grounds_in_facts :
    ∀ memories goals heads cooled,
      MemoryGraphValid memories goals heads cooled →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
        GroundsInFact memories cooled m := by
  intro memories goals heads cooled hgraph m hm _
  exact memory_grounds_in_facts memories goals heads cooled hgraph m hm

end Causa
