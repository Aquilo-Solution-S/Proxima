<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/proxima-seal-dark.svg">
    <img alt="Proxima" src="docs/assets/proxima-seal-light.svg" width="128" height="128">
  </picture>
</p>

# Proxima

Proxima is a typed, owner-scoped durable memory substrate for agentic systems.
It gives host applications and coding agents persistent Facts, Abstractions,
Perspectives, Goals, citations, retrieval, and provenance without owning the
product UX or model loop.

## Use Proxima When

| You are... | Start with |
|---|---|
| Running the MCP memory server locally | [docs/getting-started/local-dev.md](docs/getting-started/local-dev.md) |
| Connecting a coding agent | [docs/getting-started/connect-agent.md](docs/getting-started/connect-agent.md) |
| Embedding Proxima in a Rust host | [examples/embedded-minimal](examples/embedded-minimal) |
| Building a flavor | [docs/tutorials/build-first-flavor.md](docs/tutorials/build-first-flavor.md) |
| Checking invariants/design | [docs/README.md](docs/README.md) |
| Building the docs site | [docs/README.md](docs/README.md#docs-site) |

## Five-Minute Local Start

```sh
docker compose -f docker-compose.dev.yml up -d --wait postgres
export DATABASE_URL=postgres://proxima:proxima@localhost:5434/proxima
cargo run -p proxima-mcp -- --owner-user <uuid> --master-token <uuid>
```

The dev compose file exposes Postgres on `localhost:5434` with pgvector.
The binary default is `postgres://postgres@localhost/proxima_dev`; set
`DATABASE_URL` or pass `--database-url` when using compose.

Headless MCP server at `http://127.0.0.1:31415/mcp`. Agent-harness users use
[`apps/proxima-mcp`](apps/proxima-mcp) or embed
[`crates/mcp-server`](crates/mcp-server). See
[`docs/getting-started/local-dev.md`](docs/getting-started/local-dev.md) for
the full local walkthrough.

## Connecting Your Coding Agent To Proxima

Start the MCP server:

```sh
cargo run -p proxima-mcp -- --owner-user <uuid> --master-token <uuid>
```

Client config:

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

`PROXIMA_MCP_BIND` overrides the listener address. Non-loopback binds
require `PROXIMA_EXPOSE_NETWORK=true` plus the auth/origin/host gates in
[`docs/10-configuration.md`](docs/10-configuration.md) and
[`docs/15-deployment.md`](docs/15-deployment.md).

```sh
cargo check --workspace
```

Agent-specific setup and copy/paste prompts live in
[`docs/getting-started/connect-agent.md`](docs/getting-started/connect-agent.md),
[`docs/agent/quickstart.md`](docs/agent/quickstart.md),
[`llms.txt`](llms.txt), and [`llms-full.txt`](llms-full.txt).

## What `proxima-core` Means

proxima-core is the Rust runtime framework core: the domainless graph contracts,
build-time flavor registry, protocol verbs, wake/personality runtime, MCP tool
substrate, and storage ports. Applications normally embed it through the
`proxima` crate and add domains via flavor crates.

The formal kernel is [`docs/lean/Causa`](docs/lean/Causa): the
invariant spec and proof surface, not the Rust crate boundary.

## Embedding Proxima

Host apps use the `proxima` framework facade rather than assembling
`proxima-core` directly:

```rust
proxima::run::<App>().await?;

Proxima::<App>::app()
    .from_env()
    .authenticator(auth)
    .run()
    .await?;
```

Use [`examples/embedded-minimal`](examples/embedded-minimal) as the wiring
template. Runtime env/default semantics live in
[`crates/proxima`](crates/proxima).

## Design and Kernel Authority

The Lean kernel in [`docs/lean/Causa`](docs/lean/Causa) is the
source of truth for domainless invariants. The numbered Markdown docs are the
human-readable design/reference layer. When prose, code, and Lean disagree on a
domainless invariant, Lean wins until the decision is renegotiated in writing.

Status labels used below:

| Label | Meaning |
|---|---|
| `current` | Describes implemented behavior or enforced contract. |
| `current + deferred sections` | Mostly current, with explicit deferred rows/sections. |
| `design intent` | Target contract; not a full implementation claim. |
| `current + design rationale` | Current invariant or contract plus rationale/commentary. |
| `current implementation guide` | Current contributor-facing build checklist. |
| `current deployment guide` | Current deployment behavior and operator guidance. |
| `current developer fixture note` | Current developer-only fixture documentation. |

- [`docs/universe.md`](docs/universe.md) — **design intent**. Origin doc:
  ontology, the Spinning Wheel, philosophical commitments, and three concrete
  worlds.
- [`docs/01-event-source.md`](docs/01-event-source.md) — **current + design
  rationale**. Event sources, owner scoping, and the membrane between Reality
  and the agent.
- [`docs/02-memory.md`](docs/02-memory.md) — **current + design rationale**.
  Core memory entity, strict Facts → Abstraction → Perspective layering, edge
  directionality, operator scope, and read-scope matrix.
- [`docs/03-schema-registry.md`](docs/03-schema-registry.md) — **current +
  design rationale**. Compile-time payload traits, sidecars, registrations,
  renderers, and migration discipline.
- [`docs/04-consolidation.md`](docs/04-consolidation.md) — **current + deferred
  sections**. F→A and A→P set transforms, prompt locality, source-batch
  lifecycle, supersession, and deferred enforcement notes.
- [`docs/05-actions.md`](docs/05-actions.md) — **current + design rationale**.
  Actions as ordinary Facts emitted through trusted sources/tools.
- [`docs/06-goals-and-self.md`](docs/06-goals-and-self.md) — **current + design
  rationale**. Goal entity, supersession-only lifecycle, and Self as pure query.
- [`docs/07-storage.md`](docs/07-storage.md) — **current + design rationale**.
  Storage abstractions, ID types, identity rules, append-only discipline, and
  independent vector-store lifecycle.
- [`docs/08-core-and-flavors.md`](docs/08-core-and-flavors.md) — **current +
  design rationale**. Core/flavor layering, build-time registration, and
  default-off code-flavor packaging.
- [`docs/09-developing-flavors.md`](docs/09-developing-flavors.md) — **current
  implementation guide**. Flavor author checklist for typed keys, sidecars,
  registration, migrations, and tools.
- [`docs/10-configuration.md`](docs/10-configuration.md) — **current**. Runtime
  config surface for Postgres, MCP, S3, auth, tool profiles, and embeddings.
- [`docs/11-citations.md`](docs/11-citations.md) — **current + design
  rationale**. CitedObject/CitationMapping traits, bibliographic provenance,
  and Fact-only citation rule.
- [`docs/12-tool-manifest.md`](docs/12-tool-manifest.md) — **current + deferred
  sections**. Build-time tool vocabulary, MCP dispatch, wake-entry detection,
  and deferred compliance enforcement.
- [`docs/13-compliance.md`](docs/13-compliance.md) — **design intent + current
  primitive inventory**. Owner deletion, source-scope deletion, pause/resume,
  export, suppression, and audit primitives.
- [`docs/14-protocol-surface.md`](docs/14-protocol-surface.md) — **current +
  deferred sections**. Query, EventHistory, GoalWrite, EventIngest, and Schema
  verbs; owner-scoped and transport-agnostic.
- [`docs/15-deployment.md`](docs/15-deployment.md) — **current deployment
  guide**. Code-flavor MCP deployment, Docker, OIDC bearer auth, network
  exposure, and tool-surface profiles.
- [`docs/dev-perf.md`](docs/dev-perf.md) — **current developer fixture note**.
  Perf reducer fixture format.

## Design Background

We unconsciously perceive reality as it is. The filter we call our perception
stems from our intrinsic motivations as well as our conditioning based on past
experiences. Here, we refer to reality as facts (F). To abstract facts, we need
a perspective (or multiple perspectives) and relate them to goals (G). This
gives rise to insights—here, abstractions (A).

Actions that originate from us are indistinguishable from external factors—the
only difference is that, in this case, the source can be traced directly back to
us. Proxima is based on the idea that consequences can only contribute to the
continuous learning and improvement of a system through traceability.

The system is designed to be type-safe at compile time while maintaining
flexibility through custom flavors. The word flavor is intentional: domains are
mostly defined by paradigms and norms, while perspective is personal, like
taste.

See [`docs/universe.md`](docs/universe.md) for the full design background.

## Implementation Commitment

Rust.

Each deployment is a single binary. The engine, event sources, actuator
interface, memory store, and consolidation operators all live in one process per
deployment. Different deployments build different binaries from different
flavor combinations (08); within any one binary, no internal split, no feature
flags, no plugin loading.

## License

Apache License, Version 2.0. See [`LICENSE`](LICENSE) for full text and
[`LICENSING.md`](LICENSING.md) for rationale and commercial offerings.
