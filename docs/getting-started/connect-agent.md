# Connect a Coding Agent

## Start Server

Follow [local-dev.md](local-dev.md). `cargo run -p proxima-dev-idp` prints a
ready-to-paste command for Claude Code:

```sh
claude mcp add --transport http proxima http://127.0.0.1:31415/mcp \
  --header "Authorization: Bearer <token-from-dev-idp>" \
  --header "X-Proxima-Owner: personal:<user-id-from-dev-idp>"
```

Equivalent JSON, for clients that take a config file:

```json
{
  "mcpServers": {
    "proxima": {
      "url": "http://127.0.0.1:31415/mcp",
      "headers": {
        "Authorization": "Bearer <oidc-access-token>",
        "X-Proxima-Owner": "personal:<user-id>"
      }
    }
  }
}
```

Both headers are required. The bearer must come from the issuer named in
`PROXIMA_OIDC_ISSUER`; `X-Proxima-Owner` selects which owner the session is
bound to and is rechecked on every request.

## Index A Repository

The default host includes code-as-memory tools. Register a
local checkout and ingest its current tree:

1. `proxima-code_register_repo` with `path` — returns a `repo_handle`
   (`R:<uuid>`).
2. `proxima-code_ingest_head_snapshot` with that `repo_handle` — emits one
   `file-revision-v1` Fact per tracked file and `code-chunk-v1` Abstractions
   per parsed chunk.
   The response reports `embeddings_enqueued`: ingest enqueues embedding jobs
   for the owner, and the server drains them in the background. Lexical
   `proxima-code_search_chunks` works immediately; semantic results appear as
   the backlog drains (watch `proxima://graph`). With no embedding client
   configured this is `0` and the deployment stays lexical-only.

Then search with `proxima-code_search_chunks`, `proxima-code_search_commits`,
and read exact revisions with `proxima-code_open_file_revision`.

`proxima-code_search_chunks` takes both shapes of query. An identifier or
path (`lexical_tsv`, `common_candidates_sql`,
`crates/storage-pg/src/verbs`) matches as a substring and outranks everything
else. A plain-English question ("how does the chunker decide how big a chunk
should be") matches on shared content words: chunks containing all of them
rank above chunks containing some, so a question returns its best candidates
rather than nothing.

It ranks in one of three `mode`s, defaulting to `hybrid`: full-text and
embedding similarity fused by reciprocal rank. `lexical` and `semantic`
select a single arm. With no embedding model configured — or before the
backlog above has drained — `hybrid` ranks lexically and reports
`degraded_to_lexical: true` rather than quietly returning less; `semantic`
fails outright, since it has no other arm to fall back on.

To re-index a repository from scratch — which v0.0.7 requires of indexes
built by an earlier version, since chunking and rendering both changed —
erase it and ingest again:

1. `proxima-code_erase_repo` with `repo_handle` and `confirm_canonical_path`
   (the repo's exact stored path; a mismatch is refused). This is
   irreversible and removes every Fact, Abstraction, edge and embedding
   derived from that repository.
2. `proxima-code_register_repo`, then `proxima-code_ingest_head_snapshot`.

A HEAD snapshot alone will not do it: it re-derives only files whose content
changed, so files that have not moved keep their old chunks.

## First Calls

1. Connect and read `initialize.instructions`.
2. Call `tools/list`.
3. Call `resources/list` and `resources/templates/list`.
4. Read `proxima://how-to`.
5. Read `proxima://tools`.

## First Memory Flow

1. Search with `core_search_memories`.
2. In multi-space hosts, call `core_memory_spaces` before durable memory writes. Use a returned `space` key in `core_remember`, `core_record_utterance`, `core_search_memories`, `core_derive`, and `core_interpret`; hydrate a memory through `proxima://memory/{id}`. Omitted `space` preserves the current bound owner.
3. Record one observation with `core_remember`.
4. Record a derived pattern with `core_derive` over one or more source handles.
5. Read the created memory resource with neighbors expanded.
6. Poll `proxima://change-events` for changes.


## Hard Law

No tool writes a connection. Every edge follows from what a node says: an
`origin` entry from the handles a write declares it was made from, a `reference`
entry from a schema-declared payload field. Relate Facts by deriving an
Abstraction over them; claim what existing memories mean with `core_interpret`,
which authors an interpretation Perspective rather than an edge.

## Offline Agent Files

- Compact instructions: [llms.txt](https://github.com/Aquilo-Solution-S/Proxima/blob/main/llms.txt)
- Full instructions: [llms-full.txt](https://github.com/Aquilo-Solution-S/Proxima/blob/main/llms-full.txt)
- Agent ritual: [../agent/quickstart.md](../agent/quickstart.md)
