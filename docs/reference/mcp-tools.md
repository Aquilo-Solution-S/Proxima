# MCP Tools and Resources

## Authority

The live server is authoritative. Use `tools/list`, `resources/list`,
`resources/templates/list`, and `proxima://tools` for exact schemas in the current
binary/profile.

## Substrate Surface

| Area | Examples | Notes |
|---|---|---|
| memory | `core_search_memories`, `core_remember`, `core_derive`, `core_interpret` | no tool writes an edge; `core_derive` lands `origin` entries from `source_handles`, `core_interpret` authors an interpretation Perspective and returns a `P:` handle |
| spaces | `core_memory_spaces` | server-issued owner selectors; selectors are not authority |
| goals | `core_goal` | advertised only when profile includes goals |
| citations | citation/fact resources and tools | Facts only carry citation mappings |
| membership | `core_membership` | group roster only: `add_member`, `remove_member`, `list_members` |
| transfer | `core_transfer` | memory owner transfer via `transfer_to_owner`; requires `entity` plus a `to_owner` group key, admin on both sides; not ACL/share; goals do not transfer |
| memory reads | `proxima://memory/{id}{?expand_neighbors}`, `proxima://memories{?ids}`, `proxima://memory/{id}/lineage{?direction,depth,limit,cursor}` | batch read takes at most 100 ids; lineage paginates by cursor and reports `has_more` |
| goal reads | `proxima://goals{?state,limit,cursor}`, `proxima://goal/{id}` | keyset pagination |
| pin walks | lineage / neighbor expansion | `origins` / `refs` on `memory` |
| graph | `proxima://graph` | owner-scoped health: schema registry, embedding backlog, `embeddings_client_configured` |
| introspection | `proxima://tools`, `proxima://how-to` | generated from runtime profile |
| change-events | `proxima://change-events{?since,limit}` | poll-only change notification |
| wake-candidates | `proxima://wake-candidates{?fact,limit}` | armed Active Goals admitted for one trigger Fact; arm via `core_goal` `wake`/`clear_wake` |

## Offline Catalog

`mcp-catalog.example.json` is an example snapshot for readers and tests. It is
not authoritative for a running binary.
