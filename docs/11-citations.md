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

- **CitedObject** — the artefact. Uploaded blob with internal S3
  `bucket + object_key`, Image with dimensions, ChatSession with
  platform + session id, etc.
  Typed-per-domain via `CitedObjectPayload`. Idempotent on
  `content_hash` within Owner.
- **CitationMapping** — the typed annotation pointing one Memory at one
  CitedObject (page, paragraph, bbox, message id, time range, …).
  Typed-per-domain via `CitationMappingPayload`.
- **Memory.citation_mapping_id** — OPTIONAL for Fact (a Fact may cite or
  not, as of 2026-06-13 — Facts are the event stream; citations are
  optional outside-proofs), absent on Abstraction and Perspective. A cited
  Fact cites one artefact via one mapping.

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
    UNIQUE (owner_principal_kind, owner_principal_id,
            schema_id, content_hash)
    -- owner_org_id is a billing annotation, deliberately NOT in the
    -- dedup key (doc 01 §Owner, renegotiated 2026-06-11)
)
-- Per-schema sidecar (one per registered CitedObjectPayload):
cited_uploaded_blob_v1(cited_object_id pk FK, bucket, object_key, sha256, byte_len, mime, filename, etag, uploaded_at)
cited_media_image_v1(cited_object_id pk FK, bucket, object_key, sha256, dims, format, ...)
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

## Large artefact storage

Large original artefacts live in S3. Postgres stores Owner, schema,
`content_hash`, citation metadata, and opaque storage coordinates.
Clients never receive `bucket` or `object_key`; command surfaces return
presigned URLs.

Generic uploaded blob:

| Field | Location | Contract |
|---|---|---|
| `content_hash` | `cited_objects` | BLAKE3-32 of original bytes; Owner-scoped idempotency key |
| `sha256`, `byte_len`, `mime`, `filename`, `etag`, `uploaded_at` | `cited_uploaded_blob_v1` | typed sidecar metadata |
| `bucket`, `object_key` | `cited_uploaded_blob_v1` | internal storage coordinates only |

Direct upload:

1. `prepare` inserts a pending upload and returns a presigned S3 `PUT`.
2. Client uploads bytes directly to `pending/<owner-hash>/<upload-id>`.
3. `complete` verifies the pending object, streams bytes to compute
   BLAKE3 + SHA-256, copies to
   `objects/<owner-hash>/proxima-core/uploaded-blob-v1/<blake3-hex>`,
   deletes the pending object, inserts or reuses `cited_objects`,
   inserts `cited_uploaded_blob_v1`, marks upload completed.
4. Same Owner + same bytes returns the existing CitedObject and marks
   the result as an idempotent replay.
5. `abort` deletes the pending object when present and marks the
   upload aborted.

S3 preserves original bytes only. It does not replace
`CitationMapping`, Fact-only citations, or provenance edges.

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
prompt_version, personality_id)` — are inline columns
on `memories`, NULL for Facts and NOT NULL for A/P. There is no
separate `citations` table for them; the F→A / A→P invocation key
(see [04 §Idempotence and reproducibility](04-consolidation.md#idempotence-and-reproducibility)) is built from those columns directly.

## What this does not include

- **Renderers.** No `render()` on `CitedObjectPayload` or
  `CitationMappingPayload`. The artefact is the artefact; UI fetches
  large binaries via presigned read URLs, not raw storage coordinates.
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
- `large-artefact-storage`
- `owner-scoping`
- `edges-do-not-cite`
- `operator-invocation-provenance-lives-on-the-memory-row`
- `what-this-does-not-include`
