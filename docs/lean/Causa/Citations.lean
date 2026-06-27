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

Minimized trusted core (D5): the Fact↔mapping relation is stored ONCE —
`CitationMapping.fact` is primitive; the Fact-side pointer
`memory_citation` is a noncomputable DEF via choice, and
CI-1/CI-2a/CI-2c are PROVED. Doc 11 treats the pointer and the FK as
one relation kept consistent by the engine; the kernel now encodes
exactly one.

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

/-- CI-1 — only a Fact may carry a citation (citation ⇒ Fact). THEOREM.
    Citations are OPTIONAL on Facts as of 2026-06-13, so the reverse
    implication (Fact ⇒ has citation) no longer holds; this weakened from
    an `↔` (which relied on the retired `fact_has_citation`) to a `→`. -/
theorem citation_implies_fact :
    ∀ m : Memory, (memory_citation m).isSome → memory_kind m = .Fact := by
  intro m h
  unfold memory_citation at h
  by_cases hex : ∃ c : CitationMapping, citation_fact c = m
  · obtain ⟨c, hc⟩ := hex
    rw [← hc]
    exact citation_fact_is_fact c
  · rw [dif_neg hex] at h
    exact (nomatch h)

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

/-- CI-7/CI-8 — a mapping inherits its Fact's owner. Same artefact for
    a different Owner is a separate CitedObject row (no cross-owner
    citation reuse). The match is structural, not an extra axiom. -/
theorem citation_owner_match :
  ∀ c : CitationMapping,
    memory_owner (citation_fact c) = cited_object_owner (citation_object c) := by
  intro c
  exact c.owner_match

end Causa
