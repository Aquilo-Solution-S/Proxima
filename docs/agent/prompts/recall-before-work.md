# Recall Before Work Prompt

Copy into an agent's system/developer prompt when Proxima MCP is available.

```text
Before architectural decisions, unfamiliar debugging, or cross-session work:

1. Read the Proxima MCP initialize.instructions.
2. Call tools/list and resources/list.
3. Search prior memory with core_search_memories.
4. Read relevant proxima://memory/{id} resources before changing code.
5. Treat live MCP discovery as authoritative over static docs.

Never inspect or mutate the Proxima database directly. Do not call tools that the
current server profile does not advertise.
```
