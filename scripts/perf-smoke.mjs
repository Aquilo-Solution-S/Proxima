#!/usr/bin/env node
import { readFileSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { buildSummary } from "./perf-summary.mjs";

const F = join(dirname(fileURLToPath(import.meta.url)), "fixtures/perf-smoke");
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

const goldenPath = join(F, "summary.expected.md");

if (process.argv.includes("--regen")) {
  writeFileSync(goldenPath, got);
  console.log("[perf-smoke] regenerated golden summary.expected.md");
  process.exit(0);
}

const want = readFileSync(goldenPath, "utf8");

if (got.trim() !== want.trim()) {
  console.error("[perf-smoke] reducer output drifted from golden file.");
  console.error("To update the golden: node scripts/perf-smoke.mjs --regen");
  process.exit(1);
}
console.log("[perf-smoke] OK");
