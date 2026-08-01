/-
Causa — Provenance grounding

N1: persisted memories are grounded by a well-founded descent inside an
admitted memory graph. Acyclicity is NOT a global axiom over raw values: it
follows from the graph's table-scoped strict arrow of time.

Descent runs along the index, in either kind. A derived memory declares what
it was made from (`origin`); an interpretation Perspective declares what it is
about (`reference`) and consumes nothing (doc 16 §Kernel Invariants, E4). Both
are statements the memory makes about rows that already existed when it was
written, so both descend — and the strict-time field is what makes that
descent well-founded.

Supersession does NOT appear here. It is a lineage pointer, not a derivation:
the successor is the same thing persisting through revision, and it carries
its own origins (Causa.Memory).
-/

import Causa.Operators

namespace Causa

/-- Descent through a persisted index row. `m'` sits below `m`: `m` declared
    something about `m'`, in a row present in the admitted table. -/
def derivesFrom (edges : Set Edge) (m m' : Memory) : Prop :=
  ∃ e : Edge, e ∈ edges ∧ edge_source e = .memory m ∧ edge_target e = .memory m'

/-- The same descent relation, restricted to memories present in the admitted
    Memory table. -/
def derivesFromInTable (memories : Set Memory) (edges : Set Edge) (m m' : Memory) : Prop :=
  m ∈ memories ∧ m' ∈ memories ∧ derivesFrom edges m m'

/-- P5 / CN-6 — one admitted memory graph. Provenance is a table/store
    invariant: raw Lean `Memory` values do not have to carry provenance unless
    admitted by this bundle. -/
structure MemoryGraphValid
    (memories : Set Memory) (goals : Set Goal)
    (factEntities : Set FactEntity) (edges : Set Edge) : Prop where
  memoryIdUnique : MemoryIdUnique memories
  ftoaBatchExclusive : FtoaBatchExclusive memories
  goalIdUnique : GoalIdUnique goals
  factEntityIdUnique : FactEntityIdUnique factEntities
  factEntityNaturalKeyUnique : FactEntityNaturalKeyUnique factEntities
  memorySupersessionResolved : MemorySupersessionResolved memories
  memorySupersessionValid : MemorySupersessionValid memories
  memorySuccessorUnique : MemorySuccessorUnique memories
  factEntityHeadsPresent :
    ∀ e : FactEntity, e ∈ factEntities → e.current.memory ∈ memories
  /-- E2 + E3 + no self-loop, for every admitted row. -/
  edgeTableValid : EdgeTableValid edges
  /-- E1 — both endpoints of every admitted row resolve in the node tables. -/
  edgeEndpointsPresent : EdgeEndpointsExist memories goals factEntities edges
  /-- CN-6 — every admitted derived row declares at least one memory it rests
      on. An Abstraction declares its origins; an interpretation Perspective
      declares its subjects. A derived row that anchors to nothing in the
      memory table is not admitted, which is what keeps P5 total. -/
  derivedProvenance :
    ∀ m : Memory, m ∈ memories → memory_kind m ≠ .Fact →
      ∃ e : Edge, e ∈ edges ∧
        edge_source e = .memory m ∧
        (∃ mt : Memory, mt ∈ memories ∧ edge_target e = .memory mt)
  /-- A declared target must have existed when the declaring row was written
      (E1 at write time), so every descent step strictly decreases row time.
      This is the arrow of time N1 and W4 share. -/
  derivationTimeStrict :
    ∀ m m' : Memory, m ∈ memories → m' ∈ memories →
      derivesFrom edges m m' → memory_created_at m' < memory_created_at m

/-- Index validity is a projection from graph validity. -/
theorem memory_graph_edge_valid :
    ∀ memories goals factEntities edges,
      MemoryGraphValid memories goals factEntities edges →
      EdgeTableValid edges := by
  intro _ _ _ _ hgraph
  exact hgraph.edgeTableValid

/-- E1 is a projection from graph validity. -/
theorem memory_graph_edge_endpoints_exist :
    ∀ memories goals factEntities edges,
      MemoryGraphValid memories goals factEntities edges →
      EdgeEndpointsExist memories goals factEntities edges := by
  intro _ _ _ _ hgraph
  exact hgraph.edgeEndpointsPresent

/-- F→A batch exclusivity is part of admitted memory-graph validity. -/
theorem memory_graph_ftoa_batch_exclusive :
    ∀ memories goals factEntities edges,
      MemoryGraphValid memories goals factEntities edges →
      FtoaBatchExclusive memories := by
  intro _ _ _ _ hgraph
  exact hgraph.ftoaBatchExclusive

/-- Memory supersession validity is part of admitted memory-graph validity. -/
theorem memory_graph_supersession_valid :
    ∀ memories goals factEntities edges,
      MemoryGraphValid memories goals factEntities edges →
      MemorySupersessionValid memories := by
  intro _ _ _ _ hgraph
  exact hgraph.memorySupersessionValid

/-- N1 structural grounding: the admitted-table descent relation is well-founded
    because it strictly decreases `created_at`. -/
theorem grounding_wf
    (memories : Set Memory) (goals : Set Goal)
    (factEntities : Set FactEntity) (edges : Set Edge)
    (hgraph : MemoryGraphValid memories goals factEntities edges) :
    WellFounded (fun lo hi => derivesFromInTable memories edges hi lo) :=
  Subrelation.wf
    (fun {a b} h => hgraph.derivationTimeStrict b a h.1 h.2.1 h.2.2)
    (invImage memory_created_at Nat.lt_wfRel).wf

/-- A memory is grounded when repeated admitted descent reaches Facts. -/
inductive GroundsInFact (edges : Set Edge) : Memory → Prop where
  | fact {m} : memory_kind m = .Fact → GroundsInFact edges m
  | step {m m'} : derivesFrom edges m m' →
      GroundsInFact edges m' → GroundsInFact edges m

/-- N1 bottoms-out theorem: table-scoped well-founded descent plus
    per-derived-row persisted provenance entails Fact grounding for every
    admitted memory. -/
theorem memory_grounds_in_facts
    (memories : Set Memory) (goals : Set Goal)
    (factEntities : Set FactEntity) (edges : Set Edge)
    (hgraph : MemoryGraphValid memories goals factEntities edges) :
    ∀ m : Memory, m ∈ memories → GroundsInFact edges m := by
  intro m
  refine (grounding_wf memories goals factEntities edges hgraph).induction
    (C := fun m => m ∈ memories → GroundsInFact edges m) m ?_
  intro m ih hm
  by_cases hfact : memory_kind m = .Fact
  · exact GroundsInFact.fact hfact
  · obtain ⟨e, he, hs, ⟨mt, hmt, ht⟩⟩ := hgraph.derivedProvenance m hm hfact
    have hder : derivesFrom edges m mt := ⟨e, he, hs, ht⟩
    have htable : derivesFromInTable memories edges m mt := ⟨hm, hmt, hder⟩
    exact GroundsInFact.step hder (ih mt htable hmt)

/-- CN-6a — every admitted Abstraction rests on an admitted Fact or
    Abstraction. THEOREM: the target kind is pinned by E3, since an
    Abstraction source may not reach a Perspective. -/
theorem abstraction_has_provenance :
    ∀ memories goals factEntities edges,
      MemoryGraphValid memories goals factEntities edges →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
        ∃ e : Edge, e ∈ edges ∧
          edge_source e = .memory m ∧
          (∃ mt : Memory, mt ∈ memories ∧ edge_target e = .memory mt ∧
            (memory_kind mt = .Fact ∨ memory_kind mt = .Abstraction)) := by
  intro memories goals factEntities edges hgraph m hm hk
  have hne : memory_kind m ≠ .Fact := by rw [hk]; intro h; exact (nomatch h)
  obtain ⟨e, he, hs, ⟨mt, hmt, ht⟩⟩ := hgraph.derivedProvenance m hm hne
  have hlayer := edge_layer_rule e (hgraph.edgeTableValid e he) m mt hs ht
  rw [hk] at hlayer
  refine ⟨e, he, hs, ⟨mt, hmt, ht, ?_⟩⟩
  cases hkt : memory_kind mt with
  | Fact => exact Or.inl rfl
  | Abstraction => exact Or.inr rfl
  | Perspective =>
    rw [hkt] at hlayer
    exact absurd hlayer (by simp [MemoryKind.layer])

/-- CN-6b — every admitted Perspective rests on an admitted memory row; no
    Perspective is a view from nowhere.

    WEAKENED from the pre-v0.0.8 "…on an Abstraction": with the class matrix
    gone, P→F and P→P are ordinary legal rows, and an interpretation
    Perspective references its subjects directly whatever kind they are. What
    survives is the part that carries P5: a Perspective always names something
    in the memory table, so grounding descends from it. -/
theorem perspective_has_provenance :
    ∀ memories goals factEntities edges,
      MemoryGraphValid memories goals factEntities edges →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Perspective →
        ∃ e : Edge, e ∈ edges ∧
          edge_source e = .memory m ∧
          (∃ mt : Memory, mt ∈ memories ∧ edge_target e = .memory mt) := by
  intro memories goals factEntities edges hgraph m hm hk
  have hne : memory_kind m ≠ .Fact := by rw [hk]; intro h; exact (nomatch h)
  exact hgraph.derivedProvenance m hm hne

/-- A3 recovery: admitted Abstractions inherit the table-scoped Fact-grounding
    theorem. -/
theorem abstraction_grounds_in_facts :
    ∀ memories goals factEntities edges,
      MemoryGraphValid memories goals factEntities edges →
      ∀ m : Memory, m ∈ memories → memory_kind m = .Abstraction →
        GroundsInFact edges m := by
  intro memories goals factEntities edges hgraph m hm _
  exact memory_grounds_in_facts memories goals factEntities edges hgraph m hm

end Causa
