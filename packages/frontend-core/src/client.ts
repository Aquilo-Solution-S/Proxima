import type {
  ChangeEvent,
  EventDraft,
  EventHistoryRequest,
  EventHistoryResponse,
  EventIngestOutcome,
  GoalDraft,
  GoalWriteOutcome,
  QueryRequest,
  QueryResponse,
  SchemaResponse,
  SubscribeRequest,
} from "./bindings";

export interface Subscription {
  unsubscribe(): void;
}

export interface EngineClient {
  schema(): Promise<SchemaResponse>;
  query(req: QueryRequest): Promise<QueryResponse>;
  eventHistory(req: EventHistoryRequest): Promise<EventHistoryResponse>;
  subscribe(
    req: SubscribeRequest,
    onEvent: (event: ChangeEvent) => void,
  ): Promise<Subscription>;
  goalWrite(draft: GoalDraft): Promise<GoalWriteOutcome>;
  eventIngest(draft: EventDraft): Promise<EventIngestOutcome>;
}
