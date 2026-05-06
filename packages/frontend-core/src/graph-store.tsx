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
  GoalRow,
  MemoryRow,
  Owner,
  QueryRequest,
  QueryResponse,
  SchemaInfo,
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

const snapshotReq = (owner: Owner): QueryRequest => ({
  owner,
  entity_kind: null,
  schema_id: null,
  supersession: "HeadsOnly",
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

  const decodeMemory = (row: MemoryRow): DecodedMemory => {
    const codec = hub.codecFor(row.schema_id, row.schema_version);
    if (row.payload.length === 0) return { row, payload: null };
    if (codec === null) {
      return {
        row,
        payload: null,
        decodeError: {
          id: row.id,
          kind: "missing_codec",
          message: `${row.schema_id}@${row.schema_version} has no codec`,
        },
      };
    }
    try {
      return { row, payload: codec.decode(new Uint8Array(row.payload)) };
    } catch (err) {
      return {
        row,
        payload: null,
        decodeError: {
          id: row.id,
          kind: "decode_failed",
          message: err instanceof Error ? err.message : String(err),
        },
      };
    }
  };

  const applyResponse = (resp: QueryResponse): void => {
    setState((prev) => {
      const memories = new Map(prev.memoriesById);
      const goals = new Map(prev.goalsById);
      const edges = new Map(prev.edgesById);
      const pending = new Map(prev.pendingHydration);
      let errors = new Map(prev.decodeErrorsByEntity);
      for (const row of resp.memories) {
        const decoded = decodeMemory(row);
        memories.set(row.id, decoded);
        pending.delete(row.id);
        if (decoded.decodeError) {
          errors.set(row.id, decoded.decodeError);
        } else {
          errors.delete(row.id);
        }
      }
      for (const row of resp.goals) {
        goals.set(row.id, row);
        pending.delete(row.id);
      }
      for (const row of resp.edges) {
        edges.set(row.id, row);
        pending.delete(row.id);
      }
      errors = trimErrors(errors);
      return {
        ...prev,
        memoriesById: memories,
        goalsById: goals,
        edgesById: edges,
        pendingHydration: pending,
        decodeErrorsByEntity: errors,
        seqHighWater: resp.seq_high_water,
      };
    });
  };

  const refresh = async (): Promise<void> => {
    setState((prev) => ({ ...prev, streamStatus: "connecting" }));
    const [schemaResp, queryResp] = await Promise.all([
      client.schema(),
      client.query(snapshotReq(owner)),
    ]);
    setState((prev) => ({
      ...prev,
      schemas: schemaResp.schemas,
      memoriesById: new Map(),
      goalsById: new Map(),
      edgesById: new Map(),
      pendingHydration: new Map(),
      decodeErrorsByEntity: new Map(),
      seqHighWater: queryResp.seq_high_water,
    }));
    applyResponse(queryResp);
    subscription?.unsubscribe();
    subscription = await client.subscribe(
      { owner, since: queryResp.seq_high_water },
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
    setState((prev) => {
      if (prev.eventsBySeq.has(event.seq)) return prev;
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
