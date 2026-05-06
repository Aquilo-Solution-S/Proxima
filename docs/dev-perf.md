# Dev-time perf instrumentation

Every `pnpm --filter proxima-shell tauri:dev` session writes a
timestamped artifact directory under `apps/proxima-shell/perf-logs/`.

## Reading a session

Open `summary.md` first — it has six sections:

- **IPC summary** — Tauri command counts + p50/p95 latency + payload bytes.
- **MCP summary** — HTTP route counts + p50/p95 latency + bandwidth.
- **Frontend timings** — selector recompute, render, snapshot fetch.
- **Wasted IPC fields** — paths the FE actually read; manually compare to
  the response shape in `bindings.ts` to find unused fields.
- **Top PG statements** — slowest by total exec time (from
  `pg_stat_statements`).
- **Top engine spans** — slowest from the chrome trace.

For deeper inspection:

- `engine.json` — load in `chrome://tracing` or https://ui.perfetto.dev for
  a flame chart of Rust spans.
- `pg.log` — `auto_explain` plans for queries above 50ms.
- `ipc.json`, `mcp.json`, `frontend.json`, `ipc-fields.json` — raw NDJSON.

## Running without perf capture

`PROXIMA_PERF=0 pnpm --filter proxima-shell tauri:dev` skips Docker
bring-up and instrumentation entirely; the shell runs against whatever
`DATABASE_URL` is in your environment.

## Cleanup

`pnpm --filter proxima-shell perf:clean -- --keep 5` keeps the 5 newest
sessions (default 10).

`pnpm --filter proxima-shell perf:down` stops the dev Postgres container.

## Reducer smoke

`node scripts/perf-smoke.mjs` runs the summary reducer against committed
fixtures and compares to a golden `summary.expected.md`. Use `--regen`
after intentional reducer changes.
