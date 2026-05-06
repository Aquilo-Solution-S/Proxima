import type { GoalRow } from "../../bindings";
import {
  entityRefId,
  type DecodedMemory,
} from "../../graph-store";
import {
  schemaFlavorForGoal,
  schemaFlavorForMemory,
  type FilteredGraph,
} from "../../graph-selectors";
import type { Hub } from "../../hub";
import { LAYER_Z } from "./three-helpers";
import type { AtlasEdge, AtlasNode, AtlasNodeKind } from "./types";

export interface AtlasProjection {
  nodes: AtlasNode[];
  edges: AtlasEdge[];
  omittedEdgeCount: number;
}

const stringHash = (value: string): number => {
  let hash = 2166136261;
  for (let i = 0; i < value.length; i++) {
    hash ^= value.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  return hash >>> 0;
};

const coordinateFor = (id: string, axis: "x" | "y"): number => {
  const hash = stringHash(`${axis}:${id}`);
  const unit = hash / 0xffffffff;
  const bound = axis === "x" ? 9 : 6;
  return Number(((unit * 2 - 1) * bound).toFixed(4));
};

const objectValue = (
  payload: unknown,
  key: "file_path" | "summary",
): string | null => {
  if (payload === null || typeof payload !== "object") return null;
  if (Object.getPrototypeOf(payload) !== Object.prototype) return null;
  const value = (payload as Record<string, unknown>)[key];
  return typeof value === "string" ? value : null;
};

const memoryTitle = (
  schemaId: string,
  id: string,
  payload: unknown,
): string => {
  const filePath = objectValue(payload, "file_path");
  if (filePath !== null) return filePath;
  const summary = objectValue(payload, "summary");
  if (summary !== null) return summary.slice(0, 96);
  return `${schemaId} ${id.slice(0, 8)}`;
};

const goalTitle = (goal: GoalRow): string => goal.title.slice(0, 96);

const nodeFor = (
  id: string,
  kind: AtlasNodeKind,
  schemaId: string,
  schemaVersion: number,
  flavor: string | null,
  title: string,
  details: Pick<AtlasNode, "memory" | "goal" | "payload" | "decodeError"> = {},
): AtlasNode => ({
  id,
  kind,
  schemaId,
  schemaVersion,
  flavor,
  x: coordinateFor(id, "x"),
  y: coordinateFor(id, "y"),
  title,
  ...details,
});

const nodeForMemory = (
  memory: DecodedMemory,
  graph: FilteredGraph,
  hub: Hub,
): AtlasNode =>
  nodeFor(
    memory.row.id,
    memory.row.kind,
    memory.row.schema_id,
    memory.row.schema_version,
    schemaFlavorForMemory(memory.row, graph, hub),
    memoryTitle(memory.row.schema_id, memory.row.id, memory.payload),
    {
      memory: memory.row,
      payload: memory.payload,
      decodeError: memory.decodeError,
    },
  );

const nodeForGoal = (
  goal: GoalRow,
  graph: FilteredGraph,
  hub: Hub,
): AtlasNode =>
  nodeFor(
    goal.id,
    "Goal",
    goal.schema_id,
    goal.schema_version,
    schemaFlavorForGoal(goal, graph, hub),
    goalTitle(goal),
    {
      goal,
      payload: goal.payload,
    },
  );

export function atlasProjectionFromGraph(
  graph: FilteredGraph,
  hub: Hub,
): AtlasProjection {
  const nodes: AtlasNode[] = [
    ...graph.memories.map((memory) => nodeForMemory(memory, graph, hub)),
    ...graph.goals.map((goal) => nodeForGoal(goal, graph, hub)),
  ];
  const visibleNodeIds = new Set(nodes.map((node) => node.id));
  const edges: AtlasEdge[] = [];
  let omittedEdgeCount = graph.hiddenEdgeCount;
  for (const edge of graph.edges) {
    const src = entityRefId(edge.source);
    const tgt = entityRefId(edge.target);
    if (!visibleNodeIds.has(src) || !visibleNodeIds.has(tgt)) {
      omittedEdgeCount++;
      continue;
    }
    edges.push({
      id: edge.id,
      src,
      tgt,
      kind: edge.relation,
      relationClass: edge.relation_class,
    });
  }
  return { nodes, edges, omittedEdgeCount };
}

export const projectionLayerZ = (kind: AtlasNodeKind): number => LAYER_Z[kind];
