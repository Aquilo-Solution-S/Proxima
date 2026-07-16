# MCP Tools and Resources

## Authority

The live server is authoritative. Use `tools/list`, `resources/list`,
`resources/templates/list`, and `proxima://tools` for exact schemas in the current
binary/profile.

## Substrate Surface

| Area | Examples | Notes |
|---|---|---|
| memory | `core_search_memories`, `core_remember`, `core_derive`, `core_link` | agent-authored `core_link` Fact→Fact links are rejected; derive instead |
| spaces | `core_memory_spaces` | server-issued owner selectors; selectors are not authority |
| goals | `core_goal` | advertised only when profile includes goals |
| citations | citation/fact resources and tools | Facts only carry citation mappings |
| membership | `core_membership` | group roster only: `add_member`, `remove_member`, `list_members` |
| publish | `core_publish` | irreversible owner transfer via `publish_to_world`; not ACL/share |
| introspection | `proxima://tools`, `proxima://how-to` | generated from runtime profile |
| change-events | `proxima://change-events{?since,limit}` | poll-only change notification |
| wake-candidates | `proxima://wake-candidates{?fact,limit}` | armed Active Goals admitted for one trigger Fact; arm via `core_goal` `wake`/`clear_wake` |

## Offline Catalog

`mcp-catalog.example.json` is an example snapshot for readers and tests. It is
not authoritative for a running binary.
