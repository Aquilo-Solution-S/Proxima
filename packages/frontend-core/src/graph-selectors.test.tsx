import { describe, expect, it } from "vitest";
import type { EdgeRow, GoalRow, MemoryRow, Owner, SchemaInfo } from "./bindings";
import { CORE_FLAVOR_ID, defaultGraphFilterState } from "./graph-filter-store";
import { filterGraphSnapshot, schemaFlavor, visibleEntityIds } from "./graph-selectors";
import type { GraphSnapshot } from "./graph-store";
import { createHub } from "./hub";

const owner: Owner = {
  principal: { User: "00000000-0000-0000-0000-000000000000" },
  org_id: "00000000-0000-0000-0000-000000000000",
};

const memory = (id: string, kind: MemoryRow["kind"], schema_id: string): MemoryRow => ({
  id,
  kind,
  schema_id,
  schema_version: 1,
  owner,
  payload: [],
});

const goal = (id: string, text: string): GoalRow => ({
  id,
  schema_id: "core/goal-v1",
  schema_version: 1,
  owner,
  title: text,
  text,
  state: "Active",
  parent_goal_ids: [],
  supersedes: null,
  payload: [],
});

const edge = (id: string, source: string, target: string): EdgeRow => ({
  id,
  relation: "code/calls",
  relation_class: "Structural",
  source: { Memory: source },
  target: { Memory: target },
  owner,
  payload: [],
});

const snapshot = (parts: Partial<GraphSnapshot>): GraphSnapshot => ({
  owner,
  schemas: [],
  memoriesById: new Map(),
  goalsById: new Map(),
  edgesById: new Map(),
  eventsBySeq: new Map(),
  pendingHydration: new Map(),
  decodeErrorsByEntity: new Map(),
  streamStatus: "live",
  seqHighWater: null,
  ...parts,
});

describe("graph selectors", () => {
  it("filters memories and drops edges with hidden endpoints", () => {
    const fact = memory("019dfa10-0000-7000-8000-000000000001", "Fact", "proxima-code/code-chunk-v1");
    const abs = memory("019dfa10-0000-7000-8000-000000000002", "Abstraction", "proxima-code/commit-summary-v1");
    const graph = snapshot({
      memoriesById: new Map([
        [fact.id, { row: fact, payload: { text: "alpha" } }],
        [abs.id, { row: abs, payload: { summary: "beta" } }],
      ]),
      edgesById: new Map([[
        "019dfa10-0000-7000-8000-000000000003",
        edge("019dfa10-0000-7000-8000-000000000003", fact.id, abs.id),
      ]]),
    });
    const filter = { ...defaultGraphFilterState(), layers: new Set(["Fact" as const]) };
    const out = filterGraphSnapshot(graph, filter, createHub([]));
    expect(out.memories.map((m) => m.row.id)).toEqual([fact.id]);
    expect(out.edges).toHaveLength(0);
    expect(out.hiddenEdgeCount).toBe(1);
    expect(visibleEntityIds(out)).toEqual(new Set([fact.id]));
  });

  it("infers flavor from schema registrations before sidecar table fallback", () => {
    const hub = createHub([]);
    hub.registerFlavor("code", (scope) => {
      scope.registerCodec("proxima-code/code-chunk-v1", 1, {
        decode: () => ({}),
        encode: () => new Uint8Array(),
      });
    });
    const schema: SchemaInfo = {
      schema_id: "proxima-code/code-chunk-v1",
      schema_version: 1,
      kind: "Fact",
      filter_keys: [],
      sidecar_table: "proxima_code.code_chunk_v1",
      natural_key_columns: [],
      tombstone: null,
    };
    expect(schemaFlavor(schema, hub)).toBe("code");
  });

  it("matches search across schema id, entity id, goal text, and allow-listed payload fields", () => {
    const fact = memory("019dfa10-0000-7000-8000-000000000010", "Fact", "proxima-code/code-chunk-v1");
    const abs = memory("019dfa10-0000-7000-8000-000000000020", "Abstraction", "proxima-code/commit-summary-v1");
    const graph = snapshot({
      memoriesById: new Map([
        [fact.id, { row: fact, payload: { text: "needle in payload", file_path: "src/x.rs" } }],
        [abs.id, { row: abs, payload: { summary: "haystack summary" } }],
      ]),
      goalsById: new Map([["019dfa10-0000-7000-8000-000000000011", goal("019dfa10-0000-7000-8000-000000000011", "goal needle")]]),
    });
    const out = filterGraphSnapshot(graph, { ...defaultGraphFilterState(), search: "needle" }, createHub([]));
    expect(out.memories.map((m) => m.row.id)).toEqual([fact.id]);
    expect(out.goals.map((g) => g.text)).toEqual(["goal needle"]);
  });

  it("does not throw when payload contains Uint8Array or BigInt", () => {
    const fact = memory("019dfa10-0000-7000-8000-000000000030", "Fact", "proxima-code/file-revision-v1");
    const graph = snapshot({
      memoriesById: new Map([[
        fact.id,
        {
          row: fact,
          payload: {
            file_path: "src/safe.rs",
            content_sha256: new Uint8Array(32).fill(1),
            size_bytes: 9_007_199_254_740_993n,
          },
        },
      ]]),
    });
    expect(() =>
      filterGraphSnapshot(graph, { ...defaultGraphFilterState(), search: "safe" }, createHub([])),
    ).not.toThrow();
  });

  it("filters rows without a flavor through the core origin", () => {
    const fact = memory(
      "019dfa10-0000-7000-8000-000000000040",
      "Fact",
      "core/fact-v1",
    );
    const graph = snapshot({
      memoriesById: new Map([[fact.id, { row: fact, payload: null }]]),
    });
    const out = filterGraphSnapshot(
      graph,
      { ...defaultGraphFilterState(), hiddenFlavorIds: new Set([CORE_FLAVOR_ID]) },
      createHub([]),
    );
    expect(out.memories).toHaveLength(0);
    expect(out.filteredOutEntityCount).toBe(1);
  });
});
