# Connect a Coding Agent

## Start Server

Follow [local-dev.md](local-dev.md), then configure your MCP client:

```json
{
  "mcpServers": {
    "proxima": {
      "url": "http://127.0.0.1:31415/mcp",
      "headers": {
        "Authorization": "Bearer pxm_<token>"
      }
    }
  }
}
```

For the local quickstart, use the generated `MASTER_TOKEN` as `pxm_$MASTER_TOKEN`.

## First Calls

1. Connect and read `initialize.instructions`.
2. Call `tools/list`.
3. Call `resources/list` and `resources/templates/list`.
4. Read `proxima://how-to`.
5. Read `proxima://tools`.

## First Memory Flow

1. Search with `core_search_memories`.
2. In multi-space hosts, call `core_memory_spaces` before durable memory writes. Use a returned `space` key in `core_remember`, `core_search_memories`, `core_get_memory`, and `core_publish_memory`. Omitted `space` preserves the current owner behavior for single-owner deployments.
3. Record one observation with `core_remember`.
4. Record a derived pattern with `core_derive` over one or more source handles.
5. Read the created memory resource with neighbors expanded.
6. Poll `proxima://change-events` for changes.

`core_publish_memory` v1 copies only `core/agent-note-v1`; flavor-specific publish is a host/flavor concern until typed replay is designed.

## Hard Law

Agent-authored `core_link` calls cannot link Facts to Facts. Relate Facts by deriving an Abstraction over them.

## Offline Agent Files

- Compact instructions: [llms.txt](https://github.com/Aquilo-Solution-S/Proxima/blob/main/llms.txt)
- Full instructions: [llms-full.txt](https://github.com/Aquilo-Solution-S/Proxima/blob/main/llms-full.txt)
- Agent ritual: [../agent/quickstart.md](../agent/quickstart.md)
