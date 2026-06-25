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
2. Record one observation with `core_remember`.
3. Record a derived pattern with `core_derive` over one or more source handles.
4. Read the created memory resource with neighbors expanded.
5. Poll `proxima://events` for changes.

## Hard Law

Agent-authored `core_link` calls cannot link Facts to Facts. Relate Facts by deriving an Abstraction over them.

## Offline Agent Files

- Compact instructions: [llms.txt](https://github.com/Aquilo-Solution-S/Proxima/blob/main/llms.txt)
- Full instructions: [llms-full.txt](https://github.com/Aquilo-Solution-S/Proxima/blob/main/llms-full.txt)
- Agent ritual: [../agent/quickstart.md](../agent/quickstart.md)
