import "./personalities.css";

import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  type Component,
} from "solid-js";
import {
  commands,
  type AuthoredByTs,
  type ExecutionModeTs,
  type InstantiatePersonalityOutcomeTs,
  type InstantiatePersonalityTs,
  type ListPersonalityInstancesTs,
  type ModelTierTs,
  type Owner,
  type PersonalityInstanceTs,
  type ProtocolError,
  type SetWakeEntriesOutcomeTs,
  type SetWakeEntriesTs,
  type TombstonePersonalityOutcomeTs,
  type TombstonePersonalityTs,
  type TriggerKindTs,
  type WakeEntryDraftTs,
  type WakeEntryTs,
} from "../bindings";
import { sentinelOwner } from "../graph-store";
import { Mono } from "../primitives";
import {
  registeredPayloadRenderers,
  registeredPersonalityTypes,
  type RegisteredPersonalityType,
} from "../registry";

type CommandResult<T> = Promise<
  { status: "ok"; data: T } | { status: "error"; error: ProtocolError }
>;

export type PersonalityCommandClient = {
  provisionOwner: (owner: Owner) => CommandResult<null>;
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
};

const TRIGGER_KINDS: TriggerKindTs[] = ["on_memory", "on_edge"];
const AUTHORED_BY: AuthoredByTs[] = ["any", "self_author", "other"];
const EXECUTION_MODES: ExecutionModeTs[] = ["substrate_only", "workspace"];
const MODEL_TIERS: ModelTierTs[] = ["fast", "standard", "deep"];

const registeredSchemaOptions = () => {
  const seen = new Set<string>();
  return registeredPayloadRenderers()
    .map((registration) => ({
      schemaId: registration.schemaId,
      flavor: registration.flavor,
    }))
    .filter((option) => {
      if (seen.has(option.schemaId)) return false;
      seen.add(option.schemaId);
      return true;
    })
    .sort((a, b) =>
      `${a.flavor}:${a.schemaId}`.localeCompare(`${b.flavor}:${b.schemaId}`),
    );
};

const schemaOptionsFor = (schemaId: string) => {
  const options = registeredSchemaOptions();
  if (options.some((option) => option.schemaId === schemaId)) return options;
  if (schemaId.trim() === "") return options;
  return [{ schemaId, flavor: "stored config" }, ...options];
};

const emptyDraft = (): WakeEntryDraftTs => ({
  trigger_kind: "on_memory",
  trigger_id: registeredSchemaOptions()[0]?.schemaId ?? "",
  label: "",
  enabled: true,
  execution_mode: "substrate_only",
  authored_by: "any",
  probability_promille: 1000,
  recipe_ref: "",
  model_tier: "standard",
  inference_target_ref: null,
  substrate_tool_palette: [],
  workspace_tool_palette: [],
  max_rounds: 4,
});

const entryToDraft = (entry: WakeEntryTs): WakeEntryDraftTs => ({
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

export const PersonalitiesView: Component<{
  client?: PersonalityCommandClient;
  owner?: Owner;
}> = (props) => {
  const owner = props.owner ?? sentinelOwner();
  const client = props.client ?? commands;
  const [instances, setInstances] = createSignal<PersonalityInstanceTs[]>([]);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [editing, setEditing] = createSignal<PersonalityInstanceTs | null>(null);
  const [creating, setCreating] = createSignal(false);
  const [confirmingTombstone, setConfirmingTombstone] = createSignal<string | null>(
    null,
  );
  const [tombstoning, setTombstoning] = createSignal<string | null>(null);
  const personalityTypes = registeredPersonalityTypes();

  const tombstoneInstance = async (instance: PersonalityInstanceTs) => {
    setTombstoning(instance.personality_instance_id);
    setError(null);
    try {
      await unwrap(
        client.tombstonePersonality({
          owner: instance.owner,
          personality_type_id: instance.personality_type_id,
          personality_instance_id: instance.personality_instance_id,
        }),
      );
      setInstances((prev) =>
        prev.filter(
          (row) => row.personality_instance_id !== instance.personality_instance_id,
        ),
      );
      setConfirmingTombstone(null);
      void refresh();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setTombstoning(null);
    }
  };

  const visibleInstances = createMemo(() =>
    [...instances()].sort((a, b) =>
      `${a.flavor?.display_name ?? ""}:${a.display_name}`.localeCompare(
        `${b.flavor?.display_name ?? ""}:${b.display_name}`,
      ),
    ),
  );

  const refresh = async () => {
    setLoading(true);
    setError(null);
    try {
      await unwrap(client.provisionOwner(owner));
      const rows = await unwrap(
        client.listPersonalityInstances({
          owner,
          personality_type_id: null,
          include_tombstoned: false,
        }),
      );
      setInstances(rows);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  };

  createEffect(() => {
    void refresh();
  });

  const createPersonality = async (
    type: RegisteredPersonalityType,
    displayName: string,
    purpose: string,
  ) => {
    setLoading(true);
    setError(null);
    try {
      await unwrap(
        client.instantiatePersonality({
          owner,
          personality_type_id: type.typeId,
          payload_overrides: JSON.stringify({
            display_name: displayName,
            purpose,
          }),
        }),
      );
      setCreating(false);
      await refresh();
    } catch (err) {
      setError(errorMessage(err));
      setLoading(false);
    }
  };

  return (
    <section class="personality-view">
      <div class="personality-toolbar">
        <div>
          <h1>Personalities</h1>
          <Show when={error()}>
            {(message) => <p class="proxima-error">{message()}</p>}
          </Show>
        </div>
        <div class="personality-actions">
          <button
            type="button"
            class="hub-nav-item"
            disabled={loading() || personalityTypes.length === 0}
            onClick={() => setCreating(true)}
          >
            Create new Personality
          </button>
          <button type="button" class="hub-nav-item" onClick={() => void refresh()}>
            Refresh
          </button>
        </div>
      </div>

      <div class="personality-grid" aria-busy={loading()}>
        <For each={visibleInstances()}>
          {(instance) => (
            <PersonalityCard
              instance={instance}
              onEdit={() => setEditing(instance)}
              confirming={
                confirmingTombstone() === instance.personality_instance_id
              }
              busy={tombstoning() === instance.personality_instance_id}
              onTombstone={() =>
                setConfirmingTombstone(instance.personality_instance_id)
              }
              onCancelTombstone={() => setConfirmingTombstone(null)}
              onConfirmTombstone={() => void tombstoneInstance(instance)}
            />
          )}
        </For>
        <Show when={visibleInstances().length === 0}>
          <p class="personality-empty">No personalities configured.</p>
        </Show>
      </div>

      <Show when={creating()}>
        <CreatePersonalityDialog
          types={personalityTypes}
          busy={loading()}
          onClose={() => setCreating(false)}
          onCreate={(type, displayName, purpose) =>
            void createPersonality(type, displayName, purpose)
          }
        />
      </Show>

      <Show when={editing()}>
        {(instance) => (
          <WakeEditor
            instance={instance()}
            onClose={() => setEditing(null)}
            onSaved={() => {
              setEditing(null);
              void refresh();
            }}
            client={client}
          />
        )}
      </Show>
    </section>
  );
};

export const EngineerInstancesPanel = PersonalitiesView;

const CreatePersonalityDialog: Component<{
  types: RegisteredPersonalityType[];
  busy: boolean;
  onClose: () => void;
  onCreate: (
    type: RegisteredPersonalityType,
    displayName: string,
    purpose: string,
  ) => void;
}> = (props) => {
  const [selectedTypeId, setSelectedTypeId] = createSignal(
    props.types[0]?.typeId ?? "",
  );
  const selectedType = createMemo(
    () => props.types.find((type) => type.typeId === selectedTypeId()) ?? null,
  );
  const [displayName, setDisplayName] = createSignal(
    props.types[0]?.defaultDisplayName ?? "",
  );
  const [purpose, setPurpose] = createSignal(props.types[0]?.defaultPurpose ?? "");

  const chooseType = (type: RegisteredPersonalityType) => {
    setSelectedTypeId(type.typeId);
    setDisplayName(type.defaultDisplayName);
    setPurpose(type.defaultPurpose);
  };

  const create = () => {
    const type = selectedType();
    if (type === null) return;
    props.onCreate(type, displayName().trim(), purpose().trim());
  };

  return (
    <div class="personality-dialog-backdrop" role="dialog" aria-modal="true">
      <div class="personality-dialog">
        <div class="personality-dialog-head">
          <div>
            <h2>Create new Personality</h2>
            <p>Choose a flavor-provided type, then set its instance details.</p>
          </div>
          <button type="button" class="hub-nav-item" onClick={props.onClose}>
            Close
          </button>
        </div>

        <div class="personality-dialog-body">
          <section class="personality-type-picker" aria-label="Personality type">
            <For each={props.types}>
              {(type) => (
                <button
                  type="button"
                  classList={{
                    "personality-type-option": true,
                    selected: selectedTypeId() === type.typeId,
                  }}
                  onClick={() => chooseType(type)}
                >
                  <span>{type.label}</span>
                  <small>{type.flavor}</small>
                  <em>{type.purpose}</em>
                </button>
              )}
            </For>
          </section>

          <section class="personality-detail-form">
            <label>
              Display name
              <input
                value={displayName()}
                onInput={(event) => setDisplayName(event.currentTarget.value)}
              />
            </label>
            <label>
              Purpose
              <textarea
                rows="4"
                value={purpose()}
                onInput={(event) => setPurpose(event.currentTarget.value)}
              />
            </label>
          </section>
        </div>

        <div class="personality-dialog-actions">
          <button
            type="button"
            class="hub-nav-item"
            disabled={props.busy || displayName().trim() === ""}
            onClick={create}
          >
            Create
          </button>
        </div>
      </div>
    </div>
  );
};

const PersonalityCard: Component<{
  instance: PersonalityInstanceTs;
  onEdit: () => void;
  confirming: boolean;
  busy: boolean;
  onTombstone: () => void;
  onCancelTombstone: () => void;
  onConfirmTombstone: () => void;
}> = (props) => {
  const statusClass = () => props.instance.status.replace(/-/g, "_");
  const truncatedName = () => truncateName(props.instance.display_name);
  return (
    <article class="personality-row" data-testid="personality-card">
      <div class="personality-row-head">
        <div class="personality-title">
          <strong>{props.instance.display_name}</strong>
          <div class="personality-meta">
            <span class="personality-flavor-chip" data-testid="personality-flavor-chip">
              {props.instance.flavor?.display_name ?? "Instance"}
            </span>
            <Mono>{shortId(props.instance.personality_instance_id)}</Mono>
          </div>
        </div>
        <span class={`personality-status ${statusClass()}`}>
          {props.instance.status}
        </span>
      </div>
      <div class="self-perspective-ref">
        <span>Root</span>
        <button
          type="button"
          class="self-perspective-button"
          title={props.instance.current_root_perspective_memory_id}
          onClick={() =>
            void copyText(props.instance.current_root_perspective_memory_id)
          }
        >
          <Mono>{shortId(props.instance.current_root_perspective_memory_id)}</Mono>
        </button>
      </div>
      <Show when={props.instance.status === "needs_repair"}>
        <div class="repair-banner">
          <span>
            Wake entries need repair - saved entries could not be loaded after a
            recent update.
          </span>
          <button type="button" class="hub-nav-item" onClick={props.onEdit}>
            Re-edit
          </button>
        </div>
      </Show>
      <ul class="wake-list">
        <For each={props.instance.wake_entries}>
          {(entry) => <li class="wake-item">{wakeSummary(entry)}</li>}
        </For>
      </ul>
      <div class="personality-card-actions">
        <button type="button" class="hub-nav-item" onClick={props.onEdit}>
          Edit wake entries
        </button>
        <button
          type="button"
          class="hub-nav-item danger"
          disabled={props.confirming || props.busy}
          onClick={props.onTombstone}
        >
          Tombstone
        </button>
      </div>
      <Show when={props.confirming}>
        <div class="tombstone-confirm" data-testid="tombstone-confirm">
          <span>{`Tombstone ${truncatedName()}? Wakes stop; memories remain.`}</span>
          <button
            type="button"
            class="hub-nav-item danger"
            disabled={props.busy}
            onClick={props.onConfirmTombstone}
          >
            Confirm tombstone
          </button>
          <button
            type="button"
            class="hub-nav-item"
            disabled={props.busy}
            onClick={props.onCancelTombstone}
          >
            Cancel
          </button>
        </div>
      </Show>
    </article>
  );
};

const WakeEditor: Component<{
  instance: PersonalityInstanceTs;
  onClose: () => void;
  onSaved: () => void;
  client: PersonalityCommandClient;
}> = (props) => {
  const [drafts, setDrafts] = createSignal<WakeEntryDraftTs[]>(
    props.instance.status === "needs_repair"
      ? []
      : props.instance.wake_entries.map(entryToDraft),
  );
  const [error, setError] = createSignal<string | null>(null);
  const [saving, setSaving] = createSignal(false);

  const updateDraft = (index: number, mutate: (draft: WakeEntryDraftTs) => void) => {
    setDrafts((prev) =>
      prev.map((draft, i) => {
        if (i !== index) return draft;
        const copy = cloneDraft(draft);
        mutate(copy);
        return copy;
      }),
    );
  };

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      await unwrap(
        props.client.setWakeEntries({
          owner: props.instance.owner,
          personality_type_id: props.instance.personality_type_id,
          personality_instance_id: props.instance.personality_instance_id,
          entries: drafts(),
        }),
      );
      props.onSaved();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div class="wake-editor-backdrop" role="dialog" aria-modal="true">
      <div class="wake-editor">
        <div class="wake-editor-head">
          <div>
            <h2>{props.instance.display_name}</h2>
            <Mono>{props.instance.personality_instance_id}</Mono>
            <Show when={props.instance.status === "needs_repair"}>
              <p class="proxima-error">wake entries need repair</p>
            </Show>
            <Show when={error()}>
              {(message) => <p class="proxima-error">{message()}</p>}
            </Show>
          </div>
          <button type="button" class="hub-nav-item" onClick={props.onClose}>
            Close
          </button>
        </div>

        <div class="wake-editor-list" data-testid="wake-entries-list">
          <For each={drafts()}>
            {(draft, index) => (
              <WakeEntryRow
                draft={draft}
                onUpdate={(mutate) => updateDraft(index(), mutate)}
                onRemove={() =>
                  setDrafts((prev) => prev.filter((_, i) => i !== index()))
                }
              />
            )}
          </For>
        </div>

        <div class="wake-editor-actions">
          <div class="personality-actions">
            <button
              type="button"
              class="hub-nav-item"
              onClick={() => setDrafts((prev) => [...prev, emptyDraft()])}
            >
              Add WakeEntry
            </button>
          </div>
          <button
            type="button"
            class="hub-nav-item"
            disabled={saving()}
            onClick={() => void save()}
          >
            Save
          </button>
        </div>
      </div>
    </div>
  );
};

const WakeEntryRow: Component<{
  draft: WakeEntryDraftTs;
  onUpdate: (mutate: (draft: WakeEntryDraftTs) => void) => void;
  onRemove: () => void;
}> = (props) => {
  const triggerKind = () => props.draft.trigger_kind;

  return (
    <div class="wake-editor-row">
      <label>
        Label
        <input
          value={props.draft.label}
          onInput={(event) =>
            props.onUpdate((draft) => {
              draft.label = event.currentTarget.value;
            })
          }
        />
      </label>
      <label>
        Enabled
        <input
          type="checkbox"
          checked={props.draft.enabled}
          onChange={(event) =>
            props.onUpdate((draft) => {
              draft.enabled = event.currentTarget.checked;
            })
          }
        />
      </label>
      <label>
        Trigger kind
        <select
          value={props.draft.trigger_kind}
          onChange={(event) =>
            props.onUpdate((draft) => {
              draft.trigger_kind = event.currentTarget.value as TriggerKindTs;
            })
          }
        >
          <For each={TRIGGER_KINDS}>
            {(kind) => <option value={kind}>{kind}</option>}
          </For>
        </select>
      </label>
      <label>
        Trigger id
        <Show
          when={triggerKind() === "on_memory"}
          fallback={
            <input
              value={props.draft.trigger_id}
              onInput={(event) =>
                props.onUpdate((draft) => {
                  draft.trigger_id = event.currentTarget.value;
                })
              }
            />
          }
        >
          <select
            value={props.draft.trigger_id}
            onChange={(event) =>
              props.onUpdate((draft) => {
                draft.trigger_id = event.currentTarget.value;
              })
            }
          >
            <For each={schemaOptionsFor(props.draft.trigger_id)}>
              {(option) => (
                <option value={option.schemaId}>
                  {option.schemaId} ({option.flavor})
                </option>
              )}
            </For>
          </select>
        </Show>
      </label>
      <label>
        Execution mode
        <select
          value={props.draft.execution_mode}
          onChange={(event) =>
            props.onUpdate((draft) => {
              draft.execution_mode = event.currentTarget
                .value as ExecutionModeTs;
            })
          }
        >
          <For each={EXECUTION_MODES}>
            {(mode) => <option value={mode}>{mode}</option>}
          </For>
        </select>
      </label>
      <label>
        Authored by
        <select
          value={props.draft.authored_by}
          onChange={(event) =>
            props.onUpdate((draft) => {
              draft.authored_by = event.currentTarget.value as AuthoredByTs;
            })
          }
        >
          <For each={AUTHORED_BY}>
            {(author) => <option value={author}>{author}</option>}
          </For>
        </select>
      </label>
      <label>
        Probability (promille)
        <input
          type="number"
          min="0"
          max="1000"
          step="1"
          value={String(props.draft.probability_promille)}
          onInput={(event) =>
            props.onUpdate((draft) => {
              draft.probability_promille = clampInt(event.currentTarget.value, 0, 1000);
            })
          }
        />
      </label>
      <label>
        Recipe ref
        <input
          value={props.draft.recipe_ref}
          onInput={(event) =>
            props.onUpdate((draft) => {
              draft.recipe_ref = event.currentTarget.value;
            })
          }
        />
      </label>
      <label>
        Model tier
        <select
          value={props.draft.model_tier}
          onChange={(event) =>
            props.onUpdate((draft) => {
              draft.model_tier = event.currentTarget.value as ModelTierTs;
            })
          }
        >
          <For each={MODEL_TIERS}>
            {(tier) => <option value={tier}>{tier}</option>}
          </For>
        </select>
      </label>
      <label>
        Inference target ref
        <input
          value={props.draft.inference_target_ref ?? ""}
          onInput={(event) =>
            props.onUpdate((draft) => {
              const value = event.currentTarget.value.trim();
              draft.inference_target_ref = value === "" ? null : value;
            })
          }
        />
      </label>
      <label>
        Substrate tool palette
        <input
          value={props.draft.substrate_tool_palette.join(",")}
          onInput={(event) =>
            props.onUpdate((draft) => {
              draft.substrate_tool_palette = splitPalette(event.currentTarget.value);
            })
          }
        />
      </label>
      <label>
        Workspace tool palette
        <input
          value={props.draft.workspace_tool_palette.join(",")}
          onInput={(event) =>
            props.onUpdate((draft) => {
              draft.workspace_tool_palette = splitPalette(event.currentTarget.value);
            })
          }
        />
      </label>
      <label>
        Max rounds
        <input
          type="number"
          min="0"
          step="1"
          value={String(props.draft.max_rounds)}
          onInput={(event) =>
            props.onUpdate((draft) => {
              draft.max_rounds = clampInt(event.currentTarget.value, 0, 1_000);
            })
          }
        />
      </label>
      <button type="button" class="hub-nav-item" onClick={props.onRemove}>
        Remove
      </button>
    </div>
  );
};

const truncateName = (name: string): string =>
  name.length > 48 ? `${name.slice(0, 45)}...` : name;

const cloneDraft = (draft: WakeEntryDraftTs): WakeEntryDraftTs => ({
  ...draft,
  substrate_tool_palette: [...draft.substrate_tool_palette],
  workspace_tool_palette: [...draft.workspace_tool_palette],
});

const splitPalette = (value: string): string[] =>
  value
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean);

const clampInt = (value: string, min: number, max: number): number => {
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed)) return min;
  return Math.max(min, Math.min(max, parsed));
};

const shortId = (value: string): string => value.slice(0, 8);

const copyText = async (value: string): Promise<void> => {
  await navigator.clipboard?.writeText(value);
};

const wakeSummary = (entry: WakeEntryTs) => (
  <>
    <span>{entry.label || entry.trigger_kind}</span>
    <Mono>{entry.trigger_id}</Mono>
  </>
);

const unwrap = async <T, E>(
  result: Promise<{ status: "ok"; data: T } | { status: "error"; error: E }>,
): Promise<T> => {
  const value = await result;
  if (value.status === "error") throw value.error;
  return value.data;
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
