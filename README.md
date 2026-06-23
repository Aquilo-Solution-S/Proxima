# Proxima

We unconsciously perceive reality as it is. The filter we call our perception stems from our intrinsic motivations as well as our conditioning based on past experiences. Here, we refer to reality as facts (F). To abstract facts, we need a perspective (or multiple perspectives) and relate them to goals (G). This gives rise to insights—here, abstractions (A).

Actions that originate from us are indistinguishable from external factors—the only difference is that, in this case, the source can be traced directly back to us. Proxima is based on the idea that consequences can only contribute to the continuous learning and improvement of a system through traceability.

The system is designed in a way to be typesafe at compile time while maintaining a lot of flexibility by providing the possibility to create your own flavors. The word flavor was specifically choosen since domains are mostly defined by certain paradigmas and norms, but your perspective is something personal, like your taste, therefore flavor seemed better to me.

Proxima is the memory/retrieval substrate for agentic systems. It does not compete with Codex, Claude, or other agent harnesses; it gives those harnesses durable typed memory, provenance, owner-scoped retrieval, and app-specific flavor composition.

Users write their own app. The app owns the product, UX, domain workflow, auth boundary, and flavor composition. Proxima is the kernel/foundation underneath it: durable memory, retrieval, provenance, schema discipline, and runtime invariants.

## Start

```sh
docker compose -f docker-compose.dev.yml up -d postgres
export DATABASE_URL=postgres://proxima:proxima@localhost:5434/proxima
cargo run -p proxima-mcp -- --owner-user <uuid> --master-token <uuid>
```

The dev compose file exposes Postgres on `localhost:5434` with pgvector.
The binary default is `postgres://postgres@localhost/proxima_dev`; set
`DATABASE_URL` or pass `--database-url` when using compose.

Headless MCP server at `http://127.0.0.1:31415/mcp`. Agent-harness users use
[`apps/proxima-mcp`](apps/proxima-mcp) or embed
[`crates/mcp-server`](crates/mcp-server).

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
10.

```sh
cargo check --workspace
```

## What `proxima-core` Means

proxima-core is the Rust runtime framework core: the domainless graph contracts, build-time flavor registry, protocol verbs, wake/personality runtime, MCP tool substrate, and storage traits. Applications normally embed it through the `proxima` crate and add domains via flavor crates.

The formal kernel is [`docs/lean/Foundations`](docs/lean/Foundations):
the invariant spec and proof surface, not the Rust crate boundary.

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

Use [`examples/embedded-minimal`](examples/embedded-minimal) as the
wiring template. Runtime env/default semantics live in
[`crates/proxima`](crates/proxima).

Design source of truth:

- [`docs/universe.md`](docs/universe.md) — origin doc. Ontology, the Spinning
  Wheel, philosophical commitments (perspectivist constructivism, abductive
  *causa proxima*), three concrete worlds.
- [`docs/01-event-source.md`](docs/01-event-source.md) — the membrane between
  Reality and the agent.
- [`docs/02-memory.md`](docs/02-memory.md) — core memory entity, strict
  Facts → Abstraction → Perspective layering, edge directionality, operator
  scope (F→A intra-flavor, A→P intra-or-cross-domain with parallel
  operators). All sub-questions resolved.
- [`docs/03-schema-registry.md`](docs/03-schema-registry.md) — Fact
  schemas as compile-time `FactPayload` Rust structs registered at
  startup from flavor crates. Binary-scoped namespace, deploy-time
  migration discipline, on-demand renderers.
- [`docs/04-consolidation.md`](docs/04-consolidation.md) — the two-step
  inclusion model (graph embedding + personality embedding). F→A and A→P
  as set transforms, cycle resolution via edge buffering, supersession.
- [`docs/05-actions.md`](docs/05-actions.md) — actions as ordinary Facts
  emitted through trusted sources / tools. No Action entity.
- [`docs/06-goals-and-self.md`](docs/06-goals-and-self.md) — Goal as
  distinct entity (DAG, supersession-only lifecycle, embedded text
  for retrieval). Self as pure query. Conversation-extraction
  writes Goal + `SYSTEM` action-Fact in one transaction.
  `Owner = Principal` defined in 01: principal (User | Group) is the
  whole scoping primitive; there is no org/tenant field in Core
  (tenancy is a flavor/app concern). Per-memory ACL is a v2+
  extension.
- [`docs/07-storage.md`](docs/07-storage.md) — abstract storage
  layer. ID types, identity rules, core tables, append-only
  discipline. Vector store is **independent** of the entity tables
  (separate schema, separate lifecycle, no FK from entity to
  embedding).
- [`docs/08-core-and-flavors.md`](docs/08-core-and-flavors.md) —
  runtime framework core / flavor layering. Schemas, tools, sources, prompts,
  and operators register at implementation time from flavor
  crates; only *instance* config is runtime. Flavors live under
  `flavors/<name>/`; multi-domain deployments compose via linked
  flavor crates. The `proxima-mcp` host has a default-off `code`
  packaging feature.
- [`docs/09-developing-flavors.md`](docs/09-developing-flavors.md) —
  implementation checklist for flavor crates: typed payload keys,
  sidecar SQL, PG sidecar insert/load, macro registration, bundle
  composition, migrations, MCP tools, and verification.
- [`docs/10-configuration.md`](docs/10-configuration.md) — runtime
  config surface: Postgres / MCP-endpoint / S3 env, MCP authentication
  modes, and an optional host-injected embedding client for retrieval.
  Proxima hosts no model loop — no inference targets or tiers.
- [`docs/11-citations.md`](docs/11-citations.md) — `CitedObject` /
  `CitationMapping` traits; bibliographic provenance; Fact-only
  citation rule.
- [`docs/12-tool-manifest.md`](docs/12-tool-manifest.md) — build-time
  tool vocabulary, core/flavor MCP dispatch, wake-entry detect config,
  and deferred tool-compliance enforcement.
- [`docs/13-compliance.md`](docs/13-compliance.md) — compliance
  primitives: owner deletion, source-scope deletion, pause/resume,
  export, suppression, audit.
- [`docs/14-protocol-surface.md`](docs/14-protocol-surface.md) —
  the engine's contract to clients. Five verbs (Query /
  EventHistory / GoalWrite / EventIngest / Schema), owner-scoped,
  transport-agnostic; operators and tool registry stay inside the
  binary.
- [`docs/dev-perf.md`](docs/dev-perf.md) — perf reducer fixture format.

## Implementation commitment

Rust.

Each deployment is a single binary. The engine, event sources,
actuator interface, memory store, and consolidation operators all
live in one process per deployment. Different deployments build
different binaries from different flavor combinations (08); within
any one binary, no internal split, no feature flags, no plugin
loading.

## License

Apache License, Version 2.0. See [`LICENSE`](LICENSE) for full text and
[`LICENSING.md`](LICENSING.md) for rationale and commercial offerings.
