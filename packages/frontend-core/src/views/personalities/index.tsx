import "./personalities.css";

import {
  Show,
  createEffect,
  createMemo,
  createResource,
  createSignal,
  type Component,
} from "solid-js";
import {
  commands,
  type BundledRecipeTs,
  type InstantiatePersonalityOutcomeTs,
  type InstantiatePersonalityTs,
  type ListWakeInvocationsTs,
  type ListOwnerRecipesTs,
  type ListPersonalityInstancesTs,
  type McpToolTs,
  type Owner,
  type OwnerRecipesListingTs,
  type PersonalityInstanceTs,
  type ProtocolError,
  type SetWakeEntriesOutcomeTs,
  type SetWakeEntriesTs,
  type TombstonePersonalityOutcomeTs,
  type TombstonePersonalityTs,
  type WakeEntryDraftTs,
  type WakeInvocationTs,
  type WorkspaceToolTs,
} from "../../bindings";
import { sentinelOwner } from "../../graph-store";
import { PersonalityCanvas } from "./canvas";
import { CreatePersonalityDialog } from "./create-dialog";
import { Inspector } from "./inspector";
import { computeLayout } from "./layout";
import { emptyDraft, type CanvasModel, type PersonalitySelection } from "./types";

type CommandResult<T> = Promise<
  { status: "ok"; data: T } | { status: "error"; error: ProtocolError }
>;

export type PersonalityCommandClient = {
  listPersonalityInstances: (
    req: ListPersonalityInstancesTs,
  ) => CommandResult<PersonalityInstanceTs[]>;
  instantiatePersonality: (
    req: InstantiatePersonalityTs,
  ) => CommandResult<InstantiatePersonalityOutcomeTs>;
  setWakeEntries: (
    req: SetWakeEntriesTs,
  ) => CommandResult<SetWakeEntriesOutcomeTs>;
  tombstonePersonality: (
    req: TombstonePersonalityTs,
  ) => CommandResult<TombstonePersonalityOutcomeTs>;
  listOwnerRecipes: (
    req: ListOwnerRecipesTs,
  ) => CommandResult<OwnerRecipesListingTs>;
  listBundledRecipes: () => CommandResult<BundledRecipeTs[]>;
  listMcpTools: () => CommandResult<McpToolTs[]>;
  listWorkspaceTools: () => CommandResult<WorkspaceToolTs[]>;
  listWakeInvocations: (
    req: ListWakeInvocationsTs,
  ) => CommandResult<WakeInvocationTs[]>;
};

const entryToDraft = (
  entry: PersonalityInstanceTs["wake_entries"][number],
): WakeEntryDraftTs => ({
  trigger_kind: entry.trigger_kind,
  trigger_id: entry.trigger_id,
  label: entry.label,
  enabled: entry.enabled,
  execution_mode: entry.execution_mode,
  authored_by: entry.authored_by,
  probability_promille: entry.probability_promille,
  recipe_ref: entry.recipe_ref,
  model_tier: entry.model_tier,
  inference_target_ref: entry.inference_target_ref,
  substrate_tool_palette: [...entry.substrate_tool_palette],
  workspace_tool_palette: [...entry.workspace_tool_palette],
  max_rounds: entry.max_rounds,
});

const cloneDraft = (draft: WakeEntryDraftTs): WakeEntryDraftTs => ({
  ...draft,
  substrate_tool_palette: [...draft.substrate_tool_palette],
  workspace_tool_palette: [...draft.workspace_tool_palette],
});

export const PersonalitiesView: Component<{
  client?: PersonalityCommandClient;
  owner?: Owner;
  revealRecipesFolder?: (path: string) => void;
}> = (props) => {
  const owner = props.owner ?? sentinelOwner();
  const client = props.client ?? commands;
  const revealRecipesFolder =
    props.revealRecipesFolder ?? defaultRevealRecipesFolder;

  const [instances, setInstances] = createSignal<PersonalityInstanceTs[]>([]);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [creating, setCreating] = createSignal(false);
  const [selection, setSelection] = createSignal<PersonalitySelection>(null);
  const [drafts, setDrafts] = createSignal<Map<string, WakeEntryDraftTs[]>>(
    new Map(),
  );
  const [saving, setSaving] = createSignal(false);
  const [tombstoning, setTombstoning] = createSignal<string | null>(null);
  const [confirmingTombstone, setConfirmingTombstone] = createSignal<
    string | null
  >(null);
  const [recipesListing, setRecipesListing] =
    createSignal<OwnerRecipesListingTs | null>(null);
  const [bundledRecipes, setBundledRecipes] = createSignal<
    BundledRecipeTs[] | null
  >(null);
  const [recipesError, setRecipesError] = createSignal<string | null>(null);
  const [mcpTools, setMcpTools] = createSignal<McpToolTs[] | null>(null);
  const [workspaceTools, setWorkspaceTools] = createSignal<
    WorkspaceToolTs[] | null
  >(null);
  const [toolsError, setToolsError] = createSignal<string | null>(null);
  const [wakeInvocations, setWakeInvocations] = createSignal<
    WakeInvocationTs[] | null
  >(null);
  const [wakeInvocationsLoading, setWakeInvocationsLoading] = createSignal(false);
  const [wakeInvocationsError, setWakeInvocationsError] =
    createSignal<string | null>(null);
  let wakeInvocationRequestSeq = 0;

  const refreshRecipes = async () => {
    setRecipesError(null);
    try {
      const [listing, bundled] = await Promise.all([
        unwrap(client.listOwnerRecipes({ owner })),
        unwrap(client.listBundledRecipes()),
      ]);
      setRecipesListing(listing);
      setBundledRecipes(bundled);
    } catch (err) {
      setRecipesError(errorMessage(err));
    }
  };

  const refreshTools = async () => {
    setToolsError(null);
    try {
      const [substrate, workspace] = await Promise.all([
        unwrap(client.listMcpTools()),
        unwrap(client.listWorkspaceTools()),
      ]);
      setMcpTools(substrate);
      setWorkspaceTools(workspace);
    } catch (err) {
      setToolsError(errorMessage(err));
    }
  };

  const selectedInvocationScope = (sel: PersonalitySelection) => {
    if (!sel) return null;
    if (sel.kind === "personality") {
      return {
        personalityInstanceId: sel.instance_id,
        wakeEntryId: null as string | null,
      };
    }
    if (sel.kind === "wake_entry") {
      const instance = instances().find(
        (row) => row.personality_instance_id === sel.instance_id,
      );
      return {
        personalityInstanceId: sel.instance_id,
        wakeEntryId:
          instance?.wake_entries[sel.entry_index]?.wake_entry_id ?? null,
      };
    }
    if (sel.kind === "edge") {
      return {
        personalityInstanceId: sel.tgt_instance_id,
        wakeEntryId:
          instances().find(
            (row) => row.personality_instance_id === sel.tgt_instance_id,
          )?.wake_entries[sel.tgt_entry_index]?.wake_entry_id ?? null,
      };
    }
    return null;
  };

  const refreshWakeInvocations = async (scope: ReturnType<typeof selectedInvocationScope>) => {
    const seq = ++wakeInvocationRequestSeq;
    if (!scope) {
      setWakeInvocations(null);
      setWakeInvocationsError(null);
      setWakeInvocationsLoading(false);
      return;
    }
    setWakeInvocationsLoading(true);
    setWakeInvocationsError(null);
    try {
      const rows = await unwrap(
        client.listWakeInvocations({
          owner,
          personality_instance_id: scope.personalityInstanceId,
          wake_entry_id: scope.wakeEntryId,
          limit: 20,
        }),
      );
      if (seq === wakeInvocationRequestSeq) setWakeInvocations(rows);
    } catch (err) {
      if (seq === wakeInvocationRequestSeq) {
        setWakeInvocations([]);
        setWakeInvocationsError(errorMessage(err));
      }
    } finally {
      if (seq === wakeInvocationRequestSeq) setWakeInvocationsLoading(false);
    }
  };

  const refresh = async () => {
    setLoading(true);
    setError(null);
    try {
      const rows = await unwrap(
        client.listPersonalityInstances({
          owner,
          include_tombstoned: false,
        }),
      );
      setInstances(rows);
      setDrafts(new Map());
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  };

  createEffect(() => {
    void refresh();
    void refreshRecipes();
    void refreshTools();
  });

  createEffect(() => {
    void refreshWakeInvocations(selectedInvocationScope(selection()));
  });

  const dirty = createMemo(() => drafts().size > 0);

  const seedDrafts = (instanceId: string): WakeEntryDraftTs[] => {
    const instance = instances().find(
      (row) => row.personality_instance_id === instanceId,
    );
    if (!instance) return [];
    if (instance.status === "needs_repair") return [];
    return instance.wake_entries.map(entryToDraft);
  };

  const mutateDrafts = (
    instanceId: string,
    transform: (entries: WakeEntryDraftTs[]) => WakeEntryDraftTs[],
  ) => {
    setDrafts((prev) => {
      const next = new Map(prev);
      const existing = next.get(instanceId) ?? seedDrafts(instanceId);
      next.set(instanceId, transform(existing));
      return next;
    });
  };

  const updateEntry = (
    instanceId: string,
    index: number,
    mutate: (draft: WakeEntryDraftTs) => void,
  ) => {
    mutateDrafts(instanceId, (entries) =>
      entries.map((draft, i) => {
        if (i !== index) return draft;
        const copy = cloneDraft(draft);
        mutate(copy);
        return copy;
      }),
    );
  };

  const addEntry = (instanceId: string) => {
    let nextIndex = 0;
    mutateDrafts(instanceId, (entries) => {
      nextIndex = entries.length;
      return [...entries, emptyDraft("")];
    });
    setSelection({
      kind: "wake_entry",
      instance_id: instanceId,
      entry_index: nextIndex,
    });
  };

  const removeEntry = (instanceId: string, index: number) => {
    mutateDrafts(instanceId, (entries) =>
      entries.filter((_, i) => i !== index),
    );
    setSelection({ kind: "personality", instance_id: instanceId });
  };

  const cancelDrafts = () => {
    setDrafts(new Map());
    const sel = selection();
    if (sel?.kind === "wake_entry") {
      setSelection({ kind: "personality", instance_id: sel.instance_id });
    }
  };

  const saveAll = async () => {
    setSaving(true);
    setError(null);
    try {
      for (const [instanceId, entries] of drafts()) {
        await unwrap(
          client.setWakeEntries({
            owner,
            personality_instance_id: instanceId,
            entries,
          }),
        );
      }
      await refresh();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setSaving(false);
    }
  };

  const tombstoneInstance = async (instance: PersonalityInstanceTs) => {
    setTombstoning(instance.personality_instance_id);
    setError(null);
    try {
      await unwrap(
        client.tombstonePersonality({
          owner: instance.owner,
          personality_instance_id: instance.personality_instance_id,
        }),
      );
      setInstances((prev) =>
        prev.filter(
          (row) =>
            row.personality_instance_id !== instance.personality_instance_id,
        ),
      );
      const map = new Map(drafts());
      map.delete(instance.personality_instance_id);
      setDrafts(map);
      if (
        selection()?.kind === "personality" ||
        selection()?.kind === "wake_entry"
      ) {
        const sel = selection();
        if (
          sel &&
          (sel.kind === "personality" || sel.kind === "wake_entry") &&
          sel.instance_id === instance.personality_instance_id
        ) {
          setSelection(null);
        }
      }
      setConfirmingTombstone(null);
      void refresh();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setTombstoning(null);
    }
  };

  const createPersonality = async (displayName: string, purpose: string) => {
    setLoading(true);
    setError(null);
    try {
      await unwrap(
        client.instantiatePersonality({
          owner,
          display_name: displayName,
          purpose,
        }),
      );
      setCreating(false);
      await refresh();
    } catch (err) {
      setError(errorMessage(err));
      setLoading(false);
    }
  };

  const projectedInstances = createMemo<PersonalityInstanceTs[]>(() =>
    instances().map((instance) => {
      const draftEntries = drafts().get(instance.personality_instance_id);
      if (draftEntries) {
        return {
          ...instance,
          wake_entries: draftEntries.map((draft, idx) => ({
            ...(instance.wake_entries[idx] ?? {
              wake_entry_id: `pending-${idx}`,
              disabled_reason: null,
            }),
            ...draft,
          })),
        };
      }
      if (instance.status === "needs_repair") {
        return { ...instance, wake_entries: [] };
      }
      return instance;
    }),
  );

  const [layoutResource] = createResource(
    projectedInstances,
    async (sourceInstances) => computeLayout({ instances: sourceInstances }),
  );

  return (
    <section class="personality-view personality-view-graph">
      <div class="personality-toolbar">
        <div>
          <h1>Personalities</h1>
        </div>
        <div class="personality-actions">
          <button
            type="button"
            class="hub-nav-item"
            disabled={loading()}
            onClick={() => setCreating(true)}
          >
            Create new Personality
          </button>
          <button
            type="button"
            class="hub-nav-item"
            onClick={() => void refresh()}
          >
            Refresh
          </button>
        </div>
      </div>

      <div class="personality-graph-shell" aria-busy={loading()}>
        <PersonalityCanvas
          model={() => layoutResource() as CanvasModel | undefined}
          selection={selection()}
          drafts={drafts()}
          onSelect={setSelection}
        />
        <Inspector
          selection={selection()}
          instances={projectedInstances()}
          drafts={drafts()}
          dirty={dirty()}
          saving={saving()}
          error={creating() ? null : error()}
          recipes={recipesListing()}
          bundledRecipes={bundledRecipes()}
          recipesError={recipesError()}
          onRefreshRecipes={() => void refreshRecipes()}
          onRevealRecipesFolder={revealRecipesFolder}
          mcpTools={mcpTools()}
          workspaceTools={workspaceTools()}
          toolsError={toolsError()}
          wakeInvocations={wakeInvocations()}
          wakeInvocationsLoading={wakeInvocationsLoading()}
          wakeInvocationsError={wakeInvocationsError()}
          onUpdateEntry={updateEntry}
          onAddEntry={addEntry}
          onRemoveEntry={removeEntry}
          onSelectEntry={(instance_id, entry_index) =>
            setSelection({ kind: "wake_entry", instance_id, entry_index })
          }
          onSave={() => void saveAll()}
          onCancel={cancelDrafts}
          onTombstone={(instance) =>
            setConfirmingTombstone(instance.personality_instance_id)
          }
          tombstoning={tombstoning()}
          confirmingTombstone={confirmingTombstone()}
          onConfirmTombstone={(instance_id) => {
            const instance = instances().find(
              (row) => row.personality_instance_id === instance_id,
            );
            if (instance) void tombstoneInstance(instance);
          }}
          onCancelTombstone={() => setConfirmingTombstone(null)}
        />
      </div>

      <Show when={instances().length === 0 && !loading()}>
        <p class="personality-empty">No personalities configured.</p>
      </Show>

      <Show when={creating()}>
        <CreatePersonalityDialog
          busy={loading()}
          error={error}
          onClose={() => {
            setError(null);
            setCreating(false);
          }}
          onCreate={(displayName, purpose) =>
            void createPersonality(displayName, purpose)
          }
        />
      </Show>
    </section>
  );
};

export const EngineerInstancesPanel = PersonalitiesView;

const unwrap = async <T, E>(
  result: Promise<{ status: "ok"; data: T } | { status: "error"; error: E }>,
): Promise<T> => {
  const value = await result;
  if (value.status === "error") throw value.error;
  return value.data;
};

const defaultRevealRecipesFolder = (path: string): void => {
  if (!path) return;
  void import("@tauri-apps/plugin-opener")
    .then((mod) => mod.revealItemInDir(path))
    .catch((err) => {
      // Fallback for non-Tauri / dev: log so devs can still see the path.
      console.warn("revealItemInDir failed", err);
    });
};

const errorMessage = (err: unknown): string => {
  if (err && typeof err === "object") {
    if ("code" in err && "message" in err) {
      const code = (err as { code: unknown }).code;
      const message = (err as { message: unknown }).message;
      if (typeof code === "string" && typeof message === "string") {
        return `${code}: ${message}`;
      }
    }
    if ("message" in err) {
      const message = (err as { message: unknown }).message;
      if (typeof message === "string") return message;
    }
  }
  return String(err);
};
