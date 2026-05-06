// Pure functions: file content in, summary out. No I/O here.
// I/O happens in the driver, which calls buildSummary(...) and writes
// the result to summary.md.

export function percentile(xs, p) {
  if (xs.length === 0) return 0;
  const sorted = [...xs].sort((a, b) => a - b);
  const idx = (p / 100) * (sorted.length - 1);
  const lo = Math.floor(idx);
  const hi = Math.ceil(idx);
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
  return [...by.entries()]
    .map(([cmd, rs]) => ({
      cmd,
      calls: rs.length,
      p50_ms: percentile(rs.map((r) => r.dur_ms), 50),
      p95_ms: percentile(rs.map((r) => r.dur_ms), 95),
      p50_req_bytes: percentile(rs.map((r) => r.req_bytes), 50),
      p95_req_bytes: percentile(rs.map((r) => r.req_bytes), 95),
      p50_resp_bytes: percentile(rs.map((r) => r.resp_bytes), 50),
      p95_resp_bytes: percentile(rs.map((r) => r.resp_bytes), 95),
    }))
    .sort((a, b) => b.p95_ms - a.p95_ms);
}

export function summarizeMcp(rows) {
  const by = groupBy(rows, (r) => `${r.method} ${r.route}`);
  return [...by.entries()]
    .map(([key, rs]) => {
      const sp = key.indexOf(" ");
      const method = key.slice(0, sp);
      const route = key.slice(sp + 1);
      return {
        method,
        route,
        calls: rs.length,
        p50_ms: percentile(rs.map((r) => r.dur_ms), 50),
        p95_ms: percentile(rs.map((r) => r.dur_ms), 95),
        p50_resp_bytes: percentile(rs.map((r) => r.resp_bytes), 50),
        p95_resp_bytes: percentile(rs.map((r) => r.resp_bytes), 95),
        statuses: [...groupBy(rs, "status").entries()]
          .map(([s, ss]) => `${s}:${ss.length}`)
          .join(", "),
      };
    })
    .sort((a, b) => b.p95_ms - a.p95_ms);
}

export function summarizeFrontend(rows) {
  const by = groupBy(rows, (r) => `${r.kind}::${r.name}`);
  return [...by.entries()]
    .map(([key, rs]) => {
      const sep = key.indexOf("::");
      return {
        kind: key.slice(0, sep),
        name: key.slice(sep + 2),
        count: rs.length,
        p50_ms: percentile(rs.map((r) => r.dur_ms), 50),
        p95_ms: percentile(rs.map((r) => r.dur_ms), 95),
      };
    })
    .sort((a, b) => b.p95_ms - a.p95_ms);
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

export function buildSummary({
  sessionId,
  durationMs,
  ipc,
  mcp,
  frontend,
  fields,
  pgStats,
  engineTrace,
}) {
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
    lines.push(
      `| ${r.cmd} | ${r.calls} | ${r.p50_ms.toFixed(0)} | ${r.p95_ms.toFixed(0)} | ${fmtBytes(r.p95_req_bytes)} | ${fmtBytes(r.p95_resp_bytes)} |`,
    );
  }
  lines.push(``);

  lines.push(`## MCP summary`);
  lines.push(``);
  lines.push(`| method route | calls | p50 ms | p95 ms | p95 resp | statuses |`);
  lines.push(`|---|---:|---:|---:|---:|---|`);
  for (const r of summarizeMcp(mcp)) {
    lines.push(
      `| ${r.method} ${r.route} | ${r.calls} | ${r.p50_ms.toFixed(0)} | ${r.p95_ms.toFixed(0)} | ${fmtBytes(r.p95_resp_bytes)} | ${r.statuses} |`,
    );
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
  lines.push(`Per command: which fields the FE actually read.`);
  lines.push(``);
  lines.push(
    `(Wasted-set computation requires \`bindings.ts\` introspection — for now we list accessed paths only; cross-reference manually.)`,
  );
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
    lines.push(
      `| ${r.total_exec_time.toFixed(0)} | ${r.calls} | ${r.mean_exec_time.toFixed(1)} | \`${q}\` |`,
    );
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
