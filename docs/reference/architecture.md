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
        +--> optional in-process Fact-outbox delivery (`crates/proxima`) -> host `DurableIntake`
```

## Package Map

| Path | Role | Public audience |
|---|---|---|
| `apps/proxima-mcp` | canonical MCP host (code flavor default-on) | operators / agent users |
| `crates/proxima` | facade for host apps | app developers |
| `crates/core` | engine contracts/runtime | flavor authors / maintainers |
| `crates/storage-pg` | Postgres storage | deployers / maintainers |
| `crates/mcp-server` | MCP transport/self-doc | MCP integrators |
| `flavors/code` | code-memory flavor | code-agent deployments |
| `crates/llm-openai-compat` | OpenAI-compatible HTTP `EmbeddingClient`, the reference embedding adapter | hosts that embed for retrieval |
| `crates/blob-s3` | S3-backed cited-blob service (presigned upload, confirm, read) | hosts storing cited blobs |
| `crates/auth-oidc` | OIDC bearer-JWT host authenticator | hosts authenticating MCP callers |
| `crates/pg-testkit` | Postgres clone-per-test harness | maintainers |
| `tools/dev-migrate` | bootstraps a blank Postgres with the substrate and in-repo flavor migrations | developers |
| `tools/dev-idp` | loopback OIDC issuer for self-hosted Proxima | developers |
