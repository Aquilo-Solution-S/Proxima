# Proxima

We unconsciously perceive reality as it is. The filter we call our perception stems from our intrinsic motivations as well as our conditioning based on past experiences. Here, we refer to reality as facts (F). To abstract facts, we need a perspective (or multiple perspectives) and relate them to goals (G). This gives rise to insights—here, abstractions (A).

Actions that originate from us are indistinguishable from external factors—the only difference is that, in this case, the source can be traced directly back to us. Proxima is based on the idea that consequences can only contribute to the continuous learning and improvement of a system through traceability.

The system is designed in a way to be typesafe at compile time while maintaining a lot of flexibility by providing the possibility to create your own flavors. The word flavor was specifically choosen since domains are mostly defined by certain paradigmas and norms, but your perspective is something personal, like your taste, therefore flavor seemed better to me.

## Start

```sh
pnpm install
pnpm --filter proxima-shell tauri:dev
```

`tauri:dev` starts the desktop shell, brings up dev Postgres via
`docker-compose.dev.yml`, and writes perf logs under
`apps/proxima-shell/perf-logs/`.

```sh
PROXIMA_PERF=0 pnpm --filter proxima-shell tauri:dev
```

Raw shell startup. No Docker, no perf capture. Uses the current
`DATABASE_URL`.

```sh
pnpm --filter proxima-shell dev --host 127.0.0.1
cargo run -p proxima-mcp -- --owner-user <uuid> --owner-org <uuid>
```

Frontend-only Vite dev server. Headless MCP server at
`http://127.0.0.1:31415/mcp`.

```sh
cargo check --workspace
pnpm --filter @proxima/core typecheck
pnpm --filter proxima-shell typecheck
pnpm --filter proxima-shell build
pnpm --filter proxima-shell perf:down
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
- [`docs/05-actions.md`](docs/05-actions.md) — actions as ordinary Facts
  emitted through trusted sources / tools. No Action entity.
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
