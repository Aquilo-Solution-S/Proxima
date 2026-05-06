#!/usr/bin/env node
// Session driver wrapping `tauri dev` with per-session perf capture.
// Skip everything when PROXIMA_PERF=0.

import { spawn, spawnSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { fileURLToPath } from "node:url";
import * as fs from "node:fs";

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

  const docker = spawnSync("docker", ["info"], { stdio: "ignore" });
  if (docker.status !== 0) {
    console.error(
      "[perf-driver] Docker is not running. Either start Docker, or run with PROXIMA_PERF=0 to skip perf capture.",
    );
    process.exit(1);
  }

  const session = timestamp();
  const sessionDir = join(PERF_LOGS, session);
  mkdirSync(sessionDir, { recursive: true });
  console.log(`[perf-driver] session dir: ${sessionDir}`);
  const startedAt = Date.now();

  console.log("[perf-driver] bringing up Postgres…");
  runOrDie("docker", ["compose", "-f", COMPOSE, "up", "-d", "--wait"]);

  const reset = spawnSync(
    "docker",
    [
      "compose", "-f", COMPOSE, "exec", "-T", "postgres",
      "psql", "-U", "proxima", "-d", "proxima",
      "-c", "SELECT pg_stat_statements_reset();",
    ],
    { stdio: "inherit" },
  );
  if (reset.status !== 0) {
    console.warn("[perf-driver] pg_stat_statements_reset failed — continuing");
  }

  const pgLog = join(sessionDir, "pg.log");
  const tail = spawn(
    "docker",
    ["compose", "-f", COMPOSE, "logs", "-f", "--no-color", "postgres"],
    { stdio: ["ignore", "pipe", "inherit"] },
  );
  const logStream = fs.createWriteStream(pgLog);
  tail.stdout.pipe(logStream);

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
    console.log("[perf-driver] capturing pg_stat_statements…");
    const pgStatsPath = join(sessionDir, "pg-stats.json");
    try {
      const sql =
        "SELECT COALESCE(json_agg(row_to_json(t)), '[]'::json) FROM (" +
        "  SELECT query, total_exec_time, calls, mean_exec_time" +
        "  FROM pg_stat_statements ORDER BY total_exec_time DESC LIMIT 100" +
        ") t;";
      const out = spawnSync(
        "docker",
        [
          "compose", "-f", COMPOSE, "exec", "-T", "postgres",
          "psql", "-U", "proxima", "-d", "proxima",
          "-At", "-c", sql,
        ],
        { encoding: "utf8" },
      );
      if (out.status === 0 && out.stdout) {
        fs.writeFileSync(pgStatsPath, out.stdout.trim());
      }
    } catch (e) {
      console.warn(`[perf-driver] pg_stat_statements snapshot failed: ${e.message}`);
    }

    try { tail.kill("SIGTERM"); } catch {}
    try { logStream.end(); } catch {}

    console.log("[perf-driver] generating summary.md…");
    try {
      const { buildSummary } = await import(join(REPO, "scripts/perf-summary.mjs"));
      const readNdjson = (p) =>
        fs.existsSync(p)
          ? fs.readFileSync(p, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l))
          : [];
      const readJson = (p) => {
        if (!fs.existsSync(p)) return [];
        const raw = fs.readFileSync(p, "utf8").trim();
        if (!raw) return [];
        try {
          return JSON.parse(raw);
        } catch {
          return [];
        }
      };

      const md = buildSummary({
        sessionId: session,
        durationMs: Date.now() - startedAt,
        ipc: readNdjson(join(sessionDir, "ipc.json")),
        mcp: readNdjson(join(sessionDir, "mcp.json")),
        frontend: readNdjson(join(sessionDir, "frontend.json")),
        fields: readNdjson(join(sessionDir, "ipc-fields.json")),
        pgStats: readJson(pgStatsPath),
        engineTrace: readJson(join(sessionDir, "engine.json")),
      });
      fs.writeFileSync(join(sessionDir, "summary.md"), md);
      console.log(`[perf-driver] summary: ${join(sessionDir, "summary.md")}`);
    } catch (e) {
      console.warn(`[perf-driver] reducer failed: ${e.message}`);
    }
    process.exit(code ?? 0);
  };

  process.on("SIGINT", () => {
    cleanup(0);
  });
  child.on("exit", (code) => {
    cleanup(code ?? 0);
  });
}

await main();
