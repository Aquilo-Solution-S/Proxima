import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import {
  buildSummary,
  percentile,
  summarizeIpc,
  summarizeMcp,
  summarizeFrontend,
  summarizeFields,
  summarizePg,
  summarizeEngine,
} from "./perf-summary.mjs";

const FIXTURES = join(dirname(fileURLToPath(import.meta.url)), "fixtures/perf-smoke");

const readNdjson = (name) =>
  readFileSync(join(FIXTURES, name), "utf8")
    .split("\n")
    .filter(Boolean)
    .map((l) => JSON.parse(l));

test("percentile basic (linear interpolation)", () => {
  // p50 of [10,20,30,40]: idx = 0.5 * 3 = 1.5 → between 20 and 30 → 25
  assert.equal(percentile([10, 20, 30, 40], 50), 25);
  // p100 → last element
  assert.equal(percentile([10, 20, 30, 40], 100), 40);
  assert.equal(percentile([], 50), 0);
});

test("summarizeIpc groups by cmd with p50/p95", () => {
  const out = summarizeIpc(readNdjson("ipc.json"));
  const snap = out.find((r) => r.cmd === "query");
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
  const cmd = out.find((r) => r.cmd === "query");
  assert.deepEqual(
    cmd.accessed_paths.slice().sort(),
    ["goals.[].id", "goals.[].title", "meta.count"].sort(),
  );
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
