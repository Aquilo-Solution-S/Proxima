import type {
  ChangeEvent,
  EventDraft,
  EventHistoryRequest,
  EventHistoryResponse,
  EventIngestOutcome,
  GoalDraft,
  GoalReactivateTs,
  GoalWriteOutcome,
  InstantiatePersonalityOutcomeTs,
  InstantiatePersonalityTs,
  BindInferenceTierTs,
  CodexAuthStatusOutcomeTs,
  InferenceEnvStatusOutcomeTs,
  InferenceEnvStatusTs,
  InferenceTargetTs,
  InferenceTierBindingTs,
  ListInferenceTargetsTs,
  ListInferenceTierBindingsTs,
  ListPersonalityInstancesTs,
  McpToolTs,
  ProducesTs,
  PersonalityInstanceTs,
  QueryRequest,
  QueryResponse,
  RegisterInferenceTargetOutcomeTs,
  RegisterInferenceTargetTs,
  RelationTs,
  RemoveInferenceTargetOutcomeTs,
  RemoveInferenceTargetTs,
  SchemaResponse,
  SetWakeEntriesOutcomeTs,
  SetWakeEntriesTs,
  SubscribeRequest,
  TestInferenceTargetOutcomeTs,
  TestInferenceTargetTs,
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
  goalReactivate(req: GoalReactivateTs): Promise<EventIngestOutcome>;
  eventIngest(draft: EventDraft): Promise<EventIngestOutcome>;
  listPersonalityInstances(
    req: ListPersonalityInstancesTs,
  ): Promise<PersonalityInstanceTs[]>;
  instantiatePersonality(
    req: InstantiatePersonalityTs,
  ): Promise<InstantiatePersonalityOutcomeTs>;
  setWakeEntries(req: SetWakeEntriesTs): Promise<SetWakeEntriesOutcomeTs>;
  registerInferenceTarget(
    req: RegisterInferenceTargetTs,
  ): Promise<RegisterInferenceTargetOutcomeTs>;
  listInferenceTargets(
    req: ListInferenceTargetsTs,
  ): Promise<InferenceTargetTs[]>;
  removeInferenceTarget(
    req: RemoveInferenceTargetTs,
  ): Promise<RemoveInferenceTargetOutcomeTs>;
  bindInferenceTier(req: BindInferenceTierTs): Promise<void>;
  listInferenceTierBindings(
    req: ListInferenceTierBindingsTs,
  ): Promise<InferenceTierBindingTs[]>;
  inferenceEnvStatus(
    req: InferenceEnvStatusTs,
  ): Promise<InferenceEnvStatusOutcomeTs>;
  codexAuthStatus(): Promise<CodexAuthStatusOutcomeTs>;
  testInferenceTarget(
    req: TestInferenceTargetTs,
  ): Promise<TestInferenceTargetOutcomeTs>;
  listMcpTools(): Promise<McpToolTs[]>;
  listRelations(): Promise<RelationTs[]>;
  wakeEntryProduces(substratePalette: string[]): Promise<ProducesTs>;
}
