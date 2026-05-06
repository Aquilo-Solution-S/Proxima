# Dev-Time Performance Instrumentation

**Status:** Draft
**Date:** 2026-05-06
**Owner:** Heinrich
**Scope:** `pnpm --filter proxima-shell tauri:dev` only — production builds untouched.

## Motivation

At ~5k Atlas nodes (the size of this repo when ingested), the desktop shell is
visibly slow. The bottleneck could live in any of three layers — Postgres,
the Rust engine, or the frontend — plus the Tauri IPC boundary between
engine and frontend. Optimizing without measurement risks fixing the wrong
layer. We want every dev session to produce evidence by default, so the next
optimization is informed by data, not intuition.

## Goals

1. Every `tauri:dev` session produces a per-session artifact directory with
   measurements from PG, Rust engine, IPC boundary, and frontend.
2. Setup is reproducible across contributor machines via Docker — no manual
   `postgresql.conf` edits.
3. Artifacts include a generated `summary.md` so the common question
   ("where did the time go") can be answered without opening multiple tools.
4. Opt-out exists for cases where perf overhead matters or Docker is
   unavailable.

## Non-goals

- Production observability. Production deploys are out of scope; this is
  dev-time only.
- Continuous profiling / aggregation across sessions. Each session stands
  alone; cross-session analysis is manual.
- Memory leak detection. Adds noise; not the suspected bottleneck at 5k.
- Frontend network observability beyond Tauri IPC (we are not a web app).

## Architecture

```
docker-compose.dev.yml         # PG 17 with extensions preloaded
scripts/dev-with-perf.mjs      # session driver (spawned by tauri:dev)
apps/proxima-shell/
  src-tauri/
    src/perf.rs                # tracing-chrome layer + IPC counter
  src/perf.ts                  # FE timing capture + Tauri perf_log cmd
  perf-logs/<session>/         # gitignored output, one dir per session
    pg.log                     # tailed Postgres container log
    pg-stats.json              # pg_stat_statements snapshot at exit
    engine.json                # chrome:// tracing format
    ipc.json                   # one row per Tauri command call
    frontend.json              # one row per FE measurement
    summary.md                 # generated reducer output
```

The driver replaces the current `tauri:dev` script. It owns session lifecycle:
ID generation, Docker bring-up, env-var injection into `tauri dev`, and
end-of-session reduction.

## Components

### 1. `docker-compose.dev.yml`

Single service: `postgres:17`. Named volume `proxima_dev_pgdata` for
persistence. Container `command:` sets:

```
-c shared_preload_libraries=pg_stat_statements,auto_explain
-c auto_explain.log_min_duration=50ms
-c auto_explain.log_analyze=on
-c pg_stat_statements.track=all
-c log_destination=stderr
```

Init script `scripts/pg-init.sql` runs `CREATE EXTENSION IF NOT EXISTS
pg_stat_statements;` against the default DB.

The driver injects `DATABASE_URL=postgres://proxima:proxima@127.0.0.1:5432/proxima`
into the spawned `tauri dev` process, overriding any value the contributor
has set in their shell. This guarantees the session always measures against
the perf-instrumented container, not whichever PG happened to be in the
ambient env. Contributors who already have a local PG copy data into the
container manually — explicitly out of repo scope.

### 2. Engine tracing (`apps/proxima-shell/src-tauri/src/perf.rs`)

When env var `PROXIMA_PERF_SESSION_DIR` is set, `lib.rs::run()` adds a
`tracing-chrome` layer writing to `<dir>/engine.json`. The layer is gated by
`cfg(debug_assertions)` so release builds cannot include it even
accidentally.

`tracing-chrome` is added as a regular dependency (not a feature) since
debug builds are the only consumers; the env-var check is the runtime gate.

### 3. IPC instrumentation

A thin macro `#[perf_command]` wraps `#[tauri::command]`. It records:
- `cmd_name: &'static str`
- `req_bytes: usize` (serialized arg buffer length)
- `resp_bytes: usize` (serialized return buffer length)
- `dur_ms: u64`

Each call appends one JSON line to `<dir>/ipc.json` (NDJSON). Append-only
file so concurrent commands don't need locking. Existing `#[tauri::command]`
attributes are migrated to `#[perf_command]` in a single pass; the macro
falls through to the original behavior when `PROXIMA_PERF_SESSION_DIR` is
unset.

### 4. Frontend `perf.ts`

Captures three classes of measurement:
- **Snapshot fetch**: ms from invoke to resolve, already on the IPC boundary;
  cross-correlates with `ipc.json` via a request ID.
- **Selector recompute**: instrument the memoized accessors in
  `packages/frontend-core/src/graph-selectors.ts`. Wrap each with
  `measure(name, fn)` that records when the memo recomputes (not when it
  hits cache).
- **Render**: `performance.now()` around Atlas mount + commit, captured via
  Solid's `onMount` and a `MutationObserver`-free wrapper around the
  surface root.

Measurements buffer in-memory and flush every 1s (or on `beforeunload`) via
a `perf_log` Tauri command that appends NDJSON to `<dir>/frontend.json`.

### 5. Session driver (`scripts/dev-with-perf.mjs`)

Replaces `tauri:dev`'s body. Sequence:

1. Read `PROXIMA_PERF` env. If `0`, exec `tauri dev` directly and exit.
2. Generate `session = YYYY-MM-DD_HH-MM-SS`.
3. Create `apps/proxima-shell/perf-logs/<session>/`.
4. `docker compose -f docker-compose.dev.yml up -d --wait`.
5. `psql -c 'SELECT pg_stat_statements_reset()'` against the dev DB.
6. Start tailing the PG container log into `<dir>/pg.log`
   (`docker compose logs -f --no-color > pg.log` in a child process).
7. Spawn `tauri dev` with `PROXIMA_PERF_SESSION_DIR=<absolute-path>`.
8. On SIGINT (or child exit):
   - Stop the PG-log tail.
   - `psql -c 'SELECT * FROM pg_stat_statements ORDER BY total_exec_time DESC LIMIT 100'`
     piped through `psql --json` (or row-to-JSON SQL) → `pg-stats.json`.
   - Run the reducer (below) → `summary.md`.

The driver does NOT take down the Docker container on exit — leaving it
running matches the way developers expect a long-lived dev DB to behave.
Take-down is a separate `pnpm perf:down` script.

### 6. Summary reducer

A single `.mjs` file invoked at session end. Reads the four artifact files
and produces `summary.md` with:

- Session metadata (start time, duration, PG stats reset confirmed).
- **Top 10 slowest PG statements**: by total time, with calls + mean.
- **Top 10 slowest engine spans**: by self-time, from `engine.json`.
- **IPC summary**: per-command count, p50/p95 ms, p50/p95 req/resp bytes.
- **Frontend summary**: per-measurement-class p50/p95 ms.

Reducer is pure (file-in, file-out); a contributor can re-run it manually
against a saved session dir.

## Data flow

```
   pg_stat_statements ──┐
   auto_explain log ────┼──► pg.log + pg-stats.json
                        │
   tracing-chrome  ─────┼──► engine.json
                        │
   #[perf_command]  ────┼──► ipc.json (NDJSON, append-only)
                        │
   perf.ts buffer ──┐   │
                    ├──► perf_log Tauri cmd ──► frontend.json (NDJSON)
                    │   │
                    │   ▼
                    │   reducer ──► summary.md
                    │
                    └─ correlated to ipc.json via request_id
```

## Defaults and opt-out

- **Default**: `pnpm --filter proxima-shell tauri:dev` runs the driver,
  which brings up Docker PG and produces a session dir.
- **Opt-out**: `PROXIMA_PERF=0 pnpm --filter proxima-shell tauri:dev` skips
  the driver and runs raw `tauri dev` against the developer's existing
  `DATABASE_URL`. Useful when (a) Docker is unavailable, (b) the
  instrumentation overhead is itself the variable being measured, (c) a
  contributor wants to use their own PG.

## Artifact lifecycle

- `apps/proxima-shell/perf-logs/` is gitignored.
- Sessions accumulate; no auto-cleanup during runtime.
- `pnpm perf:clean` (added) trims to the last 10 sessions; configurable via
  `--keep N`.
- The summary reducer is idempotent — re-runnable against any saved session.

## Error handling

- **Docker not installed / not running**: driver detects via `docker info`
  and bails with an actionable error pointing at the opt-out env var.
- **Port 5432 already bound** (e.g., user already runs PG locally): driver
  detects and bails with a message suggesting either stopping the local PG
  or using `PROXIMA_PERF=0`.
- **`pg_stat_statements_reset()` fails** (extension not installed):
  driver logs warning, continues; the session still produces engine + FE +
  IPC artifacts.
- **`tauri dev` exits non-zero**: driver still runs the reducer over
  whatever artifacts exist, so a crash session is also analyzable.

## Testing

- Unit-testable: the summary reducer (pure file-in, file-out) gets tests
  with fixture inputs.
- Manually verifiable: run `pnpm tauri:dev`, ingest a small repo,
  Ctrl-C, open `summary.md`, confirm all four sections present and
  populated.
- Smoke: a `scripts/perf-smoke.mjs` that brings up Docker, runs the
  reducer over committed fixture artifacts, asserts `summary.md` matches a
  golden file. Run on CI behind a `--with-docker` flag.

## Open questions resolved during brainstorming

- **PG setup approach**: Docker (option A in brainstorming) — fully
  reproducible, contributors copy local data manually if needed.
- **Scope**: PG slow-queries + engine spans + IPC + frontend timings
  (option B in brainstorming). Memory excluded.
- **Default behavior**: always-on with env-var opt-out.
