# Proxima

The causal chain engine for Aquilo.

## Name

Two truths, both load-bearing.

1. **Proxima Centauri** — the next nearest star to the sun. Continues the
   Aquilo star-themed family.
2. ***Causa proxima*** — the nearest abductively plausible cause that
   meta-reflection can reach. *Not* the legal/Thomist "immediate cause"
   sense; see `docs/universe.md §3` for the redefinition, the lineage
   (Hume → Peirce → van Fraassen → Pearl), and the perspectivist-
   constructivist position the engine encodes as a hard invariant.

## What this is

A standalone engine for storing, traversing, and consolidating **causal
chains** — the network of Facts, Abstractions, and Perspectives that an
agent builds about a Reality over time.

The engine is domain-agnostic; only the **Event Sources** and the
**Actuators** differ between flavors. For the ontology, the Spinning
Wheel, the worked Code / Learning / Jurisdiction walkthroughs, and the
philosophical commitments, see [`docs/universe.md`](docs/universe.md).
For what must not slip, see [`AGENTS.md`](AGENTS.md).

## What this is not

- Not a memory store retrofitted with a graph. It is the wheel
  (`docs/universe.md §2`) implemented cold-start.
- Not a port of hippocampus. Hippocampus becomes one consumer of Proxima.
- Not a research project. It is a buildable system. The core crates,
  Postgres storage, gRPC wire layer, Code flavor, and Solid/Tauri shell
  are in-tree.

## Status

Implementation phase. Build path to a locally-runnable Code demo.

```
proxima/
├── apps/
│   ├── proxima-engine/      Rust engine binary
│   ├── proxima-code/        Rust Code-flavor binary
│   ├── proxima-mcp/         Rust headless MCP host binary (substrate + goal)
│   └── proxima-shell/       Solid + Vite + Tauri 2 shell
│       └── src-tauri/       Tauri Rust crate
├── crates/
│   ├── core/                Rust lib crate `proxima-core`
│   ├── mcp-server/          MCP HTTP listener (`proxima_mcp_server`)
│   ├── storage-pg/          Postgres storage adapter + migrations
│   ├── wire-grpc/           gRPC wire crate
│   └── llm-openai-compat/   OpenAI-compatible model client
├── packages/
│   └── frontend-core/       npm package `@proxima/core`
├── flavors/
│   ├── code/                Rust Code flavor crate
│   ├── goal/                Rust Goal flavor crate
│   └── mcp/                 Rust MCP substrate flavor crate
├── proto/                   Proxima v1 protobuf surface
├── docs/                    design source of truth
├── Cargo.toml               Rust workspace
└── pnpm-workspace.yaml      frontend workspace
```

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
- [`docs/05-actions.md`](docs/05-actions.md) — actions as `system`-source
  events. No new entity; collapses onto 01 + 03 plus a tool registry.
- [`docs/06-goals-and-self.md`](docs/06-goals-and-self.md) — Goal as
  distinct entity (DAG, supersession-only lifecycle, embedded text
  for retrieval). Self as pure query. Conversation-extraction
  writes Goal + `SYSTEM` action-Fact in one transaction.
  `Owner = (Principal, OrgId)` defined in 01: principal
  (User | Group) for access, org_id for billing. Per-memory ACL is a
  v2+ extension.
- [`docs/07-storage.md`](docs/07-storage.md) — abstract storage
  layer. ID types, identity rules, core tables, append-only
  discipline. Vector store is **independent** of the entity tables
  (separate schema, separate lifecycle, no FK from entity to
  embedding).
- [`docs/08-core-and-flavors.md`](docs/08-core-and-flavors.md) —
  bare-core / flavor layering. Schemas, tools, sources, prompts,
  and operators register at implementation time from flavor
  crates; only *instance* config is runtime. Flavors live under
  `flavors/<name>/`; multi-domain deployments compose via a
  composite crate. No feature flags — the flavor crate is the unit
  of inclusion.
- [`docs/09-frontend.md`](docs/09-frontend.md) — frontend & client
  model. Tauri 2 + Solid; one UI codebase across web, desktop,
  iOS, Android. gRPC server-streaming subscriptions; SQLite +
  sqlite-vec local cache. Schema-aware components via
  `.proto → buf → codegen`. Per-flavor frontend packages composed
  into the shell at build time. Optional embedded-engine mode for
  desktop power users.
- [`docs/10-configuration.md`](docs/10-configuration.md) — runtime
  config surface: model tiers (`Fast`/`Standard`/`Deep`) declared
  per operator, mapped per Owner; build-time `(vendor, model_id)`
  registry with capability validation; per-Owner BYOK credential
  table with secret-ref indirection; operator concurrency and
  binary-wide cost cap.
- [`docs/11-citations.md`](docs/11-citations.md) — `CitedObject` /
  `CitationMapping` traits; bibliographic provenance; Fact-only
  citation rule.
- [`docs/12-tool-manifest.md`](docs/12-tool-manifest.md) — T1
  (runtime, schema-consuming) vs T2 (build-time flavors) tool
  tiers.
- [`docs/13-flavor-marketplace.md`](docs/13-flavor-marketplace.md) —
  substrate + reference flavors, independent flavor authorship,
  composite discipline.
- [`docs/14-protocol-surface.md`](docs/14-protocol-surface.md) —
  the engine's contract to clients. Five verbs (Query / Subscribe /
  GoalWrite / EventIngest / Schema), owner-scoped, transport-agnostic;
  decider, operators, and tool registry stay inside the binary.
- [`docs/15-compliance.md`](docs/15-compliance.md) — compliance
  primitives: owner deletion, pause/resume, export, suppression, audit.
- [`docs/dev-perf.md`](docs/dev-perf.md) — dev-time perf instrumentation:
  per-session artifact layout under `apps/proxima-shell/perf-logs/`,
  IPC / MCP / engine / Postgres capture, opt-out via `PROXIMA_PERF=0`.

## Verification

Use the smallest relevant check.

| Surface | Command |
|---|---|
| Rust workspace | `cargo check --workspace` |
| Rust lint | `cargo clippy --workspace --all-targets` |
| Core frontend | `pnpm --filter @proxima/core typecheck` |
| Shell frontend | `pnpm --filter proxima-shell typecheck` |
| Shell build | `pnpm --filter proxima-shell build` |

Frontend dev server:

```sh
pnpm --filter proxima-shell dev --host 127.0.0.1
```

### Connecting Your Coding Agent To Proxima

Proxima Shell auto-starts a Streamable HTTP MCP server when the
desktop app is running:

```text
http://localhost:31415/mcp
```

Claude Code:

```jsonc
{
  "mcpServers": {
    "proxima": { "type": "http", "url": "http://localhost:31415/mcp" }
  }
}
```

Codex CLI:

```toml
[mcp_servers.proxima]
type = "http"
url = "http://localhost:31415/mcp"
```

Port override:

```sh
PROXIMA_MCP_BIND=127.0.0.1:31419 pnpm --filter proxima-shell tauri:dev
```

The listener binds loopback only and rejects missing or disallowed
`Origin` headers.

Headless:

```sh
cargo run -p proxima-mcp -- \
  --owner-user 00000000-0000-0000-0000-000000000000 \
  --owner-org  00000000-0000-0000-0000-000000000000 \
  --bind 127.0.0.1:31415
```

MCP server. Substrate tools (always):
`proxima-mcp/proxima_search_graph`, `proxima-mcp/proxima_open`,
`proxima-mcp/proxima_remember`, `proxima-mcp/proxima_derive`,
`proxima-mcp/proxima_link`. Goal flavor (composited into
`apps/proxima-mcp`): `proxima-goal/goal_propose`,
`proxima-goal/goal_accept`, `proxima-goal/goal_decline`,
`proxima-goal/goal_modify`. Other composite binaries extend the
tool list at link time; see `docs/13-flavor-marketplace.md`.

## Implementation commitment

Rust. Frontend is the only non-Rust component.

Each deployment is a single binary. The engine, event sources,
actuator interface, memory store, and consolidation operators all
live in one process per deployment. Different deployments build
different binaries from different flavor combinations (08); within
any one binary, no internal split, no feature flags, no plugin
loading.

## License

Apache License, Version 2.0. See [`LICENSE`](LICENSE) for full text and
[`LICENSING.md`](LICENSING.md) for rationale and commercial offerings.
