import { describe, expect, it } from "vitest";
import type { EdgeRow, GoalRow, MemoryRow, Owner } from "../../bindings";
import { defaultGraphFilterState } from "../../graph-filter-store";
import { filterGraphSnapshot } from "../../graph-selectors";
import type { GraphSnapshot } from "../../graph-store";
import { createHub } from "../../hub";
import { atlasProjectionFromGraph } from "./projection";

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

const goal = (id: string): GoalRow => ({
  id,
  schema_id: "core/goal-v1",
  schema_version: 1,
  owner,
  title: "Ship atlas",
  text: "Ship atlas",
  state: "Active",
  parent_goal_ids: [],
  supersedes: null,
  payload: [],
});

const graph = (memories: MemoryRow[], goals: GoalRow[], edges: EdgeRow[]): GraphSnapshot => ({
  owner,
  schemas: [],
  memoriesById: new Map(memories.map((row) => [row.id, { row, payload: null }])),
  goalsById: new Map(goals.map((row) => [row.id, row])),
  edgesById: new Map(edges.map((row) => [row.id, row])),
  eventsBySeq: new Map(),
  pendingHydration: new Map(),
  decodeErrorsByEntity: new Map(),
  memoryProvenance: new Map(),
  streamStatus: "live",
  seqHighWater: null,
});

const edge = (id: string, source: EdgeRow["source"], target: EdgeRow["target"]): EdgeRow => ({
  id,
  relation: "code/calls",
  relation_class: "Structural",
  source,
  target,
  owner,
  payload: [],
});

describe("atlas projection", () => {
  it("projects memories, goals, and valid edges", () => {
    const fact = memory("019dfa20-0000-7000-8000-000000000001", "Fact", "proxima-code/code-chunk-v1");
    const abs = memory("019dfa20-0000-7000-8000-000000000002", "Abstraction", "proxima-code/commit-summary-v1");
    const g = goal("019dfa20-0000-7000-8000-000000000003");
    const e1 = edge("019dfa20-0000-7000-8000-000000000004", { Memory: abs.id }, { Memory: fact.id });
    const e2 = edge("019dfa20-0000-7000-8000-000000000005", { Goal: g.id }, { Memory: abs.id });
    const filtered = filterGraphSnapshot(graph([fact, abs], [g], [e1, e2]), defaultGraphFilterState(), createHub([]));
    const out = atlasProjectionFromGraph(filtered, createHub([]));
    expect(out.nodes.map((n) => n.kind).sort()).toEqual(["Abstraction", "Fact", "Goal"]);
    expect(out.edges.map((e) => e.id).sort()).toEqual([e1.id, e2.id]);
    expect(out.omittedEdgeCount).toBe(0);
  });

  it("omits edges whose endpoints are missing from visible nodes", () => {
    const fact = memory("019dfa20-0000-7000-8000-000000000010", "Fact", "proxima-code/code-chunk-v1");
    const broken = edge("019dfa20-0000-7000-8000-000000000011", { Memory: fact.id }, { Memory: "019dfa20-0000-7000-8000-000000000012" });
    const filtered = filterGraphSnapshot(graph([fact], [], [broken]), defaultGraphFilterState(), createHub([]));
    const out = atlasProjectionFromGraph(filtered, createHub([]));
    expect(out.edges).toHaveLength(0);
    expect(out.omittedEdgeCount).toBe(1);
  });

  it("is deterministic for the same entity id and stays within projection bounds", () => {
    const fact = memory("019dfa20-0000-7000-8000-000000000020", "Fact", "proxima-code/code-chunk-v1");
    const filtered = filterGraphSnapshot(graph([fact], [], []), defaultGraphFilterState(), createHub([]));
    const a = atlasProjectionFromGraph(filtered, createHub([])).nodes[0]!;
    const b = atlasProjectionFromGraph(filtered, createHub([])).nodes[0]!;
    expect({ x: a.x, y: a.y }).toEqual({ x: b.x, y: b.y });
    expect(Math.abs(a.x)).toBeLessThanOrEqual(9);
    expect(Math.abs(a.y)).toBeLessThanOrEqual(6);
  });
});
