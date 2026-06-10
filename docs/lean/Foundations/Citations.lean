/-
Proxima Foundations — Citations

Bibliographic provenance is artefact-only and Fact-only (doc 11).
The three-layer model: Facts cite; Abstractions and Perspectives
NEVER cite directly — their bibliography is the transitive closure
through provenance edges down to Facts (CI-3, and CN-6 supplies the
edges).

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
axiom cited_object_owner  : CitedObject → Owner
axiom cited_object_schema : CitedObject → SchemaRef

instance : Immutable CitedObject := ⟨⟩
instance : AppendOnly CitedObject := ⟨⟩

/-- One annotation pointing one Fact at one CitedObject, with
    location/range metadata (page, paragraph, bbox, …) typed by the
    flavor — payload opaque here. Insert-only (ST-8). -/
axiom CitationMapping : Type
axiom citation_fact   : CitationMapping → Memory
axiom citation_object : CitationMapping → CitedObject

instance : Immutable CitationMapping := ⟨⟩
instance : AppendOnly CitationMapping := ⟨⟩

/-- The Fact-side pointer (`Memory.citation_mapping_id`). -/
axiom memory_citation : Memory → Option CitationMapping

-- ============================================================
-- The Fact-only rule (doc 11 §Three-layer model)
-- ============================================================

/-- CI-1 — THE citation axiom: a memory carries a citation IFF it is
    a Fact. "NOT NULL for Fact, absent on Abstraction and
    Perspective" — both directions. -/
axiom citation_iff_fact :
  ∀ m : Memory, (memory_citation m).isSome ↔ memory_kind m = .Fact

/-- CI-2a — the pointer and the mapping agree: a Fact's citation maps
    that Fact. -/
axiom citation_points_back :
  ∀ (m : Memory) (c : CitationMapping),
    memory_citation m = some c → citation_fact c = m

/-- CI-2b — exactly one mapping per Fact (UNIQUE (memory_id)):
    one Fact ↔ one CitationMapping ↔ one CitedObject. One CitedObject
    may serve N mappings for N Facts (CI-9) — no axiom restricts the
    object side, and that absence is the spec. -/
axiom citation_unique_per_fact :
  ∀ c1 c2 : CitationMapping,
    citation_fact c1 = citation_fact c2 → c1 = c2

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
