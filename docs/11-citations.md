# 11 — Citations

A citation answers "**what artefact in Reality does this memory come
from?**" One artefact table, one link column, one payload trait. The
Memory graph already
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
- **`memory.blob_id`** — OPTIONAL for **Fact and Abstraction**,
  forbidden on Perspective. Only Fact ingest sets it today: derive takes
  no citation, so the Abstraction half is admitted by the schema and
  written by no shipped path.

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

## Payload trait

Parallel to `FactPayload` / `AbstractionPayload` / `PerspectivePayload`
(03). Compile-time only; build-time registration only — same rule as
all other payload traits. A citation schema is typed
(`try_add_cited_object_schema`, parsed at ingress) or opaque
(`add_opaque_cited_object_schema`, not parsed); either way the payload is
the artefact's description, stored on no sidecar table.

```rust
trait CitedObjectPayload: Serialize + Deserialize + 'static {
    const SCHEMA_ID:        &'static str;    // "doc-pdf", "media-image", "chat-telegram-session", ...
    const SCHEMA_VERSION:   u32;

    /// Stable hash of the artefact content. Re-ingesting the same
    /// PDF / image / session produces the same key, deduplicating
    /// the CitedObject row within Owner.
    fn idempotency_key(&self) -> [u8; 32];
}
```

There is no second trait for *where* in the artefact a memory comes from.
A page range, a region or a timestamp is content of the citing memory, a
field of its own payload schema; the citation is `memory.blob_id` and
nothing else.

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
| `core/upload-v1` | Fact | That an artefact arrived, citing it. Minted by `complete`; no sidecar of its own |
| `core/mcp-call-io-v1` | CitedObject | Substrate-internal MCP call logging |

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

The upload lane is the only writer of a locator. An inline
`core/uploaded-blob-v1` citation stores a `blob` row (owner, schema,
content hash) and nothing else, so it can name an artefact but never
point the presigner at an object. `read_url` reaches bytes only through
the blob's live upload row, and serves that locator only when it is the
configured bucket and the key this store would mint for the row's own
`upload_id`; any other row is answered exactly like a missing object.
There is exactly one key scheme and no legacy branch: objects written by
an earlier scheme are not readable.

S3 preserves original bytes only. It does not replace the 0..1
citation on a memory or `origin` provenance.

<a id="the-upload-fact"></a>
## The upload Fact

A CitedObject is a thing that can be cited. It is not an event: it holds
no receipt, appears in no change history, and says nothing about who put
it there or when. So a corpus built only from `complete` could hold a
file that nothing in the substrate records the arrival of — and did,
until flavors wrote that record themselves, which made "a file entered
the corpus" true only in the flavors that bothered.

`complete` therefore also mints a `core/upload-v1` Fact citing the
artefact. Its rendered text names
the file, so an upload is findable by name through
`core_search_memories` with nobody having written a Fact for it.

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
Fact replays exactly when the upload does. A completed upload replays after
its transfer expiry, returning the same cited object and Fact.

**On a replay the Fact stays one and the upload rows do not, and that is
deliberate.** Each upload keeps its own `blob_uploads` row with the filename
and mime it was sent under; the completion response is built from what
*this* call staged. Upload `vertrag.pdf`, then the same bytes as
`rechnung-2026.pdf`, and you get back `rechnung-2026.pdf` with
`idempotent_replay: true`. Both rows stay: the upload that minted the blob
keeps `vertrag.pdf`, and the blob's ordinary reads resolve its newest
completed upload. Answering a caller with a filename they never sent would
be worse, and the artefact genuinely is the one already held — the name is
metadata about an observation, not part of the artefact's identity.
proxima-docs states the minting upload's side as CH-U15 (`Charta.Upload`),
asserted on the stored row rather than on a response field.

A flavor that wants more columns on that arrival — extraction status, a
source system's id — registers its own sidecar schema and passes the row
alongside; it lands in the Fact's transaction or not at all. It extends
the substrate's event rather than owning a parallel one. See docs/09
§Extending a substrate Fact.

### One write

The CitedObject (its `proxima_core.blob` row), the Fact that cites it, its
receipt, its embedding job, any flavor extension rows, and the completed
`blob_uploads` row are one transaction. An artefact whose arrival nothing recorded is not a state
completion can leave behind.

That works because persisting a CitedObject already *is* a Fact write with
an inline citation: storage upserts the object on
`(owner, schema, content_hash)` — the key the upload lane deduplicates on —
and the Fact carries it as its citation (`memory.blob_id`). Once it does,
the two cannot come apart. The same corpus transaction marks the
`blob_uploads` row completed against that blob.

What cannot be inside that transaction is the object-store work. Streaming,
hashing, and copying an S3 object is not a database statement and must not
hold a transaction open while it runs. Completion is therefore two database
phases:

1. **stage** — verify the bytes with one bounded object read, publish them
   conditionally to the canonical key derived from their `upload_id`, record
   the transfer locator and SHA-256 audit digest, and carry the BLAKE3 content
   address forward. It writes no corpus rows. The
   redundant pending object is retired before stage returns; a retry reads
   the recorded canonical key.
2. **the transaction** — write the cited object and Fact, and mark the upload
   row completed against the cited object. Deleting the redundant pending
   object remains best effort outside this transaction.

A host or flavor that already computed the immutable metadata before the PUT
may call `complete_upload_as_fact_with_expectation` with an
`UploadCompletionExpectation`. After the one staging call, core compares the
BLAKE3 content hash, byte length, MIME, and filename in that order, before
authorization or the Fact transaction. A mismatch is a redacted
`InvalidArgument`; no corpus rows are written, and an upload that was still
pending remains pending for an explicit abort or a retry with the corrected
expectation. Replacement bytes require abort plus a new prepare. The ordinary `complete_upload_as_fact` method has no caller
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
   `citation.cited_object_id` (by reference; `C:` prefix accepted). This
   is deliberately the only way an MCP client can cite its own upload:
   `complete` never returns `bucket`/`object_key`, and the inline
   `object_payload` path requires them. By-ref and the inline `object_*`
   fields are mutually exclusive; storage verifies the referenced object
   exists for the Fact's owner in the same transaction that writes the
   Fact.
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

Citation read-back (`core_fact` `citation_of_fact`) returns the cited
blob's `cited_object_id` and `cited_object_schema_id`, and nothing else.
`bucket`/`object_key` stay internal, and fetching bytes goes through
`read_url`.

Reconciliation keeps the same boundary. The global operator pass is
`CitedBlobStore::reconcile_all(&SystemAuthority)` and may return bounded raw
locator samples for restore work. The store and witness must share one boot
binding; a token from another `Engine` is rejected before I/O. Flavor tools receive a separate
`CitedBlobOwnerReconcileService`: it re-authorizes Fact-read for one Owner,
checks only that Owner's live upload locators, and returns a redacted report
with no bucket or object key. A blob's live upload is its newest completed
upload, ordered by `completed_at DESC NULLS LAST, upload_id DESC`; mounted
uploads resolve the object minted by `mounted_from_upload_id`. Reads and both
reconcile scans share this SQL selection. Missing samples list newest losses
first, up to 100 entries. Every completed upload, including superseded rows,
still claims its object for global orphan accounting. Both passes only
report; neither repairs nor deletes.

## Owner scoping

CitedObject carries Owner. A document ingested for `User(A)` is not
visible to `User(B)`; the same PDF re-ingested for B produces a
separate CitedObject row under B's Owner. Cross-owner access is resolved through group membership / `OwnerRoles`; the citation layer never spans owners.

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

- **Renderers.** No `render()` on `CitedObjectPayload`. The artefact
  is the artefact; UI fetches
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
- `payload-trait`
- `tables`
- `multiplicity`
- `idempotency`
- `large-artefact-storage`
- `mcp-surface`
- `owner-scoping`
- `edges-do-not-cite`
- `operator-invocation-provenance-is-not-a-citation`
- `what-this-does-not-include`
