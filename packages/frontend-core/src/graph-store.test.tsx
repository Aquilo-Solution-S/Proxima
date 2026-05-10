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
  seq: "019dfa00-0000-7000-8000-000000000030",
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
