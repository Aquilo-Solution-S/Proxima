/-
Causa — Citations

Citation is the thin core evidence anchor, not blob storage. A
`CitedObject` is an Owner-scoped external Reality artefact identity;
all storage coordinates, hashes, byte ranges, pages, message ids,
rendering, fetching, and idempotency mechanics live in flavor sidecars
or engine code.

The three-layer model remains Fact-only: only Facts may cite
(OPTIONAL as of 2026-06-13). Here "Fact" means lossless observed source
atom/transcription, not interpretation. Abstractions and Perspectives
NEVER cite directly — their bibliography is the transitive closure
through provenance edges down to cited Facts (CI-3, and CN-6 supplies
the edges).

Minimized trusted core (D8): the Fact↔mapping relation is stored ONCE —
`CitationMapping.fact` is primitive; the Fact-side pointer
`memory_citation mappings` is a noncomputable DEF over the actual
mapping table via choice, and CI-1/CI-2a/CI-2c are PROVED from the
table-scoped `CitationMappingUniqueByFact mappings` validity predicate.
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

/-- One thin evidence link from one lossless observed Fact to one
    CitedObject. Location/range metadata (page, paragraph, bbox, …) is
    flavor sidecar data, not a kernel field. Insert-only (ST-8). -/
structure CitationMapping where
  id          : CitationMappingId
  schema      : SchemaRef
  fact        : Fact
  object      : CitedObject
  owner_match : memory_owner fact.memory = object.owner

/-- Compatibility accessor for prose/Rust vocabulary. -/
def citation_mapping_id : CitationMapping → CitationMappingId := CitationMapping.id

/-- Compatibility accessor for prose/Rust vocabulary. -/
def citation_mapping_schema : CitationMapping → SchemaRef := CitationMapping.schema

/-- The Fact-side target as the Fact subtype. -/
def citation_fact_ref : CitationMapping → Fact := CitationMapping.fact

/-- Compatibility accessor: the cited Fact projected as its Memory row. -/
def citation_fact (c : CitationMapping) : Memory := c.fact.memory

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
    `Fact ⇀ CitedObject`: at most one mapping per Fact. This is not a
    global property of raw `CitationMapping` values; two invalid rows with
    the same Fact can be constructed, but not admitted into a valid table. -/
def CitationMappingUniqueByFact (mappings : Set CitationMapping) : Prop :=
  ∀ c1 c2 : CitationMapping,
    c1 ∈ mappings →
    c2 ∈ mappings →
    citation_fact c1 = citation_fact c2 →
    c1 = c2

instance : Immutable CitationMapping := ⟨⟩
instance : AppendOnly CitationMapping := ⟨⟩

-- ============================================================
-- The Fact-only rule (doc 11 §Three-layer model) — trusted core
-- ============================================================

/-- CI-1b — a mapping's target IS a Fact by structure: no mapping may
    point at an Abstraction or Perspective (doc 11 §Three-layer model). -/
theorem citation_fact_is_fact :
  ∀ c : CitationMapping, memory_kind (citation_fact c) = .Fact := by
  intro c
  exact fact_memory_kind c.fact

-- CI-1a RETIRED 2026-06-13 — citations are OPTIONAL on Facts. A Fact may
-- carry no citation (Facts are the event stream; citations are optional
-- outside-proofs). The former axiom `fact_has_citation` ("every Fact has a
-- mapping / NOT NULL for Fact") no longer holds; `memories_variant_chk` was
-- relaxed to match. Only the citation ⇒ Fact direction survives (CI-1).

-- ============================================================
-- The Fact-side pointer — a DEF, with CI-1/2a/2c as THEOREMS
-- ============================================================

open Classical in
/-- `Memory.citation_mapping_id` — the Fact-side pointer, DEFINED from the
    actual mapping table (one relation, stored once). -/
noncomputable def memory_citation
    (mappings : Set CitationMapping) (m : Memory) : Option CitationMapping :=
  if h : ∃ c : CitationMapping, c ∈ mappings ∧ citation_fact c = m
  then some h.choose
  else none

/-- CI-1 — only a Fact may carry a citation (citation ⇒ Fact). THEOREM.
    Citations are OPTIONAL on Facts as of 2026-06-13, so the reverse
    implication (Fact ⇒ has citation) no longer holds; this weakened from
    an `↔` (which relied on the retired `fact_has_citation`) to a `→`. -/
theorem citation_implies_fact :
    ∀ (mappings : Set CitationMapping) (m : Memory),
      (memory_citation mappings m).isSome → memory_kind m = .Fact := by
  intro mappings m h
  unfold memory_citation at h
  by_cases hex : ∃ c : CitationMapping, c ∈ mappings ∧ citation_fact c = m
  · have hspec := hex.choose_spec
    rw [← hspec.2]
    exact citation_fact_is_fact hex.choose
  · rw [dif_neg hex] at h
    exact (nomatch h)

/-- CI-2a — the pointer and the mapping agree. THEOREM. -/
theorem citation_points_back :
    ∀ (mappings : Set CitationMapping) (m : Memory) (c : CitationMapping),
      memory_citation mappings m = some c → citation_fact c = m := by
  intro mappings m c h
  unfold memory_citation at h
  by_cases hex : ∃ c' : CitationMapping, c' ∈ mappings ∧ citation_fact c' = m
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
  by_cases hex : ∃ c' : CitationMapping, c' ∈ mappings ∧ citation_fact c' = m
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
      CitationMappingUniqueByFact mappings →
      ∀ c : CitationMapping,
        c ∈ mappings → memory_citation mappings (citation_fact c) = some c := by
  intro mappings huniq c hc
  have hex : ∃ c' : CitationMapping,
      c' ∈ mappings ∧ citation_fact c' = citation_fact c := ⟨c, hc, rfl⟩
  unfold memory_citation
  rw [dif_pos hex]
  exact congrArg some (huniq hex.choose c hex.choose_spec.1 hc hex.choose_spec.2)

/-- CI-2b in its r1 name — projection theorem over a valid mapping table. -/
theorem citation_unique_per_fact :
    ∀ (mappings : Set CitationMapping),
      CitationMappingUniqueByFact mappings →
      ∀ c1 c2 : CitationMapping,
        c1 ∈ mappings →
        c2 ∈ mappings →
        citation_fact c1 = citation_fact c2 → c1 = c2 := by
  intro mappings huniq c1 c2 hc1 hc2 hfact
  exact huniq c1 c2 hc1 hc2 hfact

-- ============================================================
-- Owner scoping (doc 11 §Owner scoping)
-- ============================================================

/-- CI-7/CI-8 — a mapping inherits its Fact's owner. Same artefact for
    a different Owner is a separate CitedObject row (no cross-owner
    citation reuse). The match is structural, not an extra axiom. -/
theorem citation_owner_match :
  ∀ c : CitationMapping,
    memory_owner (citation_fact c) = cited_object_owner (citation_object c) := by
  intro c
  exact c.owner_match

end Causa
