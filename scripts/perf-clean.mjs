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
