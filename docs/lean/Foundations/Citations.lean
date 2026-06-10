/-
Proxima Foundations — Citations

Bibliographic provenance is artefact-only and Fact-only (doc 11).
The three-layer model: Facts cite; Abstractions and Perspectives
NEVER cite directly — their bibliography is the transitive closure
through provenance edges down to Facts (CI-3, and CN-6 supplies the
edges).

Minimized trusted core (2026-06-11): the Fact↔mapping relation is
stored ONCE — `citation_fact` (the mapping-side FK) is primitive;
the Fact-side pointer `memory_citation` is a noncomputable DEF via
choice, and CI-1/CI-2a/CI-2c are PROVED. Doc 11 treats the pointer
and the FK as one relation kept consistent by the engine; the kernel
now encodes exactly one.

CI-12/13 — edges do not cite (no citation accessor on Edge; see
Foundations.Edges). CI-14 — operator reproducibility (model id,
prompt version, personality) is inline row metadata, not citation.
S3 storage coordinates (CI-15/16) are engine concerns — excluded.
-/

import Foundations.Prelude
import Foundations.Owner
import Foundations.Identity
import Foundations.Memory

namespace Proxima

-- ============================================================
-- Entities (doc 11 §Trait families)
-- ============================================================

/-- A bibliographic artefact: blob, image, chat session, … Owner-
    scoped, content-hash idempotent within Owner (CI-4: UNIQUE
    (owner, schema_id, content_hash) — hash stays engine-level; the
    kernel keeps identity + owner + schema). Insert-only (ST-7). -/
axiom CitedObject : Type
axiom cited_object_id     : CitedObject → CitedObjectId
axiom cited_object_owner  : CitedObject → Owner
axiom cited_object_schema : CitedObject → SchemaRef

axiom cited_object_id_injective :
  ∀ c1 c2 : CitedObject, cited_object_id c1 = cited_object_id c2 → c1 = c2

instance : Immutable CitedObject := ⟨⟩
instance : AppendOnly CitedObject := ⟨⟩

/-- One annotation pointing one Fact at one CitedObject, with
    location/range metadata (page, paragraph, bbox, …) typed by the
    flavor — payload opaque here. Insert-only (ST-8). -/
axiom CitationMapping : Type
axiom citation_mapping_id     : CitationMapping → CitationMappingId
axiom citation_mapping_schema : CitationMapping → SchemaRef
axiom citation_fact   : CitationMapping → Memory
axiom citation_object : CitationMapping → CitedObject

axiom citation_mapping_id_injective :
  ∀ c1 c2 : CitationMapping,
    citation_mapping_id c1 = citation_mapping_id c2 → c1 = c2

instance : Immutable CitationMapping := ⟨⟩
instance : AppendOnly CitationMapping := ⟨⟩

-- ============================================================
-- The Fact-only rule (doc 11 §Three-layer model) — trusted core
-- ============================================================

/-- CI-1b — a mapping's target IS a Fact: no mapping may point at an
    Abstraction or Perspective (doc 11 §Three-layer model). -/
axiom citation_fact_is_fact :
  ∀ c : CitationMapping, memory_kind (citation_fact c) = .Fact

/-- CI-1a — every Fact has a mapping ("NOT NULL for Fact"). -/
axiom fact_has_citation :
  ∀ m : Memory, memory_kind m = .Fact →
    ∃ c : CitationMapping, citation_fact c = m

/-- CI-2b — at most one mapping per Fact (UNIQUE (memory_id)):
    one Fact ↔ one CitationMapping ↔ one CitedObject. One CitedObject
    may serve N mappings for N Facts (CI-9) — nothing restricts the
    object side, and that absence is the spec. -/
axiom citation_fact_injective :
  ∀ c1 c2 : CitationMapping,
    citation_fact c1 = citation_fact c2 → c1 = c2

-- ============================================================
-- The Fact-side pointer — a DEF, with CI-1/2a/2c as THEOREMS
-- ============================================================

open Classical in
/-- `Memory.citation_mapping_id` — the Fact-side pointer, DEFINED
    from the mapping-side relation (one relation, stored once). -/
noncomputable def memory_citation (m : Memory) : Option CitationMapping :=
  if h : ∃ c : CitationMapping, citation_fact c = m
  then some h.choose
  else none

/-- CI-1 — a memory carries a citation IFF it is a Fact. THEOREM. -/
theorem citation_iff_fact :
    ∀ m : Memory, (memory_citation m).isSome ↔ memory_kind m = .Fact := by
  intro m
  constructor
  · intro h
    unfold memory_citation at h
    by_cases hex : ∃ c : CitationMapping, citation_fact c = m
    · obtain ⟨c, hc⟩ := hex
      rw [← hc]
      exact citation_fact_is_fact c
    · rw [dif_neg hex] at h
      exact (nomatch h)
  · intro h
    have hex := fact_has_citation m h
    unfold memory_citation
    rw [dif_pos hex]
    rfl

/-- CI-2a — the pointer and the mapping agree. THEOREM. -/
theorem citation_points_back :
    ∀ (m : Memory) (c : CitationMapping),
      memory_citation m = some c → citation_fact c = m := by
  intro m c h
  unfold memory_citation at h
  by_cases hex : ∃ c' : CitationMapping, citation_fact c' = m
  · rw [dif_pos hex] at h
    have := hex.choose_spec
    rw [Option.some.inj h] at this
    exact this
  · rw [dif_neg hex] at h
    exact (nomatch h)

/-- CI-2c — no orphan mappings: every mapping is reachable from its
    Fact's pointer. THEOREM (uniqueness collapses choice onto c). -/
theorem citation_reverse_total :
    ∀ c : CitationMapping, memory_citation (citation_fact c) = some c := by
  intro c
  have hex : ∃ c' : CitationMapping, citation_fact c' = citation_fact c := ⟨c, rfl⟩
  unfold memory_citation
  rw [dif_pos hex]
  exact congrArg some (citation_fact_injective _ _ hex.choose_spec)

/-- CI-2b in its r1 name — alias theorem. -/
theorem citation_unique_per_fact :
    ∀ c1 c2 : CitationMapping,
      citation_fact c1 = citation_fact c2 → c1 = c2 :=
  citation_fact_injective

-- ============================================================
-- Owner scoping (doc 11 §Owner scoping)
-- ============================================================

/-- CI-7/CI-8 — a mapping inherits its Fact's owner and the engine
    checks Fact owner = CitedObject owner. Same artefact for a
    different Owner is a separate CitedObject row (no cross-owner
    citation reuse). -/
axiom citation_owner_match :
  ∀ c : CitationMapping,
    memory_owner (citation_fact c) = cited_object_owner (citation_object c)

end Proxima
