import type { EdgeRow, EntityKind, GoalRow, MemoryRow, SchemaInfo } from "./bindings";
import { flavorFilterId, type GraphFilterState } from "./graph-filter-store";
import { entityRefId, type DecodedMemory, type GraphSnapshot } from "./graph-store";
import type { Hub } from "./hub";

export interface FilteredGraph {
  memories: DecodedMemory[];
  goals: GoalRow[];
  edges: EdgeRow[];
  hiddenEdgeCount: number;
  filteredOutEntityCount: number;
  schemaFlavors: ReadonlyMap<string, string | null>;
}

const SEARCHABLE_PAYLOAD_FIELDS = [
  "text",
  "summary",
  "file_path",
  "commit_sha",
  "chunk_type",
  "language",
  "change_kind",
] as const;

const schemaKey = (schemaId: string, version: number): string =>
  `${schemaId}@${version}`;

export function searchHaystackForMemory(payload: unknown): string {
  if (payload === null || typeof payload !== "object") return "";
  if (Object.getPrototypeOf(payload) !== Object.prototype) return "";
  const obj = payload as Record<string, unknown>;
  let out = "";
  for (const key of SEARCHABLE_PAYLOAD_FIELDS) {
    const value = obj[key];
    if (typeof value === "string") out += ` ${value}`;
  }
  return out.toLowerCase();
}

export function schemaFlavor(schema: SchemaInfo, hub: Hub): string | null {
  const fromHub = hub.flavorFor(schema.schema_id, schema.schema_version);
  if (fromHub !== null) return fromHub;
  const match = /^proxima_([a-z0-9_]+)\./.exec(schema.sidecar_table ?? "");
  return match?.[1] ?? null;
}

const schemaFlavorForRow = (
  schemaId: string,
  schemaVersion: number,
  flavorsBySchema: ReadonlyMap<string, string | null>,
  hub: Hub,
): string | null =>
  flavorsBySchema.get(schemaKey(schemaId, schemaVersion)) ??
  hub.flavorFor(schemaId, schemaVersion);

export const schemaFlavorForMemory = (
  memory: MemoryRow,
  graph: FilteredGraph,
  hub: Hub,
): string | null =>
  schemaFlavorForRow(
    memory.schema_id,
    memory.schema_version,
    graph.schemaFlavors,
    hub,
  );

export const schemaFlavorForGoal = (
  goal: GoalRow,
  graph: FilteredGraph,
  hub: Hub,
): string | null =>
  schemaFlavorForRow(goal.schema_id, goal.schema_version, graph.schemaFlavors, hub);

const schemaAllowed = (
  schemaId: string,
  filter: GraphFilterState,
): boolean => filter.schemaIds.size === 0 || filter.schemaIds.has(schemaId);

const flavorAllowed = (
  flavor: string | null,
  filter: GraphFilterState,
): boolean => !filter.hiddenFlavorIds.has(flavorFilterId(flavor));

const searchMatchesMemory = (
  memory: DecodedMemory,
  search: string,
): boolean => {
  if (search.length === 0) return true;
  const row = memory.row;
  return (
    row.id.toLowerCase().includes(search) ||
    row.schema_id.toLowerCase().includes(search) ||
    searchHaystackForMemory(memory.payload).includes(search)
  );
};

const searchMatchesGoal = (goal: GoalRow, search: string): boolean => {
  if (search.length === 0) return true;
  return (
    goal.id.toLowerCase().includes(search) ||
    goal.schema_id.toLowerCase().includes(search) ||
    goal.title.toLowerCase().includes(search) ||
    goal.text.toLowerCase().includes(search)
  );
};

export function filterGraphSnapshot(
  graph: GraphSnapshot,
  filter: GraphFilterState,
  hub: Hub,
): FilteredGraph {
  const search = filter.search.trim().toLowerCase();
  const schemaFlavors = new Map<string, string | null>();
  for (const schema of graph.schemas) {
    schemaFlavors.set(schemaKey(schema.schema_id, schema.schema_version), schemaFlavor(schema, hub));
  }

  const memories = Array.from(graph.memoriesById.values()).filter((memory) => {
    const row = memory.row;
    const flavor = schemaFlavorForRow(row.schema_id, row.schema_version, schemaFlavors, hub);
    return (
      filter.layers.has(row.kind) &&
      schemaAllowed(row.schema_id, filter) &&
      flavorAllowed(flavor, filter) &&
      searchMatchesMemory(memory, search)
    );
  });

  const goals = Array.from(graph.goalsById.values()).filter((goal) => {
    const flavor = schemaFlavorForRow(goal.schema_id, goal.schema_version, schemaFlavors, hub);
    return (
      filter.layers.has("Goal") &&
      schemaAllowed(goal.schema_id, filter) &&
      flavorAllowed(flavor, filter) &&
      searchMatchesGoal(goal, search)
    );
  });

  const visible = new Set<string>([
    ...memories.map((memory) => memory.row.id),
    ...goals.map((goal) => goal.id),
  ]);
  const edges: EdgeRow[] = [];
  let hiddenEdgeCount = 0;
  for (const edge of graph.edgesById.values()) {
    if (visible.has(entityRefId(edge.source)) && visible.has(entityRefId(edge.target))) {
      edges.push(edge);
    } else {
      hiddenEdgeCount++;
    }
  }

  const rawEntityCount = graph.memoriesById.size + graph.goalsById.size;
  return {
    memories,
    goals,
    edges,
    hiddenEdgeCount,
    filteredOutEntityCount: rawEntityCount - visible.size,
    schemaFlavors,
  };
}

export const visibleEntityIds = (graph: FilteredGraph): Set<string> =>
  new Set([
    ...graph.memories.map((memory) => memory.row.id),
    ...graph.goals.map((goal) => goal.id),
  ]);

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
    const sourceMem = edge.source.Memory !== undefined ? edge.source.Memory : null;
    const targetMem = edge.target.Memory !== undefined ? edge.target.Memory : null;
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
