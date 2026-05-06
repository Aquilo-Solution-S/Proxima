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

// Optional dev-only hook installed by proxima-shell's perf module.
// When set, query/eventHistory responses are wrapped in a Proxy that
// records which fields the FE actually reads. No-op in production.
type FieldsHook = <T>(cmd: string, value: T) => T;
const fieldsHook = (): FieldsHook =>
  (globalThis as unknown as { __proximaRecordFields?: FieldsHook })
    .__proximaRecordFields ?? ((_, v) => v);

export class TauriEngineClient implements EngineClient {
  async schema(): Promise<SchemaResponse> {
    return unwrap(commands.schema());
  }

  async query(req: QueryRequest): Promise<QueryResponse> {
    return fieldsHook()("query", await unwrap(commands.query(req)));
  }

  async eventHistory(req: EventHistoryRequest): Promise<EventHistoryResponse> {
    return fieldsHook()("event_history", await unwrap(commands.eventHistory(req)));
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
