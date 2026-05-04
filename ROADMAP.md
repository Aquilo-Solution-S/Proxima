# Proxima Roadmap

A milestone path from empty repo to a locally-runnable Code demo, then
beyond. Each milestone names what compiles, what runs, and what you
can demo. Long-arc vision lives in
[`docs/13-flavor-marketplace.md`](docs/13-flavor-marketplace.md);
this doc tracks the build path.

No quarter targets, no effort estimates — both rot. Milestones close
when the "done when" criterion holds against the codebase; the close
date is recorded then.

## North star — `code-demo`

The v1 line is one command bringing up a binary that ingests this
repo's commits as Code Facts, consolidates them, and serves a web UI
rendering the result. Single Owner, `NoAuth` resolver, sqlite or
local Postgres. Anyone can clone, run, and see the substrate working
over a real codebase. This is also Aquilo's first dogfood loop.

`code-demo` lives at `products/code-demo/` in this monorepo as the
canonical reference product (per
[13 §Composition](docs/13-flavor-marketplace.md#composition) — Code
is both the contract demonstrator and a runnable exemplar).
Commercial product shells live in private repos; reference ones live
here.

## Milestones to v1

### M1 — Engine skeleton — closed 2026-05-04

- **Compiles**: `proxima-core` crate with `Schema` and `Query` verbs
  over an in-memory store; `NoAuth` resolver; single Owner.
- **Runs**: empty engine boots and answers `Schema` + `Query` over
  zero data.
- **Done when**: `cargo run` brings up the engine and a request to
  `Schema` returns the (empty) registry.

### M2 — Postgres + outbox — closed 2026-05-04

- **Compiles**: storage adapter for Postgres per
  [07](docs/07-storage.md); `change_event` outbox publisher;
  `Subscribe` verb; supersession; idempotency on `GoalWrite` and
  `EventIngest`.
- **Runs**: writes commit, change events fan out to a connected
  `Subscribe`, resume via `since=` cursor works end-to-end.
- **Done when**: an integration test writes Facts and Goals, opens a
  `Subscribe`, drops the connection, reconnects with `last_seq`, and
  observes no missed or duplicated events.

### M3 — Code flavor registers

- **Compiles**: `flavors/code/` crate using `proxima_flavor!`;
  FactPayload schemas (commit, file change, etc.) per
  [03](docs/03-schema-registry.md); per-flavor sidecar migrations
  under `proxima_code.*`.
- **Runs**: composite binary linking core + Code starts; `Schema`
  lists Code's registered payloads; sidecar tables exist after
  migration.
- **Done when**: a hand-crafted Fact insert under a Code schema is
  retrievable via `Query(entity_kind=Fact, schema_id=code/...)`.

### M4 — Self-ingestion

- **Compiles**: git EventSource per [01](docs/01-event-source.md)
  walking this repo's commits, registered against the Code flavor.
- **Runs**: source ingests Proxima's own commit history into Facts
  under a single Owner; `Subscribe` shows the stream live.
- **Done when**: every commit on `main` appears as a Code Fact, and
  new commits stream in within seconds of `git push`.

### M5 — F→A operator

- **Compiles**: Code's first F→A operator per
  [04](docs/04-consolidation.md), with prompts and cadence policy.
- **Runs**: operator consumes Code Facts and emits typed
  Abstractions; supersession behaves on rerun.
- **Done when**: querying Abstractions returns a coherent typed
  summary of the ingested commits.

### M6 — Web shell + codegen UI

- **Compiles**: `proxima-shell` (Tauri 2 + Solid) per
  [09](docs/09-frontend.md); `buf` proto codegen; generic
  schema-driven renderer; Code's per-schema overrides.
- **Runs**: web target serves a UI rendering Code's Facts and
  Abstractions live via `Subscribe`; cold-start stitching
  (`Query` → `Subscribe`) works.
- **Done when**: opening the web app shows the ingested repo's Facts
  and Abstractions, updating without refresh.

### M7 — `code-demo` ships

- **Compiles**: `products/code-demo/` (composite crate + shell + dev
  script); sqlite default; one-command bootstrap.
- **Runs**: a single command starts the binary, runs migrations,
  ingests this repo, serves the UI on `localhost`.
- **Done when**: a fresh clone + one command + one browser tab demos
  the substrate working over its own source. Cut a `v0.1.0` tag.

## Beyond v1

No commitment, ordering loose. Each is a candidate next milestone
once `code-demo` is live.

- A→P operator (intra-flavor) for Code; first Perspective UI.
- `GoalWrite` UI in `proxima-shell`; agent-discovered goals via A→Goal.
- `OIDC` resolver + multi-Owner UI; first hosted dogfood deployment.
- Personality flavor (one reference impl); per-Owner read-scope
  matrix UI.
- Second flavor (`flavors/learning/`) and a cross-flavor cognition
  flavor demonstrating composition per
  [13 §Composition](docs/13-flavor-marketplace.md#composition).
- Mobile shell (lives in private product repo per the three-tier
  split; the substrate work to support it lives here).
- T1 tool runtime install per [12](docs/12-tool-manifest.md).
- `proxima compose` CLI per
  [13 §Compose tool](docs/13-flavor-marketplace.md#compose-tool).

## Discipline

- No quarter targets, no effort estimates.
- A milestone is "done" when its named criterion holds against the
  codebase, not against intent.
- When a milestone closes, append `— closed YYYY-MM-DD` to its
  title. The criterion is not edited retroactively.
- "Beyond v1" entries promote into the numbered list only when
  actively being built.
