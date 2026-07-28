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
    const SCHEMA_ID:        &'static str;    // "doc-pdf", "media-image", "chat-telegram-session", ...
    const SCHEMA_VERSION:   u32;
    const SPECIAL_CATEGORY: bool;            // see [03 §Special-category declaration](03-schema-registry.md#special-category-declaration)
    fn sidecar_table() -> &'static str;      // "cited_<schema>_v<n>"

    /// Stable hash of the artefact content. Re-ingesting the same
    /// PDF / image / session produces the same key, deduplicating
    /// the CitedObject row within Owner.
    fn idempotency_key(&self) -> ContentHash;
}

trait CitationMappingPayload: Serialize + Deserialize + 'static {
    const SCHEMA_ID:        &'static str;    // "doc-pdf-page-paragraph", "media-image-bbox", ...
    const SCHEMA_VERSION:   u32;
    const SPECIAL_CATEGORY: bool;            // see [03 §Special-category declaration](03-schema-registry.md#special-category-declaration)
    fn sidecar_table() -> Option<&'static str>; // None for pure-link mappings

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
    UNIQUE (owner_kind, owner_id,
            schema_id, content_hash)
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
-- Optional per-schema sidecar (only when CitationMappingPayload::sidecar_table() = Some):
citation_doc_pdf_page_paragraph_v1(citation_mapping_id pk FK, page, paragraph, char_range, ...)
citation_media_image_bbox_v1(citation_mapping_id pk FK, bbox, caption?, ...)
citation_chat_telegram_message_v1(citation_mapping_id pk FK, message_external_id, char_range, ...)
```

## Multiplicity

- One CitedObject ↔ N CitationMappings ↔ N Facts. Re-ingesting the
  same PDF reuses the CitedObject row; new chunks add new mappings
  pointing at it.
- One cited Fact ↔ exactly one CitationMapping ↔ one CitedObject. A Fact
  needing to reference multiple artefacts is a modelling smell — emit
  multiple Facts, or model the relationship as Abstractions citing
  several memories.
- A/P → zero direct citations. Accumulate via provenance edges.

## Idempotency

```
CitedObject:        UNIQUE (owner, schema_id, content_hash)
CitationMapping:    UNIQUE (memory_id)
```

Re-receipt of the same observation produces the same source receipt id
(public `receipt_id`, storage `receipt_id`; see 01) and the same
`content_hash` for the cited artefact; both inserts become silent
no-ops. Different chunks of the same artefact land distinct memories
with distinct mappings, all pointing at one CitedObject.

<a id="core-registered-schemas"></a>
## Core-registered schemas

Core registers the artefact vocabulary that is not domain-specific.
Everything else is a flavor's.

| Schema | Kind | Contract |
|---|---|---|
| `core/uploaded-blob-v1` | CitedObject | An uploaded artefact; see §Large artefact storage |
| `core/uploaded-blob-whole-v1` | CitationMapping | The whole artefact. Pure link, no sidecar |
| `core/uploaded-blob-page-span-v1` | CitationMapping | A page range inside it, optionally a character range |
| `core/mcp-call-io` + `core/mcp-call-citation` | CitedObject + CitationMapping | Substrate-internal MCP call logging |

Page numbers are **one-based and inclusive at both ends** — a single page
is `page_from == page_to`. That is how a page is cited in prose and how it
is printed on the page; zero-based would make "page 1" mean the second
page in every citation read back by a human. `char_range_start` /
`char_range_end` are optional, must be present together, and are relative
to the span's text rather than the document's, so a mapping survives
re-extraction as long as the pages did not move.

A page span is core, not per-domain, because the artefact it locates into
already is: a page range in an uploaded document says nothing about what
kind of document it is, exactly as `uploaded-blob-v1` says nothing about
what the bytes mean. What stays out of core is anything needing a
coordinate-system contract — a region on a page has to agree with whoever
produced the box about pixels, points, or fractions of the page, and that
agreement belongs with the producer. Flavors register their own
`CitationMappingPayload` for it, targeting `core/uploaded-blob-v1` or their
own cited object.

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
   `objects/<owner-hash>/core/uploaded-blob-v1/<blake3-hex>`,
   deletes the pending object, inserts or reuses `cited_objects`,
   inserts `cited_uploaded_blob_v1`, marks upload completed.
4. Same Owner + same bytes returns the existing CitedObject and marks
   the result as an idempotent replay.
5. `abort` deletes the pending object when present and marks the
   upload aborted.

S3 preserves original bytes only. It does not replace
`CitationMapping`, Fact-only citations, or provenance edges.

<a id="mcp-surface"></a>
## MCP surface

The upload lane above is reachable over MCP through the `core_upload`
dispatcher (actions `prepare` / `complete` / `abort` / `read_url`).
Artefact bytes never travel through a tool call — the MCP transport caps
request bodies, and the presigned-URL policy above is the transfer path:

1. `core_upload` `prepare` (`filename`, `mime`, `byte_len`) → pending
   upload + presigned `upload_url` with its required `headers`.
2. The client HTTP `PUT`s the raw bytes to `upload_url` with exactly
   those headers, before `expires_at`.
3. `core_upload` `complete` (`upload_id`) verifies the bytes and returns
   the canonical `cited_object_id` (plus hex `content_hash`/`sha256`,
   `byte_len`, `mime`, `filename`; replays report `idempotent_replay`).
4. A Fact cites the object via `core_remember`'s
   `citation.cited_object_id` (by reference; `C:` prefix accepted) with a
   registered mapping such as `core/uploaded-blob-whole-v1` or
   `core/uploaded-blob-page-span-v1`. This is deliberately the only way
   an MCP client can cite its own upload: `complete` never returns
   `bucket`/`object_key`, and the inline `object_payload` path requires
   them. By-ref and the inline `object_*` fields are mutually exclusive;
   storage verifies the referenced object exists for the Fact's owner and
   carries the schema the mapping targets, in the same transaction that
   writes the mapping.
5. `core_upload` `read_url` (`cited_object_id`) mints a presigned
   download URL later.

All four actions resolve a `space` key exactly like `core_remember` and
re-authorize against the resolved owner. The tool is served by the
host-wired cited-blob service; a host without `PROXIMA_S3_*` configured
fails typed at call time with the enabling configuration in the message
(docs/10 §Large Artefact S3).

Citation read-back (`core_fact` `citation_of_fact` /
`citation_of_entity_head`) returns the locator alongside the ids: the
`core/uploaded-blob-page-span-v1` payload as `page_span` when the
mapping carries one, and — when the cited object is an uploaded blob —
a `document` block with `filename`/`mime`/`byte_len`/`sha256_hex`/
`uploaded_at`. What the document IS, never where it lives:
`bucket`/`object_key` stay internal, and fetching bytes goes through
`read_url`.

## Owner scoping

CitedObject carries Owner. A document ingested for `User(A)` is not
visible to `User(B)`; the same PDF re-ingested for B produces a
separate CitedObject row under B's Owner. Cross-owner access is resolved through group membership / `OwnerRoles`; the citation layer never spans owners.

CitationMapping inherits `owner` from its Fact (and equivalently from
its CitedObject — the engine checks they match).

## Edges do not cite

`Edge` has no `citation_id`. An edge's authorship class is the closed
`authorship_kind` vocabulary (`SourceIngest`, `OperatorFtoA`, `OperatorAtoA`,
`OperatorAtoP`, `OperatorAtoGoal`, `PerspectiveLink`, `PerspectiveGoalLink`,
`User`, `Engine`, `ExternalAgent`). Memory/operator provenance lives in
`authorship_owner_memory_id`, relation descriptors, and operator invocation
metadata; enum variants do not carry payload IDs.

Anything you'd want a "citation" to express on an edge is already
encoded by the authoring memory's own citation chain.

## Operator-invocation provenance lives on the Memory row

Bibliographic citation is artefact-only. The reproducibility metadata
for an operator-derived memory — `(operator_kind, model_id,
prompt_version)` plus edge-backed input/context provenance — are inline columns
and relations, NULL for Facts where not applicable and present for A/P. There is no
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
- `core-registered-schemas`
- `trait-families`
- `tables`
- `multiplicity`
- `idempotency`
- `large-artefact-storage`
- `mcp-surface`
- `owner-scoping`
- `edges-do-not-cite`
- `operator-invocation-provenance-lives-on-the-memory-row`
- `what-this-does-not-include`
