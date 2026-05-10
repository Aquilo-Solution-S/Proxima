import { Channel } from "@tauri-apps/api/core";
import {
  commands,
  type ChangeEvent,
  type EventDraft,
  type EventHistoryRequest,
  type EventHistoryResponse,
  type EventIngestOutcome,
  type GoalDraft,
  type GoalReactivateTs,
  type BundledRecipeTs,
  type GoalWriteOutcome,
  type InstantiatePersonalityOutcomeTs,
  type InstantiatePersonalityTs,
  type BindInferenceTierTs,
  type DetectedHarnessTs,
  type InferenceTargetTs,
  type InferenceTierBindingTs,
  type ListInferenceTargetsTs,
  type ListInferenceTierBindingsTs,
  type ListOwnerRecipesTs,
  type ListPersonalityInstancesTs,
  type McpToolTs,
  type OwnerRecipesListingTs,
  type ProducesTs,
  type PersonalityInstanceTs,
  type QueryRequest,
  type QueryResponse,
  type RegisterInferenceTargetOutcomeTs,
  type RegisterInferenceTargetTs,
  type RelationTs,
  type RemoveInferenceTargetOutcomeTs,
  type RemoveInferenceTargetTs,
  type SchemaResponse,
  type SetWakeEntriesOutcomeTs,
  type SetWakeEntriesTs,
  type SubscribeRequest,
  type WorkspaceToolTs,
} from "./bindings";
import type { EngineClient, Subscription } from "./client";

const unwrap = async <T, E>(
  result: Promise<{ status: "ok"; data: T } | { status: "error"; error: E }>,
  cmd: string = "tauri-command",
): Promise<T> => {
  const r = await result;
  if (r.status === "error") {
    const raw = r.error as unknown;
    const message =
      typeof raw === "object" && raw !== null && "message" in raw
        ? String((raw as { message: unknown }).message)
        : typeof raw === "string"
          ? raw
          : `${cmd} failed`;
    console.error(`tauri ${cmd} error:`, raw);
    const err = new Error(`${cmd}: ${message}`);
    (err as Error & { cause?: unknown }).cause = raw;
    throw err;
  }
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
    return unwrap(commands.schema(), "schema");
  }

  async query(req: QueryRequest): Promise<QueryResponse> {
    return fieldsHook()("query", await unwrap(commands.query(req), "query"));
  }

  async eventHistory(req: EventHistoryRequest): Promise<EventHistoryResponse> {
    return fieldsHook()(
      "event_history",
      await unwrap(commands.eventHistory(req), "event_history"),
    );
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
    await unwrap(commands.subscribe(req, channel), "subscribe");
    return {
      unsubscribe() {
        active = false;
      },
    };
  }

  async goalWrite(draft: GoalDraft): Promise<GoalWriteOutcome> {
    return unwrap(commands.goalWrite(draft), "goal_write");
  }

  async goalReactivate(req: GoalReactivateTs): Promise<EventIngestOutcome> {
    return unwrap(commands.goalReactivate(req), "goal_reactivate");
  }

  async eventIngest(draft: EventDraft): Promise<EventIngestOutcome> {
    return unwrap(commands.eventIngest(draft), "event_ingest");
  }

  async listPersonalityInstances(
    req: ListPersonalityInstancesTs,
  ): Promise<PersonalityInstanceTs[]> {
    return unwrap(
      commands.listPersonalityInstances(req),
      "list_personality_instances",
    );
  }

  async instantiatePersonality(
    req: InstantiatePersonalityTs,
  ): Promise<InstantiatePersonalityOutcomeTs> {
    return unwrap(commands.instantiatePersonality(req), "instantiate_personality");
  }

  async setWakeEntries(req: SetWakeEntriesTs): Promise<SetWakeEntriesOutcomeTs> {
    return unwrap(commands.setWakeEntries(req), "set_wake_entries");
  }

  async registerInferenceTarget(
    req: RegisterInferenceTargetTs,
  ): Promise<RegisterInferenceTargetOutcomeTs> {
    return unwrap(
      commands.registerInferenceTarget(req),
      "register_inference_target",
    );
  }

  async listInferenceTargets(
    req: ListInferenceTargetsTs,
  ): Promise<InferenceTargetTs[]> {
    return unwrap(commands.listInferenceTargets(req), "list_inference_targets");
  }

  async removeInferenceTarget(
    req: RemoveInferenceTargetTs,
  ): Promise<RemoveInferenceTargetOutcomeTs> {
    return unwrap(
      commands.removeInferenceTarget(req),
      "remove_inference_target",
    );
  }

  async bindInferenceTier(req: BindInferenceTierTs): Promise<void> {
    await unwrap(commands.bindInferenceTier(req), "bind_inference_tier");
  }

  async listInferenceTierBindings(
    req: ListInferenceTierBindingsTs,
  ): Promise<InferenceTierBindingTs[]> {
    return unwrap(
      commands.listInferenceTierBindings(req),
      "list_inference_tier_bindings",
    );
  }

  async detectLocalHarness(name: string): Promise<DetectedHarnessTs | null> {
    return unwrap(commands.detectLocalHarness(name), "detect_local_harness");
  }

  async listOwnerRecipes(
    req: ListOwnerRecipesTs,
  ): Promise<OwnerRecipesListingTs> {
    return unwrap(commands.listOwnerRecipes(req), "list_owner_recipes");
  }

  async listBundledRecipes(): Promise<BundledRecipeTs[]> {
    return unwrap(commands.listBundledRecipes(), "list_bundled_recipes");
  }

  async listMcpTools(): Promise<McpToolTs[]> {
    return unwrap(commands.listMcpTools(), "list_mcp_tools");
  }

  async listWorkspaceTools(): Promise<WorkspaceToolTs[]> {
    return unwrap(commands.listWorkspaceTools(), "list_workspace_tools");
  }

  async listRelations(): Promise<RelationTs[]> {
    return unwrap(commands.listRelations(), "list_relations");
  }

  async wakeEntryProduces(substratePalette: string[]): Promise<ProducesTs> {
    return unwrap(
      commands.wakeEntryProduces(substratePalette),
      "wake_entry_produces",
    );
  }
}

export const createTauriEngineClient = (): EngineClient =>
  new TauriEngineClient();
