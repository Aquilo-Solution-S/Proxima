# 11 — Citations

A citation answers "**what artefact in Reality does this Fact come
from?**" Three entities, two trait families. The Memory graph already
covers reasoning provenance via edges; citations cover *bibliographic*
provenance — the documents, images, videos, chat sessions, etc. that
Facts point at.

## Three-layer model

```
Fact ──► CitationMapping ──► CitedObject
         (annotation)          (artefact)

A / P    no citation_mapping_id; citations accumulate transitively
         via provenance edges (A → F, P → A → F).
```

- **CitedObject** — the artefact. Document with S3 path, Image with
  dimensions, ChatSession with platform + session id, etc.
  Typed-per-domain via `CitedObjectPayload`. Idempotent on
  `content_hash` within Owner.
- **CitationMapping** — the typed annotation pointing one Memory at one
  CitedObject (page, paragraph, bbox, message id, time range, …).
  Typed-per-domain via `CitationMappingPayload`.
- **Memory.citation_mapping_id** — `NOT NULL` for Fact, absent on
  Abstraction and Perspective. Each Fact cites one artefact via one
  mapping.

A/P never cite an artefact directly. "What grounds this Perspective?"
is `chain(p)` over `Provenance` edges → Facts → their CitationMappings
→ CitedObjects. Citations on A/P would be redundant with the
provenance graph and would invite drift between the two.

This matches the biological story: your interpretation does not cite
the source — it cites the memory that holds the source.

## Trait families

Parallel to `FactPayload` / `AbstractionPayload` / `PerspectivePayload`
(03). Compile-time only; build-time registration only — same rule as
all other payload traits (see invariant 7).

```rust
trait CitedObjectPayload: Serialize + Deserialize + 'static {
    const SCHEMA_ID:        SchemaId;        // "doc-pdf", "media-image", "chat-telegram-session", ...
    const SCHEMA_VERSION:   SchemaVersion;
    const SPECIAL_CATEGORY: bool;            // see [03 §Special-category declaration](03-schema-registry.md#special-category-declaration)
    fn sidecar_table() -> &'static str;      // "cited_<schema>_v<n>"

    /// Stable hash of the artefact content. Re-ingesting the same
    /// PDF / image / session produces the same key, deduplicating
    /// the CitedObject row within Owner.
    fn idempotency_key(&self) -> ContentHash;
}

trait CitationMappingPayload: Serialize + Deserialize + 'static {
    const SCHEMA_ID:        SchemaId;        // "doc-pdf-page-paragraph", "media-image-bbox", ...
    const SCHEMA_VERSION:   SchemaVersion;
    const SPECIAL_CATEGORY: bool;            // see [03 §Special-category declaration](03-schema-registry.md#special-category-declaration)
    fn sidecar_table() -> &'static str;      // "citation_<schema>_v<n>"

    /// Which CitedObjectPayload schema this mapping annotates.
    /// Engine validates that the linked cited_object_id resolves to
    /// a CitedObject of this schema_id.
    fn cited_object_schema() -> SchemaId;
}
```

## Tables

```
cited_objects(
    cited_object_id     pk UUIDv7,
    schema_id           NOT NULL,
    schema_version      NOT NULL,
    owner_*,
    content_hash        BLAKE3,            -- from CitedObjectPayload::idempotency_key
    created_at,
    UNIQUE (owner_principal_kind, owner_principal_id, owner_org_id,
            schema_id, content_hash)
)
-- Per-schema sidecar (one per registered CitedObjectPayload):
cited_doc_pdf_v1(cited_object_id pk FK, s3_path, sha256, mime, title?, ...)
cited_media_image_v1(cited_object_id pk FK, s3_path, sha256, dims, format, ...)
cited_chat_telegram_session_v1(cited_object_id pk FK, session_external_id, started_at, ...)

citation_mappings(
    citation_mapping_id pk UUIDv7,
    schema_id           NOT NULL,
    schema_version      NOT NULL,
    memory_id           FK memories,        -- the citing Fact
    cited_object_id     FK cited_objects,
    owner_*,
    created_at,
    UNIQUE (memory_id)                       -- one mapping per Fact (multiplicity 0..1)
)
-- Per-schema sidecar (one per registered CitationMappingPayload):
citation_doc_pdf_page_paragraph_v1(citation_mapping_id pk FK, page, paragraph, char_range, ...)
citation_media_image_bbox_v1(citation_mapping_id pk FK, bbox, caption?, ...)
citation_chat_telegram_message_v1(citation_mapping_id pk FK, message_external_id, char_range, ...)
```

## Multiplicity

- One CitedObject ↔ N CitationMappings ↔ N Facts. Re-ingesting the
  same PDF reuses the CitedObject row; new chunks add new mappings
  pointing at it.
- One Fact ↔ exactly one CitationMapping ↔ one CitedObject. A Fact
  needing to reference multiple artefacts is a modelling smell — emit
  multiple Facts, or model the relationship as Abstractions citing
  several memories.
- A/P → zero direct citations. Accumulate via provenance edges.

## Idempotency

```
CitedObject:        UNIQUE (owner, schema_id, content_hash)
CitationMapping:    UNIQUE (memory_id)
```

Re-receipt of the same observation produces the same `event_id` (per
01) and the same `content_hash` for the cited artefact; both inserts
become silent no-ops. Different chunks of the same artefact land
distinct memories with distinct mappings, all pointing at one
CitedObject.

## Owner scoping

CitedObject carries Owner. A document ingested for `User(A)` is not
visible to `User(B)`; the same PDF re-ingested for B produces a
separate CitedObject row under B's Owner. Cross-owner sharing is a
v2+ AccessGrant concern ([01](docs/01-event-source.md)); the citation layer never spans owners.

CitationMapping inherits `owner` from its Fact (and equivalently from
its CitedObject — the engine checks they match).

## Edges do not cite

`Edge` has no `citation_id`. An edge's *reasoning* is its
`authored_by`:

| `authored_by`              | Meaning                                                 |
|---------------------------|---------------------------------------------------------|
| `EventSource(SourceId)`   | payload-encoded structural fact; relation *is* the data |
| `OperatorFtoA(MemoryId)`  | reasoned by the source Abstraction                      |
| `OperatorAtoP(MemoryId)`  | reasoned by the source Perspective                      |
| `PerspectiveLink(MemoryId)` | reasoned by the source Perspective                    |
| engine `Supersedes`        | engine-authored on re-derivation                        |

Anything you'd want a "citation" to express on an edge is already
encoded by the authoring memory's own citation chain.

## Operator-invocation provenance lives on the Memory row

Bibliographic citation is artefact-only. The reproducibility metadata
for an operator-derived memory — `(operator_kind, model_id,
prompt_version, personality_state_hash)` — are inline columns
on `memories`, NULL for Facts and NOT NULL for A/P. There is no
separate `citations` table for them; the F→A / A→P invocation key
(see [04 §Idempotence](docs/04-consolidation.md#idempotence-and-reproducibility)) is built from those columns directly.

## What this does not include

- **Renderers.** No `render()` on `CitedObjectPayload` or
  `CitationMappingPayload`. The artefact is the artefact (binary blob
  in S3 / file system); UI fetches it via the typed sidecar's path
  fields.
- **Access control on artefacts.** Owner is the only scope. Per-asset
  ACLs (e.g., one document shared with N users) are a v2+ extension
  layered above Owner.
- **Versioned artefacts.** A new revision of a PDF with a different
  `sha256` is a new CitedObject. Linking revisions across CitedObjects
  is a flavor concern, modelled via Abstractions or domain-specific
  edges if needed.

## Anchors

- `three-layer-model`
- `trait-families`
- `tables`
- `multiplicity`
- `idempotency`
- `owner-scoping`
- `edges-do-not-cite`
- `operator-invocation-provenance-lives-on-the-memory-row`
- `what-this-does-not-include`
