import { createRoot } from "solid-js";
import { decode, encode } from "cbor-x";
import { describe, expect, it, vi } from "vitest";
import type {
  ChangeEvent,
  EdgeRow,
  EventDraft,
  GoalDraft,
  MemoryRow,
  Owner,
  QueryRequest,
  SchemaInfo,
} from "./bindings";
import type { EngineClient, Subscription } from "./client";
import {
  createGraphStore,
  GRAPH_SNAPSHOT_LIMIT,
  MAX_SNAPSHOT_EDGES,
} from "./graph-store";
import { createHub } from "./hub";
import type { PayloadCodec } from "./hub";

const owner: Owner = {
  principal: { User: "00000000-0000-0000-0000-000000000000" },
  org_id: "00000000-0000-0000-0000-000000000000",
};

const graphClient = (overrides: Partial<EngineClient>): EngineClient => ({
  schema: async () => ({ schemas: [] }),
  query: async () => ({ memories: [], goals: [], edges: [], seq_high_water: null }),
  eventHistory: async () => ({ events: [], seq_high_water: null }),
  subscribe: async (): Promise<Subscription> => ({ unsubscribe() {} }),
  goalWrite: async (_draft: GoalDraft) => {
    throw new Error("goalWrite not used");
  },
  goalReactivate: async () => {
    throw new Error("goalReactivate not used");
  },
  eventIngest: async (_draft: EventDraft) => {
    throw new Error("eventIngest not used");
  },
  listPersonalityInstances: async () => {
    throw new Error("listPersonalityInstances not used");
  },
  instantiatePersonality: async () => {
    throw new Error("instantiatePersonality not used");
  },
  setWakeEntries: async () => {
    throw new Error("setWakeEntries not used");
  },
  registerInferenceTarget: async () => {
    throw new Error("registerInferenceTarget not used");
  },
  listInferenceTargets: async () => [],
  removeInferenceTarget: async () => {
    throw new Error("removeInferenceTarget not used");
  },
  bindInferenceTier: async () => {
    throw new Error("bindInferenceTier not used");
  },
  listInferenceTierBindings: async () => {
    return [];
  },
  detectLocalHarness: async () => null,
  listOwnerRecipes: async () => ({ root_path: "", recipes: [] }),
  listBundledRecipes: async () => [],
  listMcpTools: async () => [],
  listWorkspaceTools: async () => [],
  listRelations: async () => [],
  wakeEntryProduces: async () => ({ schema_ids: [], relation_ids: [] }),
  ...overrides,
});

const fileRevisionCodec: PayloadCodec<Record<string, unknown>> = {
  decode(bytes: Uint8Array) {
    return decode(bytes) as Record<string, unknown>;
  },
  encode(value: Record<string, unknown>) {
    return encode(value);
  },
  naturalKey(value: Record<string, unknown>) {
    const repoId = value.repo_id;
    const filePath = value.file_path;
    if (typeof repoId !== "string" || typeof filePath !== "string") return null;
    return [repoId, filePath];
  },
};

const hubWithFileRevisionCodec = () => {
  const hub = createHub([]);
  hub.registerFlavor("proxima-code", (scope) => {
    scope.registerCodec("proxima-code/file-revision-v1", 1, fileRevisionCodec);
  });
  return hub;
};

const schema = (
  schemaId: string,
  naturalKeyColumns: string[],
  tombstone: SchemaInfo["tombstone"] = null,
): SchemaInfo => ({
  schema_id: schemaId,
  schema_version: 1,
  kind: "Fact",
  filter_keys: [],
  sidecar_table: "proxima_code.file_revision_v1",
  natural_key_columns: naturalKeyColumns,
  tombstone,
});

const memoryRow = (
  id: string,
  schemaId: string,
  payload: Record<string, unknown>,
): MemoryRow => ({
  id,
  kind: "Fact",
  schema_id: schemaId,
  schema_version: 1,
  owner,
  payload: Array.from(encode(payload)),
});

const edgeRow = (
  id: string,
  sourceMemoryId: string,
  targetMemoryId: string,
): EdgeRow => ({
  id,
  relation: "core/derived-from",
  relation_class: "Provenance",
  source: { Memory: sourceMemoryId },
  target: { Memory: targetMemoryId },
  owner,
  payload: [],
});

const entityAppendEvent = (
  memoryId: string,
  schemaId: string,
): ChangeEvent => ({
  seq: "01ARYZ6S41TS5G7QFC0V44N5KH",
  owner,
  kind: {
    EntityAppend: {
      entity_kind: "Fact",
      entity: { Memory: memoryId },
      schema_id: schemaId,
      schema_version: 1,
      supersedes: null,
    },
  },
});

// ---------------------------------------------------------------------------
// Shared test harness helpers
// ---------------------------------------------------------------------------

type TestHarness = {
  store: ReturnType<typeof createGraphStore>;
  pushEvent: (event: ChangeEvent) => void;
};

/**
 * Creates a minimal GraphStore wired to a mock client that lets tests push
 * ChangeEvents directly via the subscribe callback.
 *
 * Pass `historyEvents` to pre-populate the mock `eventHistory` response so
 * the bootstrap path in `refresh()` sees those events.
 */
const createTestHarness = (historyEvents: ChangeEvent[] = []): TestHarness => {
  const eventListeners: ((event: ChangeEvent) => void)[] = [];
  const client = graphClient({
    eventHistory: async () => ({ events: historyEvents, seq_high_water: null }),
    subscribe: async (_req, onEvent) => {
      eventListeners.push(onEvent);
      return { unsubscribe() {} };
    },
  });
  let store!: ReturnType<typeof createGraphStore>;
  createRoot(() => {
    store = createGraphStore(client, createHub([]), owner);
  });
  return {
    store,
    pushEvent: (event: ChangeEvent) => {
      for (const listener of eventListeners) listener(event);
    },
  };
};

describe("GraphStore snapshot loading", () => {
  it("requests the 5000-node snapshot window and exposes the edge cap", async () => {
    const queries: QueryRequest[] = [];
    const client = graphClient({
      query: async (req) => {
        queries.push(req);
        return { memories: [], goals: [], edges: [], seq_high_water: null };
      },
    });

    await new Promise<void>((resolve) => {
      createRoot((dispose) => {
        createGraphStore(client, createHub([]));
        void vi
          .waitFor(() => expect(queries[0]?.limit).toBe(5_000))
          .then(() => {
            expect(GRAPH_SNAPSHOT_LIMIT).toBe(5_000);
            expect(MAX_SNAPSHOT_EDGES).toBe(50_000);
            expect(queries[0]?.tombstones).toBe("PresentOnly");
            expect(queries[0]?.personality_roots).toBe("ActiveOnly");
            expect(queries[0]?.include_payloads).toBe(false);
            dispose();
            resolve();
          });
      });
    });
  });

  it("hydrates requested ids with payloads enabled", async () => {
    const queries: QueryRequest[] = [];
    const client = graphClient({
      query: async (req) => {
        queries.push(req);
        return { memories: [], goals: [], edges: [], seq_high_water: null };
      },
    });

    await new Promise<void>((resolve) => {
      createRoot((dispose) => {
        const store = createGraphStore(client, createHub([]), owner);
        void vi
          .waitFor(() => expect(queries).toHaveLength(1))
          .then(async () => {
            await store.hydrate?.({
              memory_ids: ["019dfa00-0000-7000-8000-000000000001"],
              goal_ids: ["019dfa00-0000-7000-8000-000000000002"],
            });
            expect(queries).toHaveLength(2);
            expect(queries[1]?.include_payloads).toBe(true);
            expect(queries[1]?.memory_ids).toEqual([
              "019dfa00-0000-7000-8000-000000000001",
            ]);
            expect(queries[1]?.goal_ids).toEqual([
              "019dfa00-0000-7000-8000-000000000002",
            ]);
            expect(queries[1]?.limit).toBe(2);
            dispose();
            resolve();
          });
      });
    });
  });

  it("evicts an older stateful head and its edges when a tombstone arrives", async () => {
    const older = memoryRow(
      "019dfa00-0000-7000-8000-000000000001",
      "proxima-code/file-revision-v1",
      {
        repo_id: "019dfa00-0000-7000-8000-000000000100",
        file_path: "src/deleted.rs",
        state: "Present",
      },
    );
    const tombstone = memoryRow(
      "019dfa00-0000-7000-8000-000000000002",
      "proxima-code/file-revision-v1",
      {
        repo_id: "019dfa00-0000-7000-8000-000000000100",
        file_path: "src/deleted.rs",
        state: "Tombstone",
      },
    );
    const peer = memoryRow(
      "019dfa00-0000-7000-8000-000000000004",
      "proxima-code/file-revision-v1",
      {
        repo_id: "019dfa00-0000-7000-8000-000000000100",
        file_path: "src/live.rs",
        state: "Present",
      },
    );
    const edge = edgeRow(
      "019dfa00-0000-7000-8000-000000000003",
      older.id,
      peer.id,
    );
    const events: ((event: ChangeEvent) => void)[] = [];
    const queries: QueryRequest[] = [];
    const client = graphClient({
      schema: async () => ({
        schemas: [
          schema(
            "proxima-code/file-revision-v1",
            ["repo_id", "file_path"],
            { column: "state", value: "Tombstone" },
          ),
        ],
      }),
      query: async (req) => {
        queries.push(req);
        if (req.memory_ids?.includes(tombstone.id)) {
          return {
            memories: [tombstone],
            goals: [],
            edges: [],
            seq_high_water: "019dfa00-0000-7000-8000-000000000020",
          };
        }
        return {
          memories: [older, peer],
          goals: [],
          edges: [edge],
          seq_high_water: "019dfa00-0000-7000-8000-000000000010",
        };
      },
      subscribe: async (_req, onEvent) => {
        events.push(onEvent);
        return { unsubscribe() {} };
      },
    });

    await new Promise<void>((resolve) => {
      createRoot((dispose) => {
        const store = createGraphStore(client, hubWithFileRevisionCodec(), owner);
        void vi
          .waitFor(() => expect(store.state().streamStatus).toBe("live"))
          .then(async () => {
            expect(store.state().memoriesById.has(older.id)).toBe(true);
            expect(store.state().edgesById.has(edge.id)).toBe(true);
            events[0]?.(entityAppendEvent(tombstone.id, tombstone.schema_id));
            await vi.waitFor(() =>
              expect(store.state().memoriesById.has(older.id)).toBe(false),
            );
            expect(store.state().memoriesById.has(tombstone.id)).toBe(false);
            expect(store.state().edgesById.has(edge.id)).toBe(false);
            expect(
              queries.some(
                (req) =>
                  req.memory_ids?.includes(tombstone.id) &&
                  req.tombstones === "IncludeTombstoned",
              ),
            ).toBe(true);
            dispose();
            resolve();
          });
      });
    });
  });
});

// ---------------------------------------------------------------------------
// memoryProvenance tests
// ---------------------------------------------------------------------------

describe("memoryProvenance", () => {
  it("is populated on EntityAppend ingest", async () => {
    const { store, pushEvent } = createTestHarness();
    const memId = "01ARYZ6S41TS5G7QFC0V44N5KH";
    const seq = "01ARYZ6S41TS5G7QFC0V44N5KH";

    await vi.waitFor(() => expect(store.state().streamStatus).toBe("live"));

    pushEvent({
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

  it("uses UUIDv7 change-event seqs for provenance timestamps", async () => {
    const { store, pushEvent } = createTestHarness();
    const memId = "019e12c2-2ba6-73c3-83a3-6b75d82e5032";
    const seq = "019e12c2-2ba6-73c3-83a3-6ba8f0db1d00";

    await vi.waitFor(() => expect(store.state().streamStatus).toBe("live"));

    pushEvent({
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
    expect(prov?.written_at_ms).toBe(1778431175590);
  });

  it("leaves provenance unset when no creating event has been seen", async () => {
    const { store } = createTestHarness();
    await vi.waitFor(() => expect(store.state().streamStatus).toBe("live"));
    expect(store.state().memoryProvenance.size).toBe(0);
  });

  it("earliest event wins — a second event for the same memory_id does not overwrite", async () => {
    const { store, pushEvent } = createTestHarness();
    const memId = "01ARYZ6S41TS5G7QFC0V44N5KH";
    const seqFirst = "01ARYZ6S41TS5G7QFC0V44N5KH";
    const seqSecond = "01BX5ZZKBKACTAV9WEVGEMMVS1";

    await vi.waitFor(() => expect(store.state().streamStatus).toBe("live"));

    pushEvent({
      seq: seqFirst,
      owner,
      authoring_personality_instance_id: "first-personality",
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

    pushEvent({
      seq: seqSecond,
      owner,
      authoring_personality_instance_id: "second-personality",
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
    expect(prov?.creating_seq).toBe(seqFirst);
    expect(prov?.authoring_personality_instance_id).toBe("first-personality");
  });

  it("Goal EntityAppend does not create a memory provenance entry", async () => {
    const { store, pushEvent } = createTestHarness();
    const goalId = "01ARYZ6S41TS5G7QFC0V44N5KH";
    const seq = "01ARYZ6S41TS5G7QFC0V44N5KH";

    await vi.waitFor(() => expect(store.state().streamStatus).toBe("live"));

    pushEvent({
      seq,
      owner,
      kind: {
        EntityAppend: {
          entity_kind: "Goal",
          entity: { Goal: goalId },
          schema_id: "core/goal-v1",
          schema_version: 1,
          supersedes: null,
        },
      },
    });

    expect(store.state().memoryProvenance.size).toBe(0);
  });

  it("bootstrap path applies earliest-wins across DESC-ordered eventHistory", async () => {
    const memId = "01ARYZ6S41TS5G7QFC0V44N5KH";
    const earlierSeq = "01ARYZ6S41TS5G7QFC0V44N5KH"; // older
    const laterSeq = "01BX5ZZKBKACTAV9WEVGEMMVRZ"; // newer
    const makeAppend = (seq: string, author: string): ChangeEvent => ({
      seq,
      owner,
      authoring_personality_instance_id: author,
      kind: {
        EntityAppend: {
          entity_kind: "Fact",
          entity: { Memory: memId },
          schema_id: "schema-a",
          schema_version: 1,
          supersedes: null,
        },
      },
    });
    // Mirror the engine: event_history returns seq DESC (newest first).
    const historyEvents = [
      makeAppend(laterSeq, "personality-late"),
      makeAppend(earlierSeq, "personality-early"),
    ];
    const { store } = createTestHarness(historyEvents);
    await vi.waitFor(() => expect(store.state().streamStatus).toBe("live"));
    const prov = store.state().memoryProvenance.get(memId);
    expect(prov?.creating_seq).toBe(earlierSeq);
    expect(prov?.authoring_personality_instance_id).toBe("personality-early");
  });

  it("normalises absent authoring_personality_instance_id to null", async () => {
    const { store, pushEvent } = createTestHarness();
    await vi.waitFor(() => expect(store.state().streamStatus).toBe("live"));
    const memId = "01ARYZ6S41TS5G7QFC0V44N5KH";
    pushEvent({
      seq: memId,
      owner,
      // authoring_personality_instance_id intentionally omitted (external ingestion)
      kind: {
        EntityAppend: {
          entity_kind: "Fact",
          entity: { Memory: memId },
          schema_id: "schema-a",
          schema_version: 1,
          supersedes: null,
        },
      },
    });
    const prov = store.state().memoryProvenance.get(memId);
    expect(prov?.authoring_personality_instance_id).toBe(null);
  });
});
