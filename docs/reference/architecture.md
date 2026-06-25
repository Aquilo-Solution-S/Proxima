# Architecture Reference

## Runtime Composition

```text
agent / app client
        |
        v
MCP HTTP transport or embedded API
        |
        v
proxima facade (`crates/proxima`)
        |
        v
core engine (`crates/core`)
        |
        +--> flavor bundle(s) (`flavors/*`)
        +--> storage trait -> `crates/storage-pg` -> Postgres + pgvector
        +--> optional embedding client (`crates/llm-openai-compat`)
        +--> optional cited blob service (`crates/blob-s3`)
```

## Package Map

| Path | Role | Public audience |
|---|---|---|
| `apps/proxima-mcp` | headless MCP host binary | operators / agent users |
| `crates/proxima` | facade for host apps | app developers |
| `crates/core` | engine contracts/runtime | flavor authors / maintainers |
| `crates/storage-pg` | Postgres storage | deployers / maintainers |
| `crates/mcp-server` | MCP transport/self-doc | MCP integrators |
| `flavors/code` | code-memory flavor | code-agent deployments |
| `examples/embedded-minimal` | minimal host template | new app developers |
