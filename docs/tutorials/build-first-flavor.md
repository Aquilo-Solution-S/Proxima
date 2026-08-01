# Build Your First Flavor

## Goal

Add one typed Fact schema, one sidecar table, one registration path, and one MCP
tool surface in a host/flavor without violating Proxima invariants.

## Path

1. [Add a Fact schema](add-first-fact-schema.md)
2. [Add an MCP tool](add-first-mcp-tool.md)
3. Check the full reference: [../09-developing-flavors.md](../09-developing-flavors.md)

## Guardrails

- Schemas, tools, sources, and prompts are build-time registered.
- Facts are immutable observations.
- A/P payloads require typed sidecars.
- Connections are not registered vocabulary: a payload declares `references()`
  and ingest writes one index row per declaration. No tool writes an edge.
- An edge is always owned by its source.
