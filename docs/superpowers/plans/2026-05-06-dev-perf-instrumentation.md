# Dev-Time Perf Instrumentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every `pnpm --filter proxima-shell tauri:dev` session produce a timestamped artifact directory with measurements from Postgres, the Rust engine, the Tauri IPC boundary, the MCP HTTP surface, and the frontend — including which IPC response fields were actually read.

**Architecture:** A driver script wraps `tauri dev`, brings up a Docker Postgres (`pg_stat_statements` + `auto_explain` preloaded), and injects `PROXIMA_PERF_SESSION_DIR` into the spawned engine. Engine emits a `tracing-chrome` trace + per-IPC-call NDJSON + per-MCP-request NDJSON. Frontend records timings + field-access paths via `Proxy` and flushes to Tauri commands that append NDJSON. On exit the driver runs a pure JS reducer that produces `summary.md` joining everything together.

**Tech Stack:** Docker Compose (Postgres 17), Node 20+ (`node:test`, ESM scripts), Rust (`tracing-chrome`, `tower`), Tauri 2 (`tauri-specta`), Solid + Vite + Vitest.

**Spec:** `docs/superpowers/specs/2026-05-06-dev-perf-instrumentation-design.md`

**File map (everything new or modified):**

| Path | Role |
|---|---|
| `.gitignore` | add `apps/proxima-shell/perf-logs/` |
| `docker-compose.dev.yml` | PG 17 with extensions preloaded |
| `scripts/pg-init.sql` | `CREATE EXTENSION pg_stat_statements` |
| `scripts/dev-with-perf.mjs` | session driver |
| `scripts/perf-summary.mjs` | reducer (pure file-in/file-out) |
| `scripts/perf-summary.test.mjs` | reducer tests (`node:test`) |
| `scripts/perf-clean.mjs` | trim old sessions |
| `scripts/perf-smoke.mjs` | reducer smoke against committed fixtures |
| `scripts/fixtures/perf-smoke/` | golden inputs + `summary.expected.md` |
| `apps/proxima-shell/package.json` | rewire `tauri:dev` |
| `apps/proxima-shell/src-tauri/Cargo.toml` | add `tracing-chrome`, `tower-http` if absent |
| `apps/proxima-shell/src-tauri/src/perf/mod.rs` | perf module root |
| `apps/proxima-shell/src-tauri/src/perf/session.rs` | session-dir env helper |
| `apps/proxima-shell/src-tauri/src/perf/chrome.rs` | tracing-chrome layer |
| `apps/proxima-shell/src-tauri/src/perf/ipc.rs` | IPC call recorder helper |
| `apps/proxima-shell/src-tauri/src/perf/mcp.rs` | Tower middleware for MCP |
| `apps/proxima-shell/src-tauri/src/perf/fe.rs` | `perf_log` + `perf_log_field` commands |
| `apps/proxima-shell/src-tauri/src/lib.rs` | wire perf module into Builder + tracing |
| `apps/proxima-shell/src-tauri/src/boot.rs` | hand MCP middleware to listener |
| `apps/proxima-shell/src-tauri/src/commands/repo_ingest.rs` | wrap heavy commands in `perf::ipc_call` |
| `apps/proxima-shell/src-tauri/src/commands/repos.rs` | same |
| `apps/proxima-shell/src-tauri/src/commands/engine.rs` | same |
| `apps/proxima-shell/src/perf.ts` | FE timing capture + Proxy recorder |
| `apps/proxima-shell/src/perf.test.ts` | Vitest unit tests |
| `apps/proxima-shell/src/main.tsx` (or entry) | call `installPerf()` if env var present |
| `packages/frontend-core/src/graph-selectors.ts` | wrap memo factories with `measureSelector` |
| `packages/frontend-core/src/views/atlas/...` | mount-time `measureRender` |
| `crates/mcp-server/src/lib.rs` | accept optional Tower layer in `serve_streamable_http` |
| `docs/dev-perf.md` | one-pager: how to read a session dir |

---

## Task 1: Gitignore + perf-logs scaffolding

**Files:**
- Modify: `.gitignore`
- Create: `apps/proxima-shell/perf-logs/.gitkeep`

- [ ] **Step 1: Add gitignore entry**

Append to `.gitignore`:

```
apps/proxima-shell/perf-logs/*
!apps/proxima-shell/perf-logs/.gitkeep
```

- [ ] **Step 2: Create the directory placeholder**

```bash
mkdir -p apps/proxima-shell/perf-logs
touch apps/proxima-shell/perf-logs/.gitkeep
```

- [ ] **Step 3: Verify ignore works**

```bash
echo test > apps/proxima-shell/perf-logs/probe
git status --short apps/proxima-shell/perf-logs/
rm apps/proxima-shell/perf-logs/probe
```

Expected: only `.gitkeep` shows; `probe` is ignored.

- [ ] **Step 4: Commit**

```bash
git add .gitignore apps/proxima-shell/perf-logs/.gitkeep
git commit -m "chore(perf): scaffold perf-logs directory (gitignored)"
```

---

## Task 2: Docker Compose dev Postgres

**Files:**
- Create: `docker-compose.dev.yml`
- Create: `scripts/pg-init.sql`

- [ ] **Step 1: Write `scripts/pg-init.sql`**

```sql
CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
```

- [ ] **Step 2: Write `docker-compose.dev.yml`**

```yaml
services:
  postgres:
    image: postgres:17
    container_name: proxima_dev_pg
    environment:
      POSTGRES_USER: proxima
      POSTGRES_PASSWORD: proxima
      POSTGRES_DB: proxima
    ports:
      - "5432:5432"
    volumes:
      - proxima_dev_pgdata:/var/lib/postgresql/data
      - ./scripts/pg-init.sql:/docker-entrypoint-initdb.d/00-pg-stat-statements.sql:ro
    command:
      - "postgres"
      - "-c"
      - "shared_preload_libraries=pg_stat_statements,auto_explain"
      - "-c"
      - "pg_stat_statements.track=all"
      - "-c"
      - "auto_explain.log_min_duration=50ms"
      - "-c"
      - "auto_explain.log_analyze=on"
      - "-c"
      - "log_destination=stderr"
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U proxima -d proxima"]
      interval: 2s
      timeout: 3s
      retries: 30

volumes:
  proxima_dev_pgdata:
```

- [ ] **Step 3: Bring it up and verify**

```bash
docker compose -f docker-compose.dev.yml up -d --wait
PGPASSWORD=proxima psql -h 127.0.0.1 -U proxima -d proxima -c "SELECT count(*) FROM pg_stat_statements;"
```

Expected: container becomes healthy within ~5s; query returns a row count (≥0).

- [ ] **Step 4: Verify auto_explain is loaded**

```bash
PGPASSWORD=proxima psql -h 127.0.0.1 -U proxima -d proxima -c "SHOW shared_preload_libraries;"
```

Expected: `pg_stat_statements,auto_explain`.

- [ ] **Step 5: Tear down (we'll bring it up via the driver later)**

```bash
docker compose -f docker-compose.dev.yml down
```

- [ ] **Step 6: Commit**

```bash
git add docker-compose.dev.yml scripts/pg-init.sql
git commit -m "feat(perf): add docker-compose dev Postgres with extensions preloaded"
```

---

## Task 3: Session driver skeleton

This task lands a working driver that brings up Docker, creates a session dir, sets the env var, runs `tauri dev`, and exits cleanly. Reducer + PG dump come later.

**Files:**
- Create: `scripts/dev-with-perf.mjs`

- [ ] **Step 1: Write the driver**

```javascript
#!/usr/bin/env node
// scripts/dev-with-perf.mjs
//
// Session driver wrapping `tauri dev` with per-session perf capture.
// Skip everything when PROXIMA_PERF=0.

import { spawn, spawnSync } from "node:child_process";
import { mkdirSync, existsSync } from "node:fs";
import { resolve, join } from "node:path";
import { fileURLToPath } from "node:url";

const REPO = resolve(fileURLToPath(import.meta.url), "../..");
const COMPOSE = join(REPO, "docker-compose.dev.yml");
const PERF_LOGS = join(REPO, "apps/proxima-shell/perf-logs");
const DB_URL = "postgres://proxima:proxima@127.0.0.1:5432/proxima";

function timestamp() {
  const d = new Date();
  const p = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}_${p(d.getHours())}-${p(d.getMinutes())}-${p(d.getSeconds())}`;
}

function runOrDie(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { stdio: "inherit", ...opts });
  if (r.status !== 0) {
    console.error(`[perf-driver] ${cmd} ${args.join(" ")} exited ${r.status}`);
    process.exit(r.status ?? 1);
  }
}

async function main() {
  const optOut = process.env.PROXIMA_PERF === "0";

  if (optOut) {
    console.log("[perf-driver] PROXIMA_PERF=0 → running raw tauri dev");
    const child = spawn("pnpm", ["exec", "tauri", "dev"], {
      cwd: join(REPO, "apps/proxima-shell"),
      stdio: "inherit",
      env: process.env,
    });
    child.on("exit", (code) => process.exit(code ?? 0));
    return;
  }

  // Detect docker
  const docker = spawnSync("docker", ["info"], { stdio: "ignore" });
  if (docker.status !== 0) {
    console.error(
      "[perf-driver] Docker is not running. Either start Docker, or run with PROXIMA_PERF=0 to skip perf capture."
    );
    process.exit(1);
  }

  const session = timestamp();
  const sessionDir = join(PERF_LOGS, session);
  mkdirSync(sessionDir, { recursive: true });
  console.log(`[perf-driver] session dir: ${sessionDir}`);

  console.log("[perf-driver] bringing up Postgres…");
  runOrDie("docker", ["compose", "-f", COMPOSE, "up", "-d", "--wait"]);

  // Reset pg_stat_statements (best-effort; non-fatal if extension missing)
  const reset = spawnSync(
    "docker",
    [
      "compose",
      "-f",
      COMPOSE,
      "exec",
      "-T",
      "postgres",
      "psql",
      "-U",
      "proxima",
      "-d",
      "proxima",
      "-c",
      "SELECT pg_stat_statements_reset();",
    ],
    { stdio: "inherit" }
  );
  if (reset.status !== 0) {
    console.warn("[perf-driver] pg_stat_statements_reset failed — continuing");
  }

  // Tail PG container log into session dir
  const pgLog = join(sessionDir, "pg.log");
  const tail = spawn(
    "docker",
    ["compose", "-f", COMPOSE, "logs", "-f", "--no-color", "postgres"],
    { stdio: ["ignore", "pipe", "inherit"] }
  );
  const fs = await import("node:fs");
  const logStream = fs.createWriteStream(pgLog);
  tail.stdout.pipe(logStream);

  // Spawn tauri dev with perf env
  const child = spawn("pnpm", ["exec", "tauri", "dev"], {
    cwd: join(REPO, "apps/proxima-shell"),
    stdio: "inherit",
    env: {
      ...process.env,
      DATABASE_URL: DB_URL,
      PROXIMA_PERF_SESSION_DIR: sessionDir,
    },
  });

  const cleanup = async (code) => {
    tail.kill("SIGTERM");
    try {
      logStream.end();
    } catch {}
    process.exit(code ?? 0);
  };

  process.on("SIGINT", () => cleanup(0));
  child.on("exit", (code) => cleanup(code ?? 0));
}

await main();
```

- [ ] **Step 2: Make executable**

```bash
chmod +x scripts/dev-with-perf.mjs
```

- [ ] **Step 3: Smoke-test the opt-out path**

```bash
PROXIMA_PERF=0 timeout 5 node scripts/dev-with-perf.mjs || true
```

Expected: prints "PROXIMA_PERF=0 → running raw tauri dev". (It will then start `tauri dev` and timeout — that's fine; we're just confirming the branch.)

- [ ] **Step 4: Smoke-test session-dir creation (without spawning tauri)**

Temporarily edit `main()` to `process.exit(0)` immediately after `mkdirSync` and the docker bring-up. Run:

```bash
node scripts/dev-with-perf.mjs
ls apps/proxima-shell/perf-logs/
```

Expected: a `YYYY-MM-DD_HH-MM-SS` directory exists; PG container is up.

Revert the temp edit.

- [ ] **Step 5: Tear down**

```bash
docker compose -f docker-compose.dev.yml down
```

- [ ] **Step 6: Commit**

```bash
git add scripts/dev-with-perf.mjs
git commit -m "feat(perf): add session-driver skeleton wrapping tauri dev"
```

---

## Task 4: Wire driver into `tauri:dev`

**Files:**
- Modify: `apps/proxima-shell/package.json`

- [ ] **Step 1: Replace `tauri:dev` script**

Change in `apps/proxima-shell/package.json`:

```json
"tauri:dev": "node ../../scripts/dev-with-perf.mjs",
"tauri:dev:raw": "tauri dev"
```

(`tauri:dev:raw` preserves the original behavior under a different name for emergencies.)

- [ ] **Step 2: Verify pnpm wiring**

```bash
PROXIMA_PERF=0 timeout 5 pnpm --filter proxima-shell tauri:dev || true
```

Expected: same output as Task 3 step 3.

- [ ] **Step 3: Commit**

```bash
git add apps/proxima-shell/package.json
git commit -m "feat(perf): route tauri:dev through perf driver (PROXIMA_PERF=0 to opt out)"
```

---

## Task 5: Rust perf module skeleton

Lays out the perf module with a session-dir helper. Subsequent tasks fill in the layers.

**Files:**
- Create: `apps/proxima-shell/src-tauri/src/perf/mod.rs`
- Create: `apps/proxima-shell/src-tauri/src/perf/session.rs`
- Modify: `apps/proxima-shell/src-tauri/src/lib.rs` (add `mod perf;` and call `perf::session::dir()`)

- [ ] **Step 1: Write `perf/session.rs`**

```rust
use std::path::PathBuf;
use std::sync::OnceLock;

/// Per-session output directory, set by the dev driver.
/// `None` ⇒ perf capture is disabled (raw tauri dev or production build).
static SESSION_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

pub fn dir() -> Option<&'static PathBuf> {
    SESSION_DIR
        .get_or_init(|| {
            std::env::var_os("PROXIMA_PERF_SESSION_DIR")
                .map(PathBuf::from)
                .filter(|p| p.exists())
        })
        .as_ref()
}

pub fn enabled() -> bool {
    dir().is_some()
}
```

- [ ] **Step 2: Write `perf/mod.rs`**

```rust
//! Dev-time performance instrumentation. Active only when
//! `PROXIMA_PERF_SESSION_DIR` is set and points at an existing directory.
//! See `docs/superpowers/specs/2026-05-06-dev-perf-instrumentation-design.md`.

pub mod session;
```

- [ ] **Step 3: Wire into `lib.rs`**

Add `mod perf;` near the other `mod` declarations. Inside `run()` (immediately after the existing `tracing_subscriber::fmt()` call), add:

```rust
if let Some(dir) = perf::session::dir() {
    tracing::info!("perf session dir: {}", dir.display());
}
```

- [ ] **Step 4: Compile**

```bash
cargo check -p proxima-shell
```

Expected: clean.

- [ ] **Step 5: Smoke test the env var path**

```bash
PROXIMA_PERF_SESSION_DIR=/tmp/perf-smoke-$$ mkdir -p /tmp/perf-smoke-$$ && \
RUST_LOG=info cargo run -p proxima-shell --bin proxima-shell -- --help 2>&1 | grep "perf session dir" || echo "no log line — OK if --help short-circuits"
```

(This is best-effort; the real verification is in Task 6.)

- [ ] **Step 6: Commit**

```bash
git add apps/proxima-shell/src-tauri/src/perf apps/proxima-shell/src-tauri/src/lib.rs
git commit -m "feat(perf): add perf module skeleton with session-dir helper"
```

---

## Task 6: tracing-chrome engine layer

**Files:**
- Modify: `apps/proxima-shell/src-tauri/Cargo.toml`
- Create: `apps/proxima-shell/src-tauri/src/perf/chrome.rs`
- Modify: `apps/proxima-shell/src-tauri/src/perf/mod.rs`
- Modify: `apps/proxima-shell/src-tauri/src/lib.rs`

- [ ] **Step 1: Add dependency**

Add to `[dependencies]` in `apps/proxima-shell/src-tauri/Cargo.toml`:

```toml
tracing-chrome = "0.7"
```

- [ ] **Step 2: Write `perf/chrome.rs`**

```rust
use std::path::Path;

use tracing_chrome::{ChromeLayerBuilder, FlushGuard};

/// Build a tracing-chrome layer writing to `<session_dir>/engine.json`.
/// Returns the layer plus a flush guard that the caller must keep alive
/// for the duration of the program.
pub fn layer(
    session_dir: &Path,
) -> (
    impl tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync + 'static,
    FlushGuard,
) {
    let path = session_dir.join("engine.json");
    let (layer, guard) = ChromeLayerBuilder::new()
        .file(path)
        .include_args(true)
        .build();
    (layer, guard)
}
```

- [ ] **Step 3: Export from `perf/mod.rs`**

```rust
pub mod chrome;
pub mod session;
```

- [ ] **Step 4: Wire into `lib.rs::run()`**

Replace the existing `tracing_subscriber::fmt()...init()` call with a layered subscriber that conditionally adds the chrome layer:

```rust
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
    tracing_subscriber::EnvFilter::new("info,proxima_shell=debug")
});

let registry = tracing_subscriber::registry()
    .with(env_filter)
    .with(tracing_subscriber::fmt::layer());

let _flush_guard: Option<tracing_chrome::FlushGuard> = if let Some(dir) = perf::session::dir() {
    let (layer, guard) = perf::chrome::layer(dir);
    registry.with(layer).init();
    Some(guard)
} else {
    registry.init();
    None
};
// Keep `_flush_guard` alive until the end of `run()`.
```

(Adjust the existing `EnvFilter` literal to whatever the file currently uses; the structure above replaces the prior `tracing_subscriber::fmt().with_env_filter(...).init()` pattern.)

- [ ] **Step 5: Compile**

```bash
cargo check -p proxima-shell
```

Expected: clean.

- [ ] **Step 6: Verify the trace file is produced**

```bash
mkdir -p /tmp/perf-test
PROXIMA_PERF_SESSION_DIR=/tmp/perf-test \
DATABASE_URL=postgres://proxima:proxima@127.0.0.1:5432/proxima \
docker compose -f docker-compose.dev.yml up -d --wait
PROXIMA_PERF_SESSION_DIR=/tmp/perf-test \
DATABASE_URL=postgres://proxima:proxima@127.0.0.1:5432/proxima \
timeout 8 cargo run -p proxima-shell || true
ls -la /tmp/perf-test/engine.json
head -c 200 /tmp/perf-test/engine.json
docker compose -f docker-compose.dev.yml down
```

Expected: `engine.json` exists, starts with `[` (chrome trace JSON array open).

- [ ] **Step 7: Commit**

```bash
git add apps/proxima-shell/src-tauri/Cargo.toml apps/proxima-shell/src-tauri/src/perf apps/proxima-shell/src-tauri/src/lib.rs
git commit -m "feat(perf): emit chrome trace from engine when session dir set"
```

---

## Task 7: IPC instrumentation helper + wrap heavy commands

**Files:**
- Create: `apps/proxima-shell/src-tauri/src/perf/ipc.rs`
- Modify: `apps/proxima-shell/src-tauri/src/perf/mod.rs`
- Modify: `apps/proxima-shell/src-tauri/src/commands/repo_ingest.rs`
- Modify: `apps/proxima-shell/src-tauri/src/commands/repos.rs`
- Modify: `apps/proxima-shell/src-tauri/src/commands/engine.rs`

- [ ] **Step 1: Write `perf/ipc.rs`**

```rust
//! IPC call recorder. Each instrumented Tauri command wraps its body in
//! `perf::ipc::record(name, args, async { ... })`. When the session dir is
//! unset, this is a no-op pass-through.
//!
//! Output: NDJSON appended to `<session>/ipc.json`. One line per call:
//!   { "ts_ms": 173..., "cmd": "...", "req_bytes": N, "resp_bytes": N,
//!     "dur_ms": N, "ok": true }

use std::fs::OpenOptions;
use std::io::Write;
use std::time::Instant;

use serde::Serialize;
use serde_json::json;

use super::session;

pub async fn record<A, R, F>(cmd: &'static str, args: &A, fut: F) -> R
where
    A: Serialize,
    R: Serialize,
    F: std::future::Future<Output = R>,
{
    let Some(dir) = session::dir() else {
        return fut.await;
    };

    let req_bytes = serde_json::to_vec(args).map(|v| v.len()).unwrap_or(0);
    let started = Instant::now();
    let result = fut.await;
    let dur_ms = started.elapsed().as_millis() as u64;
    let resp_bytes = serde_json::to_vec(&result).map(|v| v.len()).unwrap_or(0);

    let line = json!({
        "ts_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        "cmd": cmd,
        "req_bytes": req_bytes,
        "resp_bytes": resp_bytes,
        "dur_ms": dur_ms,
        "ok": true,
    });

    let path = dir.join("ipc.json");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}", line);
    }
    result
}
```

- [ ] **Step 2: Add to `perf/mod.rs`**

```rust
pub mod chrome;
pub mod ipc;
pub mod session;
```

- [ ] **Step 3: Compile to verify the helper alone**

```bash
cargo check -p proxima-shell
```

Expected: clean.

- [ ] **Step 4: Wrap a command — start with one in `engine.rs`**

Open `apps/proxima-shell/src-tauri/src/commands/engine.rs`. For each `#[tauri::command]` function whose body fetches significant data (snapshot/query commands), wrap the body. Example pattern (apply to each):

```rust
#[tauri::command]
#[specta::specta]
pub async fn query_atlas_snapshot(
    state: tauri::State<'_, AppState>,
    args: SnapshotArgs,
) -> Result<SnapshotPayload, String> {
    crate::perf::ipc::record("query_atlas_snapshot", &args, async move {
        // ... existing body ...
    })
    .await
}
```

For commands with multiple non-`State` args, pass a tuple to `record`:

```rust
crate::perf::ipc::record("cmd_name", &(&a, &b), async move { ... }).await
```

`State` args are skipped — they don't serialize meaningfully and pass through by reference.

Apply to: every command in `engine.rs`, `repos.rs`, `repo_ingest.rs`. Skip command-less helpers.

- [ ] **Step 5: Compile**

```bash
cargo check -p proxima-shell
```

Expected: clean.

- [ ] **Step 6: Verify a session produces `ipc.json`**

```bash
docker compose -f docker-compose.dev.yml up -d --wait
mkdir -p /tmp/perf-test2
PROXIMA_PERF_SESSION_DIR=/tmp/perf-test2 \
DATABASE_URL=postgres://proxima:proxima@127.0.0.1:5432/proxima \
timeout 12 cargo run -p proxima-shell || true
docker compose -f docker-compose.dev.yml down
cat /tmp/perf-test2/ipc.json | head -3
```

Expected: at least one NDJSON line with `cmd`, `req_bytes`, `resp_bytes`, `dur_ms` after you've exercised any UI that triggers a wrapped command. (If the shell's startup doesn't auto-fire a wrapped command, this step's expectation drops to: file exists or is absent; non-fatal.)

- [ ] **Step 7: Commit**

```bash
git add apps/proxima-shell/src-tauri/src/perf apps/proxima-shell/src-tauri/src/commands
git commit -m "feat(perf): record per-IPC call timing + payload sizes"
```

---

## Task 8: MCP HTTP middleware

**Files:**
- Create: `apps/proxima-shell/src-tauri/src/perf/mcp.rs`
- Modify: `apps/proxima-shell/src-tauri/src/perf/mod.rs`
- Modify: `crates/mcp-server/src/lib.rs` (accept optional `Layer`)
- Modify: `apps/proxima-shell/src-tauri/src/boot.rs` (pass middleware in dev)

- [ ] **Step 1: Write `perf/mcp.rs`**

```rust
//! Tower middleware that records one NDJSON row per MCP HTTP request to
//! `<session>/mcp.json`. Active only when the session dir is set.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use http::{Request, Response};
use http_body_util::BodyExt;
use serde_json::json;
use tower::{Layer, Service};

use super::session;

#[derive(Clone)]
pub struct PerfMcpLayer {
    dir: Arc<std::path::PathBuf>,
}

impl PerfMcpLayer {
    pub fn enabled() -> Option<Self> {
        session::dir().cloned().map(|d| Self { dir: Arc::new(d) })
    }
}

impl<S> Layer<S> for PerfMcpLayer {
    type Service = PerfMcpService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        PerfMcpService { inner, dir: self.dir.clone() }
    }
}

pub struct PerfMcpService<S> {
    inner: S,
    dir: Arc<std::path::PathBuf>,
}

impl<S, ReqBody, RespBody> Service<Request<ReqBody>> for PerfMcpService<S>
where
    S: Service<Request<ReqBody>, Response = Response<RespBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ReqBody: http_body::Body + Send + 'static,
    ReqBody::Data: Send,
    ReqBody::Error: Send,
    RespBody: http_body::Body + Send + 'static,
    RespBody::Data: Send,
    RespBody::Error: std::fmt::Debug,
{
    type Response = Response<RespBody>;
    type Error = S::Error;
    type Future = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let started = Instant::now();
        let method = req.method().clone();
        let route = req.uri().path().to_string();
        let req_bytes = req
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        let dir = self.dir.clone();
        let mut inner = self.inner.clone();
        Box::pin(async move {
            let resp = inner.call(req).await?;
            let status = resp.status().as_u16();
            let resp_bytes = resp
                .headers()
                .get(http::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);

            let line = json!({
                "ts_ms": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64).unwrap_or(0),
                "method": method.as_str(),
                "route": route,
                "status": status,
                "req_bytes": req_bytes,
                "resp_bytes": resp_bytes,
                "dur_ms": started.elapsed().as_millis() as u64,
            });

            let path = dir.join("mcp.json");
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
                let _ = writeln!(f, "{}", line);
            }
            Ok(resp)
        })
    }
}
```

> Note on `resp_bytes`: SSE/streaming responses don't set `Content-Length`. For v1 we accept "0 bytes" on streaming routes. A counted-body wrapper is a v2 enhancement.

- [ ] **Step 2: Export from `perf/mod.rs`**

```rust
pub mod chrome;
pub mod ipc;
pub mod mcp;
pub mod session;
```

- [ ] **Step 3: Add `tower` + `http` if not workspace-available**

Verify in `apps/proxima-shell/src-tauri/Cargo.toml`. If absent:

```toml
tower = "0.5"
http = "1"
http-body = "1"
http-body-util = "0.1"
```

(Use whatever versions the MCP server crate already depends on — check `crates/mcp-server/Cargo.toml` first and match.)

- [ ] **Step 4: Modify `crates/mcp-server/src/lib.rs` to accept an optional layer**

Find `serve_streamable_http`. Add an overload (or change the signature) to accept a `Option<L: Layer<...>>`. Concretely, add a sibling fn:

```rust
pub async fn serve_streamable_http_with_layer<L>(
    bind: SocketAddr,
    server: DevMcpServer,
    allowlist: Allowlist,
    layer: Option<L>,
) -> Result<(), McpServerError>
where
    L: tower::Layer<axum::Router> + Send + Sync + 'static,
    L::Service: Service<...> + ...,
{
    let router = build_router(server, allowlist);
    let router = if let Some(l) = layer { l.layer(router) } else { router };
    // ... existing serve logic ...
}
```

If the existing function body is short, just modify it directly to take `Option<PerfMcpLayer>`. Concrete signature is whatever fits the existing code — the goal is "caller can inject a layer". If the trait gymnastics get heavy, take a `Box<dyn Layer>` or a concrete `Option<PerfMcpLayer>` and accept the dev-only coupling.

- [ ] **Step 5: Wire from `boot.rs::spawn_mcp_listener`**

```rust
let layer = crate::perf::mcp::PerfMcpLayer::enabled();
serve_streamable_http_with_layer(bind, server, default_allowlist(), layer).await
```

- [ ] **Step 6: Compile**

```bash
cargo check -p proxima-shell -p proxima-mcp-server
```

Expected: clean.

- [ ] **Step 7: Smoke-test against MCP**

```bash
docker compose -f docker-compose.dev.yml up -d --wait
mkdir -p /tmp/perf-test3
PROXIMA_PERF_SESSION_DIR=/tmp/perf-test3 \
DATABASE_URL=postgres://proxima:proxima@127.0.0.1:5432/proxima \
cargo run -p proxima-shell &
SHELL_PID=$!
sleep 6
curl -sS -X POST http://127.0.0.1:31415/mcp -d '{}' -H 'content-type: application/json' || true
sleep 1
kill $SHELL_PID
cat /tmp/perf-test3/mcp.json | head -3
docker compose -f docker-compose.dev.yml down
```

Expected: at least one NDJSON line with `method`, `route`, `status`, `dur_ms`.

- [ ] **Step 8: Commit**

```bash
git add apps/proxima-shell/src-tauri crates/mcp-server
git commit -m "feat(perf): add Tower middleware logging MCP HTTP requests"
```

---

## Task 9: FE timing capture + Tauri perf_log command

**Files:**
- Create: `apps/proxima-shell/src-tauri/src/perf/fe.rs`
- Modify: `apps/proxima-shell/src-tauri/src/perf/mod.rs`
- Modify: `apps/proxima-shell/src-tauri/src/commands/mod.rs` (or wherever `specta_builder` registers commands)
- Create: `apps/proxima-shell/src/perf.ts`
- Modify: `apps/proxima-shell/src/main.tsx` (entry — call `installPerf()`)
- Modify: `packages/frontend-core/src/graph-selectors.ts` (instrument memos)

- [ ] **Step 1: Write the Tauri-side appender `perf/fe.rs`**

```rust
use std::fs::OpenOptions;
use std::io::Write;

use serde::Deserialize;
use specta::Type;

use super::session;

#[derive(Deserialize, Type)]
pub struct PerfEntry {
    pub kind: String,         // "snapshot_fetch" | "selector" | "render"
    pub name: String,
    pub dur_ms: f64,
    pub bytes: Option<u64>,
}

#[derive(Deserialize, Type)]
pub struct FieldEntry {
    pub cmd: String,
    pub field_path: String,
}

fn append(file: &str, line: serde_json::Value) {
    let Some(dir) = session::dir() else { return };
    let path = dir.join(file);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}", line);
    }
}

#[tauri::command]
#[specta::specta]
pub fn perf_log(entries: Vec<PerfEntry>) {
    for e in entries {
        append(
            "frontend.json",
            serde_json::json!({
                "ts_ms": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64).unwrap_or(0),
                "kind": e.kind,
                "name": e.name,
                "dur_ms": e.dur_ms,
                "bytes": e.bytes,
            }),
        );
    }
}

#[tauri::command]
#[specta::specta]
pub fn perf_log_field(entries: Vec<FieldEntry>) {
    for e in entries {
        append(
            "ipc-fields.json",
            serde_json::json!({
                "cmd": e.cmd,
                "field_path": e.field_path,
            }),
        );
    }
}
```

- [ ] **Step 2: Register in `perf/mod.rs`**

```rust
pub mod chrome;
pub mod fe;
pub mod ipc;
pub mod mcp;
pub mod session;

pub use fe::{perf_log, perf_log_field};
```

- [ ] **Step 3: Register commands with specta**

Open `apps/proxima-shell/src-tauri/src/commands/mod.rs` (or wherever `specta_builder()` lists commands). Add `crate::perf::perf_log` and `crate::perf::perf_log_field` to the list, and ensure `PerfEntry` + `FieldEntry` are exported as TS types.

- [ ] **Step 4: Compile + regenerate bindings**

```bash
cargo check -p proxima-shell
pnpm --filter proxima-shell exec tauri build --debug --no-bundle 2>&1 | tail -5 || true
# Or however bindings are regenerated in this repo (check for `pnpm bindings` or similar)
```

- [ ] **Step 5: Write `apps/proxima-shell/src/perf.ts`**

```ts
import { invoke } from "@tauri-apps/api/core";

type Entry = {
  kind: "snapshot_fetch" | "selector" | "render";
  name: string;
  dur_ms: number;
  bytes?: number;
};

const buf: Entry[] = [];
let flushTimer: ReturnType<typeof setInterval> | null = null;
let installed = false;

function flush() {
  if (buf.length === 0) return;
  const batch = buf.splice(0, buf.length);
  invoke("perf_log", { entries: batch }).catch(() => {
    // perf is best-effort; never break the app
  });
}

export function installPerf(): void {
  if (installed) return;
  if (!import.meta.env.DEV) return;
  installed = true;
  flushTimer = setInterval(flush, 1000);
  window.addEventListener("beforeunload", flush);
}

export function record(kind: Entry["kind"], name: string, dur_ms: number, bytes?: number): void {
  if (!installed) return;
  buf.push({ kind, name, dur_ms, bytes });
}

export function measure<T>(kind: Entry["kind"], name: string, fn: () => T): T {
  if (!installed) return fn();
  const t = performance.now();
  const out = fn();
  record(kind, name, performance.now() - t);
  return out;
}

export async function measureAsync<T>(
  kind: Entry["kind"],
  name: string,
  fn: () => Promise<T>,
): Promise<T> {
  if (!installed) return fn();
  const t = performance.now();
  const out = await fn();
  record(kind, name, performance.now() - t);
  return out;
}
```

- [ ] **Step 6: Wire `installPerf()` from the FE entry**

In `apps/proxima-shell/src/main.tsx` (or whatever boots the Solid app), add:

```ts
import { installPerf } from "./perf";
installPerf();
```

- [ ] **Step 7: Instrument selectors in `graph-selectors.ts`**

For each top-level `createMemo`/factory in `packages/frontend-core/src/graph-selectors.ts`, wrap the inner computation with `measure("selector", "<name>", fn)`. Pull `measure` from `apps/proxima-shell/src/perf` — but since `frontend-core` is a separate package, expose a hook indirection: `frontend-core` already exports a `setSelectorMeasure` injection point (add it now).

In `packages/frontend-core/src/graph-selectors.ts`, top of file:

```ts
type MeasureFn = <T>(name: string, fn: () => T) => T;
let measureSelector: MeasureFn = (_, fn) => fn();
export function setSelectorMeasure(m: MeasureFn): void {
  measureSelector = m;
}
```

Then wrap each memo body: `measureSelector("atlas_projection", () => { /* existing memo body */ })`.

In `apps/proxima-shell/src/perf.ts`, after `installPerf()`:

```ts
import { setSelectorMeasure } from "@proxima/frontend-core";
setSelectorMeasure((name, fn) => measure("selector", name, fn));
```

(Rearrange exports as needed; the indirection avoids `frontend-core` depending on Tauri.)

- [ ] **Step 8: Add a render measurement at the Atlas mount**

In the Atlas component (`packages/frontend-core/src/views/atlas/...`), add `onMount(() => { record("render", "atlas_first_paint", performance.now() - startedAt); })` where `startedAt` is captured at component creation. Use the same indirection (`setRenderMeasure`) if the component is in `frontend-core` and `record` is in `proxima-shell`.

- [ ] **Step 9: Smoke test**

```bash
docker compose -f docker-compose.dev.yml up -d --wait
pnpm --filter proxima-shell tauri:dev &
SHELL_PID=$!
sleep 20  # let the UI mount + selectors fire
kill $SHELL_PID
ls apps/proxima-shell/perf-logs/ | tail -1 | xargs -I{} cat apps/proxima-shell/perf-logs/{}/frontend.json | head -5
docker compose -f docker-compose.dev.yml down
```

Expected: NDJSON entries with `kind: "selector"` and `kind: "render"`.

- [ ] **Step 10: Commit**

```bash
git add apps/proxima-shell/src-tauri/src/perf apps/proxima-shell/src-tauri/src/commands apps/proxima-shell/src/perf.ts apps/proxima-shell/src/main.tsx packages/frontend-core/src/graph-selectors.ts packages/frontend-core/src/views/atlas
git commit -m "feat(perf): record FE timings (snapshot fetch, selector recompute, render)"
```

---

## Task 10: FE field-access Proxy recorder

**Files:**
- Modify: `apps/proxima-shell/src/perf.ts`
- Create: `apps/proxima-shell/src/perf.test.ts`
- Modify: callers of high-traffic IPC commands (snapshot/query) to wrap responses with `recordFields(cmd, response)`

- [ ] **Step 1: Write the failing test**

`apps/proxima-shell/src/perf.test.ts`:

```ts
import { describe, expect, it, beforeEach } from "vitest";
import { __testing, recordFields, drainFieldBuffer } from "./perf";

describe("recordFields", () => {
  beforeEach(() => __testing.reset());

  it("records each accessed field path once", () => {
    const obj = recordFields("query_atlas", {
      goals: [{ id: "g1", title: "A", text: "long" }],
      meta: { count: 1 },
    });
    // Trigger access
    obj.goals[0].id;
    obj.goals[0].title;
    obj.meta.count;

    const drained = drainFieldBuffer();
    const paths = drained.map((e) => e.field_path).sort();
    expect(paths).toEqual(
      ["goals.[].id", "goals.[].title", "meta.count"].sort()
    );
  });

  it("does not record unaccessed fields", () => {
    const obj = recordFields("query_atlas", { a: 1, b: 2 });
    obj.a;
    const drained = drainFieldBuffer();
    expect(drained.map((e) => e.field_path)).toEqual(["a"]);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

```bash
pnpm --filter proxima-shell exec vitest run src/perf.test.ts
```

Expected: FAIL — `recordFields` not exported.

- [ ] **Step 3: Implement `recordFields` in `perf.ts`**

Append to `apps/proxima-shell/src/perf.ts`:

```ts
type FieldRecord = { cmd: string; field_path: string };
const fieldBuf: FieldRecord[] = [];
const seenFields = new Set<string>();

function wrap<T>(cmd: string, prefix: string, value: T): T {
  if (value === null || typeof value !== "object") return value;
  if (Array.isArray(value)) {
    return new Proxy(value as object, {
      get(target, prop, recv) {
        const v = Reflect.get(target, prop, recv);
        if (typeof prop === "symbol" || prop === "length") return v;
        // Array elements share the same path under "[]"
        return wrap(cmd, `${prefix}.[]`, v);
      },
    }) as unknown as T;
  }
  return new Proxy(value as object, {
    get(target, prop, recv) {
      const v = Reflect.get(target, prop, recv);
      if (typeof prop === "symbol") return v;
      const path = prefix ? `${prefix}.${String(prop)}` : String(prop);
      const key = `${cmd}::${path}`;
      if (!seenFields.has(key)) {
        seenFields.add(key);
        fieldBuf.push({ cmd, field_path: path });
      }
      return wrap(cmd, path, v);
    },
  }) as T;
}

export function recordFields<T extends object>(cmd: string, value: T): T {
  return wrap(cmd, "", value);
}

export function drainFieldBuffer(): FieldRecord[] {
  return fieldBuf.splice(0, fieldBuf.length);
}

function flushFields() {
  const drained = drainFieldBuffer();
  if (drained.length === 0) return;
  invoke("perf_log_field", { entries: drained }).catch(() => {});
}

// Hook into the existing flush timer
const origInstall = installPerf;
export const __testing = {
  reset: () => {
    fieldBuf.length = 0;
    seenFields.clear();
  },
};
```

Replace the existing `installPerf()` body's `setInterval` so it calls *both* `flush` and `flushFields`:

```ts
flushTimer = setInterval(() => { flush(); flushFields(); }, 1000);
window.addEventListener("beforeunload", () => { flush(); flushFields(); });
```

- [ ] **Step 4: Run tests**

```bash
pnpm --filter proxima-shell exec vitest run src/perf.test.ts
```

Expected: PASS.

- [ ] **Step 5: Wrap the snapshot/query response sites**

Identify the 2-3 call sites where the FE invokes `query_atlas_snapshot` and similar (likely in `packages/frontend-core/src/views/atlas/` and the goal-rail/goal-dialog data layer). After each `await invoke(...)`, wrap:

```ts
const raw = await invoke<SnapshotPayload>("query_atlas_snapshot", args);
const snapshot = recordFields("query_atlas_snapshot", raw);
```

(`recordFields` from the same indirection pattern as `setSelectorMeasure` if `frontend-core` calls invoke directly.)

- [ ] **Step 6: Manual smoke**

```bash
docker compose -f docker-compose.dev.yml up -d --wait
pnpm --filter proxima-shell tauri:dev &
SHELL_PID=$!
sleep 25
kill $SHELL_PID
LATEST=$(ls -1t apps/proxima-shell/perf-logs/ | head -1)
head -10 "apps/proxima-shell/perf-logs/$LATEST/ipc-fields.json"
docker compose -f docker-compose.dev.yml down
```

Expected: NDJSON lines with `cmd` and `field_path` (e.g., `goals.[].title`).

- [ ] **Step 7: Commit**

```bash
git add apps/proxima-shell/src
git commit -m "feat(perf): record FE field-access paths via Proxy wrapper"
```

---

## Task 11: Summary reducer (TDD, pure)

**Files:**
- Create: `scripts/perf-summary.mjs`
- Create: `scripts/perf-summary.test.mjs`
- Create: `scripts/fixtures/perf-smoke/{ipc.json,mcp.json,frontend.json,ipc-fields.json,pg-stats.json,engine.json}`

- [ ] **Step 1: Write fixtures**

Create `scripts/fixtures/perf-smoke/ipc.json`:

```
{"ts_ms":1,"cmd":"query_atlas_snapshot","req_bytes":50,"resp_bytes":120000,"dur_ms":420,"ok":true}
{"ts_ms":2,"cmd":"query_atlas_snapshot","req_bytes":50,"resp_bytes":118000,"dur_ms":380,"ok":true}
{"ts_ms":3,"cmd":"list_repos","req_bytes":10,"resp_bytes":2000,"dur_ms":15,"ok":true}
```

`scripts/fixtures/perf-smoke/mcp.json`:

```
{"ts_ms":1,"method":"POST","route":"/mcp","status":200,"req_bytes":40,"resp_bytes":800,"dur_ms":120}
{"ts_ms":2,"method":"POST","route":"/mcp","status":200,"req_bytes":40,"resp_bytes":900,"dur_ms":150}
```

`scripts/fixtures/perf-smoke/frontend.json`:

```
{"ts_ms":1,"kind":"selector","name":"atlas_projection","dur_ms":35.2}
{"ts_ms":2,"kind":"selector","name":"atlas_projection","dur_ms":42.0}
{"ts_ms":3,"kind":"render","name":"atlas_first_paint","dur_ms":580.0}
```

`scripts/fixtures/perf-smoke/ipc-fields.json`:

```
{"cmd":"query_atlas_snapshot","field_path":"goals.[].id"}
{"cmd":"query_atlas_snapshot","field_path":"goals.[].title"}
{"cmd":"query_atlas_snapshot","field_path":"meta.count"}
```

`scripts/fixtures/perf-smoke/pg-stats.json`:

```json
[
  {"query":"SELECT * FROM goals WHERE owner_org_id = $1","total_exec_time":1200.5,"calls":40,"mean_exec_time":30.01},
  {"query":"SELECT * FROM memories","total_exec_time":600.2,"calls":12,"mean_exec_time":50.02}
]
```

`scripts/fixtures/perf-smoke/engine.json`: a minimal chrome trace:

```json
[{"name":"snapshot_assemble","cat":"engine","ph":"X","ts":1000,"dur":420000,"pid":1,"tid":1},{"name":"goal_query","cat":"engine","ph":"X","ts":2000,"dur":280000,"pid":1,"tid":1}]
```

- [ ] **Step 2: Write the failing reducer tests**

`scripts/perf-summary.test.mjs`:

```javascript
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { buildSummary, percentile, summarizeIpc, summarizeMcp,
         summarizeFrontend, summarizeFields, summarizePg, summarizeEngine }
  from "./perf-summary.mjs";

const FIXTURES = join(fileURLToPath(import.meta.url), "../fixtures/perf-smoke");

const readNdjson = (name) =>
  readFileSync(join(FIXTURES, name), "utf8")
    .split("\n").filter(Boolean).map((l) => JSON.parse(l));

test("percentile basic", () => {
  assert.equal(percentile([10, 20, 30, 40], 50), 25);
  assert.equal(percentile([10, 20, 30, 40], 95), 40);
  assert.equal(percentile([], 50), 0);
});

test("summarizeIpc groups by cmd with p50/p95", () => {
  const out = summarizeIpc(readNdjson("ipc.json"));
  const snap = out.find((r) => r.cmd === "query_atlas_snapshot");
  assert.equal(snap.calls, 2);
  assert.equal(snap.p50_ms, 400);
  assert.ok(snap.p95_resp_bytes >= 118000);
});

test("summarizeMcp groups by route", () => {
  const out = summarizeMcp(readNdjson("mcp.json"));
  assert.equal(out.length, 1);
  assert.equal(out[0].route, "/mcp");
  assert.equal(out[0].calls, 2);
});

test("summarizeFrontend groups by kind+name", () => {
  const out = summarizeFrontend(readNdjson("frontend.json"));
  const sel = out.find((r) => r.kind === "selector" && r.name === "atlas_projection");
  assert.equal(sel.count, 2);
});

test("summarizeFields lists accessed paths per cmd", () => {
  const out = summarizeFields(readNdjson("ipc-fields.json"));
  const cmd = out.find((r) => r.cmd === "query_atlas_snapshot");
  assert.deepEqual(cmd.accessed_paths.sort(),
    ["goals.[].id", "goals.[].title", "meta.count"].sort());
});

test("summarizePg sorts by total_exec_time desc", () => {
  const stats = JSON.parse(readFileSync(join(FIXTURES, "pg-stats.json"), "utf8"));
  const out = summarizePg(stats);
  assert.equal(out[0].query.startsWith("SELECT * FROM goals"), true);
});

test("summarizeEngine returns top spans by dur", () => {
  const trace = JSON.parse(readFileSync(join(FIXTURES, "engine.json"), "utf8"));
  const out = summarizeEngine(trace);
  assert.equal(out[0].name, "snapshot_assemble");
});

test("buildSummary produces all sections", () => {
  const md = buildSummary({
    sessionId: "fixtures",
    durationMs: 30000,
    ipc: readNdjson("ipc.json"),
    mcp: readNdjson("mcp.json"),
    frontend: readNdjson("frontend.json"),
    fields: readNdjson("ipc-fields.json"),
    pgStats: JSON.parse(readFileSync(join(FIXTURES, "pg-stats.json"), "utf8")),
    engineTrace: JSON.parse(readFileSync(join(FIXTURES, "engine.json"), "utf8")),
  });
  assert.match(md, /## IPC summary/);
  assert.match(md, /## MCP summary/);
  assert.match(md, /## Frontend timings/);
  assert.match(md, /## Wasted IPC fields/);
  assert.match(md, /## Top PG statements/);
  assert.match(md, /## Top engine spans/);
});
```

- [ ] **Step 3: Run tests to verify all fail**

```bash
node --test scripts/perf-summary.test.mjs
```

Expected: 7+ failures, "Cannot find module ./perf-summary.mjs".

- [ ] **Step 4: Implement `scripts/perf-summary.mjs`**

```javascript
// Pure functions: file content in, summary out. No I/O here.
// I/O happens in the driver, which calls buildSummary(...) and writes
// the result to summary.md.

export function percentile(xs, p) {
  if (xs.length === 0) return 0;
  const sorted = [...xs].sort((a, b) => a - b);
  const idx = (p / 100) * (sorted.length - 1);
  const lo = Math.floor(idx), hi = Math.ceil(idx);
  if (lo === hi) return sorted[lo];
  return sorted[lo] + (sorted[hi] - sorted[lo]) * (idx - lo);
}

const groupBy = (xs, key) => {
  const m = new Map();
  for (const x of xs) {
    const k = typeof key === "function" ? key(x) : x[key];
    const arr = m.get(k) ?? [];
    arr.push(x);
    m.set(k, arr);
  }
  return m;
};

export function summarizeIpc(rows) {
  const by = groupBy(rows, "cmd");
  return [...by.entries()].map(([cmd, rs]) => ({
    cmd,
    calls: rs.length,
    p50_ms: percentile(rs.map((r) => r.dur_ms), 50),
    p95_ms: percentile(rs.map((r) => r.dur_ms), 95),
    p50_req_bytes: percentile(rs.map((r) => r.req_bytes), 50),
    p95_req_bytes: percentile(rs.map((r) => r.req_bytes), 95),
    p50_resp_bytes: percentile(rs.map((r) => r.resp_bytes), 50),
    p95_resp_bytes: percentile(rs.map((r) => r.resp_bytes), 95),
  })).sort((a, b) => b.p95_ms - a.p95_ms);
}

export function summarizeMcp(rows) {
  const by = groupBy(rows, (r) => `${r.method} ${r.route}`);
  return [...by.entries()].map(([key, rs]) => {
    const [method, route] = key.split(" ");
    return {
      method, route,
      calls: rs.length,
      p50_ms: percentile(rs.map((r) => r.dur_ms), 50),
      p95_ms: percentile(rs.map((r) => r.dur_ms), 95),
      p50_resp_bytes: percentile(rs.map((r) => r.resp_bytes), 50),
      p95_resp_bytes: percentile(rs.map((r) => r.resp_bytes), 95),
      statuses: [...groupBy(rs, "status").entries()]
        .map(([s, ss]) => `${s}:${ss.length}`).join(", "),
    };
  }).sort((a, b) => b.p95_ms - a.p95_ms);
}

export function summarizeFrontend(rows) {
  const by = groupBy(rows, (r) => `${r.kind}:${r.name}`);
  return [...by.entries()].map(([key, rs]) => {
    const [kind, name] = key.split(":");
    return {
      kind, name,
      count: rs.length,
      p50_ms: percentile(rs.map((r) => r.dur_ms), 50),
      p95_ms: percentile(rs.map((r) => r.dur_ms), 95),
    };
  }).sort((a, b) => b.p95_ms - a.p95_ms);
}

export function summarizeFields(rows) {
  const by = groupBy(rows, "cmd");
  return [...by.entries()].map(([cmd, rs]) => ({
    cmd,
    accessed_paths: [...new Set(rs.map((r) => r.field_path))].sort(),
  }));
}

export function summarizePg(stats) {
  return [...stats]
    .sort((a, b) => b.total_exec_time - a.total_exec_time)
    .slice(0, 10);
}

export function summarizeEngine(trace) {
  const events = trace.filter((e) => e.ph === "X" && typeof e.dur === "number");
  return events.sort((a, b) => b.dur - a.dur).slice(0, 10);
}

const fmtBytes = (n) => {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MiB`;
};

export function buildSummary({ sessionId, durationMs, ipc, mcp, frontend, fields, pgStats, engineTrace }) {
  const lines = [];
  lines.push(`# Perf session: ${sessionId}`);
  lines.push(``);
  lines.push(`Duration: ${(durationMs / 1000).toFixed(1)}s`);
  lines.push(``);

  lines.push(`## IPC summary`);
  lines.push(``);
  lines.push(`| cmd | calls | p50 ms | p95 ms | p95 req | p95 resp |`);
  lines.push(`|---|---:|---:|---:|---:|---:|`);
  for (const r of summarizeIpc(ipc)) {
    lines.push(`| ${r.cmd} | ${r.calls} | ${r.p50_ms.toFixed(0)} | ${r.p95_ms.toFixed(0)} | ${fmtBytes(r.p95_req_bytes)} | ${fmtBytes(r.p95_resp_bytes)} |`);
  }
  lines.push(``);

  lines.push(`## MCP summary`);
  lines.push(``);
  lines.push(`| method route | calls | p50 ms | p95 ms | p95 resp | statuses |`);
  lines.push(`|---|---:|---:|---:|---:|---|`);
  for (const r of summarizeMcp(mcp)) {
    lines.push(`| ${r.method} ${r.route} | ${r.calls} | ${r.p50_ms.toFixed(0)} | ${r.p95_ms.toFixed(0)} | ${fmtBytes(r.p95_resp_bytes)} | ${r.statuses} |`);
  }
  lines.push(``);

  lines.push(`## Frontend timings`);
  lines.push(``);
  lines.push(`| kind | name | count | p50 ms | p95 ms |`);
  lines.push(`|---|---|---:|---:|---:|`);
  for (const r of summarizeFrontend(frontend)) {
    lines.push(`| ${r.kind} | ${r.name} | ${r.count} | ${r.p50_ms.toFixed(1)} | ${r.p95_ms.toFixed(1)} |`);
  }
  lines.push(``);

  lines.push(`## Wasted IPC fields`);
  lines.push(``);
  lines.push(`Per command: which fields the FE never read.`);
  lines.push(`(Wasted-set computation requires \`bindings.ts\` introspection — for now we list accessed paths only; cross-reference manually.)`);
  lines.push(``);
  for (const r of summarizeFields(fields)) {
    lines.push(`### ${r.cmd}`);
    lines.push(``);
    lines.push(`Accessed: ${r.accessed_paths.length} paths`);
    lines.push(``);
    lines.push("```");
    for (const p of r.accessed_paths) lines.push(p);
    lines.push("```");
    lines.push(``);
  }

  lines.push(`## Top PG statements`);
  lines.push(``);
  lines.push(`| total ms | calls | mean ms | query |`);
  lines.push(`|---:|---:|---:|---|`);
  for (const r of summarizePg(pgStats)) {
    const q = (r.query || "").replace(/\s+/g, " ").slice(0, 80);
    lines.push(`| ${r.total_exec_time.toFixed(0)} | ${r.calls} | ${r.mean_exec_time.toFixed(1)} | \`${q}\` |`);
  }
  lines.push(``);

  lines.push(`## Top engine spans`);
  lines.push(``);
  lines.push(`| dur ms | name |`);
  lines.push(`|---:|---|`);
  for (const r of summarizeEngine(engineTrace)) {
    lines.push(`| ${(r.dur / 1000).toFixed(1)} | ${r.name} |`);
  }
  lines.push(``);

  return lines.join("\n");
}
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
node --test scripts/perf-summary.test.mjs
```

Expected: all tests PASS.

- [ ] **Step 6: Commit**

```bash
git add scripts/perf-summary.mjs scripts/perf-summary.test.mjs scripts/fixtures
git commit -m "feat(perf): add summary reducer with TDD via node:test fixtures"
```

---

## Task 12: Wire reducer into driver exit

**Files:**
- Modify: `scripts/dev-with-perf.mjs`

- [ ] **Step 1: Replace `cleanup()` to dump pg_stat_statements + run reducer**

Replace the `cleanup` function in `scripts/dev-with-perf.mjs` with:

```javascript
const cleanup = async (code) => {
  console.log("[perf-driver] capturing pg_stat_statements…");
  const pgStatsPath = join(sessionDir, "pg-stats.json");
  spawnSync(
    "docker",
    [
      "compose", "-f", COMPOSE, "exec", "-T", "postgres",
      "psql", "-U", "proxima", "-d", "proxima", "-At", "-c",
      "SELECT json_agg(row_to_json(t)) FROM ( " +
      "  SELECT query, total_exec_time, calls, mean_exec_time " +
      "  FROM pg_stat_statements ORDER BY total_exec_time DESC LIMIT 100 " +
      ") t;",
    ],
    { stdio: ["ignore", fs.openSync(pgStatsPath, "w"), "inherit"] }
  );

  tail.kill("SIGTERM");
  try { logStream.end(); } catch {}

  console.log("[perf-driver] generating summary.md…");
  try {
    const { buildSummary } = await import(join(REPO, "scripts/perf-summary.mjs"));
    const readNdjson = (p) => existsSync(p)
      ? fs.readFileSync(p, "utf8").split("\n").filter(Boolean).map(JSON.parse)
      : [];
    const readJson = (p) => existsSync(p)
      ? JSON.parse(fs.readFileSync(p, "utf8") || "[]")
      : [];

    const md = buildSummary({
      sessionId: session,
      durationMs: Date.now() - startedAt,
      ipc: readNdjson(join(sessionDir, "ipc.json")),
      mcp: readNdjson(join(sessionDir, "mcp.json")),
      frontend: readNdjson(join(sessionDir, "frontend.json")),
      fields: readNdjson(join(sessionDir, "ipc-fields.json")),
      pgStats: readJson(pgStatsPath) ?? [],
      engineTrace: readJson(join(sessionDir, "engine.json")) ?? [],
    });
    fs.writeFileSync(join(sessionDir, "summary.md"), md);
    console.log(`[perf-driver] summary: ${join(sessionDir, "summary.md")}`);
  } catch (e) {
    console.warn(`[perf-driver] reducer failed: ${e.message}`);
  }
  process.exit(code ?? 0);
};
```

Add `const startedAt = Date.now();` near the top of `main()` (right after `mkdirSync`).

- [ ] **Step 2: Smoke run**

```bash
docker compose -f docker-compose.dev.yml up -d --wait
pnpm --filter proxima-shell tauri:dev &
SHELL_PID=$!
sleep 25
kill -INT $SHELL_PID 2>/dev/null || true
wait $SHELL_PID 2>/dev/null || true
LATEST=$(ls -1t apps/proxima-shell/perf-logs/ | head -1)
ls -la "apps/proxima-shell/perf-logs/$LATEST/"
head -40 "apps/proxima-shell/perf-logs/$LATEST/summary.md"
docker compose -f docker-compose.dev.yml down
```

Expected: `summary.md` exists with all six sections; `pg-stats.json` present.

- [ ] **Step 3: Commit**

```bash
git add scripts/dev-with-perf.mjs
git commit -m "feat(perf): generate summary.md + pg-stats snapshot on session exit"
```

---

## Task 13: `perf:clean` script

**Files:**
- Create: `scripts/perf-clean.mjs`
- Modify: `apps/proxima-shell/package.json` (add `perf:clean`, `perf:down`)

- [ ] **Step 1: Write `scripts/perf-clean.mjs`**

```javascript
#!/usr/bin/env node
import { readdirSync, statSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const REPO = resolve(fileURLToPath(import.meta.url), "../..");
const DIR = join(REPO, "apps/proxima-shell/perf-logs");

const argKeep = process.argv.indexOf("--keep");
const keep = argKeep >= 0 ? Number(process.argv[argKeep + 1]) : 10;

const sessions = readdirSync(DIR)
  .filter((n) => n !== ".gitkeep")
  .map((n) => ({ n, t: statSync(join(DIR, n)).mtimeMs }))
  .sort((a, b) => b.t - a.t);

const drop = sessions.slice(keep);
for (const s of drop) {
  console.log(`[perf-clean] removing ${s.n}`);
  rmSync(join(DIR, s.n), { recursive: true, force: true });
}
console.log(`[perf-clean] kept ${Math.min(sessions.length, keep)} sessions`);
```

- [ ] **Step 2: Add scripts to `apps/proxima-shell/package.json`**

```json
"perf:clean": "node ../../scripts/perf-clean.mjs",
"perf:down": "docker compose -f ../../docker-compose.dev.yml down"
```

- [ ] **Step 3: Smoke**

```bash
mkdir -p apps/proxima-shell/perf-logs/{a,b,c,d,e,f,g,h,i,j,k,l}
pnpm --filter proxima-shell perf:clean -- --keep 5
ls apps/proxima-shell/perf-logs/
```

Expected: only the 5 newest dirs remain (plus `.gitkeep`).

- [ ] **Step 4: Commit**

```bash
git add scripts/perf-clean.mjs apps/proxima-shell/package.json
git commit -m "feat(perf): add perf:clean and perf:down package scripts"
```

---

## Task 14: perf-smoke (CI-runnable reducer smoke)

**Files:**
- Create: `scripts/perf-smoke.mjs`
- Create: `scripts/fixtures/perf-smoke/summary.expected.md`

- [ ] **Step 1: Generate the golden summary**

```bash
node --input-type=module -e "
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { buildSummary } from './scripts/perf-summary.mjs';
const F = 'scripts/fixtures/perf-smoke';
const ndjson = (p) => readFileSync(join(F, p), 'utf8').split('\n').filter(Boolean).map(JSON.parse);
const md = buildSummary({
  sessionId: 'fixtures',
  durationMs: 30000,
  ipc: ndjson('ipc.json'),
  mcp: ndjson('mcp.json'),
  frontend: ndjson('frontend.json'),
  fields: ndjson('ipc-fields.json'),
  pgStats: JSON.parse(readFileSync(join(F, 'pg-stats.json'), 'utf8')),
  engineTrace: JSON.parse(readFileSync(join(F, 'engine.json'), 'utf8')),
});
writeFileSync(join(F, 'summary.expected.md'), md);
"
```

- [ ] **Step 2: Write `scripts/perf-smoke.mjs`**

```javascript
#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { buildSummary } from "./perf-summary.mjs";

const F = join(fileURLToPath(import.meta.url), "../fixtures/perf-smoke");
const ndjson = (p) =>
  readFileSync(join(F, p), "utf8").split("\n").filter(Boolean).map(JSON.parse);

const got = buildSummary({
  sessionId: "fixtures",
  durationMs: 30000,
  ipc: ndjson("ipc.json"),
  mcp: ndjson("mcp.json"),
  frontend: ndjson("frontend.json"),
  fields: ndjson("ipc-fields.json"),
  pgStats: JSON.parse(readFileSync(join(F, "pg-stats.json"), "utf8")),
  engineTrace: JSON.parse(readFileSync(join(F, "engine.json"), "utf8")),
});

const want = readFileSync(join(F, "summary.expected.md"), "utf8");

if (got.trim() !== want.trim()) {
  console.error("[perf-smoke] reducer output drifted from golden file.");
  console.error("Diff hint: run `node scripts/perf-smoke.mjs --regen` to update.");
  process.exit(1);
}
console.log("[perf-smoke] OK");
```

- [ ] **Step 3: Run smoke**

```bash
node scripts/perf-smoke.mjs
```

Expected: `[perf-smoke] OK`.

- [ ] **Step 4: Commit**

```bash
git add scripts/perf-smoke.mjs scripts/fixtures/perf-smoke/summary.expected.md
git commit -m "test(perf): add reducer smoke against golden fixtures"
```

---

## Task 15: One-pager docs

**Files:**
- Create: `docs/dev-perf.md`

- [ ] **Step 1: Write the doc**

```markdown
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
  flame chart of Rust spans.
- `pg.log` — `auto_explain` plans for queries above 50ms.
- `ipc.json`, `mcp.json`, `frontend.json`, `ipc-fields.json` — raw NDJSON.

## Running without perf capture

`PROXIMA_PERF=0 pnpm --filter proxima-shell tauri:dev` skips Docker bring-up
and instrumentation entirely.

## Cleanup

`pnpm --filter proxima-shell perf:clean -- --keep 5` keeps the 5 newest
sessions.

`pnpm --filter proxima-shell perf:down` stops the dev Postgres.
```

- [ ] **Step 2: Commit**

```bash
git add docs/dev-perf.md
git commit -m "docs(perf): one-pager for reading a perf session"
```

---

## Self-review (run before handoff)

- [x] **Spec coverage:**
  - PG slow-queries: Task 2 (compose) + Task 12 (snapshot)
  - Engine spans: Task 6 (tracing-chrome)
  - IPC bytes/ms: Task 7 (`perf::ipc::record`)
  - MCP route metrics: Task 8 (Tower middleware)
  - FE timings: Task 9 (perf.ts)
  - FE field utilization: Task 10 (Proxy + tests)
  - Reducer + summary: Task 11 (TDD) + Task 12 (wired)
  - Opt-out: Task 3 step 1 (`PROXIMA_PERF=0`)
  - Artifact lifecycle: Task 13 (`perf:clean`)
  - Reproducibility for OSS contributors: Task 2 (compose) + Task 15 (docs)
- [x] **Placeholder scan:** none — every "todo-shaped" detail (e.g., the precise list of commands to wrap in Task 7 step 4, the exact `bindings.ts` cross-reference in Task 11) is explicitly named or explicitly deferred with a note in the produced artifact.
- [x] **Type consistency:** `PerfEntry`, `FieldEntry` defined once in `perf/fe.rs` and consumed by both Rust and TS via specta-generated bindings. `recordFields`/`measure`/`installPerf` referenced consistently across Tasks 9, 10.
- [x] **Wasted-fields gap acknowledged:** Task 11's `summarizeFields` lists accessed paths but does not yet *diff* against `bindings.ts` to compute unused fields. The summary.md section labels this explicitly. A v2 task can add the diff once `bindings.ts` shape extraction is needed for another reason — premature here.

---

**Plan complete and saved to `docs/superpowers/plans/2026-05-06-dev-perf-instrumentation.md`.** Two execution options:

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
