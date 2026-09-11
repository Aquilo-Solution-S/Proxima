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
        +--> storage ports -> `crates/storage-pg` -> Postgres + pgvector
        +--> optional embedding client (`crates/llm-openai-compat`)
        +--> optional cited blob service (`crates/blob-s3`)
        +--> optional Fact-outbox publisher (`crates/outbox-nats`) -> NATS JetStream
```

## Package Map

| Path | Role | Public audience |
|---|---|---|
| `apps/proxima-mcp` | canonical MCP host (code flavor default-on) | operators / agent users |
| `crates/proxima` | facade for host apps | app developers |
| `crates/core` | engine contracts/runtime | flavor authors / maintainers |
| `crates/storage-pg` | Postgres storage | deployers / maintainers |
| `crates/mcp-server` | MCP transport/self-doc | MCP integrators |
| `crates/outbox-nats` | optional Fact-outbox JetStream publisher | hosts publishing declared Facts |
| `flavors/code` | code-memory flavor | code-agent deployments |
