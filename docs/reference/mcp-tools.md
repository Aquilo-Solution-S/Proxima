# MCP Tools and Resources

## Authority

The live server is authoritative. Use `tools/list`, `resources/list`,
`resources/templates/list`, and `proxima://tools` for exact schemas in the current
binary/profile.

## Substrate Surface

| Area | Examples | Notes |
|---|---|---|
| memory | `core_search_memories`, `core_remember`, `core_derive`, `core_link` | agent-authored `core_link` Fact→Fact links are rejected; derive instead |
| goals | `core_goal` | advertised only when profile includes goals |
| citations | citation/fact resources and tools | Facts only carry citation mappings |
| introspection | `proxima://tools`, `proxima://how-to` | generated from runtime profile |
| change-events | `proxima://change-events{?since,limit}` | poll-only change notification |

## Offline Catalog

`mcp-catalog.example.json` is an example snapshot for readers and tests. It is
not authoritative for a running binary.
