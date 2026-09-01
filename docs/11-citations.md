# 11 — Citations

A citation answers "**what artefact in Reality does this memory come
from?**" Three entities, two trait families. The Memory graph already
covers reasoning provenance via `origin` and `reference` entries; citations
cover *bibliographic* provenance — the documents, images, videos, chat
sessions, computation records, etc. that a Fact or an Abstraction points at.

## Three-layer model

```
Fact        ──► blob_id 0..1 ──► blob
Abstraction ──► blob_id 0..1 ──► blob

Perspective     no blob_id; citations accumulate transitively
                through origins/refs (P → A → F).
```

- **blob** — the artefact (`proxima_core.blob`). Content-addressed per
  owner (`owner_id, schema_id, content_hash`).
- **CitationMapping** — optional sidecar when the citation carries
  schema-specific metadata (page, bbox, …). A fieldless link needs no
  sidecar.
- **`memory.blob_id`** — OPTIONAL for **Fact and Abstraction**,
  forbidden on Perspective.

An Abstraction may cite because a **computed score is an Abstraction**
(see [16 §Computed Scores Are Abstractions](16-edges.md#computed-scores-are-abstractions)):
a similarity, ranking, or quality verdict about other nodes holds the value
and the method in its payload, points at its inputs with references, and
proves itself by citing the computation record — parameters, model id,
receipt — as a content-addressed CitedObject. Without that, the proof of an
algorithmic verdict had nowhere to live but an edge property or a cache row,
and neither is a claim anything can check.

Perspectives never cite directly. Grounding is `chain(p)` over `refs`
and `origins` → Facts and Abstractions → their `blob_id`.

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
blob(
    blob_id        pk uuidv7,
    owner_id       FK owners,
    schema_id      text,
    content_hash   bytea,            -- BLAKE3-32
    UNIQUE (owner_id, schema_id, content_hash)
)

memory.blob_id     0..1 FK blob     -- F/A only
```

Optional mapping sidecar keyed by `t` when the citation carries extra
columns (page, bbox). A whole-artefact link needs no sidecar.

## Multiplicity

- One `blob` ↔ N memories (`blob_id`). Same owner + schema + hash reuses
  the blob.
- One memory ↔ 0..1 `blob_id`. Multiple artefacts → multiple Facts, or
  an Abstraction whose `refs` name those Facts.
- Perspective → zero `blob_id`; closure walks `origins`/`refs`.

## Idempotency

```
blob: UNIQUE (owner_id, schema_id, content_hash)
```

<a id="core-registered-schemas"></a>
## Core-registered schemas

Core registers the artefact vocabulary that is not domain-specific.
Everything else is a flavor's.

| Schema | Kind | Contract |
|---|---|---|
| `core/uploaded-blob-v1` | CitedObject | An uploaded artefact; see §Large artefact storage |
| `core/uploaded-blob-whole-v1` | CitationMapping | The whole artefact. Pure link, no sidecar |
| `core/uploaded-blob-page-span-v1` | CitationMapping | A page range inside it, optionally a character range |
| `core/upload-v1` | Fact | That an artefact arrived, citing it through `core/uploaded-blob-whole-v1`. Minted by `complete`; no sidecar of its own |
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
| `content_hash` | `blob` | BLAKE3-32; Owner-scoped unique with schema |
| upload metadata | `blob_uploads` | pending → completed / aborted |

Direct upload:

1. `prepare` inserts a pending upload and returns a presigned S3 `PUT`.
2. Client uploads bytes directly to `pending/<upload-id>`.
3. `complete` performs one bounded read of the row-selected object to
   compute BLAKE3 + SHA-256 and its length, conditionally publishes those
   exact bytes to `objects/<upload-id>`, records the canonical locator and
   SHA-256 audit digest, and carries the BLAKE3 content address into the corpus
   transaction. It retires the pending transfer copy on a best-effort basis —
   a provider failure there never fails a completion that already succeeded, and
   the bucket lifecycle rule reclaims the leftovers — then inserts or reuses
   `blob` and marks the upload completed.
4. Same Owner + same bytes returns the existing CitedObject and marks
   the result as an idempotent replay.
5. `abort` purges every version of the pending transfer key when present and
   marks the upload aborted. It retains a canonical object because a finish
   may have committed the corpus before an abort observes the row.

Keys carry no owner. `upload_id` is the server-minted primary key of
the minting `blob_uploads` row, so that row's canonical key is unique;
mounted rows may intentionally share the source row's canonical key. An
owner transfer is an `owner_id` update on `blob` and `blob_uploads` and
performs no object-store work at all.

The upload lane is the only writer the presigner trusts. An inline
`core/uploaded-blob-v1` citation payload is a caller-asserted locator:
the substrate stores its `bucket`/`object_key` verbatim and never
verifies they point at anything. Owning the row is therefore not
enough — a forged row is owned by the forger. `read_url` serves a
locator only when it is the configured bucket and the key this store
would mint for THAT row's own `upload_id`, and answers any other row
exactly like a missing object. There is exactly one key scheme and no
legacy branch: objects written by an earlier scheme are not readable.

S3 preserves original bytes only. It does not replace
`CitationMapping`, the 0..1 citation on a memory, or `origin` provenance.

<a id="the-upload-fact"></a>
## The upload Fact

A CitedObject is a thing that can be cited. It is not an event: it holds
no receipt, appears in no change history, and says nothing about who put
it there or when. So a corpus built only from `complete` could hold a
file that nothing in the substrate records the arrival of — and did,
until flavors wrote that record themselves, which made "a file entered
the corpus" true only in the flavors that bothered.

`complete` therefore also mints a `core/upload-v1` Fact citing the
artefact through `core/uploaded-blob-whole-v1`. Its rendered text names
the file, so an upload is findable by name through
`core/search_memories` with nobody having written a Fact for it.

**Findable, not embedded.** `core/upload-v1` declares
`EmbeddingRecipe::Never`, so the Fact keeps its text — and with
it lexical search on the filename, which is the whole point of rendering
one — and is never queued for a vector. A filename is worth finding and
not worth embedding: every upload renders from the same template and
differs only in a name, a mime and an integer, so their vectors would be
mutual near-neighbours crowding the index that real prose lives in. This
matters at scale because an upload is not necessarily a document: a
flavor that stores page scans and figure crops as their own artefacts
produces tens of thousands of these Facts per corpus.

The exclusion holds on the write path *and* on both enqueue-side repair
paths (`reconcile_embeddings`, the owner-scoped backfill). Gating only
the write would be undone by the next operator maintenance pass, since
healing a missing job is exactly what those passes do.

The Fact's replay key is the **content hash alone**. For one owner a
content hash resolves to exactly one CitedObject, so keying on filename or
mime as well would let one file acquire two arrival Facts by being
uploaded under a second name. One file, one upload Fact, per owner: the
Fact replays exactly when the upload does, and re-completing is both
idempotent and the repair path.

**On a replay the corpus and the response name different files, and that
is deliberate.** `cited_uploaded_blob_v1` is inserted `ON CONFLICT DO
NOTHING`, so the corpus keeps the filename and mime it recorded *first*;
the completion response is built from what *this* call staged, so it
returns the name the caller just uploaded. Upload `vertrag.pdf`, then the
same bytes as `rechnung-2026.pdf`, and you get back `rechnung-2026.pdf`
with `idempotent_replay: true` while the stored row still says
`vertrag.pdf`. Answering a caller with a filename they never sent would be
worse, and the artefact genuinely is the one already held — the name is
metadata about an observation, not part of the artefact's identity. A
client that needs the recorded name must read it back
(`core_fact`'s `citation_of_fact`) rather than trust the completion
response. The kernel states the row's side of this as CH-U15 in the
proxima-docs Charta module, which is why that theorem is asserted by
selecting the row rather than by reading a response field.

A flavor that wants more columns on that arrival — extraction status, a
source system's id — registers its own sidecar schema and passes the row
alongside; it lands in the Fact's transaction or not at all. It extends
the substrate's event rather than owning a parallel one. See docs/09
§Extending a substrate Fact.

### One write

The CitedObject, its `cited_uploaded_blob_v1` row, the citation, the Fact,
its receipt, its embedding job, and any flavor extension rows are one
transaction. An artefact whose arrival nothing recorded is not a state
completion can leave behind.

That works because persisting a CitedObject already *is* a Fact write with
an inline citation: storage upserts the object on
`(owner, schema, content_hash)` — the key the upload lane deduplicates on —
and inserts its typed row through the registered cited-object sidecar. Once
the Fact carries the artefact as its citation, the two cannot come apart.
The blob store no longer writes those rows itself.

What cannot be inside that transaction is the object-store work. Streaming,
hashing, and copying an S3 object is not a database statement and must not
hold a transaction open while it runs. Completion is therefore three steps:

1. **stage** — verify the bytes with one bounded object read, publish them
   conditionally to the canonical key derived from their `upload_id`, record
   the transfer locator and SHA-256 audit digest, and carry the BLAKE3 content
   address forward. It writes no corpus rows. The
   redundant pending object is retired before stage returns; a retry reads
   the recorded canonical key. `finish` repeats the pending-key purge, so a
   provider failure during either cleanup is repaired by a later retry.
2. **the transaction** — everything above.
3. **finish** — mark the upload row completed against the cited object and
   retry deleting the now-redundant pending object.

A crash before step 3 leaves an upload row still saying `pending` whose
artefact is already recorded. That is bookkeeping for the transfer
protocol, invisible in the corpus, and resolved by completing the same
upload again.

A host or flavor that already computed the immutable metadata before the PUT
may call `complete_upload_as_fact_with_expectation` with an
`UploadCompletionExpectation`. After the one staging call, core compares the
BLAKE3 content hash, byte length, MIME, and filename in that order, before
authorization or the Fact transaction. A mismatch is a redacted
`InvalidArgument`; no corpus rows are written, `finish` is not called, and
an upload that was still pending remains pending for an explicit abort or a
retry with the corrected expectation. Replacement bytes require abort plus a
new prepare. The ordinary `complete_upload_as_fact` method has no caller
expectation and remains the path used by MCP's unchanged
`complete(upload_id)` action.

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
3. `core_upload` `complete` (`upload_id`) verifies the bytes, records
   the arrival as a `core/upload-v1` Fact citing the artefact, and
   returns the canonical `cited_object_id` (plus hex
   `content_hash`/`sha256`, `byte_len`, `mime`, `filename`; replays
   report `idempotent_replay`) together with the Fact's handle as
   `fact`. See §The upload Fact.
4. Further Facts cite the same object via `core_remember`'s
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
   download URL later — only for locators the upload lane itself wrote;
   a caller-asserted inline locator never presigns (see §Large artefact
   storage).

All four actions resolve a `space` key exactly like `core_remember` and
re-authorize against the resolved owner. The tool is served by the
host-wired cited-blob service; a host without `PROXIMA_S3_*` configured
fails typed at call time with the enabling configuration in the message
(docs/10 §Large Artefact S3).

MCP tools are not the only consumer of that service. The runtime also
publishes the same `CitedBlobService` instance through the composed
`FlavorServices`; a background worker resolves it with
`FlavorWorkerContext::service` (docs/09 §Background Workers), so a flavor can
process an uploaded artefact after the tool call that received it has returned.
The presigned-only rule above applies identically to a worker caller: it reads
through `read_url` and never learns the bucket or object key.

Citation read-back (`core_fact` `citation_of_fact`) returns the locator alongside the ids: the
`core/uploaded-blob-page-span-v1` payload as `page_span` when the
mapping carries one, and — when the cited object is an uploaded blob —
a `document` block with `filename`/`mime`/`byte_len`/`sha256_hex`/
`uploaded_at`. What the document IS, never where it lives:
`bucket`/`object_key` stay internal, and fetching bytes goes through
`read_url`.

Reconciliation keeps the same boundary. The global operator pass is
`CitedBlobStore::reconcile_all(&SystemAuthority)` and may return bounded raw
locator samples for restore work. The store and witness must share one boot
binding; a token from another `Engine` is rejected before I/O. Flavor tools receive a separate
`CitedBlobOwnerReconcileService`: it re-authorizes Fact-read for one Owner,
lists only that Owner's object prefix, and returns a redacted report with no
bucket or object key. Both passes only report missing objects, unclaimed
objects, and foreign locators; neither repairs nor deletes.

## Owner scoping

CitedObject carries Owner. A document ingested for `User(A)` is not
visible to `User(B)`; the same PDF re-ingested for B produces a
separate CitedObject row under B's Owner. Cross-owner access is resolved through group membership / `OwnerRoles`; the citation layer never spans owners.

CitationMapping inherits `owner` from its Fact (and equivalently from
its CitedObject — the engine checks they match).

## Edges do not cite

An edge has no citation, and nowhere that could hold one: it is a pin in the
source row's `origins` / `refs` column and nothing else. There is no
authorship column either — who reasoned is answered by the write-act Fact that
produced the statement, not by the pin that follows from it.

Anything you'd want a "citation" to express on an edge is already encoded by
the citing memory's own citation chain. An edge that seemed to need a
citation is a node that has not been written yet.

## Operator-invocation provenance is not a citation

Bibliographic citation is artefact-only. Declared inputs for an
operator-derived Memory live in `origins`; schema-specific recipe metadata may
live in typed Content. Operator id, input contract, and embedding model are not
universal Memory columns. There is no operator/invocation table; the invocation
manifest validates the declared inputs before admission (see
[04 §Idempotence and reproducibility](04-consolidation.md#idempotence-and-reproducibility)).

## What this does not include

- **Renderers.** No `render()` on `CitedObjectPayload` or
  `CitationMappingPayload`. The artefact is the artefact; UI fetches
  large binaries via presigned read URLs, not raw storage coordinates.
- **Access control on artefacts.** Owner is the only scope. Per-asset
  ACLs (e.g., one document shared with N users) are a v2+ extension
  layered above Owner.
- **Versioned artefacts.** A new revision of a PDF with a different
  `sha256` is a new CitedObject. Relating revisions across CitedObjects
  is a flavor concern, modelled as an Abstraction over them or as
  schema-declared references, never as a new edge kind.

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
