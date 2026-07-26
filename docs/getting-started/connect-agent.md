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

With `--features code`, the server exposes code-as-memory tools. Register a
local checkout and ingest its current tree:

1. `proxima-code_register_repo` with `path` — returns a `repo_handle`
   (`R:<uuid>`).
2. `proxima-code_ingest_head_snapshot` with that `repo_handle` — emits one
   `file-revision-v1` Fact per tracked file and `code-chunk-v1` Abstractions
   per parsed chunk.
3. `proxima-mcp maintain-embeddings --missing-only --drain` — ingest does not
   enqueue embedding jobs itself, so run this before expecting semantic
   results. Lexical `proxima-code_search_chunks` works immediately.

Then search with `proxima-code_search_chunks`, `proxima-code_search_commits`,
and read exact revisions with `proxima-code_open_file_revision`.

`proxima-code_search_chunks` is lexical and conjunctive — every term must
match. Search it with identifiers and paths (`lexical_tsv`,
`common_candidates_sql`, `crates/storage-pg/src/verbs`), not with
natural-language questions.

## First Calls

1. Connect and read `initialize.instructions`.
2. Call `tools/list`.
3. Call `resources/list` and `resources/templates/list`.
4. Read `proxima://how-to`.
5. Read `proxima://tools`.

## First Memory Flow

1. Search with `core_search_memories`.
2. In multi-space hosts, call `core_memory_spaces` before durable memory writes. Use a returned `space` key in `core_remember`, `core_record_utterance`, `core_search_memories`, `core_derive`, and `core_link`; hydrate a memory through `proxima://memory/{id}`. Omitted `space` preserves the current bound owner.
3. Record one observation with `core_remember`.
4. Record a derived pattern with `core_derive` over one or more source handles.
5. Read the created memory resource with neighbors expanded.
6. Poll `proxima://change-events` for changes.


## Hard Law

Agent-authored `core_link` calls cannot link Facts to Facts. Relate Facts by deriving an Abstraction over them.

## Offline Agent Files

- Compact instructions: [llms.txt](https://github.com/Aquilo-Solution-S/Proxima/blob/main/llms.txt)
- Full instructions: [llms-full.txt](https://github.com/Aquilo-Solution-S/Proxima/blob/main/llms-full.txt)
- Agent ritual: [../agent/quickstart.md](../agent/quickstart.md)
