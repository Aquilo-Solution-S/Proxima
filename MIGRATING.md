# Migrating a Proxima host

Runbook for moving a host (embedding consumer, MCP deployment, or flavor
crate) across a Proxima tag. This file answers one question: **what must I
change, and what will behave differently if I change nothing.**

It is not a changelog — `CHANGELOG.md` is that, generated from commits — and
not a reference. Deployment and env vars live in
[15-deployment.md](docs/15-deployment.md); public API tiers live in
[public-api.md](docs/reference/public-api.md).

## Where to start

| You are | Read |
|---|---|
| An **operator** promoting a deployment | [the v0.0.7 schema lane](#the-v007-schema-lane), then [scheduled maintenance](#scheduled-maintenance-changes) |
| Running the **code flavor** | the above, then [re-index every repository](#re-index-every-code-repository) |
| An **MCP client / agent** author | [wire changes](#wire-changes-mcp-clients) |
| An **embedding host** driving `Engine` in Rust | [Rust host changes](#rust-host-changes) |
| A **flavor** author | [flavor SDK changes](#flavor-sdk-changes) |
| Booting against a **pre-v0.0.4 database** | [the v0.0.4 reset](#the-v004-reset) first — nothing else applies until it is done |

Every upgrade also needs [the lock-step rules](#rules-for-every-upgrade) and
[the closing checks](#checks-before-calling-an-upgrade-done).

Older lanes are kept below: [v0.0.5 → v0.0.6](#v005--v006) and
[the v0.0.4 reset](#the-v004-reset).

---

# v0.0.6 → v0.0.7

## The v0.0.7 schema lane

Seven migrations, in this order. **Apply all of them.** A host booting with
`PROXIMA_SKIP_MIGRATIONS=true` against a database missing any of them fails
at `ensure_core_schema_current`, not at first query — but it does fail.

| # | Source | File | Rewrites tables? |
|---|---|---|---|
| 11 | core | `0011_v007.sql` | **yes** |
| 12 | core | `0012_v007.sql` | no — functions only |
| 13 | core | `0013_v007.sql` | no — one new table |
| 14 | core | `0014_v007.sql` | **yes** |
| — | code flavor | `20260726000020_v007.sql` | **yes** |
| — | code flavor | `20260728000020_v007_language.sql` | **yes** |
| — | code flavor | `20260729000020_v007_ingest_scope.sql` | no — metadata-only `ADD COLUMN` |

What each one does:

- **0011** rebuilds the `embeddings` primary key to include `chunk_index`;
  adds STORED generated `search_tsv` to `memories`, `agent_derivation_v1`,
  `agent_note_v1`; adds four `proxima_core.lexical_*` functions.
- **0012** makes the text-search configuration a database property
  (`proxima_core.lexical_config()`), so the document side and the query side
  cannot drift. Returns `english`; rewrites nothing.
- **0013** adds `citation_uploaded_blob_page_span_v1` — the table behind
  citing an uploaded document by page.
- **0014** makes the lexical language a `regconfig` **column** per row and
  demotes `lexical_config()` to the default for rows that do not say.
  Rebinds each generated column to the two-argument `lexical_tsv`.
- **`20260726000020`** puts `search_tsv` on `code_chunk_v1`, built from
  `proxima_core.lexical_tsv` — config `english`, changed from `simple`.
- **`20260728000020`** pins code chunks to `english` per row so a deployment
  that switches its documents to another language does not retokenise its
  code as collateral.
- **`20260729000020`** adds `include_globs`/`exclude_globs` to
  `proxima_code.repos`, both defaulting to empty.

### This lane is not online-safe

Unlike v0.0.6, four of these files rewrite tables. `ADD COLUMN ... GENERATED
ALWAYS AS ... STORED` and `ALTER COLUMN ... SET EXPRESSION` each rewrite their
target and hold `ACCESS EXCLUSIVE` for the duration, and sqlx runs each file
in one transaction — so every table in a file stays locked until the last one
finishes.

Measured: **54.7 s** for a 149k-row `memories` plus a 24.8k-row sidecar, and
it scales with corpus size. A queued `ACCESS EXCLUSIVE` request also blocks
every reader that arrives behind it, so this is a read outage, not a write
pause.

- **Large deployments: apply out of band.** Run the files through GitOps
  against a real backup during a maintenance window, then boot with
  `PROXIMA_SKIP_MIGRATIONS=true`. Do not discover the lock window during a
  rolling update.
- **Small deployments** can let boot apply it. Boot migrations set
  `lock_timeout = 5s`, so a migration that cannot get the lock fails and
  retries on the next pod rather than freezing the table behind a lock queue.

### Rollback is by image, never by reversing a migration

There are no `.down.sql` files, and `DROP COLUMN search_tsv` is a second full
rewrite under the same lock. Rolling the binary back to v0.0.6 against a
version-14 database is safe **only** if no over-limit memory was
chunk-embedded — a v0.0.6 binary assumes one embedding row per
`(entity, version, model)`. Check and clear first:

```sql
SELECT count(*) FROM proxima_core.embeddings WHERE chunk_index > 0;
```

## Behaviour that changes with no action from you

Three things move as soon as the lane is applied. None needs configuration;
all three change what a query returns.

**Over-limit memories are now embedded in chunks.** A memory whose text
exceeds the provider's input limit is stored as several chunk rows under one
embedding version, where it previously went un-embedded entirely.

**A derived memory whose text the embedder refuses is now written anyway.**
`core_derive` and every `Engine::author_derived_authorized` caller used to
fail the whole write — deterministically, on every retry — when one
`/embeddings` call refused or crashed on the text, taking every model call
already paid for upstream with it. The memory now lands with no vector and a
pending embedding job enqueued in the same transaction, so the drain (which
bisects over-limit input) supplies the vector; the response carries
`embedding_deferred: true` and the memory is lexically findable but not
semantically findable until a drain runs. A provider that is simply *down*
still fails the write, so an outage cannot quietly produce a corpus of
unembedded memories. Hosts that run no drain should call
`Engine::drain_embedding_jobs` or `maintain-embeddings --drain` — see
[scheduled maintenance](#scheduled-maintenance-changes).

**Lexical search stems and drops English stopwords** (`simple` → `english`),
in core and in the code flavor alike. Result *sets* change, not just
ordering: a query of only stopwords no longer matches, and `running` now
matches `run`. Re-check any saved query or test that pins exact lexical hits.
Exact identifier lookup moved to the substring arm, which carries a larger
score bonus than any rank, so `embed_in_chunks` still matches verbatim.

**Lexical ranking moves, and by a lot.** `LEAST(ts_rank_cd(v, q) * 10.0, 1.0)`
returned 1.0 for every matching row — nothing in this schema assigns lexeme
weights, so every document is weight D, where `ts_rank_cd` starts at exactly
0.1. Measured on an indexed corpus, **3,170 of 3,170 matching rows
saturated**: within a score band nothing was ranked at all. Cover density is
now normalised, and the OR-rescue arm ranks by length-normalised `ts_rank`
instead of cover density:

| corpus | before | after |
|---|---|---|
| 17 real bug reports | 1 of 17 | **5 of 17** |
| 7 real bug reports | 3 of 7 | **5 of 7** |
| 24 plain-English questions | 12 of 24 | **17 of 24** |

Same set, different and better order; no re-index required. Only the rescue
arm changed — the strict arm keeps cover density. Score *bands* are unchanged,
so anything comparing against the documented `[0.5, 1.0]` / `(0.25, 0.45]` /
`0.25` ranges still holds. `semantic` and undegraded `hybrid` are untouched.

## Re-index every code repository

**Required if you run the code flavor.** Two changes alter how a repository is
chunked and rendered, and neither reaches an index that already exists:

- **The chunker no longer drops comments.** In the Rust grammar a doc comment
  is a *sibling* of the item it documents, and the merge step skipped comment
  nodes — so every `///` and `//!` block was excluded from the corpus.
  Measured over 444 indexed Rust files, chunk spans covered 95.3% of source
  bytes overall but only 14.2% of `flavors/code/src/migrations.rs`: the loss
  landed exactly on the files that carry their reasoning in prose. After the
  fix, 99.2%.
- **`code-chunk-v1` renders its body**, as `path:start-end` plus the chunk
  text rather than the header alone. That render is `memories.text`, so it is
  what gets embedded — chunk embeddings previously encoded a file path and two
  line numbers, and search could only retrieve code whose *filename* resembled
  the question.

A HEAD snapshot re-derives only files whose blob hash moved, so an unchanged
repository keeps its old chunks permanently. That skip cannot be bypassed: a
derived Abstraction must carry the same `source_batch_id` as the Facts it came
from. The supported path is erase and re-ingest:

```
proxima-code_erase_repo    { repo_handle, confirm_canonical_path }
proxima-code_register_repo { path }
proxima-code_ingest_head_snapshot { repo_handle }
```

`proxima-code_erase_repo` is new, and is the first supported way to remove an
indexed repository at all — the storage verb existed but was reachable only
through `cfg(debug_assertions)` testkit, so in a release build a repository
once indexed was permanent. It deletes every Fact, Abstraction, edge,
embedding, receipt, citation mapping and cited object derived from that
repository and returns a receipt counting each. Irreversible;
`confirm_canonical_path` must match the stored path exactly.

**Budget for the re-embed.** Chunk embeddings now cover real content, so the
queue is proportional to corpus size: a 620-file tree enqueues 4,083 jobs.

While you are here, consider [scoping the repository](#scoping-what-gets-indexed)
— 22% of one measured three-repo index was a single repository's test
fixtures.

## Scheduled maintenance changes

**The reconcile CLI is renamed.** `proxima-mcp reconcile-embeddings` is now
`proxima-mcp maintain-embeddings`, same flags. **Update cron and deploy
specs**; the old subcommand fails with a message naming the new one. The pass
gained an orphan-row sweep and a health report (job backlog, orphan counts,
ANN recall canary), and is serialized by a Postgres advisory lock — a run that
finds the lock held prints a skip notice and exits `0`, so cron overlap is
safe.

**Startup catch-up is automatic.** When an embedding client is configured, the
in-process worker runs one `missing-only` reconcile before its first drain, so
Facts ingested while no client was configured, and jobs stuck in the `failed`
dead-end, are re-enqueued on restart. There is still no recurring in-process
scheduler; recurring maintenance stays external.

**Retention is now enforceable — decide before you schedule it.**
`owner_fact_retention.retention_seconds` was inert config, and `change_event`
grew without bound. One new cron-safe pass handles both:

```sh
proxima-mcp maintain-retention --enforce-fact-retention \
    --prune-change-events-older-than 90d
```

Review before scheduling:

- **Configured windows become real.** Owners with a `retention_seconds` value
  have Facts older than the window tombstoned — hidden from present-only
  reads, rows and provenance kept; physical destruction stays exclusive to the
  compliance-erase family. Audit Facts (`core/mcp-call-logged-v1`) are always
  excluded. **If a window was set speculatively, clear it before scheduling.**
- **Tombstoning emits `EntityDelete` change events** — the first producer of
  that kind. The variant has been on the wire since v0.0.4, but consumers that
  only ever matched `EntityAppend` should be checked.
- **Pruned change events are gone for every consumer.** A forward poller whose
  `since` cursor predates the prune horizon misses them with no gap signal.
  Pick a horizon comfortably larger than the slowest consumer's lag, or
  re-baseline lagging consumers via cold-start stitching (docs/14 §Change Log).
- **Legal holds gate both halves.** Held owners are skipped and reported; the
  pass never blocks on a hold.

**Embedding jobs already marked terminal are not recovered.** A transient
upstream failure that Ollama reports as HTTP 400 used to be filed as a
permanently rejected input. That is fixed going forward, but rows already
marked `failed` with the permanent marker stay terminal by design. To re-offer
them:

```sql
UPDATE proxima_core.embedding_jobs
   SET status = 'pending', attempts = 0, last_error = NULL,
       next_attempt_at = now()
 WHERE status = 'failed'
   AND last_error LIKE '%: EOF%';
```

Widen the `last_error` filter only to messages you recognise as transport
failures; a genuinely over-limit input should stay terminal.

## Wire changes: MCP clients

### Breaking

| Change | Was | Now |
|---|---|---|
| `core_get_memory` `memory` arg | bare uuid accepted | prefixed `F:`/`A:`/`P:` only |
| Unknown `proxima://` path | `invalid_params` | `resource_not_found` (-32002) |
| Missing/invisible memory, goal, edge via a resource | `invalid_request: "Forbidden: entry not found"` | `resource_not_found` with the wire handle |
| `Protocol(NotFound)` tool errors | -32600 | -32602 |
| `core_membership:list_members` output | bare array | `{members, next_cursor, has_more}` |
| lineage `truncated` flag | `truncated` | `has_more` |
| `neighbor_edges[].handle` | `handle` | `edge` |
| `idempotency_key` | untrimmed 1..=200 | trimmed, 1..=180 |
| memory read `space` field | literal `entry` | `current` / `personal:<uuid>` |
| `limit: 0` on any paged read | clamped to 1, or an empty page | `InvalidInput` |
| `proxima-code_open_file_revision` `max_text_bytes: 0` | `text: ""` | `InvalidInput` |
| `proxima-code_search_chunks` default mode | lexical | **hybrid** |
| `core_derive` auto-derived idempotency key | hashes body | hashes title + body + tags |
| `core_derive` on an unembeddable body | tool error, nothing written | memory written, `embedding_deferred: true` |

`core_derive`'s new `embedding_deferred` field is **omitted when false**, so
an ordinary response is byte-identical to v0.0.6's. Present and `true`, it
means the memory exists and semantic search will not find it until an
embedding drain runs.

Four of these need more than a line.

**`space: "entry"` was never a value you could send back.**
`core_memory_spaces` defines the space-key vocabulary and every write
validates against it; the read path reported a placeholder no space is called,
so following the server's own instruction — *use a returned `space` key in
`core_remember`* — failed with `unknown memory space: entry`. Expect `current`
where you saw `entry`, and `personal:<uuid>` when reading across owners.

**`limit: 0` is rejected everywhere now, and the ends of a page bound are not
symmetric.** A limit *above* the maximum can be clamped, because "as many as
you will give me" is still the caller's intent. Zero answers nothing — and it
answered differently on every surface: `InvalidInput` on one tool, `{"commits":
[]}` on another, a clamped page of one on the rest. An empty page is the worst
of the three: well-formed, and indistinguishable from "nothing matched". The
engine has rejected `limit == 0` from the start; the MCP layer clamped before
that guard could fire. Upper bounds are unchanged — over-large limits still
clamp silently.

**`search_chunks` defaults to hybrid, so its `score` changes meaning.** Under
hybrid it is a fused rank score of roughly 0.0–0.07, not a lexical band score
of 0.0–15. Compare scores within one response, never across modes.
`lexical_score` still carries the band score. If you have no embedding client
configured nothing changes: hybrid finds none, ranks lexically, and sets
`degraded_to_lexical: true`. If you do, the default order changes — that is the
point:

| corpus | lexical | hybrid |
|---|---|---|
| 17 real knip bug reports | 0.331 MRR, 9 of 17 | **0.598, 13 of 17** |
| 7 real prek bug reports | 0.466, 7 of 7 | **0.592, 6 of 7** |
| 24 plain-English questions | 0.541, 18 of 24 | **0.636, 22 of 24** |

`mode: "lexical"` pins the old behaviour exactly. `mode: "semantic"` *fails*
rather than degrading, because answering lexically would answer a different
question. Freshly ingested chunks are lexical-only until a
`maintain-embeddings` drain, and report it via `degraded_to_lexical`.

**`core_derive` without an explicit `idempotency_key` will write once more
than you expect.** The auto-derived key hashed the body alone, and so does the
storage-side replay proof — while title and tags live in a sidecar the proof
never reads. Two derivations over one body with different titles were one
write, and the second caller got `idempotent_replay: true` over content that
was never stored. The key now covers title, body and tags, so a derivation
replayed across this upgrade is a new write rather than a no-op. Callers
passing their own `idempotency_key` are unaffected — there the caller is
asserting "these are the same derivation", which is what the parameter is for.

### Error text changed

Only relevant if you match on error strings. The same inputs are refused; the
sentences change, and they now distinguish two mistakes that shared one
message.

Every authoring surface answered a blank value and an oversized one
identically. `core_remember` told a two-space body `body must be 1..=20000
chars` — a range two characters satisfies — so the rejection read as a server
fault rather than an instruction to send content:

```
core_remember body: "  "         -> body must not be blank; it is empty
                                    after trimming whitespace
core_remember body: "a" x 20001  -> body must be at most 20000 chars after
                                    trimming; got 20001
```

The rule is `proxima_core::text_bounds::check_trimmed_len`, the lowest module
in the crate, so the tool SDK, the `verbs` layer and the code flavor all refuse
the same input in the same words. It backs `core_remember` (title, body),
`core_derive` (title, body, `model_id`), `core_record_utterance` (text),
`core_link` (reason), `core_goal` (title, text, wake prompt), every
`idempotency_key`, `core_remember.source_batch_key`, each tag, the search
query, and `proxima-code`'s `emit_execution_request` family.

Also fixed here: `GoalWakeToolId::parse` bounded `value.len()` — **bytes** —
behind a message saying `1..=200 characters`. A 120-character Cyrillic id is
240 bytes, so it was refused for exceeding a limit it was nowhere near in the
unit the message named.

One consequence beyond wording: `core_derive` tested
`model_id.trim().is_empty()` and then hashed the *untrimmed* string, so
`" claude "` and `"claude"` were one label to the validator and two to the
idempotency key. `model_id` is now trimmed before use. A caller sending a
padded `model_id` sees one new Abstraction on the first call after upgrading,
then replays collapse.

A second Abstraction over one source batch now reaches the rule that governs
it, which used to answer `duplicate key value violates unique constraint
"memories_ftoa_batch_exclusive_uidx"`. It now states the rule. Constraints
`map_err` does not recognise still forward Postgres's text — a wider leak, and
only the one demonstrably reachable from an agent tool is translated.

### Schemas now declare the bounds they enforce

**Relevant only to clients that validate against `inputSchema` locally.** No
runtime behaviour changes; values that were refused before are refused now,
just earlier and without being sent.

Ten parameters promised a floor in prose (`0 is rejected`, `at least 1`) and
twenty-two promised a ceiling (`1 to 240 chars`, `at most 16 tags`), while the
schema said otherwise or said nothing. An `Option<u32>` emits `minimum: 0`
from the Rust type and nothing in `String` says 240, so a strict client was
told that `limit: 0` and a 30,000-character body both validate.

Ceilings are the worse half: a floor at least has a Rust default behind it,
while a client learns about a ceiling only after paying to send the body.

Now carrying `minimum: 1`: `core_search_memories.limit`/`.body_max_chars`,
`core_fact:facts_citing_object.limit`, `core_membership:list_members.limit`,
`proxima-code_list_repos.limit`, `proxima-code_search_chunks.limit`/
`.snippet_max_chars`, `proxima-code_search_commits.limit`,
`proxima-code_open_file_revision.max_text_bytes`/`.line_start`/`.line_limit`.
`core_link.confidence` declares `maximum: 100` instead of the `u8` type's 255.

Now carrying `maxLength`: `core_remember.title`/`.body`,
`core_derive.title`/`.body`/`.model_id`, `core_record_utterance.text`,
`core_link.reason`, `core_goal.title`/`.text`, `core_search_memories.query`,
`proxima-code_search_chunks.query`, `proxima-code_search_commits.query`,
`proxima-code_emit_execution_request.title`/`.instructions`/`.idempotency_key`.
Now carrying `maxItems`: `core_remember.tags`, `core_derive.tags`,
`core_search_memories.tags`/`.spaces`, `core_goal.children`,
`proxima-code_register_repo.include_globs`/`.exclude_globs`.

The rule is `proxima_core::mcp::schema_bound_mismatches`, which reports every
parameter whose description promises a bound its schema omits. An out-of-tree
flavor can call it on its own frozen registry. It is deliberately **not**
enforced in `try_freeze`: unlike an undeclared `ANNOTATIONS`, a bound stated
only in prose is a documentation defect and should not stop a deployment from
booting.

### Fixed silently — nothing to do, but searches that returned nothing may now match

**A tag filter matches the tag that was stored.** The write side folds a tag to
`trim().to_ascii_lowercase()`; the search filter did not fold and matched the
raw string. A caller using the same literal on both sides got nothing:

```
core_remember        tags: ["Rust"]  -> stored as ["rust"]
core_search_memories tags: ["Rust"]  -> no matches
```

There was no error to read, and a filter that matches nothing is
indistinguishable from a memory that was never written. Both sides now fold,
and the filter sorts and dedups afterwards, so `["Rust", "rust"]` is one
predicate — which matters under `tag_match: all`.

**A viewer can see a flavor's read tools.** `tools/list` resolved
read-vs-write from a table over *core* tool names and fell through to "demand
write" for anything else, so a read-only principal saw no flavor tool at all —
not refused, **absent**, which is the harder symptom to trace. The same filter
drives `initialize`'s instructions and `proxima://how-to`, so a viewer's
onboarding text omitted them too. Expect `..._search_chunks`, `..._list_repos`,
`..._open_file_revision`, `..._search_commits` and `proxima-docs_search` to
appear for viewer-role tokens. Write tools stay hidden.

**`proxima-code_open_file_revision` reports the span it actually returned.**
When `max_text_bytes` truncates the window, the reported `text_line_range` end
is now the last line *sent* rather than the last line selected; a caller using
the old value to place the snippet was wrong about every line after the cut. A
truncated chunk also carries `text_truncated: true` (omitted when false), and
`line_limit` above 500 now clamps instead of erroring.

**A `repo_handle` naming no repository is rejected** by
`proxima-code_search_chunks`, `_search_commits` and `_open_file_revision`,
with `repo_handle not found for owner: <handle>`. They previously returned an
empty result, which reads exactly like "this repository has nothing indexed".
Only the *name* forms were checked; a handle or bare UUID short-circuited on
parse, so a stale handle after `erase_repo`, a typo, or another owner's id all
resolved silently. Another owner's repository reports exactly what a
nonexistent one does.

**One crashing input no longer blocks its embedding batch.** A provider that
*dies because of* an input reports the same thing as an outage; Proxima
released the whole claim without burning attempts, so the poisonous input came
back with its batch on every drain and never went terminal. Observed while
ingesting a scanned book: one page whose OCR hallucinated a 300-row CJK table
killed the model runner, and the other **31 pages of its batch stayed
unembedded at `attempts = 0`**. The drain now probes the provider with a
trivial input after a transient batch failure — if it answers, the batch's
contents are at fault and its jobs are embedded individually. Expect one extra
small request per failed batch.

### New surface, nothing to do

| Added | What it gives you |
|---|---|
| `proxima://goals{?state,limit,cursor}`, `proxima://goal/{id}` | owner-scoped goal listing and single read, including stored wake config |
| `proxima://edges{?relation,source,target,…}`, `proxima://edge/{id}` | the graph was writable but not traversable — `core_link` returned an `E:` handle no verb could dereference |
| `proxima://memories{?ids}` | batch read, at most 100 prefixed ids, in request order plus a `missing` list |
| `core_upload` (`prepare`/`complete`/`abort`/`read_url`) | the S3 lane as an MCP tool; served automatically when `PROXIMA_S3_*` is configured |
| `core_search_memories` `min_score`, `semantic_weight`, `cursor` | relevance floor, hybrid fusion weight, keyset resume |
| `proxima://wake-candidates` `has_more` | truncation at the 200-candidate cap is signalled, never silent |
| `core_fact` citation read-back `page_span` / `document` | a caller holding a cited Fact learned *that* it cited something, not *what* |

Two of these have contracts worth reading before you build on them.

**Citing an uploaded document.** `core/uploaded-blob-v1` has been a registered
`CitedObject` schema since the baseline and the upload lane has been writing
rows for it — but no registered `CitationMapping` named it, and a mapping is
the only path from a Fact to a cited object. Core shipped an upload lane whose
artefacts nothing could cite. Two mappings now target it:
`core/uploaded-blob-whole-v1` (pure link, pass `"mapping_payload": {}`) and
`core/uploaded-blob-page-span-v1` (`page_from`, `page_to`, optional
`char_range_start`/`char_range_end`). Pages are one-based and inclusive at
both ends; char ranges are relative to the span's text, not the document's, so
a mapping survives re-extraction as long as the pages did not move. Re-citing
one document reuses its single `cited_objects` row.

`core_remember`'s `citation` accepts either `cited_object_id` (optionally
`C:`-prefixed, as returned by `core_upload` `complete`) or the three inline
`object_*` fields — exactly one shape, `mapping_*` required either way:

```json
{
  "title": "Mindestbreite einer Tür",
  "body": "Die lichte Durchgangsbreite beträgt mindestens 90 cm.",
  "citation": {
    "cited_object_id": "C:0198…",
    "mapping_schema_id": "core/uploaded-blob-page-span-v1",
    "mapping_schema_version": 1,
    "mapping_payload": { "page_from": 47, "page_to": 47 }
  }
}
```

Regions on a page are deliberately not included — a bounding box has to agree
with whoever produced it about pixels, points, or fractions of the page, and
core cannot make that agreement on a producer's behalf. Register a flavor
mapping targeting `core/uploaded-blob-v1`; see
[docs/11 §Core-registered schemas](docs/11-citations.md#core-registered-schemas).

**Bytes never travel through MCP.** `core_upload` mints presigned URLs; the
client `PUT`s raw bytes to `upload_url` with exactly the returned headers,
then calls `complete`. `read_url` presigns only locators the upload lane
itself wrote — the inline citation path stores a caller-asserted
`bucket`/`object_key` verbatim and never verifies it, and asking `read_url`
for such an object answers exactly like a missing one.

### Scoping what gets indexed

`proxima-code_register_repo` takes `include_globs` and `exclude_globs`,
gitignore-shaped, both defaulting to empty — which is what every existing repo
already has.

```json
{"path": "/src/knip", "exclude_globs": ["**/fixtures/**"]}
```

`*` stops at a `/` and `**` crosses directories. A path is indexed when it
matches some include (or there are no includes) and matches no exclude.
Measured over a three-repo dogfood index, 3,389 of knip's 4,935 chunks (68.7%)
sit under a `fixtures/` or test path — 22% of the whole deployment's
embeddings, with no way to say so.

- **Scope belongs to the repo, not to a call.** The incremental poller applies
  the same scope the snapshot does, or the indexed set would depend on which
  verb ran last.
- **Re-registering an existing path updates the scope** — deliberately unlike
  `display_name`, which is ignored on replay. Sending either list replaces
  both, so `{"exclude_globs": []}` clears a scope; omitting both leaves it
  alone.
- **Narrowing a scope tombstones what left it** on the next ingest.
  `files_excluded` in the ingest report counts what scope removed, and
  `list_repos` echoes both lists.

## Rust host changes

Every row here is a compile error until you act on it.

| Symbol | Change |
|---|---|
| `StoragePortsBuilder` | new required `goal_wake_candidate(GoalWakeCandidatePort)` handle; `PgStorage::storage_ports()` users unaffected |
| `MemoryReadPort::search_memories` | returns `verbs::query::MemorySearchPage { results, has_more }`, not `Vec<MemorySearchResult>` |
| `MemorySearchRequest` | three `#[serde(default)]` fields: `min_score`, `semantic_weight`, `after` |
| `QueryRequest` | new `#[serde(default)] goal_state: Option<GoalState>` |
| `GoalReadPort` | new `load_goal_wake_configs(read_owners, goal_ids)` — an empty vec preserves prior behaviour |
| `ReadVerbStoragePorts` | carries a `goal_read` handle |
| `EmbeddingMaintenancePort` | new `reconcile_embeddings(options, proof)` |
| `EdgeRow` | gained `source_kind`, `target_kind`, `created_at`; dead `payload: Vec<u8>` became `Option<SidecarPayload>`; no longer derives serde |
| `EdgeReadRequest` | new `cursor`, `include_payloads` |
| `EdgeReadPort::read_edges` | takes `payload_specs: &[EdgePayloadSpec]`; ignoring it preserves lean reads |
| `MemoryInspectPort` | new `load_memories_by_ids` |
| `CitationPort::facts_citing_object` | takes `after`/`limit`, returns `FactCitationPage` |
| `OwnerMembershipAdminPort` | new `list_group_members_page` |
| `FactIngestPort` | new `ingest_fact_with_citation_ref_and_typed_sidecar` |
| `Engine::list_members` | takes `limit`/`after`, returns `GroupMemberPage` |
| `Engine::backfill_fact_embeddings` | renamed `backfill_missing_embeddings`; returns `ProtocolError`, not `StorageError` |
| `GoalWriteBuildError::InvalidTitle` / `InvalidText` | now carry a `TrimmedLenViolation` payload |
| `proxima_code::repos::erase_repo` | lost its unused `schemas` parameter |
| `EmbedCaps` | new `max_input_chars` field — a struct literal no longer compiles; use `EmbedCaps::new(dim, matryoshka)` |
| `AuthorDerivedRequest` | `embedding: Option<Vec<f32>>` + `embedding_model_id: Option<&str>` collapsed into one `embedding: DerivedEmbedding` |
| `AuthorDerivedOutcome` | new `embedding_deferred: bool` — storage sets it when it enqueued a job instead of writing a vector |
| `AuthorDerivedAuthorizedOutcome` | same new `embedding_deferred: bool` |
| `proxima_storage_pg::verbs::derive_append::DerivedDraft` | same two fields collapsed into `embedding: DerivedEmbedding` |

Five of these are more than a signature.

**`EmbedCaps` gained a field**, which breaks every struct literal:

```rust
// before
EmbedCaps { dim, matryoshka }
// after
EmbedCaps::new(dim, matryoshka)
// and, if your provider does not reject over-long input cleanly:
EmbedCaps::new(dim, matryoshka).with_max_input_chars(NonZeroU32::new(16_384).unwrap())
```

Behaviour is unchanged when you do nothing but switch to the constructor:
`max_input_chars` defaults to `None`, which sends every input exactly as
before. Prefer `new` + `with_*` over a literal from here on — a literal
names every field, so the next capability axis breaks you again.

Set the cap for a provider that *dies* on over-long input instead of
rejecting it (a local Ollama does — it sizes a runner's context at load and
an input past it kills the runner, which arrives as a transport error and
gets retried unchanged). Over-cap input is then refused without a request
and bisected into chunked embeddings instead. The floor is
`llm::MIN_EMBED_INPUT_CAP_CHARS` (4095) and a lower value fails at
construction — see [docs/10 §Bounding embedding
input](docs/10-configuration.md#bounding-embedding-input). Operators of
`apps/proxima-mcp` set `PROXIMA_EMBED_MAX_INPUT_CHARS` instead.

**The derived write's two embedding fields became one enum.** A custom
`MemoryAuthoringPort` (or a flavor building a `DerivedDraft`) now reads:

```rust
// before
(req.embedding.as_ref(), req.embedding_model_id)
// after
match &req.embedding {
    DerivedEmbedding::None => { /* write no vector, enqueue nothing */ }
    DerivedEmbedding::Ready { model_id, vector } => { /* write the vector */ }
    DerivedEmbedding::Deferred { model_id } => {
        // NEW, and required: enqueue an embedding job for (owner, kind,
        // memory_id, model_id) in the SAME transaction as the row.
    }
}
```

A pair of `Option`s could spell two states that mean nothing, and the
third — write the row, owe the vector — could not be spelled at all, which
is why an unembeddable derived text was a permanently failing write. Flavor
drafts that passed `embedding: None, embedding_model_id: None` become
`embedding: DerivedEmbedding::None` and behave exactly as before.

An implementation that ignores the `Deferred` arm compiles (it is a match
arm, not a new method) but silently drops the memory's only route to a
vector, so treat it as required work, not a rename. `PgStorage` users are
unaffected.

**`backfill_fact_embeddings` → `backfill_missing_embeddings`** is a widening,
not a rename. It now enqueues missing embeddings for Facts **and** derived
memories. The Fact-only filter was a real gap: a flavor that materializes
derived memories through its own sidecar path — as `proxima-code`'s ingest
does for every `code-chunk-v1` Abstraction — has no embedding client in scope
at write time and enqueues nothing, and those rows were invisible to the
owner-scoped backfill too. An indexed repository stayed lexically searchable
and semantically empty until someone ran a *global* pass. Custom
`EmbeddingJobPort` implementations need no change, but if yours filters to
Facts internally, widen it.

**`EmbeddingTextPort::load_embedding_text` takes a
`non_embeddable_schemas: &[String]`**, matching its sibling
`list_facts_missing_embedding`, which already carried one. Implementors must
add the parameter and exclude those schema ids — a row whose schema declared
`FactPayload::EMBEDDABLE = false` has no text to embed, however it is
reached. Passing an empty slice restores the old behaviour and is correct
only where the caller cannot see a declined schema (the inline job drain,
which only ever sees rows a job was enqueued for).

The exclusion could not stay solely at the enqueue sites. `Engine::
ensure_fact_embedding` takes a `MemoryId`, never passes through the job
queue, and writes the vector directly with no `embedding_jobs` row — so the
three SQL filters that guard reconcile and both backfills were structurally
unable to see it. `PgStorage` users are unaffected.

Relatedly, **`Engine::fact_ingest` now honours `EMBEDDABLE`**. It previously
computed `embed_client().map(model_id)` and passed it straight to storage,
which asks "is there an embedder" — a question the schema's declaration
overrides. It was the one of four Fact-writing verbs that did not consult
the gate, because it does not share the `ingest_fact_*` name. No signature
changed; a schema that declared `EMBEDDABLE = false` simply stops receiving
a vector through this verb.

**`GoalWriteBuildError`'s variants carry a payload**, so matching arms need
`(_)`:

```rust
// before
match err {
    GoalWriteBuildError::InvalidTitle => ...,
    GoalWriteBuildError::InvalidText  => ...,
}
// after — or bind the violation to report max/got yourself
match err {
    GoalWriteBuildError::InvalidTitle(_) => ...,
    GoalWriteBuildError::InvalidText(_)  => ...,
}
```

A unit variant on a `Copy` enum structurally cannot report what the caller
sent, which is why these two kept one message for two mistakes after every
other surface stopped. `TrimmedLenViolation` and `check_trimmed_len` are
re-exported from `proxima::host` — a variant payload a host cannot name is a
variant a host cannot match.

**Wake-candidate admission intersects the deployment tool surface.**
`Engine::with_deployment_tool_scope` (default `ToolScope::All`) is forwarded
automatically by the `proxima` runtime facade; hosts composing `Engine`
directly should pass their deployment palette, so Host-API wake reads cannot
exceed the deployed tool surface even under an `AuthzContext` with
`ToolScope::All`. Tool-scope palettes that should expose the new resource must
include `resource:wake-candidates` (profile `memory` includes it).

### The host facade names its own types now

**No action required — pure re-exports.** Each of these types was already in a
public signature on the facade and could not be *named* by a host depending on
`proxima` alone, which is the usual shape of an out-of-tree blocker: not a
missing feature, an unnameable type.

| Now exported | Was unnameable in |
|---|---|
| `GroupId`, `SourceId` | `GroupSourceScope` / `PersonalSourceScope` — two of five variants of an exported enum were unconstructible |
| `UPLOADED_BLOB_WHOLE_SCHEMA_ID`, `UPLOADED_BLOB_PAGE_SPAN_SCHEMA_ID` | `CitationSpec::v1` takes `impl Into<String>`, so this never failed to compile — it pushed flavors onto bare string literals no compiler could check |
| `EmbedCaps` | `OpenAiCompatEmbeddingClient::new` — `mistral()` supplies its own, which is why the gap was easy to miss; every other OpenAI-compatible endpoint (a local Ollama, any provider needing `matryoshka: true`) was unreachable |
| `SearchReadRequest`/`Response`, `MemorySearchRequest`/`Result`/`Page`, `SearchMode`, `SearchOrder`, `TagMatch`, `SearchCursor`, `DEFAULT_HYBRID_SEMANTIC_WEIGHT`, `MAX_SEARCH_PAGE_LIMIT` | `Engine::search` — an out-of-tree flavor could write a corpus and had no sanctioned way to query it |
| `CitedBlobStore`, `S3RuntimeConfig`, `BlobError` | `BuiltProxima::blobs` (a `pub` field) and `Proxima::s3` — with `S3RuntimeConfig` unnameable, `from_env()` was the only way to configure the blob lane, so a library API demanded process environment |
| `OwnerRefKind` | half the return of `OwnerRef::columns()`, which every flavor with its own tables calls |
| `TrimmedLenViolation`, `check_trimmed_len` | `GoalWriteBuildError`'s variant payloads |
| `Role` | minting a group-owner `AuthzContext` |

`public_api_tiers.rs` now constructs every variant of the scope enum, so a
sixth one has to be added there too.

### Search stopped resolving owners through the goals union

**No action required.** Query-shape change inside `core_search_memories`; the
returned `(memory_id, lexical_score)` set is unchanged.

Every candidate branch reads `FROM proxima_core.memories m` but resolved the
candidate's home owner by outer-joining a `memories UNION ALL goals` union —
which for a memory row can only ever return `m` itself. The cost was not the
join: the planner drove the whole query off that union, seq-scanning both
tables per branch, which is also why no index could serve the lexical
predicate.

Measured on a 15,265-memory corpus with six search projections, six lexical
queries, median of seven interleaved runs: **1,832.8 ms → 1,321.9 ms
(1.39×)**, per-query 1.36×–1.42×, byte-identical results on every query.

One note for anyone diffing search results: rows tied on `created_at` between
the base branch and a projection branch can swap order, which changes which of
two already-arbitrary snippets survives `merge_row`. Ranking and membership do
not change; a projection declaring `SearchProjectionField::MEMORY_TEXT` removes
the ambiguity by construction.

## Flavor SDK changes

### Breaking

**`SearchProjection` and `MemorySearchProjection` gained two fields.** Neither
is `#[non_exhaustive]` and neither derives `Default`, so out-of-tree struct
literals fail with E0063:

```rust
fn search_projection() -> Option<SearchProjection> {
    Some(SearchProjection {
        fields: &[/* ... */],
        tag_column: None,
        tsv_column: None,       // <- add
        language_column: None,  // <- add
    })
}
```

`None` on both keeps v0.0.6 behaviour exactly: the builder computes the vector
inline from the projected search text, through the same
`proxima_core.lexical_tsv` definition the generated columns use, so scoring is
identical either way.

Set `tsv_column: Some("search_tsv")` only after your own migration adds a
matching column generated from the same concatenation the projection emits:

```sql
ALTER TABLE my_flavor.my_sidecar
    ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (
        proxima_core.lexical_tsv(lexical_language, proxima_core.lexical_join(
            NULLIF(title, ''),
            NULLIF(body, ''),
            proxima_core.lexical_text_array(tags)))) STORED;
```

A `tsv_column` naming a column that does not exist surfaces as a Postgres
error on the first lexical search against that schema, not at boot — so
exercise one search after adding it. Getting the generated expression *wrong*
is worse: it fails silently, scoring that sidecar differently from every other
one. Pin it with a test shaped like
`crates/storage-pg/tests/integration/search_pg/stored_tsv.rs`, which asserts
the stored column equals `lexical_tsv` over the projection's own
concatenation. Declare `language_column: Some("lexical_language")` alongside
it, and add the column plus mirror trigger in your migration.

**Edge sidecars must implement read-back.** `PgSidecarRegistry::add_edge` now
requires `PgEdgePayload` — a batched `load_edge_batch(ctx, edge_ids)` loader —
alongside `PgEdgeSidecar`, and `freeze_against` rejects an edge sidecar
without one. An edge payload that can be written but never read back is a
write-only API hole. Core ships readers for `AgentLinkV1` and the code
flavor's `EdgeCallsV1`; a custom one needs a single
`SELECT ... WHERE edge_id = ANY($1)`.

**A tool that declares no `ANNOTATIONS` no longer boots.** The host fails at
registry freeze — at startup, before serving — with
`FlavorRegistryError::UndeclaredToolBehavior`:

```
tool proxima-yours_search declares no ANNOTATIONS, so the owner-role gate
cannot tell a read from a write and will demand write access; set
`const ANNOTATIONS` on the tool
```

```rust
const ANNOTATIONS: Option<McpToolAnnotations> =
    Some(McpToolAnnotations::new().read_only(true).open_world(false));
```

Why fatal rather than defaulted: `ScopeGateBehavior::enforce_owner_role` asks
whether a tool is read-only and demands WRITE when it cannot tell, so before
`ANNOTATIONS` existed every flavor tool missed and a viewer was refused
`proxima-code_search_chunks` — a read, refused, with nothing naming the cause.
Silence has to keep meaning "write", because guessing "read" would hand a
viewer a mutation; the only remaining fix is to stop accepting silence. Freeze
is where it is caught because Rust cannot express it at compile time — a trait
const with a default is always satisfiable.

Code that needs to know whether a tool is a read should call
`McpToolDescriptor::resolved_annotations()` or `is_read_only()` beside it, not
`core_tool_annotations` — that table answers for substrate tools only, and
resolving the tool's own declaration first and the manifest second is the order
`try_freeze` guarantees and the call-path gate uses.

### Additive

| Added | For |
|---|---|
| `FlavorBundle::spawn_workers(&FlavorWorkerContext) -> Vec<FlavorWorker>` | durable background workers; default empty body, so every existing bundle compiles unchanged |
| `FlavorWorkerContext::blobs: Option<CitedBlobService>` | a worker reading the artefact a tool call accepted; `None` unless the host configured `PROXIMA_S3_*` |
| `McpToolExtensions` on `proxima::flavor` | the return type of `FlavorApp::mcp_tool_extensions` — without it that override could not be written at all |
| `McpPresentationExt` | `format_*`/`resolve_*` on any `ToolCtx`, replacing a twelve-method per-flavor shim |
| `FactTombstone` | the return type of `FactPayload::tombstone`, needed to declare a *stateful* Fact schema |
| `AuthorDerivedRequestInput` + 8 companions | `Engine::author_derived_authorized` — a flavor could declare derived schemas but not write one |
| `SearchProjectionColumnKind::MemoryText`, `SearchProjectionField::MEMORY_TEXT` | projecting `memories.text` instead of copying it into the sidecar |
| `FactPayload::EMBEDDABLE` (defaults to `true`) | a Fact that should be readable and lexically findable but never vectorised |

Contract details worth knowing before building on these:

- **Workers** must terminate on the provided cancellation token; a panic is
  logged at join and never takes the host down. A worker has no request to
  inherit authority from — it supplies its own `AuthzContext` and `OwnerRef`
  per job. Note `AuthzContext::single_owner` denies for a group owner; use
  `for_subject_with_role`. `Proxima::build` (serverless) spawns no workers.
- **An Abstraction's `sidecar_table()` is required**, unlike a Fact's, so a
  flavor registering one always owns a migration for it. And a derived memory
  is embedded **synchronously**, inside the write, so a provider failure fails
  the write — Facts instead enqueue a durable job. A flavor deriving many
  memories in a worker should checkpoint per output, not per batch.
- **A stateful Fact without `FactTombstone` fails quietly**: the schema still
  gets head-by-natural-key resolution, but storage has no discriminator for
  `PresentOnly`, so an entity deleted upstream stays a live head forever.
- **`EMBEDDABLE = false` gates the vector only.** The Fact still writes
  `render()` to `memories.text`, so it stays readable and stays matched by
  full-text search; only the embedding job is skipped, on the write path and
  on both repair paths. Nothing to do for existing schemas — the default is
  `true`, and an unknown schema is treated as embeddable, because a surplus
  vector is waste while a missing one is silent. `core/upload-v1` is the
  first schema to opt out (docs/11 §The upload Fact); if you were relying on
  upload Facts appearing in semantic results, they now appear in lexical
  ones only, and existing rows are left as they are — nothing deletes a
  vector already written.
- **`MemoryText` also resolves the stored vector.** A projection of exactly
  that one field with no `language_column` reads `memories.search_tsv` instead
  of tokenising per candidate row. Such a sidecar needs no text column, no
  tsvector column and no GIN index on text — only `tags`. The copy it replaces
  bought nothing (every branch already joins `memories`) and made a second
  corpus that silently returns different text from a scoped query than from an
  unscoped one the moment it drifts.
- **The pool stays off the supported export tier on purpose.** The intended
  shape is a flavor-owned store type built from `clone_pool_for_host` that
  keeps `proxima_core.*` SQL private, mirroring `proxima-code`'s
  `CodeFlavorStore`. See `docs/09-developing-flavors.md` § MCP Tools — a tool
  must treat an absent service as a typed failure rather than assume the host
  wired it.
- **`MemorySearchRequest::tags` is the only predicate that narrows a search to
  a subset of a corpus.** `schema_id` is exact-match and there is no
  per-column filter, so a flavor that wants "search inside this book" declares
  a `tag_column` on its projection and filters there.

### Core SQL functions are callable from flavor SQL

The guardrail on raw SQL against `proxima_core.*` is about core *data*. A
flavor query may call the pure functions — `lexical_scrub`, `lexical_tsv`,
`lexical_join`, `lexical_text_array`, `memory_entity_kind` — which are
IMMUTABLE, read no row and enforce no authorization. They exist to be shared:
a flavor that could not call them would have to restate the definition its own
generated column is built from, which is exactly the drift they prevent. The
guardrail masks those calls and still fails on any literal naming a core table
or view.

## Optional: change the lexical language

**Skip this if you index English text.** Nothing changes by default —
`lexical_config()` returns `english`, and every query builds the same tsquery
as before.

The language is a `regconfig` column per row, and the database setting is the
default for rows that do not say. Measured on 2,350 pages of German technical
literature with 130 verified questions:

| arm | `english` | `german` |
|---|---|---|
| recall@5 | 0.438 | **0.577** |
| MRR | 0.349 | **0.490** |
| recall@5, questions not reusing their page's wording | 0.068 | **0.250** |

The last row is the one that matters. Questions phrased in the source's own
words score well under any configuration; the third of questions phrased
independently are what separate a working lexical arm from a decorative one,
and there `english` on German text answers 1 in 15.

**Per write**, `core_remember`, `core_record_utterance` and `core_derive` take
an optional `language`: a configuration name (`"german"`), an ISO 639 code or
BCP-47 tag (`"de"`, `"de-DE"` — the primary subtag decides), or `"auto"`.
Detection is gated on the detector's own reliability signal, measured ≥98%
accurate wherever the gate opens, while ungated detection under ~80 characters
is 50–83% — worse than useless. An unreliable detection falls back to the
default; a reliably detected language with no shipped stemmer (CJK, most
Slavic and Indic) maps to `simple`. Typed ingest paths set
`FactWriteCommand::lexical_language`. The language is not receipt-key
material, so enabling detection later replays cleanly.

**To change the default**, one call — and it must not be done any other way:

```sql
SELECT * FROM proxima_core.set_lexical_config('german');
```

It sets the default, registers it as an active language, and returns the
columns it rebuilt. Existing rows keep the language they were stamped with.
**Run it only after the full v0.0.7 lane is applied**, core and flavor, never
between its steps — before the code flavor pins `english`, a switch
retokenises every code chunk with the new stemmer as collateral.

**Do not redefine `proxima_core.lexical_config()` yourself.** PostgreSQL
permits it and does not recompute stored generated columns, so rows written
before the change keep their old tokenisation and rows written after get the
new one, with no error at any point. Half the corpus stops being reachable by
the other half's queries. `set_lexical_config` exists because that failure is
silent.

**Search stays one query over a mixed corpus.** The builder MATCHES with the
OR of one tsquery per active language and RANKS each candidate with its own
row's configuration. Measured against the same goldset, ranked per-row the OR
is bit-identical to a single-language baseline (0 of 130 top-5 sets changed);
ranked against the OR query it costs 6.2 points of recall@5 — which is why the
rank expression reads `c.lexical_language`.

**The language is immutable, like the text it describes.** Re-languaging is a
re-ingest, not an UPDATE. And **removing a language is guarded**: PostgreSQL
does not block `DROP TEXT SEARCH CONFIGURATION` while rows still hold the
value, leaving them with dangling OIDs that fail on any UPDATE. Run
`proxima_core.lexical_language_forget('cfg')` first; it refuses while any row
references the configuration.

`german` is stemming plus stopwords, not compound splitting. Embeddings and
hybrid fusion are untouched — the semantic arm is language-agnostic and
remains what carries cross-language recall.

---

# v0.0.5 → v0.0.6

## Fixed-owner MCP serving is removed

`proxima-mcp --owner-user`, `OidcAuthenticator::single_owner` and
`IdentityResolution::FixedOwner` are gone. Serving has one path:

```text
bearer -> UserId -> OwnerAccessPort::resolve_roles_for_subject -> OwnerRoles
```

| Step | Contract |
|---|---|
| initialize | client sends `X-Proxima-Owner: personal:<uuid>` / `group:<uuid>` / `world:00000000-0000-0000-0000-000000000001` |
| session | server binds the selected owner to `Mcp-Session-Id` |
| later calls | no owner argument; the bound owner is rechecked against fresh roles |
| revocation | membership removal denies the next request |

Loopback master-token auth is removed: MCP serving requires a host
`Authenticator` plus `OwnerAccessPort`, and stale `Bearer pxm_*` credentials
fail closed without reaching host auth. `McpToolHost` has no default owner —
embedded direct calls pass the owner explicitly per call.

## The OIDC group-auth path changed

`OidcAuthConfig` **no longer has an `owner` field**. Identity mapping is a
separate, explicit step. `OidcAuthConfig`/`OidcAuthenticator`/
`OidcSubjectMap`/`HttpJwksResolver` live in `proxima-auth-oidc`, not the
`proxima` facade — add it as a direct dependency. `?` below stands in for
whatever error type your host maps each fallible step into; see
`apps/proxima-mcp/src/lib.rs::oidc_from_env`, which maps each one into its own
`CliError` variant.

```rust
use std::sync::Arc;
use proxima_auth_oidc::{HttpJwksResolver, OidcAuthConfig, OidcAuthenticator, OidcSubjectMap};

// 1. Validation-only config: no identity mapping.
let oidc_config = OidcAuthConfig {
    issuer: issuer.clone(),
    jwks_uri,               // None => discover via {issuer}/.well-known/openid-configuration
    audience,
    allowed_subjects,       // unchanged: still an optional `sub` allowlist
    leeway_secs: 60,
};
let keys = Arc::new(HttpJwksResolver::new(issuer.clone(), oidc_config.jwks_uri.clone())?);

// 2. (iss, sub) -> UserId, explicit and issuer-aware.
let subject_map = OidcSubjectMap::from_json(&subject_map_json)?; // or ::from_legacy_shorthand

// 3. Exported OwnerAccessPort — drop any hand-rolled resolver that raw-SQLs
//    proxima_core.group_memberships; PgOwnerAccessResolver wraps the same table.
let owner_access: Arc<dyn proxima::OwnerAccessPort> =
    Arc::new(proxima::PgOwnerAccessResolver::connect_lazy(&database_url)?);

// 4. Composes the same shape `AuthzContext::server_resolved(roles,
//    AuthPath::HostBearer)` used to be assembled by hand.
let authenticator = OidcAuthenticator::new(oidc_config, keys, subject_map, owner_access)?;
```

`OidcAuthenticator::authenticate` builds the `AuthzContext` internally. Wire
the result in as before:

```rust
proxima::Proxima::<MyApp>::app()
    .database_url(database_url)
    .owner_access(owner_access.clone())
    .authenticator(Arc::new(authenticator))
    .with_mcp()
    .run()
    .await?;
```

Embedded hosts that do not serve MCP may still configure a boot owner for
host-owned direct calls; MCP serving ignores it and requires
`OwnerAccessPort`. For multi-audience composition, register one `OidcBinding`
per `(issuer, audience, subject-map, role-shape)` in an `OidcBindingSet` —
construction rejects duplicate `(issuer, audience)` routes and authentication
rejects a token unless exactly one binding validates. The lower-level
`OidcTokenValidator`/`ValidatedOidcClaims` surface remains for fully custom
hosts (`crates/auth-oidc/tests/custom_host_validation.rs`).

Hand-rolled agent tool-palette filtering is one call. `built` is
`Proxima::<App>::build()`'s result, whose `registry: Arc<FlavorRegistryFrozen>`
this reads:

```rust
let scope = proxima::tool_palette_excluding(&built.registry, &["dangerous_tool_id"]);
let authz = /* ... */.with_tool_scope(scope);
```

It expands action-scoped tools to `tool:action` granularity itself, so
excluding a tool name excludes every one of its actions — no partial-exclusion
gap when a tool grows a new action.

## `RuntimeBuilder::tool_scope` is now required

An unset tool scope no longer defaults to `ToolScope::All`. A host that never
called `.tool_scope(...)` silently advertised the full MCP surface — including
`core_publish` (irreversible owner transfer to World) and `core_membership` —
to every token. `build()`/`run()` now return `ProximaError::Config("tool_scope
is required: ...")` at `resolve()` time:

```rust
// one-line fix — restores the previous full-surface behaviour explicitly
Proxima::<App>::app().tool_scope(proxima::ToolScope::All)
```

Agent-facing hosts should prefer a narrow palette:
`.tool_scope(proxima::ToolScope::Palette(vec!["core_search_memories".into(), /* … */]))`.
The check is unconditional in the builder, not gated on `.with_mcp()`.

## `AuthorizationHook` membership direction

`AuthzOperation::Membership` is now directional, so veto consumers can tell a
grant from a removal instead of inferring direction from the called tool:

```rust
AuthzOperation::Membership {
    change: MembershipChange::Add | MembershipChange::Remove,
    group,
    member,
    relation,
}
```

## `publish_to_world` is an owner transfer

Publishing is a transfer to `OwnerRef::World` (`Engine::publish_to_world`), not
an ACL flag or a share row. Published entities become readable by everyone and
writable by no one; re-publishing an already-World entity fails closed with
`Forbidden`. The `core_membership:publish_to_world` action key is **removed** —
update tool-scope entries and MCP clients to the `core_publish` dispatcher.

## `proxima-storage-pg` raw write API requires `OwnerWritePermit`

These were never part of the supported Host API or Flavor SDK tiers (see
[public-api.md](docs/reference/public-api.md#supported-tiers)), but if
something depended on them anyway:

| Symbol | Was | Now |
|---|---|---|
| `verbs::fact_ingest::ingest_fact` / `_in_tx` / `_for_owner` | engine/authz/owner arguments | `&OwnerWritePermit` + payload + optional embedding model |
| `verbs::derive_append::append_derived_with_edges_in_tx` | raw owner in `DerivedDraft` | `&OwnerWritePermit` + `DerivedDraft` + operator edge proofs |
| `verbs::edge_write::append_owner_checked_*` | raw `&Owner` authority | `&OwnerWritePermit` |
| `verbs::close_batch`, `verbs::persist_mcp_call`, source cursor / retention / legal-hold writes | raw owner authority | `&OwnerWritePermit` |
| `verbs::fact_embeddings::insert_embedding`, `insert_memory_embedding` | `pub` | `pub(crate)` — use the proof-gated `EmbeddingWritePort` |
| `verbs::fact_embeddings::insert_fact_embedding` / `upsert_*` / `insert_goal_embedding` | `pub` | deleted; use the proof-gated port |
| `verbs::fact_ingest::ingest_fact_command_in_tx`, `_with_derived_sidecar_in_tx` | `pub` | `pub(crate)` |

Permit minting is an engine operation:

```rust
let permit = engine
    .authorize_owner_write(&authz, &owner, proxima_core::AccessKind::Fact)
    .await?;

proxima_storage_pg::verbs::fact_ingest::ingest_fact_in_tx(
    &mut tx, &permit, &payload, None,
    |tx, outcome| Box::pin(async move { /* sidecar write */ Ok(()) }),
).await?;
```

`AuthPath::System` no longer mints write permits by shape alone. Hosts that
intentionally need System writes hold `BuiltProxima::system_authority()` /
`RunningProxima::system_authority()` and call
`Engine::authorize_owner_write_with_system_authority(...)`. Flavor tools and
MCP-wire code do not receive this witness.

## Flavor authors: raw SQL against `proxima_core.*` is guardrail-denied

`scripts/check-architecture-guardrails.py` fails the build on new sites.
Migrate reads onto the exported facade:

```rust
use proxima::flavor::{authorized_memory_ids, authorized_fact_payloads};
```

See the module doc on `crates/proxima/src/flavor/authorized_read.rs` for the
full helper set — all route through `Engine::query`, the same
owner/group/`World` visibility path every other authorized read uses. (v0.0.7
carves out the pure SQL functions; see
[above](#core-sql-functions-are-callable-from-flavor-sql).)

## Code flavor repo erase is physical and rebuildable

`proxima_code::erase_repo` no longer returns the old "deferred to PR9" storage
error. It deletes the repo record, ingestion runs via FK cascade, sidecars,
owner-scoped substrate memories, receipts/batches, citations, edges and
embeddings, then returns `RepoEraseReceipt`. Unlike compliance erasure it
writes no suppression keys, so re-registering and re-ingesting the same repo is
allowed.

## `layered_router` now caps body size

`layered_router`/`layered_router_with_revalidation` had no
`DefaultBodyLimit`/`enforce_body_limit` layer, unlike `build_router` and the
streamable transport — an embedding host serving them network-facing had no cap
on inbound body size. Both now carry `proxima_mcp_server::enforce_body_limit`
outermost, matching `build_router`'s order (body limit before auth). No
signature change.

## v0.0.6 schema lane (core 9→10 + flavor append-only)

| Source | Files | Notes |
|---|---|---|
| Proxima core | `0009_v006.sql`, `0010_v006.sql` | GIN index drops; embedding backoff column; prefix-redundant btree drops; F/A/P append-only triggers |
| Code flavor | `20260709000020_append_only.sql` | Code sidecar append-only triggers |

Online-safe: nullable column add, idempotent index drops, trigger creation — no
backfill.

---

# The v0.0.4 reset

Applies only to a database still carrying pre-v0.0.4 Proxima schema artifacts
(or a stale baseline checksum). Nothing else in this file applies until it is
done.

## Detect

`ProximaBuilder::boot()` / `Proxima::<App>::build()`/`run()` return a typed
error rather than a stringly-typed one:

```rust
let running = match proxima::Proxima::<MyApp>::app()
    .database_url(url)
    .owner(owner)
    /* ... */
    .run()
    .await
{
    Err(proxima::ProximaError::V004ResetRequired { details }) => {
        eprintln!("database needs a v0.0.4 reset before this host can boot: {details}");
        eprintln!("see MIGRATING.md#the-v004-reset");
        std::process::exit(1);
    }
    Err(other) => return Err(other.into()),
    Ok(running) => running,
};
```

`ProximaBuilder::boot()` callers match
`proxima::EmbedError::V004ResetRequired { details }` the same way. Both are
distinct from the generic `Storage(String)` arm precisely so hosts can match
instead of parsing an error string.

## Back up, then reset

```sh
pg_dump "$DATABASE_URL" -Fc -f pre-v0.0.5-backup.dump
```

Reset with `tools/dev-migrate` — never `sqlx migrate run`, since core and
flavor migrators share one `_sqlx_migrations` table, which trips
`VersionMissing` on the second source:

```sh
SQLX_OFFLINE=true cargo build -p proxima-dev-migrate

# target resolution: --database-url first, then DATABASE_URL; always
# printed before anything runs
PROXIMA_V004_RESET_CONFIRM=reset-my-dev-db \
  ./target/debug/dev-migrate --database-url "$DATABASE_URL" --reset
```

`--reset` refuses non-local hosts and protected database names
(`postgres`/`template0`/`template1`) even with the confirm env set. It is a
**local dev tool**, not a production migration path.

**Production promotion follows GitOps, not this tool.** Apply
`crates/storage-pg/migrations/0008_v005.sql` — the only append-only v0.0.5
migration; `0001_init.sql` is the immutable shipped baseline and versions 2–7
are permanently retired (`RETIRED_PRE_V004_MIGRATION_VERSIONS`) — through your
normal deploy pipeline against a real backup.

## Confirm the restart

```sh
DATABASE_URL="$DATABASE_URL" ./your-host-binary   # or: docker restart <container>
```

Boot succeeds once `ensure_v004_baseline_compatible` sees only the current
baseline version. Tail logs for the `{source} migrations applied` lines,
confirming both `proxima-core` and every flavor source ran.

---

# Rules for every upgrade

## Lock-step version bump

Every Proxima crate this host depends on — `proxima`, `proxima-core`,
`proxima-storage-pg`, `proxima-auth-oidc`, and any flavor crates — moves
together. There is no supported skew across a tag. Bump all of them in the same
commit, then run [the checks](#checks-before-calling-an-upgrade-done).

## Migration version lanes

| Source | Reserved versions |
|---|---|
| Proxima core | `1..=9999`; `2..=7` retired pre-v0.0.4 rows |
| example/host migrators | timestamp versions ending `00..=19` |
| first-party flavors | timestamp versions ending `20..=39` |
| downstream host composition | timestamp versions ending `60..=99`; a host composing migrators outside `run_core_and_flavor_migrations` owns collision avoidance before touching the database |

Run `python3 scripts/check-migration-ranges.py` after adding or bumping any
in-repo migration.

## Lean consumers

If a downstream package requires `docs/lean` as `causa` (e.g. a
`kernel/lakefile.toml` with `require causa rev=...`), bump `rev` in the same
commit as the Cargo tag bump — a Proxima tag bump is a dual Rust+Lean bump,
never just one.

Before bumping `rev`, run `python3 scripts/check-lean-axioms.py`. It rebuilds
`docs/lean` and diffs the kernel's current axiom set against
`scripts/lean-axioms.allowlist.txt`. A silent axiom-set change must never be
absorbed into a downstream kernel unnoticed — a reported diff is a
stop-and-review signal, not a rubber stamp.

## Checks before calling an upgrade done

```sh
cargo test -p proxima --lib
cargo test -p proxima-storage-pg --lib
cargo check -p proxima-dev-migrate
cargo clippy -p proxima -p proxima-dev-migrate --all-targets -- -D warnings
python3 scripts/check-architecture-guardrails.py
python3 scripts/check-sql-policy.py
python3 scripts/check-migration-ranges.py
```
