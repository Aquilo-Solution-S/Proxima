# Proxima Overview

## What Proxima Is

Proxima is a Rust memory/retrieval substrate for agentic systems. It stores typed
Facts, derives Abstractions and Perspectives with provenance, tracks Goals, and
serves owner-scoped retrieval through host applications and MCP.

## What Proxima Is Not

- Not an autonomous agent loop.
- Not a replacement for Claude, Codex, or an app-specific UX.
- Not a runtime plugin marketplace; flavors are linked at build time.
- Not a compliance guarantee beyond the current primitives/status tables.

## Runtime Shape

```text
host app / proxima-mcp
        |
        v
proxima facade -> proxima-core engine -> storage-pg -> Postgres + pgvector
        |
        +-> optional flavor crates
        +-> optional embedding client
        +-> optional cited blob service
```

## Next Documents

- Run locally: [local-dev.md](local-dev.md)
- Connect an agent: [connect-agent.md](connect-agent.md)
- Run the MCP host: [../tutorials/run-mcp-server.md](../tutorials/run-mcp-server.md)
- Build a flavor: [../tutorials/build-first-flavor.md](../tutorials/build-first-flavor.md)
