import { Channel } from "@tauri-apps/api/core";
import {
  commands,
  type ChangeEvent,
  type EventDraft,
  type EventHistoryRequest,
  type EventHistoryResponse,
  type EventIngestOutcome,
  type GoalDraft,
  type GoalWriteOutcome,
  type QueryRequest,
  type QueryResponse,
  type SchemaResponse,
  type SubscribeRequest,
} from "./bindings";
import type { EngineClient, Subscription } from "./client";

const unwrap = async <T, E>(
  result: Promise<{ status: "ok"; data: T } | { status: "error"; error: E }>,
): Promise<T> => {
  const r = await result;
  if (r.status === "error") throw r.error;
  return r.data;
};

export class TauriEngineClient implements EngineClient {
  async schema(): Promise<SchemaResponse> {
    return unwrap(commands.schema());
  }

  async query(req: QueryRequest): Promise<QueryResponse> {
    return unwrap(commands.query(req));
  }

  async eventHistory(req: EventHistoryRequest): Promise<EventHistoryResponse> {
    return unwrap(commands.eventHistory(req));
  }

  async subscribe(
    req: SubscribeRequest,
    onEvent: (event: ChangeEvent) => void,
  ): Promise<Subscription> {
    let active = true;
    const channel = new Channel<ChangeEvent>();
    channel.onmessage = (event) => {
      if (active) onEvent(event);
    };
    await unwrap(commands.subscribe(req, channel));
    return {
      unsubscribe() {
        active = false;
      },
    };
  }

  async goalWrite(draft: GoalDraft): Promise<GoalWriteOutcome> {
    return unwrap(commands.goalWrite(draft));
  }

  async eventIngest(draft: EventDraft): Promise<EventIngestOutcome> {
    return unwrap(commands.eventIngest(draft));
  }
}

export const createTauriEngineClient = (): EngineClient =>
  new TauriEngineClient();
