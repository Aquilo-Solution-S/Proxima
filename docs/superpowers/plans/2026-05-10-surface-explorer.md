# Surface Explorer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the current three-stacked-section Surface view with a
dev-first store explorer: pillar tabs, chip rail + filter drawer,
schema-driven detail pane (PAYLOAD / LINEAGE / METADATA), and activity
strip. All work is frontend; no engine or wire-grpc changes.

**Architecture:** Pure Solid + Tauri work in
`packages/frontend-core`. The current 863-line `surface.tsx` is
decomposed into focused subcomponents under `views/surface/`. The
existing `useGraphFilter` and `filterGraphSnapshot` are extended with
three new facets (authoredBy, timeRange, sizeRange). A new
`memoryProvenance` map on `GraphSnapshot`, populated during ChangeEvent
ingest, supplies authored-by + ULID-decoded written-at without any
new transport. 1-hop lineage is a pure selector over the
already-loaded `edgesById`.

**Tech Stack:** Solid 1.x, Tauri 2, Vitest + @solidjs/testing-library,
TypeScript, cbor-x, the existing `Hub` schema-renderer registry.

**Spec:** `docs/superpowers/specs/2026-05-10-surface-explorer-design.md`

---

## File Structure

**New files (all under `packages/frontend-core/src/`):**

| Path | Responsibility |
| --- | --- |
| `ulid.ts`                          | `ulidTimestampMs(ulid)` — Crockford-base32 decode of the timestamp half of a ULID. ~30 lines. |
| `ulid.test.ts`                     | Round-trip + spec-vector tests. |
| `views/surface/index.tsx`          | Public re-export of `FullSurface`. |
| `views/surface/tab-strip.tsx`      | Pillar tabs (`All / P / A / F / G`) with counts. |
| `views/surface/chip-rail.tsx`      | Active filter chips above the list. |
| `views/surface/filter-drawer.tsx`  | Slide-in form for all 6 facets. |
| `views/surface/row-list.tsx`       | Virtualized list with per-tab columns. |
| `views/surface/detail-pane.tsx`    | Three-block detail (PAYLOAD / LINEAGE / METADATA). |
| `views/surface/activity-strip.tsx` | Bottom strip + Event Stream drawer toggle. |
| `views/surface/keys.ts`            | Keyboard mappings (⌘1-5, ⌘F, ⌘E, …). |
| `views/surface/<sibling>.test.tsx` | Per-component tests. |

**Modified files:**

| Path | Change |
| --- | --- |
| `graph-store.tsx`                  | Add `memoryProvenance` map; populate during EntityAppend ingest and bootstrap event seed. |
| `graph-store.test.tsx`             | Add provenance-population tests. |
| `graph-filter-store.tsx`           | Widen `GraphFilterState` with `authoredBy`, `timeRange`, `sizeRange`. New setters + reset. |
| `graph-selectors.ts`               | Honor new facets in `filterGraphSnapshot`. Add `oneHopLineage()`. |
| `graph-selectors.test.tsx`         | Cover new filters and `oneHopLineage`. |
| `views/surface.tsx`                | Replaced with thin orchestrator — composes new subcomponents, retires `GoalRail`, wires keys. Drops from 863 lines to ~200. |
| `views/surface.test.tsx`           | Rewritten green; covers filter chip ↔ drawer round-trip, tab switch, detail pane on selection. |
| `views/surface.css`                | New rules for tab-strip, chip-rail, filter-drawer, detail-pane, activity-strip. Old goal-rail rules removed. |

**Deleted (in step 11):** all legacy `Section`, `MemoryExplorer`,
`TraversalLanes`, `GoalRail`, `LayerHeader`, `MemoryCard`,
`renderGoalPayload`, `renderMemoryPayload` helpers in `surface.tsx`.

**Run tests:** `pnpm -C packages/frontend-core test` (vitest run).
**Typecheck:** `pnpm -C packages/frontend-core typecheck`.

---

## Task 1: ULID timestamp helper

**Files:**
- Create: `packages/frontend-core/src/ulid.ts`
- Test: `packages/frontend-core/src/ulid.test.ts`

- [ ] **Step 1: Write the failing test**

```ts
// packages/frontend-core/src/ulid.test.ts
import { describe, expect, it } from "vitest";
import { ulidTimestampMs } from "./ulid";

describe("ulidTimestampMs", () => {
  it("decodes the spec example", () => {
    // 01ARYZ6S41 → 1469918176385 (ULID spec vector)
    expect(ulidTimestampMs("01ARYZ6S41TS5G7QFC0V44N5KH")).toBe(1469918176385);
  });

  it("decodes a min-time ULID", () => {
    expect(ulidTimestampMs("00000000000000000000000000")).toBe(0);
  });

  it("rejects too-short input", () => {
    expect(() => ulidTimestampMs("01ARYZ6S")).toThrow(/26 characters/);
  });

  it("rejects invalid Crockford characters", () => {
    expect(() => ulidTimestampMs("01ARYZ6S41ULOI0000000000000")).toThrow();
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm -C packages/frontend-core test src/ulid.test.ts`
Expected: FAIL — module `./ulid` does not exist.

- [ ] **Step 3: Implement minimal code**

```ts
// packages/frontend-core/src/ulid.ts
const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const TIME_LEN = 10;
const ULID_LEN = 26;

export function ulidTimestampMs(ulid: string): number {
  if (ulid.length !== ULID_LEN) {
    throw new Error(`ULID must be 26 characters; got ${ulid.length}`);
  }
  const upper = ulid.toUpperCase();
  let ms = 0;
  for (let i = 0; i < TIME_LEN; i++) {
    const idx = CROCKFORD.indexOf(upper[i]);
    if (idx < 0) {
      throw new Error(`ULID character at ${i} (${upper[i]}) is not Crockford-base32`);
    }
    ms = ms * 32 + idx;
  }
  return ms;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `pnpm -C packages/frontend-core test src/ulid.test.ts`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add packages/frontend-core/src/ulid.ts packages/frontend-core/src/ulid.test.ts
git commit -m "$(cat <<'EOF'
feat(frontend-core): add ulidTimestampMs helper

Decodes the timestamp half of a Crockford-base32 ULID. Surface
detail pane uses this to project written_at without a wire field.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Provenance index in `graph-store`

**Goal:** Add a `memoryProvenance` map to `GraphSnapshot` that maps
`memory_id → { creating_seq, authoring_personality_instance_id,
written_at_ms }`. Populate it during `EntityAppend` ingestion and
during bootstrap event seed.

**Files:**
- Modify: `packages/frontend-core/src/graph-store.tsx`
- Test: `packages/frontend-core/src/graph-store.test.tsx`

- [ ] **Step 1: Write the failing test**

Append to `graph-store.test.tsx`:

```tsx
describe("memoryProvenance", () => {
  it("is populated on EntityAppend ingest", async () => {
    const { store, hub } = createTestHarness();
    const memId = "01ARYZ6S41TS5G7QFC0V44N5KH";
    const seq = "01ARYZ6S41TS5G7QFC0V44N5KH";
    await store.refresh();
    pushChangeEvent(store, {
      seq,
      owner,
      authoring_personality_instance_id: "personality-rust",
      kind: {
        EntityAppend: {
          entity_kind: "Fact",
          entity: { Memory: memId },
          schema_id: "proxima-code/code-chunk-v1",
          schema_version: 1,
          supersedes: null,
        },
      },
    });
    const prov = store.state().memoryProvenance.get(memId);
    expect(prov).toBeDefined();
    expect(prov?.creating_seq).toBe(seq);
    expect(prov?.authoring_personality_instance_id).toBe("personality-rust");
    expect(prov?.written_at_ms).toBe(1469918176385);
  });

  it("leaves provenance unset when no creating event has been seen", async () => {
    const { store } = createTestHarness();
    await store.refresh();
    expect(store.state().memoryProvenance.size).toBe(0);
  });
});
```

If `createTestHarness` and `pushChangeEvent` helpers don't exist, factor
them out of the existing tests at the top of the file (they already
build a store + hub + sentinel owner — extract into helpers used by
both old and new tests). Use the existing pattern of pushing
`ChangeEvent` through whatever stream-fixture the file already uses.

- [ ] **Step 2: Run the test to verify it fails**

Run: `pnpm -C packages/frontend-core test src/graph-store.test.tsx`
Expected: FAIL — `memoryProvenance` not found on `GraphSnapshot`.

- [ ] **Step 3: Add the type and field**

In `graph-store.tsx` add to the type set near `DecodedMemory`:

```ts
export interface MemoryProvenance {
  creating_seq: string;
  authoring_personality_instance_id: string | null;
  written_at_ms: number;
}
```

Extend `GraphSnapshot`:

```ts
export interface GraphSnapshot {
  // ...existing fields...
  memoryProvenance: ReadonlyMap<string, MemoryProvenance>;
}
```

Initial state: include `memoryProvenance: new Map()` everywhere
`GraphSnapshot` is constructed (search the file for the empty-state
factory and the bootstrap reducer; both need updating).

- [ ] **Step 4: Populate during EntityAppend ingest**

Find the `ingestEvent` (or equivalent — search for the function that
handles `EntityAppend` and adds to `memoriesById`). Where it records
the new memory id, also write to `memoryProvenance`:

```ts
import { ulidTimestampMs } from "./ulid";

// inside the EntityAppend branch:
const append = event.kind.EntityAppend;
if (append) {
  const memoryId =
    "Memory" in append.entity ? append.entity.Memory : null;
  if (memoryId !== null) {
    const provenance: MemoryProvenance = {
      creating_seq: event.seq,
      authoring_personality_instance_id:
        event.authoring_personality_instance_id ?? null,
      written_at_ms: ulidTimestampMs(event.seq),
    };
    nextProvenance = new Map(prevProvenance);
    nextProvenance.set(memoryId, provenance);
  }
}
```

Carry `memoryProvenance: nextProvenance ?? prevProvenance` into the
new snapshot. Do NOT overwrite an existing entry (memories may receive
multiple events; the earliest wins — guard with `if
(!nextProvenance.has(memoryId))`).

- [ ] **Step 5: Run tests to verify they pass**

Run: `pnpm -C packages/frontend-core test src/graph-store.test.tsx`
Expected: PASS, including the new provenance tests and the existing
suite (regression-free).

- [ ] **Step 6: Typecheck**

Run: `pnpm -C packages/frontend-core typecheck`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add packages/frontend-core/src/graph-store.tsx packages/frontend-core/src/graph-store.test.tsx
git commit -m "$(cat <<'EOF'
feat(frontend-core): index memory provenance from ChangeEvents

GraphSnapshot now carries a memory_id → { creating_seq,
authoring_personality_instance_id, written_at_ms } map populated
during EntityAppend ingestion. Surface filters use this to support
authored_by and time facets without a wire change.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: One-hop lineage selector

**Files:**
- Modify: `packages/frontend-core/src/graph-selectors.ts`
- Test: `packages/frontend-core/src/graph-selectors.test.tsx`

- [ ] **Step 1: Write the failing test**

Append to `graph-selectors.test.tsx`:

```ts
describe("oneHopLineage", () => {
  it("groups outbound and inbound by relation/target_pillar/target_schema_id", () => {
    const memoryId = "F1";
    const memoriesById = new Map<string, DecodedMemory>([
      ["F1", makeMemory("F1", "Fact", "schema-a")],
      ["A1", makeMemory("A1", "Abstraction", "schema-b")],
      ["A2", makeMemory("A2", "Abstraction", "schema-b")],
      ["P1", makeMemory("P1", "Perspective", "schema-c")],
    ]);
    const edgesById = new Map<string, EdgeRow>([
      ["e1", edge("e1", "informs", { Memory: "F1" }, { Memory: "A1" })],
      ["e2", edge("e2", "informs", { Memory: "F1" }, { Memory: "A2" })],
      ["e3", edge("e3", "asserts", { Memory: "P1" }, { Memory: "F1" })],
    ]);
    const result = oneHopLineage(memoryId, edgesById, memoriesById);
    expect(result.outbound).toEqual([
      { relation: "informs", target_kind: "Abstraction", target_schema_id: "schema-b", count: 2 },
    ]);
    expect(result.inbound).toEqual([
      { relation: "asserts", target_kind: "Perspective", target_schema_id: "schema-c", count: 1 },
    ]);
  });

  it("returns empty arrays when no edges incident on the memory", () => {
    const result = oneHopLineage("LONELY", new Map(), new Map());
    expect(result.outbound).toEqual([]);
    expect(result.inbound).toEqual([]);
  });
});
```

Add `makeMemory` and `edge` test helpers near the top of the file if
not already present.

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm -C packages/frontend-core test src/graph-selectors.test.tsx`
Expected: FAIL — `oneHopLineage` not exported.

- [ ] **Step 3: Implement**

Add to `graph-selectors.ts`:

```ts
export interface LineageGroup {
  relation: string;
  target_kind: EntityKind;
  target_schema_id: string;
  count: number;
}

export interface OneHopLineage {
  outbound: LineageGroup[];
  inbound: LineageGroup[];
}

export function oneHopLineage(
  memoryId: string,
  edgesById: ReadonlyMap<string, EdgeRow>,
  memoriesById: ReadonlyMap<string, DecodedMemory>,
): OneHopLineage {
  const outboundCounts = new Map<string, LineageGroup>();
  const inboundCounts = new Map<string, LineageGroup>();
  for (const edge of edgesById.values()) {
    const sourceMem =
      "Memory" in edge.source ? edge.source.Memory : null;
    const targetMem =
      "Memory" in edge.target ? edge.target.Memory : null;
    if (sourceMem === memoryId && targetMem !== null) {
      const target = memoriesById.get(targetMem);
      if (target === undefined) continue;
      const key = `${edge.relation}|${target.row.kind}|${target.row.schema_id}`;
      const existing = outboundCounts.get(key);
      if (existing) existing.count += 1;
      else outboundCounts.set(key, {
        relation: edge.relation,
        target_kind: target.row.kind,
        target_schema_id: target.row.schema_id,
        count: 1,
      });
    } else if (targetMem === memoryId && sourceMem !== null) {
      const source = memoriesById.get(sourceMem);
      if (source === undefined) continue;
      const key = `${edge.relation}|${source.row.kind}|${source.row.schema_id}`;
      const existing = inboundCounts.get(key);
      if (existing) existing.count += 1;
      else inboundCounts.set(key, {
        relation: edge.relation,
        target_kind: source.row.kind,
        target_schema_id: source.row.schema_id,
        count: 1,
      });
    }
  }
  return {
    outbound: Array.from(outboundCounts.values()),
    inbound: Array.from(inboundCounts.values()),
  };
}
```

- [ ] **Step 4: Run tests**

Run: `pnpm -C packages/frontend-core test src/graph-selectors.test.tsx`
Expected: PASS (new + existing).

- [ ] **Step 5: Commit**

```bash
git add packages/frontend-core/src/graph-selectors.ts packages/frontend-core/src/graph-selectors.test.tsx
git commit -m "$(cat <<'EOF'
feat(frontend-core): add oneHopLineage selector

Pure selector over the already-loaded edgesById map. Groups
incident edges by (relation, target_kind, target_schema_id) and
returns outbound/inbound counts. Surface detail pane uses this for
the LINEAGE block; no engine work needed since the graph store
already loads all edges.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Widen `GraphFilterState`

**Files:**
- Modify: `packages/frontend-core/src/graph-filter-store.tsx`
- Modify: `packages/frontend-core/src/graph-selectors.ts`
- Test: `packages/frontend-core/src/graph-selectors.test.tsx`

- [ ] **Step 1: Write the failing test**

Append to `graph-selectors.test.tsx` — three small cases:

```ts
describe("filterGraphSnapshot — new facets", () => {
  it("filters by authoredBy via memoryProvenance", () => {
    const snapshot = snapshotWith({
      memories: [
        ["m1", "Fact", "schema-a"],
        ["m2", "Fact", "schema-a"],
      ],
      provenance: {
        m1: prov("01ARYZ6S41TS5G7QFC0V44N5KH", "personality-rust"),
        m2: prov("01ARYZ6S42TS5G7QFC0V44N5KH", "personality-go"),
      },
    });
    const filter = {
      ...defaultGraphFilterState(),
      authoredBy: new Set(["personality-rust"]),
    };
    const result = filterGraphSnapshot(snapshot, filter, hub);
    expect(result.memories.map((m) => m.row.id)).toEqual(["m1"]);
  });

  it("filters by timeRange (inclusive)", () => {
    const snapshot = snapshotWith({
      memories: [
        ["m1", "Fact", "schema-a"],
        ["m2", "Fact", "schema-a"],
      ],
      provenance: {
        m1: provAt(1000),
        m2: provAt(5000),
      },
    });
    const filter = {
      ...defaultGraphFilterState(),
      timeRange: { fromMs: 2000, toMs: 9000 },
    };
    expect(filterGraphSnapshot(snapshot, filter, hub).memories.map((m) => m.row.id))
      .toEqual(["m2"]);
  });

  it("filters by sizeRange using payload byte length", () => {
    const snapshot = snapshotWith({
      memories: [
        ["m1", "Fact", "schema-a", new Uint8Array(100)],
        ["m2", "Fact", "schema-a", new Uint8Array(2000)],
      ],
    });
    const filter = {
      ...defaultGraphFilterState(),
      sizeRange: { minBytes: 1000, maxBytes: 5000 },
    };
    expect(filterGraphSnapshot(snapshot, filter, hub).memories.map((m) => m.row.id))
      .toEqual(["m2"]);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm -C packages/frontend-core test src/graph-selectors.test.tsx`
Expected: FAIL — `authoredBy`, `timeRange`, `sizeRange` not on
`GraphFilterState`.

- [ ] **Step 3: Widen `GraphFilterState`**

Edit `graph-filter-store.tsx`:

```ts
export interface GraphFilterState {
  layers: ReadonlySet<GraphLayer>;
  schemaIds: ReadonlySet<string>;
  hiddenFlavorIds: ReadonlySet<string>;
  search: string;
  authoredBy: ReadonlySet<string>;
  timeRange: { fromMs: number; toMs: number } | null;
  sizeRange: { minBytes: number; maxBytes: number } | null;
}

export const defaultGraphFilterState = (): GraphFilterState => ({
  layers: new Set(GRAPH_LAYERS),
  schemaIds: new Set(),
  hiddenFlavorIds: new Set(),
  search: "",
  authoredBy: new Set(),
  timeRange: null,
  sizeRange: null,
});
```

Add three setters to `GraphFilterStore` and `createGraphFilterStore`:

```ts
setAuthor(personalityId: string, enabled: boolean): void;
setTimeRange(range: { fromMs: number; toMs: number } | null): void;
setSizeRange(range: { minBytes: number; maxBytes: number } | null): void;
```

Implementations mirror `setSchema` for the multi-select; the range
setters just `setState((prev) => ({ ...prev, timeRange }))`.

- [ ] **Step 4: Update `filterGraphSnapshot`**

Edit `graph-selectors.ts` — extend the memory predicate:

```ts
const memoryProvenance = graph.memoryProvenance;

const memories = Array.from(graph.memoriesById.values()).filter((memory) => {
  const row = memory.row;
  const flavor = schemaFlavorForRow(row.schema_id, row.schema_version, schemaFlavors, hub);
  if (!filter.layers.has(row.kind)) return false;
  if (!schemaAllowed(row.schema_id, filter)) return false;
  if (!flavorAllowed(flavor, filter)) return false;
  if (!searchMatchesMemory(memory, search)) return false;

  // New facets:
  const prov = memoryProvenance.get(row.id);
  if (filter.authoredBy.size > 0) {
    const author = prov?.authoring_personality_instance_id ?? null;
    if (author === null || !filter.authoredBy.has(author)) return false;
  }
  if (filter.timeRange && prov) {
    if (prov.written_at_ms < filter.timeRange.fromMs) return false;
    if (prov.written_at_ms > filter.timeRange.toMs) return false;
  }
  if (filter.timeRange && !prov) return false;  // unknown time excluded when range set
  if (filter.sizeRange) {
    const bytes = row.payload.length;
    if (bytes < filter.sizeRange.minBytes) return false;
    if (bytes > filter.sizeRange.maxBytes) return false;
  }
  return true;
});
```

Goals are not subject to the new facets in v1 (no provenance map for
goals yet). Document this in a one-line code comment **only if** it
would surprise a reader who later looks at `goals = …`.

- [ ] **Step 5: Run tests**

Run: `pnpm -C packages/frontend-core test src/graph-selectors.test.tsx src/graph-filter-store.test.tsx`
Expected: PASS, no regressions.

- [ ] **Step 6: Commit**

```bash
git add packages/frontend-core/src/graph-filter-store.tsx packages/frontend-core/src/graph-selectors.ts packages/frontend-core/src/graph-selectors.test.tsx
git commit -m "$(cat <<'EOF'
feat(frontend-core): add authoredBy/timeRange/sizeRange filter facets

GraphFilterState gains three new facets backed by the provenance
map and payload-byte length. filterGraphSnapshot honors them on the
memory predicate. Goals are not yet subject (no goal-provenance map
in v1).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: ChipRail component

**Files:**
- Create: `packages/frontend-core/src/views/surface/chip-rail.tsx`
- Test: `packages/frontend-core/src/views/surface/chip-rail.test.tsx`

- [ ] **Step 1: Write the failing test**

```tsx
// chip-rail.test.tsx
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { GraphFilterProvider, createGraphFilterStore } from "../../graph-filter-store";
import { ChipRail } from "./chip-rail";

afterEach(cleanup);

const renderWithStore = (setup: (store: ReturnType<typeof createGraphFilterStore>) => void) => {
  const store = createGraphFilterStore();
  setup(store);
  render(() => (
    <GraphFilterProvider store={store}>
      <ChipRail flavors={["proxima-code"]} />
    </GraphFilterProvider>
  ));
  return store;
};

describe("ChipRail", () => {
  it("renders a chip for each active facet", () => {
    renderWithStore((store) => {
      store.setSchema("proxima-code/code-chunk-v1", true);
      store.setAuthor("personality-rust", true);
    });
    expect(screen.getByText(/code-chunk-v1/)).toBeInTheDocument();
    expect(screen.getByText(/personality-rust/)).toBeInTheDocument();
  });

  it("removes the chip when ✕ is clicked", () => {
    const store = renderWithStore((s) => {
      s.setSchema("proxima-code/code-chunk-v1", true);
    });
    const remove = screen.getByLabelText(/remove schema chip/i);
    fireEvent.click(remove);
    expect(screen.queryByText(/code-chunk-v1/)).not.toBeInTheDocument();
    expect(store.state().schemaIds.size).toBe(0);
  });

  it("renders nothing when no facets are active", () => {
    renderWithStore(() => {});
    expect(screen.queryByRole("listitem")).not.toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run test to fail**

Run: `pnpm -C packages/frontend-core test src/views/surface/chip-rail.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement `chip-rail.tsx`**

```tsx
import { For, Show, type Component } from "solid-js";
import { useGraphFilter, GRAPH_LAYERS, type GraphLayer } from "../../graph-filter-store";

interface Chip {
  facet: "schema" | "flavor" | "author" | "time" | "size" | "pillar";
  label: string;
  remove: () => void;
}

const formatTime = (range: { fromMs: number; toMs: number }): string => {
  const fmt = (ms: number) => new Date(ms).toISOString().slice(0, 16);
  return `${fmt(range.fromMs)} → ${fmt(range.toMs)}`;
};

const formatSize = (range: { minBytes: number; maxBytes: number }): string =>
  `${range.minBytes}–${range.maxBytes} B`;

export const ChipRail: Component<{ flavors: string[] }> = (props) => {
  const filter = useGraphFilter();

  const chips = (): Chip[] => {
    const s = filter.state();
    const out: Chip[] = [];
    for (const flavorId of s.hiddenFlavorIds) {
      out.push({
        facet: "flavor",
        label: `flavor != ${flavorId}`,
        remove: () => filter.setFlavor(flavorId, true),
      });
    }
    for (const sid of s.schemaIds) {
      out.push({
        facet: "schema",
        label: `schema: ${sid}`,
        remove: () => filter.setSchema(sid, false),
      });
    }
    for (const author of s.authoredBy) {
      out.push({
        facet: "author",
        label: `author: ${author}`,
        remove: () => filter.setAuthor(author, false),
      });
    }
    if (s.timeRange) {
      out.push({
        facet: "time",
        label: `time: ${formatTime(s.timeRange)}`,
        remove: () => filter.setTimeRange(null),
      });
    }
    if (s.sizeRange) {
      out.push({
        facet: "size",
        label: `size: ${formatSize(s.sizeRange)}`,
        remove: () => filter.setSizeRange(null),
      });
    }
    if (s.layers.size !== GRAPH_LAYERS.length) {
      const active = Array.from(s.layers).join(", ");
      out.push({
        facet: "pillar",
        label: `pillar: ${active}`,
        remove: () => {
          for (const l of GRAPH_LAYERS) filter.setLayer(l, true);
        },
      });
    }
    return out;
  };

  return (
    <Show when={chips().length > 0}>
      <ul class="surface-chip-rail" role="list">
        <For each={chips()}>
          {(chip) => (
            <li class={`surface-chip surface-chip--${chip.facet}`} role="listitem">
              <span class="surface-chip__label">{chip.label}</span>
              <button
                type="button"
                class="surface-chip__remove"
                aria-label={`remove ${chip.facet} chip`}
                onClick={chip.remove}
              >
                ✕
              </button>
            </li>
          )}
        </For>
      </ul>
    </Show>
  );
};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pnpm -C packages/frontend-core test src/views/surface/chip-rail.test.tsx`
Expected: PASS, 3 tests.

- [ ] **Step 5: Add CSS**

Append to `views/surface.css` minimal rules; reuse existing variables
where possible:

```css
.surface-chip-rail {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  padding: 6px 12px;
  border-bottom: 1px solid var(--surface-border);
  list-style: none;
  margin: 0;
}
.surface-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 8px;
  border-radius: 12px;
  background: var(--surface-chip-bg);
  font-size: 12px;
  font-family: var(--mono-font);
}
.surface-chip__remove {
  background: none;
  border: none;
  cursor: pointer;
  padding: 0 2px;
  color: inherit;
  opacity: 0.6;
}
.surface-chip__remove:hover { opacity: 1; }
```

If a CSS variable doesn't exist (`--surface-chip-bg`, `--mono-font`),
add it under the existing `:root { … }` block at the top of
`surface.css`.

- [ ] **Step 6: Commit**

```bash
git add packages/frontend-core/src/views/surface/chip-rail.tsx packages/frontend-core/src/views/surface/chip-rail.test.tsx packages/frontend-core/src/views/surface.css
git commit -m "$(cat <<'EOF'
feat(frontend-core): add Surface ChipRail component

Renders the active filter state as removable chips above the row
list. One chip per facet, ✕ removes via the matching store setter.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: FilterDrawer component

**Files:**
- Create: `packages/frontend-core/src/views/surface/filter-drawer.tsx`
- Test: `packages/frontend-core/src/views/surface/filter-drawer.test.tsx`

- [ ] **Step 1: Write the failing test**

```tsx
// filter-drawer.test.tsx
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import { GraphFilterProvider, createGraphFilterStore } from "../../graph-filter-store";
import { FilterDrawer } from "./filter-drawer";

const FACETS = {
  flavors: ["proxima-code"],
  schemas: [
    { schemaId: "proxima-code/code-chunk-v1", flavor: "proxima-code" },
    { schemaId: "proxima-code/commit-summary-v1", flavor: "proxima-code" },
  ],
  authors: ["personality-rust", "personality-go"],
};

afterEach(cleanup);

describe("FilterDrawer", () => {
  it("toggles a schema checkbox and updates the store", () => {
    const store = createGraphFilterStore();
    render(() => (
      <GraphFilterProvider store={store}>
        <FilterDrawer open={true} onClose={() => {}} facets={FACETS} />
      </GraphFilterProvider>
    ));
    fireEvent.click(screen.getByLabelText(/code-chunk-v1/));
    expect(store.state().schemaIds.has("proxima-code/code-chunk-v1")).toBe(true);
  });

  it("Reset clears all facets", () => {
    const store = createGraphFilterStore();
    store.setAuthor("personality-rust", true);
    render(() => (
      <GraphFilterProvider store={store}>
        <FilterDrawer open={true} onClose={() => {}} facets={FACETS} />
      </GraphFilterProvider>
    ));
    fireEvent.click(screen.getByRole("button", { name: /reset/i }));
    expect(store.state().authoredBy.size).toBe(0);
  });

  it("does not render when closed", () => {
    render(() => (
      <GraphFilterProvider store={createGraphFilterStore()}>
        <FilterDrawer open={false} onClose={() => {}} facets={FACETS} />
      </GraphFilterProvider>
    ));
    expect(screen.queryByRole("button", { name: /reset/i })).not.toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm -C packages/frontend-core test src/views/surface/filter-drawer.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement `filter-drawer.tsx`**

```tsx
import { For, Show, type Component } from "solid-js";
import { useGraphFilter, GRAPH_LAYERS } from "../../graph-filter-store";

export interface FilterFacets {
  flavors: string[];
  schemas: { schemaId: string; flavor: string | null }[];
  authors: string[];
}

export const FilterDrawer: Component<{
  open: boolean;
  onClose: () => void;
  facets: FilterFacets;
}> = (props) => {
  const filter = useGraphFilter();

  const onTimeFrom = (e: Event) => {
    const value = (e.currentTarget as HTMLInputElement).value;
    const fromMs = value === "" ? null : new Date(value).getTime();
    const current = filter.state().timeRange;
    if (fromMs === null) {
      filter.setTimeRange(null);
      return;
    }
    filter.setTimeRange({ fromMs, toMs: current?.toMs ?? Date.now() });
  };
  const onTimeTo = (e: Event) => {
    const value = (e.currentTarget as HTMLInputElement).value;
    const toMs = value === "" ? null : new Date(value).getTime();
    const current = filter.state().timeRange;
    if (toMs === null) {
      filter.setTimeRange(null);
      return;
    }
    filter.setTimeRange({ fromMs: current?.fromMs ?? 0, toMs });
  };
  const onSize = (key: "minBytes" | "maxBytes", e: Event) => {
    const value = Number((e.currentTarget as HTMLInputElement).value);
    const current = filter.state().sizeRange ?? { minBytes: 0, maxBytes: Number.MAX_SAFE_INTEGER };
    filter.setSizeRange({ ...current, [key]: value });
  };

  return (
    <Show when={props.open}>
      <aside class="surface-filter-drawer" role="dialog" aria-label="Filters">
        <header class="surface-filter-drawer__header">
          <h2>Filters</h2>
          <button type="button" onClick={props.onClose} aria-label="close">×</button>
        </header>

        <fieldset>
          <legend>Pillar</legend>
          <For each={GRAPH_LAYERS}>
            {(layer) => (
              <label>
                <input
                  type="checkbox"
                  checked={filter.state().layers.has(layer)}
                  onInput={(e) =>
                    filter.setLayer(layer, e.currentTarget.checked)
                  }
                />
                {layer}
              </label>
            )}
          </For>
        </fieldset>

        <fieldset>
          <legend>Flavor</legend>
          <For each={props.facets.flavors}>
            {(flavor) => (
              <label>
                <input
                  type="checkbox"
                  checked={!filter.state().hiddenFlavorIds.has(flavor)}
                  onInput={(e) => filter.setFlavor(flavor, e.currentTarget.checked)}
                />
                {flavor}
              </label>
            )}
          </For>
        </fieldset>

        <fieldset>
          <legend>Schema</legend>
          <For each={props.facets.schemas}>
            {(schema) => (
              <label>
                <input
                  type="checkbox"
                  checked={filter.state().schemaIds.has(schema.schemaId)}
                  onInput={(e) =>
                    filter.setSchema(schema.schemaId, e.currentTarget.checked)
                  }
                />
                {schema.schemaId}
              </label>
            )}
          </For>
        </fieldset>

        <fieldset>
          <legend>Authored by</legend>
          <For each={props.facets.authors}>
            {(author) => (
              <label>
                <input
                  type="checkbox"
                  checked={filter.state().authoredBy.has(author)}
                  onInput={(e) =>
                    filter.setAuthor(author, e.currentTarget.checked)
                  }
                />
                {author}
              </label>
            )}
          </For>
        </fieldset>

        <fieldset>
          <legend>Time</legend>
          <label>
            from <input type="datetime-local" onInput={onTimeFrom} />
          </label>
          <label>
            to <input type="datetime-local" onInput={onTimeTo} />
          </label>
        </fieldset>

        <fieldset>
          <legend>Size (bytes)</legend>
          <label>
            min <input type="number" min="0" onInput={(e) => onSize("minBytes", e)} />
          </label>
          <label>
            max <input type="number" min="0" onInput={(e) => onSize("maxBytes", e)} />
          </label>
        </fieldset>

        <footer class="surface-filter-drawer__footer">
          <button type="button" onClick={() => filter.reset()}>Reset</button>
          <button type="button" onClick={props.onClose}>Done</button>
        </footer>
      </aside>
    </Show>
  );
};
```

- [ ] **Step 4: Run tests**

Run: `pnpm -C packages/frontend-core test src/views/surface/filter-drawer.test.tsx`
Expected: PASS, 3 tests.

- [ ] **Step 5: Add CSS**

Append `.surface-filter-drawer { … }` rules to `surface.css` —
position fixed/absolute on the right, width ~340px, background +
border matching existing rails. Header + footer flex containers.

- [ ] **Step 6: Commit**

```bash
git add packages/frontend-core/src/views/surface/filter-drawer.tsx packages/frontend-core/src/views/surface/filter-drawer.test.tsx packages/frontend-core/src/views/surface.css
git commit -m "$(cat <<'EOF'
feat(frontend-core): add Surface FilterDrawer component

Slide-in form covering all six v1 facets (pillar, flavor,
schema_id, authored_by, time, size). Mutates the shared
useGraphFilter store; chip rail reflects state on Apply / Reset.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: RowList component

**Files:**
- Create: `packages/frontend-core/src/views/surface/row-list.tsx`
- Test: `packages/frontend-core/src/views/surface/row-list.test.tsx`

- [ ] **Step 1: Write the failing test**

```tsx
// row-list.test.tsx
import { cleanup, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import type { DecodedMemory } from "../../graph-store";
import { RowList } from "./row-list";

afterEach(cleanup);

const memory = (id: string, kind: DecodedMemory["row"]["kind"], schemaId: string): DecodedMemory => ({
  row: {
    id, kind, schema_id: schemaId, schema_version: 1,
    owner: { principal: { User: "00000000-0000-0000-0000-000000000000" }, org_id: "00000000-0000-0000-0000-000000000000" },
    payload: [1, 2, 3, 4],
  },
  payload: {},
});

describe("RowList", () => {
  it("shows pillar badge column on All tab", () => {
    render(() => (
      <RowList
        rows={[memory("m1", "Fact", "schema-a")]}
        provenance={new Map()}
        activeTab="All"
        selectedId={null}
        onSelect={() => {}}
      />
    ));
    expect(screen.getByText("F")).toBeInTheDocument();
    expect(screen.getByText("schema-a")).toBeInTheDocument();
  });

  it("hides pillar badge column on per-pillar tabs", () => {
    render(() => (
      <RowList
        rows={[memory("m1", "Fact", "schema-a")]}
        provenance={new Map()}
        activeTab="Fact"
        selectedId={null}
        onSelect={() => {}}
      />
    ));
    expect(screen.queryByText("F")).not.toBeInTheDocument();
    expect(screen.getByText("schema-a")).toBeInTheDocument();
  });

  it("invokes onSelect when a row is clicked", () => {
    let chosen = "";
    render(() => (
      <RowList
        rows={[memory("m1", "Fact", "schema-a")]}
        provenance={new Map()}
        activeTab="All"
        selectedId={null}
        onSelect={(id) => { chosen = id; }}
      />
    ));
    screen.getByText("schema-a").closest("[role='row']")?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(chosen).toBe("m1");
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm -C packages/frontend-core test src/views/surface/row-list.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement `row-list.tsx`**

```tsx
import { For, Show, type Component } from "solid-js";
import type { DecodedMemory, MemoryProvenance } from "../../graph-store";

export type ActiveTab = "All" | "Fact" | "Abstraction" | "Perspective" | "Goal";

const KIND_BADGE: Record<DecodedMemory["row"]["kind"], string> = {
  Fact: "F",
  Abstraction: "A",
  Perspective: "P",
  Goal: "G",
};

const formatRelative = (ms: number | undefined): string => {
  if (ms === undefined) return "—";
  const diff = Date.now() - ms;
  const min = Math.round(diff / 60_000);
  if (min < 1) return "now";
  if (min < 60) return `${min}m`;
  const hr = Math.round(min / 60);
  if (hr < 24) return `${hr}h`;
  return `${Math.round(hr / 24)}d`;
};

export const RowList: Component<{
  rows: DecodedMemory[];
  provenance: ReadonlyMap<string, MemoryProvenance>;
  activeTab: ActiveTab;
  selectedId: string | null;
  onSelect: (id: string) => void;
}> = (props) => (
  <div class="surface-row-list" role="grid">
    <For each={props.rows}>
      {(row) => {
        const prov = props.provenance.get(row.row.id);
        const isSelected = props.selectedId === row.row.id;
        return (
          <div
            role="row"
            class="surface-row"
            classList={{ "surface-row--selected": isSelected }}
            onClick={() => props.onSelect(row.row.id)}
          >
            <Show when={props.activeTab === "All"}>
              <span class={`surface-row__pillar surface-row__pillar--${row.row.kind}`}>
                {KIND_BADGE[row.row.kind]}
              </span>
            </Show>
            <span class="surface-row__schema">{row.row.schema_id}</span>
            <span class="surface-row__author">
              {prov?.authoring_personality_instance_id ?? "—"}
            </span>
            <span class="surface-row__size">{row.row.payload.length} B</span>
            <span class="surface-row__time">{formatRelative(prov?.written_at_ms)}</span>
          </div>
        );
      }}
    </For>
  </div>
);
```

Note: v1 ships without `VirtualList` here for simplicity; profile in
Task 11 and only add virtualization back if list count > ~500 causes
visible jank. The existing `VirtualList` is preserved in the repo and
can be slotted in trivially.

- [ ] **Step 4: Run tests**

Run: `pnpm -C packages/frontend-core test src/views/surface/row-list.test.tsx`
Expected: PASS, 3 tests.

- [ ] **Step 5: Add CSS**

CSS grid with 5 (or 4 on per-pillar) columns; selected row uses
existing accent-highlight rule.

- [ ] **Step 6: Commit**

```bash
git add packages/frontend-core/src/views/surface/row-list.tsx packages/frontend-core/src/views/surface/row-list.test.tsx packages/frontend-core/src/views/surface.css
git commit -m "$(cat <<'EOF'
feat(frontend-core): add Surface RowList component

Single grid-style list driven by the active-tab prop. Pillar badge
shown only on All. Reads provenance map for author + time columns.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: TabStrip component

**Files:**
- Create: `packages/frontend-core/src/views/surface/tab-strip.tsx`
- Test: `packages/frontend-core/src/views/surface/tab-strip.test.tsx`

- [ ] **Step 1: Write the failing test**

```tsx
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TabStrip } from "./tab-strip";

afterEach(cleanup);

describe("TabStrip", () => {
  it("renders five tabs with counts", () => {
    render(() => (
      <TabStrip
        active="All"
        counts={{ All: 2485, Fact: 2360, Abstraction: 118, Perspective: 7, Goal: 0 }}
        onChange={() => {}}
        onToggleFilters={() => {}}
      />
    ));
    expect(screen.getByRole("tab", { name: /All 2485/ })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /F 2360/ })).toBeInTheDocument();
  });

  it("invokes onChange when a tab is clicked", () => {
    const onChange = vi.fn();
    render(() => (
      <TabStrip
        active="All"
        counts={{ All: 2485, Fact: 2360, Abstraction: 118, Perspective: 7, Goal: 0 }}
        onChange={onChange}
        onToggleFilters={() => {}}
      />
    ));
    fireEvent.click(screen.getByRole("tab", { name: /F 2360/ }));
    expect(onChange).toHaveBeenCalledWith("Fact");
  });

  it("invokes onToggleFilters when ⚙ Filters is clicked", () => {
    const onToggleFilters = vi.fn();
    render(() => (
      <TabStrip
        active="All"
        counts={{ All: 0, Fact: 0, Abstraction: 0, Perspective: 0, Goal: 0 }}
        onChange={() => {}}
        onToggleFilters={onToggleFilters}
      />
    ));
    fireEvent.click(screen.getByRole("button", { name: /filters/i }));
    expect(onToggleFilters).toHaveBeenCalled();
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm -C packages/frontend-core test src/views/surface/tab-strip.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement `tab-strip.tsx`**

```tsx
import { For, type Component } from "solid-js";
import type { ActiveTab } from "./row-list";

const TABS: { key: ActiveTab; label: string }[] = [
  { key: "All", label: "All" },
  { key: "Perspective", label: "P" },
  { key: "Abstraction", label: "A" },
  { key: "Fact", label: "F" },
  { key: "Goal", label: "G" },
];

export const TabStrip: Component<{
  active: ActiveTab;
  counts: Record<ActiveTab, number>;
  onChange: (tab: ActiveTab) => void;
  onToggleFilters: () => void;
}> = (props) => (
  <div class="surface-tab-strip" role="tablist">
    <For each={TABS}>
      {(tab) => (
        <button
          role="tab"
          aria-selected={props.active === tab.key}
          class="surface-tab"
          classList={{ "surface-tab--active": props.active === tab.key }}
          onClick={() => props.onChange(tab.key)}
        >
          {tab.label} {props.counts[tab.key]}
        </button>
      )}
    </For>
    <span class="surface-tab-strip__spacer" />
    <button
      type="button"
      class="surface-tab-strip__filters"
      aria-label="Filters"
      onClick={props.onToggleFilters}
    >
      ⚙ Filters
    </button>
  </div>
);
```

- [ ] **Step 4: Run tests**

Run: `pnpm -C packages/frontend-core test src/views/surface/tab-strip.test.tsx`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add packages/frontend-core/src/views/surface/tab-strip.tsx packages/frontend-core/src/views/surface/tab-strip.test.tsx
git commit -m "$(cat <<'EOF'
feat(frontend-core): add Surface TabStrip component

Pillar tab strip with counts and the ⚙ Filters toggle. Pure
display + callback shape; surface.tsx wires keyboard and state.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: ActivityStrip component

**Files:**
- Create: `packages/frontend-core/src/views/surface/activity-strip.tsx`
- Test: `packages/frontend-core/src/views/surface/activity-strip.test.tsx`

- [ ] **Step 1: Write the failing test**

```tsx
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ActivityStrip } from "./activity-strip";

afterEach(cleanup);

describe("ActivityStrip", () => {
  it("renders state, last wake, and personality count", () => {
    render(() => (
      <ActivityStrip
        state="idle"
        lastWakeAtMs={Date.now() - 60_000}
        activePersonalityCount={2}
        onToggleEventStream={() => {}}
      />
    ));
    expect(screen.getByText(/idle/i)).toBeInTheDocument();
    expect(screen.getByText(/2 active/i)).toBeInTheDocument();
    expect(screen.getByText(/1m/)).toBeInTheDocument();
  });

  it("toggles the Event Stream drawer on click", () => {
    const onToggle = vi.fn();
    render(() => (
      <ActivityStrip
        state="idle"
        lastWakeAtMs={null}
        activePersonalityCount={0}
        onToggleEventStream={onToggle}
      />
    ));
    fireEvent.click(screen.getByRole("button", { name: /events/i }));
    expect(onToggle).toHaveBeenCalled();
  });
});
```

- [ ] **Step 2: Run to fail**

Run: `pnpm -C packages/frontend-core test src/views/surface/activity-strip.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement `activity-strip.tsx`**

```tsx
import { Show, type Component } from "solid-js";

export type EngineState = "idle" | "waking" | "deciding" | "writing" | "error";

const formatRelative = (ms: number | null): string => {
  if (ms === null) return "—";
  const diff = Date.now() - ms;
  const min = Math.round(diff / 60_000);
  if (min < 1) return "now";
  if (min < 60) return `${min}m`;
  return `${Math.round(min / 60)}h`;
};

export const ActivityStrip: Component<{
  state: EngineState;
  lastWakeAtMs: number | null;
  activePersonalityCount: number;
  onToggleEventStream: () => void;
}> = (props) => (
  <button
    type="button"
    class="surface-activity-strip"
    aria-label="events"
    onClick={props.onToggleEventStream}
  >
    <span class={`surface-activity-strip__dot surface-activity-strip__dot--${props.state}`} />
    <span class="surface-activity-strip__state">{props.state}</span>
    <Show when={props.lastWakeAtMs !== null}>
      <span class="surface-activity-strip__sep">·</span>
      <span>last wake {formatRelative(props.lastWakeAtMs)} ago</span>
    </Show>
    <span class="surface-activity-strip__sep">·</span>
    <span>{props.activePersonalityCount} active personalities</span>
  </button>
);
```

For v1, the `state`, `lastWakeAtMs`, and `activePersonalityCount` are
read in `surface.tsx` from the existing graph-store (`streamStatus →
state`) and personality list. Engine-state mapping:
`connecting | live → idle`, `degraded → error`, `stopped → error`.

- [ ] **Step 4: Run tests**

Run: `pnpm -C packages/frontend-core test src/views/surface/activity-strip.test.tsx`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add packages/frontend-core/src/views/surface/activity-strip.tsx packages/frontend-core/src/views/surface/activity-strip.test.tsx
git commit -m "$(cat <<'EOF'
feat(frontend-core): add Surface ActivityStrip component

Bottom-of-Surface single-line strip showing engine state, last
wake, and active personality count. Click toggles the existing
Event Stream drawer.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: DetailPane component

**Files:**
- Create: `packages/frontend-core/src/views/surface/detail-pane.tsx`
- Test: `packages/frontend-core/src/views/surface/detail-pane.test.tsx`

- [ ] **Step 1: Write the failing test**

```tsx
import { cleanup, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import type { DecodedMemory, MemoryProvenance } from "../../graph-store";
import { createHub } from "../../hub";
import { clearRegistriesForTests } from "../../registry";
import { DetailPane } from "./detail-pane";

afterEach(() => { cleanup(); clearRegistriesForTests(); });

const memory = (): DecodedMemory => ({
  row: {
    id: "m1", kind: "Fact",
    schema_id: "proxima-code/code-chunk-v1", schema_version: 1,
    owner: { principal: { User: "u" }, org_id: "o" },
    payload: [1, 2, 3, 4, 5],
  },
  payload: { state: "Present", chunk: 1, type: "block", language: "rust" },
});

const prov: MemoryProvenance = {
  creating_seq: "01ARYZ6S41TS5G7QFC0V44N5KH",
  authoring_personality_instance_id: "personality-rust",
  written_at_ms: 1469918176385,
};

describe("DetailPane", () => {
  it("renders header, payload, lineage, and metadata blocks", () => {
    render(() => (
      <DetailPane
        memory={memory()}
        provenance={prov}
        lineage={{ outbound: [], inbound: [] }}
        flavor="proxima-code"
        hub={createHub([])}
      />
    ));
    expect(screen.getByText(/PAYLOAD/)).toBeInTheDocument();
    expect(screen.getByText(/LINEAGE/)).toBeInTheDocument();
    expect(screen.getByText(/METADATA/)).toBeInTheDocument();
    expect(screen.getByText(/personality-rust/)).toBeInTheDocument();
    expect(screen.getByText(/proxima-code\/code-chunk-v1/)).toBeInTheDocument();
  });

  it("falls back to flat key/value when no renderer registered", () => {
    render(() => (
      <DetailPane
        memory={memory()}
        provenance={prov}
        lineage={{ outbound: [], inbound: [] }}
        flavor="proxima-code"
        hub={createHub([])}
      />
    ));
    expect(screen.getByText("state")).toBeInTheDocument();
    expect(screen.getByText("Present")).toBeInTheDocument();
    expect(screen.getByText("language")).toBeInTheDocument();
    expect(screen.getByText("rust")).toBeInTheDocument();
  });

  it("renders 1-hop lineage groups", () => {
    render(() => (
      <DetailPane
        memory={memory()}
        provenance={prov}
        lineage={{
          outbound: [{ relation: "informs", target_kind: "Abstraction", target_schema_id: "schema-b", count: 2 }],
          inbound: [],
        }}
        flavor="proxima-code"
        hub={createHub([])}
      />
    ));
    expect(screen.getByText(/informs/)).toBeInTheDocument();
    expect(screen.getByText(/schema-b/)).toBeInTheDocument();
    expect(screen.getByText(/×2/)).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run to fail**

Run: `pnpm -C packages/frontend-core test src/views/surface/detail-pane.test.tsx`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement `detail-pane.tsx`**

```tsx
import { For, Show, type Component } from "solid-js";
import type { DecodedMemory, MemoryProvenance } from "../../graph-store";
import type { OneHopLineage } from "../../graph-selectors";
import type { Hub } from "../../hub";

const formatRelative = (ms: number): string => {
  const diff = Date.now() - ms;
  const min = Math.round(diff / 60_000);
  if (min < 1) return "now";
  if (min < 60) return `${min}m ago`;
  const hr = Math.round(min / 60);
  if (hr < 24) return `${hr}h ago`;
  return `${Math.round(hr / 24)}d ago`;
};

const FallbackPayload: Component<{ payload: unknown }> = (props) => {
  if (props.payload === null || typeof props.payload !== "object") {
    return <div class="detail-pane__fallback-scalar">{String(props.payload)}</div>;
  }
  const entries = Object.entries(props.payload as Record<string, unknown>);
  return (
    <dl class="detail-pane__kv">
      <For each={entries}>
        {([k, v]) => (
          <>
            <dt>{k}</dt>
            <dd>{typeof v === "object" ? JSON.stringify(v) : String(v)}</dd>
          </>
        )}
      </For>
    </dl>
  );
};

export const DetailPane: Component<{
  memory: DecodedMemory;
  provenance: MemoryProvenance | undefined;
  lineage: OneHopLineage;
  flavor: string | null;
  hub: Hub;
}> = (props) => {
  const renderer = () =>
    props.hub.rendererFor(
      props.memory.row.schema_id,
      props.memory.row.schema_version,
      props.memory.row.kind,
    );

  return (
    <section class="detail-pane">
      <header class="detail-pane__header">
        <div class="detail-pane__title">
          {props.memory.row.schema_id} v{props.memory.row.schema_version}
        </div>
        <div class="detail-pane__meta-line">
          {props.memory.row.id.slice(0, 8)} ·
          {" "}{props.memory.row.payload.length} bytes ·
          {" "}{props.provenance?.authoring_personality_instance_id ?? "—"}
        </div>
      </header>

      <section class="detail-pane__block">
        <h3>PAYLOAD</h3>
        <Show when={renderer()} fallback={<FallbackPayload payload={props.memory.payload} />}>
          {renderer()!.render({ memory: props.memory.row, payload: props.memory.payload })}
        </Show>
      </section>

      <section class="detail-pane__block">
        <h3>LINEAGE (1-hop)</h3>
        <Show
          when={props.lineage.outbound.length > 0 || props.lineage.inbound.length > 0}
          fallback={<div class="detail-pane__empty">no incident edges</div>}
        >
          <ul>
            <For each={props.lineage.outbound}>
              {(group) => (
                <li>→ {group.relation} {group.target_kind} {group.target_schema_id} ×{group.count}</li>
              )}
            </For>
            <For each={props.lineage.inbound}>
              {(group) => (
                <li>← {group.relation} {group.target_kind} {group.target_schema_id} ×{group.count}</li>
              )}
            </For>
          </ul>
        </Show>
      </section>

      <section class="detail-pane__block">
        <h3>METADATA</h3>
        <dl class="detail-pane__kv">
          <dt>schema_id</dt><dd>{props.memory.row.schema_id}</dd>
          <dt>schema_version</dt><dd>{props.memory.row.schema_version}</dd>
          <dt>flavor</dt><dd>{props.flavor ?? "core"}</dd>
          <dt>pillar</dt><dd>{props.memory.row.kind}</dd>
          <dt>authored_by</dt>
          <dd>{props.provenance?.authoring_personality_instance_id ?? "—"}</dd>
          <dt>written_at</dt>
          <dd>{props.provenance ? formatRelative(props.provenance.written_at_ms) : "—"}</dd>
          <dt>byte_size</dt>
          <dd>{props.memory.row.payload.length}</dd>
        </dl>
      </section>
    </section>
  );
};
```

- [ ] **Step 4: Run tests**

Run: `pnpm -C packages/frontend-core test src/views/surface/detail-pane.test.tsx`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add packages/frontend-core/src/views/surface/detail-pane.tsx packages/frontend-core/src/views/surface/detail-pane.test.tsx
git commit -m "$(cat <<'EOF'
feat(frontend-core): add Surface DetailPane component

Three-block (PAYLOAD / LINEAGE / METADATA) detail view. PAYLOAD
delegates to Hub.rendererFor with a flat-kv fallback. LINEAGE
consumes oneHopLineage output; METADATA reads provenance.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Compose `surface.tsx` orchestrator

**Files:**
- Modify: `packages/frontend-core/src/views/surface.tsx` (full rewrite)
- Modify: `packages/frontend-core/src/views/surface.css` (drop goal-rail rules)
- Create: `packages/frontend-core/src/views/surface/keys.ts`
- Create: `packages/frontend-core/src/views/surface/index.tsx` (re-export)

This is the integration step. No new tests yet — the existing
`surface.test.tsx` will be rewritten in Task 12.

- [ ] **Step 1: Add keyboard map**

Create `views/surface/keys.ts`:

```ts
import type { ActiveTab } from "./row-list";

export interface KeyHandlers {
  onTab: (tab: ActiveTab) => void;
  onToggleFilters: () => void;
  onToggleEventStream: () => void;
  onCloseDrawer: () => void;
}

export const installSurfaceKeys = (handlers: KeyHandlers): (() => void) => {
  const onKey = (e: KeyboardEvent) => {
    const meta = e.metaKey || e.ctrlKey;
    if (!meta && e.key !== "Escape") return;
    if (meta && e.key === "1") { handlers.onTab("All"); e.preventDefault(); }
    else if (meta && e.key === "2") { handlers.onTab("Perspective"); e.preventDefault(); }
    else if (meta && e.key === "3") { handlers.onTab("Abstraction"); e.preventDefault(); }
    else if (meta && e.key === "4") { handlers.onTab("Fact"); e.preventDefault(); }
    else if (meta && e.key === "5") { handlers.onTab("Goal"); e.preventDefault(); }
    else if (meta && e.key.toLowerCase() === "f") { handlers.onToggleFilters(); e.preventDefault(); }
    else if (meta && e.key.toLowerCase() === "e") { handlers.onToggleEventStream(); e.preventDefault(); }
    else if (e.key === "Escape") { handlers.onCloseDrawer(); }
  };
  window.addEventListener("keydown", onKey);
  return () => window.removeEventListener("keydown", onKey);
};
```

- [ ] **Step 2: Rewrite `surface.tsx`**

Replace the entire file with the orchestrator. Target ~200 lines.

```tsx
import "./surface.css";
import { Show, createMemo, createSignal, onCleanup, onMount, type Component } from "solid-js";
import { useGraph } from "../graph-store";
import { useGraphFilter } from "../graph-filter-store";
import { filterGraphSnapshot, oneHopLineage } from "../graph-selectors";
import type { Hub } from "../hub";
import { ActivityStrip, type EngineState } from "./surface/activity-strip";
import { ChipRail } from "./surface/chip-rail";
import { DetailPane } from "./surface/detail-pane";
import { FilterDrawer, type FilterFacets } from "./surface/filter-drawer";
import { RowList, type ActiveTab } from "./surface/row-list";
import { TabStrip } from "./surface/tab-strip";
import { installSurfaceKeys } from "./surface/keys";
import { EventStream } from "./surface-events";

const KIND_TO_TAB: Record<string, ActiveTab> = {
  All: "All",
  Fact: "Fact",
  Abstraction: "Abstraction",
  Perspective: "Perspective",
  Goal: "Goal",
};

const tabLayer = (tab: ActiveTab): "Fact" | "Abstraction" | "Perspective" | "Goal" | null =>
  tab === "All" ? null : tab;

const STATE_FROM_STREAM: Record<string, EngineState> = {
  connecting: "idle",
  live: "idle",
  degraded: "error",
  stopped: "error",
};

export const FullSurface: Component<{ hub: Hub }> = (props) => {
  const graph = useGraph();
  const filter = useGraphFilter();
  const [activeTab, setActiveTab] = createSignal<ActiveTab>("All");
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [eventsOpen, setEventsOpen] = createSignal(false);
  const [selectedId, setSelectedId] = createSignal<string | null>(null);

  const filtered = createMemo(() => {
    const layer = tabLayer(activeTab());
    const baseFilter = filter.state();
    const adjusted = layer === null
      ? baseFilter
      : { ...baseFilter, layers: new Set([layer]) };
    return filterGraphSnapshot(graph.state(), adjusted, props.hub);
  });

  const counts = createMemo(() => {
    const all = graph.state().memoriesById.size + graph.state().goalsById.size;
    let fact = 0, abs = 0, per = 0;
    for (const m of graph.state().memoriesById.values()) {
      if (m.row.kind === "Fact") fact++;
      else if (m.row.kind === "Abstraction") abs++;
      else if (m.row.kind === "Perspective") per++;
    }
    return {
      All: all,
      Fact: fact,
      Abstraction: abs,
      Perspective: per,
      Goal: graph.state().goalsById.size,
    };
  });

  const facets = createMemo<FilterFacets>(() => {
    const flavors = new Set<string>();
    const schemas = new Map<string, { schemaId: string; flavor: string | null }>();
    const authors = new Set<string>();
    for (const m of graph.state().memoriesById.values()) {
      schemas.set(m.row.schema_id, {
        schemaId: m.row.schema_id,
        flavor: props.hub.flavorFor(m.row.schema_id, m.row.schema_version),
      });
      const flv = props.hub.flavorFor(m.row.schema_id, m.row.schema_version);
      if (flv !== null) flavors.add(flv);
    }
    for (const prov of graph.state().memoryProvenance.values()) {
      if (prov.authoring_personality_instance_id !== null) {
        authors.add(prov.authoring_personality_instance_id);
      }
    }
    return {
      flavors: Array.from(flavors).sort(),
      schemas: Array.from(schemas.values()).sort((a, b) => a.schemaId.localeCompare(b.schemaId)),
      authors: Array.from(authors).sort(),
    };
  });

  const selectedMemory = createMemo(() => {
    const id = selectedId();
    if (id === null) return null;
    return graph.state().memoriesById.get(id) ?? null;
  });

  const lineage = createMemo(() => {
    const id = selectedId();
    if (id === null) return { outbound: [], inbound: [] };
    return oneHopLineage(id, graph.state().edgesById, graph.state().memoriesById);
  });

  onMount(() => {
    const cleanup = installSurfaceKeys({
      onTab: setActiveTab,
      onToggleFilters: () => setDrawerOpen((o) => !o),
      onToggleEventStream: () => setEventsOpen((o) => !o),
      onCloseDrawer: () => { setDrawerOpen(false); setEventsOpen(false); },
    });
    onCleanup(cleanup);
  });

  return (
    <div class="surface">
      <TabStrip
        active={activeTab()}
        counts={counts()}
        onChange={setActiveTab}
        onToggleFilters={() => setDrawerOpen((o) => !o)}
      />
      <ChipRail flavors={facets().flavors} />
      <div class="surface__body">
        <RowList
          rows={filtered().memories}
          provenance={graph.state().memoryProvenance}
          activeTab={activeTab()}
          selectedId={selectedId()}
          onSelect={setSelectedId}
        />
        <Show when={selectedMemory()}>
          <DetailPane
            memory={selectedMemory()!}
            provenance={graph.state().memoryProvenance.get(selectedMemory()!.row.id)}
            lineage={lineage()}
            flavor={props.hub.flavorFor(selectedMemory()!.row.schema_id, selectedMemory()!.row.schema_version)}
            hub={props.hub}
          />
        </Show>
      </div>
      <ActivityStrip
        state={STATE_FROM_STREAM[graph.state().streamStatus] ?? "idle"}
        lastWakeAtMs={null /* v1: derive from latest event seq when wired */}
        activePersonalityCount={0 /* v1: derive from a personality query */}
        onToggleEventStream={() => setEventsOpen((o) => !o)}
      />
      <FilterDrawer open={drawerOpen()} onClose={() => setDrawerOpen(false)} facets={facets()} />
      <Show when={eventsOpen()}>
        <EventStream hub={props.hub} />
      </Show>
    </div>
  );
};
```

For `lastWakeAtMs` and `activePersonalityCount`: leave as `null`/`0`
in v1 unless a graph-store accessor already exposes them. If
`list_personality_instances` is already invoked by another view,
reuse that store. Do NOT add a new Tauri command for this in v1 —
defer to a follow-up.

- [ ] **Step 3: Update `surface.css`**

Remove all rules referencing `goal-rail`, `MemoryExplorer`,
`TraversalLanes`, `LayerHeader`, `MemoryCard`. Add rules for:
`.surface`, `.surface__body { display: flex }`, `.surface-tab-strip`,
`.surface-row-list`, `.surface-row`, `.detail-pane`,
`.surface-activity-strip`, `.surface-filter-drawer`. Width and color
match the dev-friendly density called out in the spec.

- [ ] **Step 4: Create `views/surface/index.tsx`**

```tsx
export { FullSurface } from "../surface";
```

(Optional re-export so subcomponents that import from
`./surface/...` resolve cleanly.)

- [ ] **Step 5: Typecheck**

Run: `pnpm -C packages/frontend-core typecheck`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add packages/frontend-core/src/views/surface.tsx packages/frontend-core/src/views/surface.css packages/frontend-core/src/views/surface/keys.ts packages/frontend-core/src/views/surface/index.tsx
git commit -m "$(cat <<'EOF'
refactor(frontend-core): rewrite Surface as composition of subviews

surface.tsx is now a thin orchestrator over TabStrip, ChipRail,
RowList, DetailPane, FilterDrawer, ActivityStrip. Goal rail is
retired; Goals is the G tab. Keyboard handlers (⌘1-5, ⌘F, ⌘E,
Esc) are installed at mount.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Rewrite `surface.test.tsx`

**Files:**
- Modify: `packages/frontend-core/src/views/surface.test.tsx`

The existing test file is 661 lines testing the three-section view.
Most assertions don't apply. Rewrite it green covering: tab switch,
chip rail round-trip, drawer toggle, detail pane on row select.

- [ ] **Step 1: Strip the file to imports + harness**

Keep the imports, the `clearRegistriesForTests` afterEach, and the
test harness setup that builds graph + filter + hub. Delete every
existing `it(...)` block.

- [ ] **Step 2: Add the new tests**

```tsx
describe("Surface — orchestration", () => {
  it("starts on the All tab and shows pillar badges", async () => {
    const { hub } = await renderSurface([
      memory("m1", "Fact", "schema-a"),
      memory("m2", "Abstraction", "schema-b"),
    ]);
    expect(screen.getByRole("tab", { name: /All/ })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("F")).toBeInTheDocument();
    expect(screen.getByText("A")).toBeInTheDocument();
  });

  it("switching to F tab hides Abstractions", async () => {
    await renderSurface([
      memory("m1", "Fact", "schema-a"),
      memory("m2", "Abstraction", "schema-b"),
    ]);
    fireEvent.click(screen.getByRole("tab", { name: /F 1/ }));
    expect(screen.getByText("schema-a")).toBeInTheDocument();
    expect(screen.queryByText("schema-b")).not.toBeInTheDocument();
  });

  it("toggles filter drawer on ⚙ Filters click", async () => {
    await renderSurface([memory("m1", "Fact", "schema-a")]);
    expect(screen.queryByRole("dialog", { name: /filters/i })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /filters/i }));
    expect(screen.getByRole("dialog", { name: /filters/i })).toBeInTheDocument();
  });

  it("checking a schema in drawer adds a chip and filters list", async () => {
    await renderSurface([
      memory("m1", "Fact", "schema-a"),
      memory("m2", "Fact", "schema-b"),
    ]);
    fireEvent.click(screen.getByRole("button", { name: /filters/i }));
    fireEvent.click(screen.getByLabelText(/schema-a/));
    fireEvent.click(screen.getByRole("button", { name: /done/i }));
    expect(screen.getByText(/schema: schema-a/)).toBeInTheDocument();
    expect(screen.queryByText("schema-b")).not.toBeInTheDocument();
  });

  it("removing a chip restores the row", async () => {
    const store = await renderSurface([
      memory("m1", "Fact", "schema-a"),
      memory("m2", "Fact", "schema-b"),
    ]);
    store.filter.setSchema("schema-a", true);
    fireEvent.click(screen.getByLabelText(/remove schema chip/i));
    expect(screen.getByText("schema-a")).toBeInTheDocument();
    expect(screen.getByText("schema-b")).toBeInTheDocument();
  });

  it("clicking a row opens the detail pane with payload + metadata", async () => {
    await renderSurface([memory("m1", "Fact", "schema-a")]);
    fireEvent.click(screen.getByText("schema-a").closest("[role='row']")!);
    expect(screen.getByText(/PAYLOAD/)).toBeInTheDocument();
    expect(screen.getByText(/METADATA/)).toBeInTheDocument();
  });
});
```

`renderSurface` is a helper that wires up `GraphProvider` +
`GraphFilterProvider` around `<FullSurface hub={hub} />` and seeds the
graph with the supplied memories via the existing test path. Reuse
`memory(id, kind, schemaId)` factory from the old file.

- [ ] **Step 3: Run tests**

Run: `pnpm -C packages/frontend-core test src/views/surface.test.tsx`
Expected: PASS, 6 tests.

- [ ] **Step 4: Run the full frontend test suite**

Run: `pnpm -C packages/frontend-core test`
Expected: PASS — verify no regressions in atlas, schemas,
goal-dialog, surface-events, etc.

- [ ] **Step 5: Typecheck the full package**

Run: `pnpm -C packages/frontend-core typecheck`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add packages/frontend-core/src/views/surface.test.tsx
git commit -m "$(cat <<'EOF'
test(frontend-core): rewrite Surface integration tests

Covers tab switch, ⚙ Filters drawer toggle, chip ↔ drawer round-trip,
and DetailPane open-on-select. Replaces 3-section assertions from
the legacy view.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-Review Notes

**Spec coverage check** (mapped against
`docs/superpowers/specs/2026-05-10-surface-explorer-design.md`):

| Spec section | Task |
| --- | --- |
| Layout (tab strip, chip rail, body, activity strip) | 8, 5, 7+10, 9, 11 |
| Filter rail (chips + drawer) | 5, 6 |
| Drawer facets (flavor / schema / author / time / size / pillar) | 4, 6 |
| Detail pane (PAYLOAD / LINEAGE / METADATA) | 10 |
| 1-hop lineage selector | 3 |
| Provenance index | 1, 2 |
| Tabs and per-tab columns | 7, 8, 11 |
| Activity strip + Event Stream toggle | 9, 11 |
| Keyboard map | 11 (`keys.ts`) |
| Goal DAG rail retirement | 11 (drop from `surface.tsx` + css) |
| Migration from current Surface (1-9 in spec) | matches Tasks 1-12 |

**Out-of-scope verified absent:**
- `batch` facet — not in `GraphFilterState`, not in drawer, not in
  any column.
- `⌘K` palette — no `keys.ts` mapping for it.
- Multi-column sort — column headers in `RowList` are static.
- Flavor-shipped React — `DetailPane` strictly uses `Hub.rendererFor`
  with a flat-kv fallback; no dynamic-import path.

**Type consistency check:**
- `ActiveTab` defined in `row-list.tsx` and re-imported in
  `tab-strip.tsx` and `surface.tsx`. ✓
- `MemoryProvenance` defined in `graph-store.tsx`, consumed by
  `row-list.tsx`, `detail-pane.tsx`, `surface.tsx`. ✓
- `OneHopLineage` defined in `graph-selectors.ts`, consumed by
  `detail-pane.tsx` and `surface.tsx`. ✓
- `EngineState` defined in `activity-strip.tsx`, consumed only there
  and in `surface.tsx`. ✓
- Filter store setter names: `setSchema`, `setAuthor`,
  `setTimeRange`, `setSizeRange`, `setLayer`, `setFlavor`. Used
  consistently in chip-rail, filter-drawer, surface tests.

No placeholders or TODOs — every step has either complete code or an
exact decision rule (e.g., the deferral of `lastWakeAtMs` to a
follow-up is an explicit rule, not a placeholder).
