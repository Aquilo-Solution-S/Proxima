import type {
  ChangeEvent,
  EventDraft,
  EventHistoryRequest,
  EventHistoryResponse,
  EventIngestOutcome,
  GoalDraft,
  GoalWriteOutcome,
  InstantiatePersonalityOutcomeTs,
  InstantiatePersonalityTs,
  ListPersonalityInstancesTs,
  PersonalityInstanceTs,
  QueryRequest,
  QueryResponse,
  SchemaResponse,
  SetWakeConfigOutcomeTs,
  SetWakeConfigTs,
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
  provisionOwner(owner: QueryRequest["owner"]): Promise<void>;
  listPersonalityInstances(
    req: ListPersonalityInstancesTs,
  ): Promise<PersonalityInstanceTs[]>;
  instantiatePersonality(
    req: InstantiatePersonalityTs,
  ): Promise<InstantiatePersonalityOutcomeTs>;
  setWakeConfig(req: SetWakeConfigTs): Promise<SetWakeConfigOutcomeTs>;
}
