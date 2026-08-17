<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/proxima-seal-dark.svg">
    <img alt="Proxima" src="docs/assets/proxima-seal-light.svg" width="128" height="128">
  </picture>
</p>

# Proxima

Typed, owner-scoped durable memory for agentic systems. Hosts and coding
agents get persistent Facts, Abstractions, Perspectives, Goals, citations,
retrieval, and provenance. Proxima does not own the product UX or the
model loop.

Identity is timeseries: `(handle, t)`. `handle` is the series. `t` is this
version (uuidv7, the row id). There is no Edge table — pins live on the
node (`origins[]` / `refs[]`). Schema is hard-cut
`crates/storage-pg/migrations/0001_v008.sql`.

## Use Proxima When

| You are... | Start with |
|---|---|
| Running the MCP memory server locally | [docs/getting-started/local-dev.md](docs/getting-started/local-dev.md) |
| Connecting a coding agent | [docs/getting-started/connect-agent.md](docs/getting-started/connect-agent.md) |
| Embedding Proxima in a Rust host | [crates/proxima](crates/proxima) |
| Building a flavor | [docs/tutorials/build-first-flavor.md](docs/tutorials/build-first-flavor.md) |
| Checking invariants / design | [docs/README.md](docs/README.md) |
| Building the docs site | [docs/README.md](docs/README.md#docs-site) |

## Five-Minute Local Start

Your machine, your Postgres, your embedding model. No hosted service.

```sh
# 1. Postgres with pgvector
docker compose -f docker-compose.dev.yml up -d --wait postgres

# 2. A local OIDC issuer. One auth path: RS256 bearer vs JWKS.
#    Local means a local issuer, not a bypass. Prints env + client config.
cargo run -p proxima-dev-idp

# 3. In another shell, paste what step 2 printed, then:
export DATABASE_URL=postgres://proxima:proxima@localhost:5434/proxima
export PROXIMA_TOOL_PROFILE=full
cargo run -p proxima-mcp
```

Headless MCP at `http://127.0.0.1:31415/mcp`. Code flavor is default-on;
`--no-default-features` is substrate-only.

Semantic search needs an OpenAI-compatible `/embeddings` endpoint
(1024-dim). Local, no API key:

```sh
ollama pull qwen3-embedding:0.6b
export PROXIMA_EMBED_BASE_URL=http://127.0.0.1:11434/v1
export PROXIMA_EMBED_MODEL=qwen3-embedding:0.6b
```

Without an embedding client the server starts degraded: lexical search
works; semantic and hybrid report the missing capability.

Full walkthrough: [docs/getting-started/local-dev.md](docs/getting-started/local-dev.md).
Hosted embed providers: [docs/10-configuration.md](docs/10-configuration.md).

## Connecting Your Coding Agent To Proxima

`cargo run -p proxima-dev-idp` prints a ready-to-paste command. Claude Code:

```sh
claude mcp add --transport http proxima http://127.0.0.1:31415/mcp \
  --header "Authorization: Bearer <token-from-dev-idp>" \
  --header "X-Proxima-Owner: personal:<user-id-from-dev-idp>"
```

JSON, for clients that take a config file:

```json
{
  "mcpServers": {
    "proxima": {
      "url": "http://127.0.0.1:31415/mcp",
      "headers": {
        "Authorization": "Bearer <oidc-access-token>",
        "X-Proxima-Owner": "personal:<user-uuid>"
      }
    }
  }
}
```

`X-Proxima-Owner` is required on MCP `initialize`. The server binds that
owner to the returned `Mcp-Session-Id` and rechecks authority on every
request. `PROXIMA_MCP_BIND` overrides the listener. Non-loopback binds
require `PROXIMA_EXPOSE_NETWORK=true` plus the gates in
[docs/10-configuration.md](docs/10-configuration.md) and
[docs/15-deployment.md](docs/15-deployment.md).

In production, replace `dev-idp` with your issuer (Entra, Zitadel, Auth0,
any standard JWKS). The server verifies both the same way.

Agent setup and prompts: [docs/getting-started/connect-agent.md](docs/getting-started/connect-agent.md),
[docs/agent/quickstart.md](docs/agent/quickstart.md),
[llms.txt](llms.txt), [llms-full.txt](llms-full.txt).

## Graph

| Kind | What | Produced by |
|---|---|---|
| Fact | Admitted observation. Never revised. | FactIngest |
| Abstraction | Re-derivable interpretation over Facts. | F→A |
| Perspective | Re-derivable integration over Abstractions. | A→P |
| Goal | Desired end-state. Lifecycle is supersession. | GoalWrite |

Self is a query, not a row. Citation is Fact ∪ Abstraction only.

Pins are node content. Two kinds, kind follows the operation, no verb
writes a pin. See [docs/16-edges.md](docs/16-edges.md).

| Array | Statement |
|---|---|
| `origins[]` | made-from (`derived_from`) |
| `refs[]` | points-at (`references()` on the payload) |

Engine contract to clients: Query / ChangeHistory / GoalWrite /
FactIngest / Schema. Owner-scoped, transport-agnostic
([docs/14-protocol-surface.md](docs/14-protocol-surface.md)). MCP tools
are thin callers of those verbs. Forget cools; erase is abandonment-only.

Reads are Tesla-valve: sidecar / index → admit → project in Rust. No
raw flavor SQL against `proxima_core.*`.

## Host And Flavor Tiers

| Tier | Import | Contract |
|---|---|---|
| Host API | `use proxima::{Proxima, RuntimeBuilder, Engine};` | compose/run a binary; call graph verbs; server-resolved `AuthzContext` |
| Flavor SDK | `use proxima::flavor::{FlavorBundle, FlavorRegistry, FactPayload, pg_sidecar};` | build-time schemas/tools/sidecars; no `PgPool`, no core-table SQL |

```rust
proxima::run::<App>().await?;

Proxima::<App>::app()
    .from_env()
    .authenticator(auth)
    .run()
    .await?;
```

Wiring template: [apps/proxima-mcp](apps/proxima-mcp). Env/defaults:
[crates/proxima](crates/proxima).

## Authority

The Lean kernel at [docs/lean/Causa](docs/lean/Causa) is the source of
truth for domainless invariants (F/A/P layering, pins, operators, owner
scoping, goals, citations, compliance). Numbered docs under `docs/` are
rationale. When code or prose disagrees with the kernel, the kernel wins
until renegotiated in writing.

Index: [docs/README.md](docs/README.md). Origin ontology:
[docs/universe.md](docs/universe.md).

## Implementation

Rust. One binary per deployment: Engine, storage, transports, and the
flavor crates linked at build time. No runtime registry, no plugin
tier, no in-process flavor catalog.

Existing databases on a pre-v0.0.8 ledger reset. There is no in-place
ALTER lane from the prior schema.

## License

Apache License, Version 2.0. See [LICENSE](LICENSE) and
[LICENSING.md](LICENSING.md).
