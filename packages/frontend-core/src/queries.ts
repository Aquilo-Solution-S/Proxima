import { createResource, createSignal, type Accessor, type Resource } from "solid-js";
import {
  type ChangeEvent,
  type QueryRequest,
  type QueryResponse,
  type SubscribeRequest,
} from "./bindings";
import type { EngineClient } from "./client";
import { createTauriEngineClient } from "./tauri-client";

export function useQuery(
  req: Accessor<QueryRequest>,
  client: EngineClient = createTauriEngineClient(),
): Resource<QueryResponse> {
  const [data] = createResource(req, async (r) => {
    return client.query(r);
  });
  return data;
}

export function useSubscribe(
  req: SubscribeRequest,
  client: EngineClient = createTauriEngineClient(),
): Accessor<ChangeEvent[]> {
  const [events, setEvents] = createSignal<ChangeEvent[]>([]);
  void client.subscribe(req, (event) => {
    setEvents((prev) => [...prev, event]);
  }).catch((error: unknown) => {
    console.error("subscribe failed:", error);
  });

  return events;
}
