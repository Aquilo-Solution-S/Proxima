/-
Causa — Citations

Citation is the thin core evidence anchor, not blob storage. A
`CitedObject` is an Owner-scoped external Reality artefact identity;
all storage coordinates, hashes, byte ranges, pages, message ids,
rendering, fetching, and idempotency mechanics live in flavor sidecars
or engine code.

Who may cite is Fact ∪ Abstraction (doc 16 §Computed Scores Are Abstractions,
amending doc 11 §Multiplicity). A Fact cites the source it transcribes; an
Abstraction cites the computation record that produced it — a persisted
computed score is a claim, so it is an Abstraction with its proof attached
rather than an edge property or a cache row. Citation stays OPTIONAL (since
2026-06-13) and stays 0..1 per memory.

PERSPECTIVES STILL NEVER CITE (`citation_perspective_never_cites`). An
interpretation grounds through its references, so bibliographic closure for
A/P terminates at Fact citations AND direct Abstraction citations (CI-3, with
the admitted index supplying the descent).

Minimized trusted core (D8): the subject↔mapping relation is stored ONCE —
`CitationMapping.subject` is primitive; the memory-side pointer
`memory_citation mappings` is a noncomputable DEF over the actual
mapping table via choice, and CI-1/CI-2a/CI-2c are PROVED from the
table-scoped `CitationMappingUniqueBySubject mappings` validity predicate.
Doc 11 treats the pointer and the FK as one relation kept consistent by
the engine; the kernel now encodes exactly one.

CI-12/13 — edges do not cite (no citation accessor on Edge; see
Causa.Edges). CI-14 — operator reproducibility (model id,
prompt version, wake context) is inline row metadata, not citation.
S3 storage coordinates (CI-15/16) are engine concerns — excluded.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity
import Causa.Memory

namespace Causa

-- ============================================================
-- Entities (doc 11 §Trait families)
-- ============================================================

/-- A bibliographic artefact identity: blob, image, chat session, …
    Owner-scoped, insert-only (ST-7). Content hash/idempotency and storage
    location stay outside the kernel; core keeps only identity + owner +
    schema. -/
structure CitedObject where
  id     : CitedObjectId
  owner  : Owner
  schema : SchemaRef

/-- Compatibility accessor for prose/Rust vocabulary. -/
def cited_object_id : CitedObject → CitedObjectId := CitedObject.id

/-- Compatibility accessor for prose/Rust vocabulary. -/
def cited_object_owner : CitedObject → Owner := CitedObject.owner

/-- Compatibility accessor for prose/Rust vocabulary. -/
def cited_object_schema : CitedObject → SchemaRef := CitedObject.schema

/-- Cited-object id uniqueness is a table/store invariant, not a global
    property of raw structure values. -/
def CitedObjectIdUnique (objects : Set CitedObject) : Prop :=
  ∀ c1 c2 : CitedObject,
    c1 ∈ objects →
    c2 ∈ objects →
    cited_object_id c1 = cited_object_id c2 →
    c1 = c2

instance : Immutable CitedObject := ⟨⟩
instance : AppendOnly CitedObject := ⟨⟩

/-- The citable half of the F/A/P ontology: a Fact (a lossless observed source
    atom) or an Abstraction (a computed claim citing the record that produced
    it). NOT a Perspective — an interpretation grounds through its references,
    never through a direct citation. -/
def Citable : Type := { m : Memory // memory_kind m = .Fact ∨ memory_kind m = .Abstraction }

/-- Projection from the Citable subtype back to the memory row. -/
def Citable.memory (c : Citable) : Memory := c.val

/-- Every Fact is citable. -/
def Fact.citable (f : Fact) : Citable := ⟨f.val, Or.inl f.property⟩

/-- One thin evidence link from one citable memory to one CitedObject.
    Location/range metadata (page, paragraph, bbox, …) is flavor sidecar data,
    not a kernel field. Insert-only (ST-8). -/
structure CitationMapping where
  id          : CitationMappingId
  schema      : SchemaRef
  subject     : Citable
  object      : CitedObject
  owner_match : memory_owner subject.memory = object.owner

/-- Compatibility accessor for prose/Rust vocabulary. -/
def citation_mapping_id : CitationMapping → CitationMappingId := CitationMapping.id

/-- Compatibility accessor for prose/Rust vocabulary. -/
def citation_mapping_schema : CitationMapping → SchemaRef := CitationMapping.schema

/-- The citing side as the Citable subtype. -/
def citation_subject_ref : CitationMapping → Citable := CitationMapping.subject

/-- Compatibility accessor: the citing memory projected as its Memory row. -/
def citation_subject (c : CitationMapping) : Memory := c.subject.memory

/-- Compatibility accessor for prose/Rust vocabulary. -/
def citation_object : CitationMapping → CitedObject := CitationMapping.object

/-- Citation-mapping id uniqueness is a table/store invariant, not a global
    property of raw structure values. -/
def CitationMappingIdUnique (mappings : Set CitationMapping) : Prop :=
  ∀ c1 c2 : CitationMapping,
    c1 ∈ mappings →
    c2 ∈ mappings →
    citation_mapping_id c1 = citation_mapping_id c2 →
    c1 = c2

/-- CI-2b — the actual citation-mapping table is a partial function
    `Citable ⇀ CitedObject`: at most one mapping per memory (multiplicity
    stays 0..1). This is not a global property of raw `CitationMapping`
    values; two invalid rows with the same subject can be constructed, but not
    admitted into a valid table. -/
def CitationMappingUniqueBySubject (mappings : Set CitationMapping) : Prop :=
  ∀ c1 c2 : CitationMapping,
    c1 ∈ mappings →
    c2 ∈ mappings →
    citation_subject c1 = citation_subject c2 →
    c1 = c2

instance : Immutable CitationMapping := ⟨⟩
instance : AppendOnly CitationMapping := ⟨⟩

-- ============================================================
-- The Fact ∪ Abstraction rule (doc 16 §Computed Scores Are
-- Abstractions, amending doc 11 §Three-layer model) — trusted core
-- ============================================================

/-- CI-1b — a mapping's subject IS a Fact or an Abstraction by structure. -/
theorem citation_subject_is_citable :
  ∀ c : CitationMapping,
    memory_kind (citation_subject c) = .Fact ∨
      memory_kind (citation_subject c) = .Abstraction := by
  intro c
  exact c.subject.property

/-- CI-1 — a Perspective NEVER cites directly. THEOREM: the subtype admits
    only the two lower layers, so there is no mapping row a Perspective could
    occupy. Its bibliography is the closure through its references. -/
theorem citation_perspective_never_cites :
  ∀ c : CitationMapping, memory_kind (citation_subject c) ≠ .Perspective := by
  intro c hperspective
  rcases citation_subject_is_citable c with h | h <;> rw [h] at hperspective <;>
    exact (nomatch hperspective)

-- CI-1a RETIRED 2026-06-13 — citations are OPTIONAL. A Fact may carry no
-- citation (Facts are the event stream; citations are optional
-- outside-proofs), and so may an Abstraction. The former axiom
-- `fact_has_citation` no longer holds; `memories_variant_chk` was relaxed to
-- match, and v0.0.8 widened it again to Fact ∪ Abstraction. Only the
-- citation ⇒ not-a-Perspective direction survives (CI-1).

-- ============================================================
-- The memory-side pointer — a DEF, with CI-1/2a/2c as THEOREMS
-- ============================================================

open Classical in
/-- `Memory.citation_mapping_id` — the memory-side pointer, DEFINED from the
    actual mapping table (one relation, stored once). -/
noncomputable def memory_citation
    (mappings : Set CitationMapping) (m : Memory) : Option CitationMapping :=
  if h : ∃ c : CitationMapping, c ∈ mappings ∧ citation_subject c = m
  then some h.choose
  else none

/-- CI-1 — only a Fact or an Abstraction may carry a citation. THEOREM.
    Citations are OPTIONAL, so the reverse implication does not hold. -/
theorem citation_implies_citable :
    ∀ (mappings : Set CitationMapping) (m : Memory),
      (memory_citation mappings m).isSome →
        memory_kind m = .Fact ∨ memory_kind m = .Abstraction := by
  intro mappings m h
  unfold memory_citation at h
  by_cases hex : ∃ c : CitationMapping, c ∈ mappings ∧ citation_subject c = m
  · have hspec := hex.choose_spec
    rw [← hspec.2]
    exact citation_subject_is_citable hex.choose
  · rw [dif_neg hex] at h
    exact (nomatch h)

/-- CI-1 — a Perspective carries no citation pointer at all. -/
theorem citation_pointer_never_on_perspective :
    ∀ (mappings : Set CitationMapping) (m : Memory),
      memory_kind m = .Perspective → (memory_citation mappings m).isSome → False := by
  intro mappings m hkind hsome
  rcases citation_implies_citable mappings m hsome with h | h <;>
    rw [hkind] at h <;> exact (nomatch h)

/-- CI-2a — the pointer and the mapping agree. THEOREM. -/
theorem citation_points_back :
    ∀ (mappings : Set CitationMapping) (m : Memory) (c : CitationMapping),
      memory_citation mappings m = some c → citation_subject c = m := by
  intro mappings m c h
  unfold memory_citation at h
  by_cases hex : ∃ c' : CitationMapping, c' ∈ mappings ∧ citation_subject c' = m
  · rw [dif_pos hex] at h
    have hspec := hex.choose_spec
    have hc : hex.choose = c := Option.some.inj h
    rw [← hc]
    exact hspec.2
  · rw [dif_neg hex] at h
    exact (nomatch h)

/-- CI-2a support — a returned mapping is in the mapping table. THEOREM. -/
theorem citation_points_to_row :
    ∀ (mappings : Set CitationMapping) (m : Memory) (c : CitationMapping),
      memory_citation mappings m = some c → c ∈ mappings := by
  intro mappings m c h
  unfold memory_citation at h
  by_cases hex : ∃ c' : CitationMapping, c' ∈ mappings ∧ citation_subject c' = m
  · rw [dif_pos hex] at h
    have hspec := hex.choose_spec
    have hc : hex.choose = c := Option.some.inj h
    rw [← hc]
    exact hspec.1
  · rw [dif_neg hex] at h
    exact (nomatch h)

/-- CI-2c — no orphan mappings in a valid mapping table: every row is
    reachable from its Fact's pointer. THEOREM (table-scoped uniqueness
    collapses choice onto c). -/
theorem citation_reverse_total :
    ∀ (mappings : Set CitationMapping),
      CitationMappingUniqueBySubject mappings →
      ∀ c : CitationMapping,
        c ∈ mappings → memory_citation mappings (citation_subject c) = some c := by
  intro mappings huniq c hc
  have hex : ∃ c' : CitationMapping,
      c' ∈ mappings ∧ citation_subject c' = citation_subject c := ⟨c, hc, rfl⟩
  unfold memory_citation
  rw [dif_pos hex]
  exact congrArg some (huniq hex.choose c hex.choose_spec.1 hc hex.choose_spec.2)

/-- CI-2b in its r1 name — projection theorem over a valid mapping table. -/
theorem citation_unique_per_subject :
    ∀ (mappings : Set CitationMapping),
      CitationMappingUniqueBySubject mappings →
      ∀ c1 c2 : CitationMapping,
        c1 ∈ mappings →
        c2 ∈ mappings →
        citation_subject c1 = citation_subject c2 → c1 = c2 := by
  intro mappings huniq c1 c2 hc1 hc2 hsubject
  exact huniq c1 c2 hc1 hc2 hsubject

-- ============================================================
-- Owner scoping (doc 11 §Owner scoping)
-- ============================================================

/-- CI-7/CI-8 — a mapping inherits its subject's owner. Same artefact for
    a different Owner is a separate CitedObject row (no cross-owner
    citation reuse). The match is structural, not an extra axiom. -/
theorem citation_owner_match :
  ∀ c : CitationMapping,
    memory_owner (citation_subject c) = cited_object_owner (citation_object c) := by
  intro c
  exact c.owner_match

end Causa
