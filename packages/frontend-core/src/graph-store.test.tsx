import { createRoot } from "solid-js";
import { describe, expect, it, vi } from "vitest";
import type { EventDraft, GoalDraft, QueryRequest } from "./bindings";
import type { EngineClient, Subscription } from "./client";
import {
  createGraphStore,
  GRAPH_SNAPSHOT_LIMIT,
  MAX_SNAPSHOT_EDGES,
} from "./graph-store";
import { createHub } from "./hub";

describe("GraphStore snapshot loading", () => {
  it("requests the 5000-node snapshot window and exposes the edge cap", async () => {
    const queries: QueryRequest[] = [];
    const client: EngineClient = {
      schema: async () => ({ schemas: [] }),
      query: async (req) => {
        queries.push(req);
        return { memories: [], goals: [], edges: [], seq_high_water: null };
      },
      eventHistory: async () => ({ events: [], seq_high_water: null }),
      subscribe: async (): Promise<Subscription> => ({ unsubscribe() {} }),
      goalWrite: async (_draft: GoalDraft) => {
        throw new Error("goalWrite not used");
      },
      eventIngest: async (_draft: EventDraft) => {
        throw new Error("eventIngest not used");
      },
      provisionOwner: async () => {
        throw new Error("provisionOwner not used");
      },
      listPersonalityInstances: async () => {
        throw new Error("listPersonalityInstances not used");
      },
      instantiatePersonality: async () => {
        throw new Error("instantiatePersonality not used");
      },
      setWakeConfig: async () => {
        throw new Error("setWakeConfig not used");
      },
    };

    await new Promise<void>((resolve) => {
      createRoot((dispose) => {
        createGraphStore(client, createHub([]));
        void vi
          .waitFor(() => expect(queries[0]?.limit).toBe(5_000))
          .then(() => {
            expect(GRAPH_SNAPSHOT_LIMIT).toBe(5_000);
            expect(MAX_SNAPSHOT_EDGES).toBe(50_000);
            dispose();
            resolve();
          });
      });
    });
  });
});
