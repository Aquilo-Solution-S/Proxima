// Dev-time perf capture. Active only when DEV build AND
// PROXIMA_PERF_SESSION_DIR is set on the engine side. The Tauri
// commands are no-ops in production / when the session dir is unset,
// so installing here is unconditional in dev.

import { commands } from "@proxima/core";

type Kind = "snapshot_fetch" | "selector" | "render";

type Entry = {
  kind: Kind;
  name: string;
  dur_ms: number;
  bytes: number | null;
};

type FieldRecord = { cmd: string; field_path: string };

const buf: Entry[] = [];
const fieldBuf: FieldRecord[] = [];
const seenFields = new Set<string>();
let installed = false;

function flush() {
  if (buf.length > 0) {
    const batch = buf.splice(0, buf.length);
    commands.perfLog(batch).catch(() => {});
  }
  if (fieldBuf.length > 0) {
    const batch = fieldBuf.splice(0, fieldBuf.length);
    commands.perfLogField(batch).catch(() => {});
  }
}

export function installPerf(): void {
  if (installed) return;
  if (!import.meta.env.DEV) return;
  installed = true;
  setInterval(flush, 1000);
  window.addEventListener("beforeunload", flush);
  // Expose recordFields to frontend-core's tauri-client via a global
  // hook so query responses get Proxy-wrapped automatically.
  (globalThis as unknown as { __proximaRecordFields: typeof recordFields }).__proximaRecordFields =
    recordFields;
}

export function record(kind: Kind, name: string, dur_ms: number, bytes: number | null = null): void {
  if (!installed) return;
  buf.push({ kind, name, dur_ms, bytes });
}

export function measure<T>(kind: Kind, name: string, fn: () => T): T {
  if (!installed) return fn();
  const t = performance.now();
  const out = fn();
  record(kind, name, performance.now() - t);
  return out;
}

export async function measureAsync<T>(
  kind: Kind,
  name: string,
  fn: () => Promise<T>,
): Promise<T> {
  if (!installed) return fn();
  const t = performance.now();
  const out = await fn();
  record(kind, name, performance.now() - t);
  return out;
}

function wrap<T>(cmd: string, prefix: string, value: T): T {
  if (value === null || typeof value !== "object") return value;
  if (Array.isArray(value)) {
    return new Proxy(value as object, {
      get(target, prop, recv) {
        const v = Reflect.get(target, prop, recv);
        if (typeof prop === "symbol" || prop === "length") return v;
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

export function recordFields<T>(cmd: string, value: T): T {
  if (!installed) return value;
  return wrap(cmd, "", value);
}

export function drainFieldBuffer(): FieldRecord[] {
  return fieldBuf.splice(0, fieldBuf.length);
}

export const __testing = {
  reset: () => {
    buf.length = 0;
    fieldBuf.length = 0;
    seenFields.clear();
  },
  forceInstall: () => {
    installed = true;
  },
};
