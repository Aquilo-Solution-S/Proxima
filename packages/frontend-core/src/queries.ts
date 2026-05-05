import { createResource, createSignal, type Accessor, type Resource } from "solid-js";
import { Channel } from "@tauri-apps/api/core";
import {
  commands,
  type ChangeEvent,
  type QueryRequest,
  type QueryResponse,
  type SubscribeRequest,
} from "./bindings";

export function useQuery(
  req: Accessor<QueryRequest>,
): Resource<QueryResponse> {
  const [data] = createResource(req, async (r) => {
    const result = await commands.query(r);
    if (result.status === "error") {
      throw result.error;
    }
    return result.data;
  });
  return data;
}

export function useSubscribe(req: SubscribeRequest): Accessor<ChangeEvent[]> {
  const [events, setEvents] = createSignal<ChangeEvent[]>([]);
  const channel = new Channel<ChangeEvent>();
  channel.onmessage = (event) => {
    setEvents((prev) => [...prev, event]);
  };

  void commands.subscribe(req, channel).then((result) => {
    if (result.status === "error") {
      console.error("subscribe failed:", result.error);
    }
  });

  return events;
}
