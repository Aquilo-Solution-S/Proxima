import {
  createContext,
  createSignal,
  useContext,
  type Accessor,
  type JSX,
} from "solid-js";
import type {
  ChangeEvent,
  EntityKind,
  EntityRef,
  EdgeRow,
  EventHistoryResponse,
  GoalRow,
  MemoryRow,
  Owner,
  QueryRequest,
  QueryResponse,
  SchemaInfo,
  TombstoneFilter,
} from "./bindings";
import type { EngineClient, Subscription } from "./client";
import type { Hub } from "./hub";

export type StreamStatus = "connecting" | "live" | "degraded" | "stopped";

export interface DecodeError {
  id: string;
  kind: "missing_codec" | "decode_failed" | "hydration_missing";
  message: string;
}

export interface DecodedMemory {
  row: MemoryRow;
  payload: unknown;
  decodeError?: DecodeError;
}

export interface GraphSnapshot {
  owner: Owner;
  schemas: SchemaInfo[];
  memoriesById: ReadonlyMap<string, DecodedMemory>;
  goalsById: ReadonlyMap<string, GoalRow>;
  edgesById: ReadonlyMap<string, EdgeRow>;
  eventsBySeq: ReadonlyMap<string, ChangeEvent>;
  pendingHydration: ReadonlyMap<
    string,
    { kind: "memory" | "goal" | "edge"; since: string; attempts: number }
  >;
  decodeErrorsByEntity: ReadonlyMap<string, DecodeError>;
  streamStatus: StreamStatus;
  seqHighWater: string | null;
}

export interface GraphStore {
  state: Accessor<GraphSnapshot>;
  refresh(): Promise<void>;
}

const DECODE_ERROR_CAP = 256;
const HYDRATION_WINDOW_MS = 50;
const MAX_BATCH = 500;
const BURST_THRESHOLD = 5_000;
const HISTORY_SEED_LIMIT = 200;
// Node window for owner snapshots. Snapshot edges are loaded by the
// backend as closure over returned nodes and capped separately.
export const GRAPH_SNAPSHOT_LIMIT = 5_000;
// Mirrors `MAX_SNAPSHOT_EDGES` in storage-pg. Used only for the Atlas
// status pill; there is no wire field for the cap in v1.
export const MAX_SNAPSHOT_EDGES = 50_000;

const GraphContext = createContext<GraphStore>();

export const sentinelOwner = (): Owner => ({
  principal: { User: "00000000-0000-0000-0000-000000000000" },
  org_id: "00000000-0000-0000-0000-000000000000",
});

const snapshotReq = (
  owner: Owner,
  tombstones: TombstoneFilter = "PresentOnly",
): QueryRequest => ({
  owner,
  entity_kind: null,
  schema_id: null,
  supersession: "HeadsOnly",
  tombstones,
  limit: GRAPH_SNAPSHOT_LIMIT,
});

const seqValue = (seq: string): bigint | null => {
  const hex = seq.replace(/-/g, "");
  if (!/^[0-9a-fA-F]+$/.test(hex)) return null;
  return BigInt(`0x${hex}`);
};

const isSeqGap = (prev: string | null, next: string): boolean => {
  if (prev === null) return false;
  const a = seqValue(prev);
  const b = seqValue(next);
  return a !== null && b !== null && b <= a;
};

const seedEventsBySeq = (
  resp: EventHistoryResponse,
): ReadonlyMap<string, ChangeEvent> => {
  const map = new Map<string, ChangeEvent>();
  for (const event of resp.events) {
    map.set(event.seq, event);
  }
  return map;
};

const maxSeq = (a: string | null, b: string | null): string | null => {
  if (a === null) return b;
  if (b === null) return a;
  const av = seqValue(a);
  const bv = seqValue(b);
  if (av === null || bv === null) return a;
  return bv > av ? b : a;
};

export const entityRefId = (ref: EntityRef): string =>
  ref.Memory !== undefined ? ref.Memory : ref.Goal!;

const trimErrors = (
  errors: Map<string, DecodeError>,
): Map<string, DecodeError> => {
  if (errors.size <= DECODE_ERROR_CAP) return errors;
  const next = new Map(errors);
  for (const key of next.keys()) {
    next.delete(key);
    if (next.size <= DECODE_ERROR_CAP) break;
  }
  return next;
};

const endpointId = (ref: EntityRef): string => entityRefId(ref);

const edgeTouches = (edge: EdgeRow, ids: ReadonlySet<string>): boolean =>
  ids.has(endpointId(edge.source)) || ids.has(endpointId(edge.target));

const edgeEndpointsVisible = (
  edge: EdgeRow,
  memories: ReadonlyMap<string, DecodedMemory>,
  goals: ReadonlyMap<string, GoalRow>,
): boolean =>
  (edge.source.Memory !== undefined
    ? memories.has(edge.source.Memory)
    : goals.has(edge.source.Goal!)) &&
  (edge.target.Memory !== undefined
    ? memories.has(edge.target.Memory)
    : goals.has(edge.target.Goal!));

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

export function createGraphStore(
  client: EngineClient,
  hub: Hub,
  owner: Owner = sentinelOwner(),
): GraphStore {
  const [state, setState] = createSignal<GraphSnapshot>({
    owner,
    schemas: [],
    memoriesById: new Map(),
    goalsById: new Map(),
    edgesById: new Map(),
    eventsBySeq: new Map(),
    pendingHydration: new Map(),
    decodeErrorsByEntity: new Map(),
    streamStatus: "connecting",
    seqHighWater: null,
  });
  let subscription: Subscription | null = null;
  let hydrationTimer: ReturnType<typeof setTimeout> | null = null;
  let hydrationInFlight = false;
  let stopped = false;
  let activeMemoryIdByNaturalKey = new Map<string, string>();
  let naturalKeyByMemoryId = new Map<string, string>();
  const missingNaturalKeyWarnings = new Set<string>();
  const decodeWarnings = new Set<string>();
  const hydrationWarnings = new Set<string>();

  const decodeMemory = (row: MemoryRow): DecodedMemory => {
    const codec = hub.codecFor(row.schema_id, row.schema_version);
    if (row.payload.length === 0) return { row, payload: null };
    const schemaKey = `${row.schema_id}@${row.schema_version}`;
    if (codec === null) {
      if (!decodeWarnings.has(`missing_codec:${schemaKey}`)) {
        decodeWarnings.add(`missing_codec:${schemaKey}`);
        console.warn(`payload decode: ${schemaKey} has no codec`);
      }
      return {
        row,
        payload: null,
        decodeError: {
          id: row.id,
          kind: "missing_codec",
          message: `${schemaKey} has no codec`,
        },
      };
    }
    try {
      return { row, payload: codec.decode(new Uint8Array(row.payload)) };
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (!decodeWarnings.has(`decode_failed:${schemaKey}`)) {
        decodeWarnings.add(`decode_failed:${schemaKey}`);
        console.warn(`payload decode: ${schemaKey} threw — ${message}`);
      }
      return {
        row,
        payload: null,
        decodeError: {
          id: row.id,
          kind: "decode_failed",
          message,
        },
      };
    }
  };

  const naturalKeyFor = (
    decoded: DecodedMemory,
    schema: SchemaInfo | undefined,
  ): string | null => {
    if (schema === undefined || schema.natural_key_columns.length === 0) {
      return null;
    }
    const codec = hub.codecFor(decoded.row.schema_id, decoded.row.schema_version);
    if (codec?.naturalKey === undefined) {
      const key = `${decoded.row.schema_id}@${decoded.row.schema_version}`;
      if (!missingNaturalKeyWarnings.has(key)) {
        missingNaturalKeyWarnings.add(key);
        console.warn(`${key} is stateful but has no naturalKey codec`);
      }
      return null;
    }
    if (decoded.decodeError) return null;
    const naturalKey = codec.naturalKey(decoded.payload);
    if (naturalKey === null) return null;
    return JSON.stringify([
      decoded.row.schema_id,
      decoded.row.schema_version,
      ...naturalKey,
    ]);
  };

  const isTombstoneMemory = (
    decoded: DecodedMemory,
    schema: SchemaInfo | undefined,
  ): boolean => {
    const tombstone = schema?.tombstone;
    if (tombstone === null || tombstone === undefined) return false;
    return (
      isRecord(decoded.payload) &&
      decoded.payload[tombstone.column] === tombstone.value
    );
  };

  const applyResponse = (resp: QueryResponse): void => {
    setState((prev) => {
      const memories = new Map(prev.memoriesById);
      const goals = new Map(prev.goalsById);
      const edges = new Map(prev.edgesById);
      const pending = new Map(prev.pendingHydration);
      let errors = new Map(prev.decodeErrorsByEntity);
      const removedMemoryIds = new Set<string>();
      for (const row of resp.memories) {
        const decoded = decodeMemory(row);
        pending.delete(row.id);
        const schema = prev.schemas.find(
          (s) =>
            s.schema_id === row.schema_id &&
            s.schema_version === row.schema_version,
        );
        const stableKey = naturalKeyFor(decoded, schema);
        if (stableKey !== null) {
          const previous = activeMemoryIdByNaturalKey.get(stableKey);
          if (previous !== undefined && previous !== row.id) {
            memories.delete(previous);
            errors.delete(previous);
            pending.delete(previous);
            naturalKeyByMemoryId.delete(previous);
            removedMemoryIds.add(previous);
          }
          naturalKeyByMemoryId.set(row.id, stableKey);
        }
        if (decoded.decodeError) {
          memories.set(row.id, decoded);
          errors.set(row.id, decoded.decodeError);
        } else if (stableKey !== null && isTombstoneMemory(decoded, schema)) {
          memories.delete(row.id);
          errors.delete(row.id);
          naturalKeyByMemoryId.delete(row.id);
          activeMemoryIdByNaturalKey.delete(stableKey);
          removedMemoryIds.add(row.id);
        } else {
          memories.set(row.id, decoded);
          errors.delete(row.id);
          if (stableKey !== null) {
            activeMemoryIdByNaturalKey.set(stableKey, row.id);
          }
        }
      }
      for (const id of removedMemoryIds) {
        const stableKey = naturalKeyByMemoryId.get(id);
        if (stableKey !== undefined) {
          naturalKeyByMemoryId.delete(id);
          if (activeMemoryIdByNaturalKey.get(stableKey) === id) {
            activeMemoryIdByNaturalKey.delete(stableKey);
          }
        }
      }
      for (const row of resp.goals) {
        goals.set(row.id, row);
        pending.delete(row.id);
      }
      if (removedMemoryIds.size > 0) {
        for (const [id, edge] of edges) {
          if (edgeTouches(edge, removedMemoryIds)) edges.delete(id);
        }
      }
      for (const row of resp.edges) {
        pending.delete(row.id);
        if (edgeEndpointsVisible(row, memories, goals)) {
          edges.set(row.id, row);
        } else {
          edges.delete(row.id);
        }
      }
      errors = trimErrors(errors);
      return {
        ...prev,
        memoriesById: memories,
        goalsById: goals,
        edgesById: edges,
        pendingHydration: pending,
        decodeErrorsByEntity: errors,
        seqHighWater: maxSeq(prev.seqHighWater, resp.seq_high_water),
      };
    });
  };

  const refresh = async (): Promise<void> => {
    setState((prev) => ({ ...prev, streamStatus: "connecting" }));
    const [schemaResp, queryResp, historyResp] = await Promise.all([
      client.schema(),
      client.query(snapshotReq(owner)),
      client.eventHistory({ owner, limit: HISTORY_SEED_LIMIT, before: null }),
    ]);
    activeMemoryIdByNaturalKey = new Map();
    naturalKeyByMemoryId = new Map();
    setState((prev) => ({
      ...prev,
      schemas: schemaResp.schemas,
      memoriesById: new Map(),
      goalsById: new Map(),
      edgesById: new Map(),
      eventsBySeq: seedEventsBySeq(historyResp),
      pendingHydration: new Map(),
      decodeErrorsByEntity: new Map(),
      seqHighWater: maxSeq(queryResp.seq_high_water, historyResp.seq_high_water),
    }));
    applyResponse(queryResp);
    subscription?.unsubscribe();
    subscription = await client.subscribe(
      { owner, since: state().seqHighWater },
      handleEvent,
    );
    setState((prev) => ({ ...prev, streamStatus: "live" }));
  };

  const scheduleHydration = (): void => {
    if (hydrationTimer !== null || hydrationInFlight) return;
    hydrationTimer = setTimeout(() => {
      hydrationTimer = null;
      void flushHydration();
    }, HYDRATION_WINDOW_MS);
  };

  const markPending = (
    kind: "memory" | "goal" | "edge",
    id: string,
    seq: string,
  ): void => {
    setState((prev) => {
      const pending = new Map(prev.pendingHydration);
      const current = pending.get(id);
      pending.set(id, {
        kind,
        since: current?.since ?? seq,
        attempts: current?.attempts ?? 0,
      });
      return { ...prev, pendingHydration: pending };
    });
    if (state().pendingHydration.size > BURST_THRESHOLD) {
      void refresh();
      return;
    }
    scheduleHydration();
  };

  const handleEvent = (event: ChangeEvent): void => {
    if (state().eventsBySeq.has(event.seq)) return;
    setState((prev) => {
      const events = new Map(prev.eventsBySeq);
      events.set(event.seq, event);
      return {
        ...prev,
        eventsBySeq: events,
        streamStatus: isSeqGap(prev.seqHighWater, event.seq)
          ? "degraded"
          : prev.streamStatus,
        seqHighWater: event.seq,
      };
    });
    const append = event.kind.EntityAppend;
    if (append !== undefined) {
      markPending(
        append.entity_kind === "Goal" ? "goal" : "memory",
        entityRefId(append.entity),
        event.seq,
      );
      return;
    }
    const edge = event.kind.EdgeAppend;
    if (edge !== undefined) {
      markPending("edge", edge.edge_id, event.seq);
    }
  };

  const flushHydration = async (): Promise<void> => {
    if (hydrationInFlight || stopped) return;
    const pendingEntries = Array.from(state().pendingHydration.entries()).slice(
      0,
      MAX_BATCH,
    );
    if (pendingEntries.length === 0) return;
    const memoryIds = pendingEntries
      .filter(([, entry]) => entry.kind === "memory")
      .map(([id]) => id);
    const goalIds = pendingEntries
      .filter(([, entry]) => entry.kind === "goal")
      .map(([id]) => id);
    const edgeIds = pendingEntries
      .filter(([, entry]) => entry.kind === "edge")
      .map(([id]) => id);
    hydrationInFlight = true;
    try {
      const resp = await client.query({
        ...snapshotReq(owner),
        tombstones: memoryIds.length > 0 ? "IncludeTombstoned" : "PresentOnly",
        limit: Math.max(pendingEntries.length, 1),
        memory_ids: memoryIds,
        goal_ids: goalIds,
        edge_ids: edgeIds,
      });
      applyResponse(resp);
      setState((prev) => {
        const pending = new Map(prev.pendingHydration);
        let errors = new Map(prev.decodeErrorsByEntity);
        const surfaced = new Set([
          ...resp.memories.map((row) => row.id),
          ...resp.goals.map((row) => row.id),
          ...resp.edges.map((row) => row.id),
        ]);
        for (const [id] of pendingEntries) {
          if (surfaced.has(id)) continue;
          const current = pending.get(id);
          if (current === undefined) continue;
          const attempts = current.attempts + 1;
          if (attempts >= 3) {
            pending.delete(id);
            if (!hydrationWarnings.has(id)) {
              hydrationWarnings.add(id);
              console.warn(
                `payload decode: hydration_missing for ${current.kind} ${id} (3 attempts)`,
              );
            }
            errors.set(id, {
              id,
              kind: "hydration_missing",
              message: "hydration query did not return the entity",
            });
          } else {
            pending.set(id, { ...current, attempts });
          }
        }
        errors = trimErrors(errors);
        return {
          ...prev,
          pendingHydration: pending,
          decodeErrorsByEntity: errors,
        };
      });
    } catch {
      setState((prev) => ({ ...prev, streamStatus: "degraded" }));
    } finally {
      hydrationInFlight = false;
      if (state().pendingHydration.size > 0) scheduleHydration();
    }
  };

  void refresh().catch(() => {
    setState((prev) => ({ ...prev, streamStatus: "degraded" }));
  });

  return { state, refresh };
}

export const GraphProvider = (props: {
  store: GraphStore;
  children: JSX.Element;
}): JSX.Element => (
  <GraphContext.Provider value={props.store}>
    {props.children}
  </GraphContext.Provider>
);

export const useGraph = (): GraphStore => {
  const store = useContext(GraphContext);
  if (store === undefined) {
    throw new Error("GraphProvider is missing");
  }
  return store;
};

export const memoriesByKind = (
  graph: GraphSnapshot,
  kind: EntityKind,
): DecodedMemory[] =>
  Array.from(graph.memoriesById.values()).filter((m) => m.row.kind === kind);
