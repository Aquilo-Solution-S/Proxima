import {
  For,
  Show,
  createMemo,
  type Component,
} from "solid-js";
import type {
  AuthoredByTs,
  ExecutionModeTs,
  ModelTierTs,
  PersonalityInstanceTs,
  TriggerKindTs,
  WakeEntryDraftTs,
} from "../../bindings";
import { Mono } from "../../primitives";
import { registeredPayloadRenderers } from "../../registry";
import {
  AUTHORED_BY,
  EXECUTION_MODES,
  MODEL_TIERS,
  TRIGGER_KINDS,
  type PersonalitySelection,
} from "./types";

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

interface InspectorProps {
  selection: PersonalitySelection;
  instances: PersonalityInstanceTs[];
  drafts: Map<string, WakeEntryDraftTs[]>;
  dirty: boolean;
  saving: boolean;
  error: string | null;
  onUpdateEntry: (
    instanceId: string,
    index: number,
    mutate: (draft: WakeEntryDraftTs) => void,
  ) => void;
  onAddEntry: (instanceId: string) => void;
  onRemoveEntry: (instanceId: string, index: number) => void;
  onSelectEntry: (instanceId: string, index: number) => void;
  onSave: () => void;
  onCancel: () => void;
  onTombstone: (instance: PersonalityInstanceTs) => void;
  tombstoning: string | null;
  confirmingTombstone: string | null;
  onConfirmTombstone: (instanceId: string) => void;
  onCancelTombstone: () => void;
}

const findInstance = (
  instances: PersonalityInstanceTs[],
  id: string,
): PersonalityInstanceTs | undefined =>
  instances.find((row) => row.personality_instance_id === id);

export const Inspector: Component<InspectorProps> = (props) => {
  const selectedInstance = createMemo(() => {
    const sel = props.selection;
    if (!sel) return undefined;
    if (sel.kind === "personality") return findInstance(props.instances, sel.instance_id);
    if (sel.kind === "wake_entry") return findInstance(props.instances, sel.instance_id);
    if (sel.kind === "edge") return findInstance(props.instances, sel.tgt_instance_id);
    return undefined;
  });

  const draftsFor = (instanceId: string): WakeEntryDraftTs[] =>
    props.drafts.get(instanceId) ??
    findInstance(props.instances, instanceId)?.wake_entries ??
    [];

  const selectedDraft = createMemo<WakeEntryDraftTs | undefined>(() => {
    const sel = props.selection;
    if (sel?.kind !== "wake_entry") return undefined;
    return draftsFor(sel.instance_id)[sel.entry_index];
  });

  return (
    <aside class="personality-inspector" aria-label="Inspector">
      <Show
        when={props.selection}
        fallback={
          <div class="personality-inspector-empty">
            <p>Select a personality or wake entry to inspect.</p>
          </div>
        }
      >
        <Show when={props.selection?.kind === "personality" && selectedInstance()}>
          {(instance) => (
            <PersonalityDetail
              instance={instance()}
              drafts={draftsFor(instance().personality_instance_id)}
              tombstoning={props.tombstoning}
              confirming={
                props.confirmingTombstone === instance().personality_instance_id
              }
              onTombstone={() => props.onTombstone(instance())}
              onConfirm={() =>
                props.onConfirmTombstone(instance().personality_instance_id)
              }
              onCancelConfirm={props.onCancelTombstone}
              onSelectEntry={(index) =>
                props.onSelectEntry(instance().personality_instance_id, index)
              }
              onAddEntry={() => props.onAddEntry(instance().personality_instance_id)}
            />
          )}
        </Show>

        <Show
          when={
            props.selection?.kind === "wake_entry" &&
            selectedInstance() &&
            selectedDraft()
          }
        >
          <WakeEntryDetail
            instance={selectedInstance()!}
            draft={selectedDraft()!}
            entryIndex={
              (props.selection as { entry_index: number }).entry_index
            }
            onUpdate={(mutate) => {
              const sel = props.selection;
              if (sel?.kind !== "wake_entry") return;
              props.onUpdateEntry(sel.instance_id, sel.entry_index, mutate);
            }}
            onRemove={() => {
              const sel = props.selection;
              if (sel?.kind !== "wake_entry") return;
              props.onRemoveEntry(sel.instance_id, sel.entry_index);
            }}
          />
        </Show>

        <Show when={props.selection?.kind === "edge" && selectedInstance()}>
          {(instance) => {
            const sel = props.selection as {
              schema_id: string;
              tgt_entry_index: number;
            };
            return (
              <EdgeDetail
                instance={instance()}
                schemaId={sel.schema_id}
                entry={instance().wake_entries[sel.tgt_entry_index]}
              />
            );
          }}
        </Show>
      </Show>

      <Show when={props.error}>
        {(message) => (
          <p class="proxima-error personality-inspector-error" role="alert">
            {message()}
          </p>
        )}
      </Show>

      <Show when={props.dirty}>
        <div class="personality-inspector-savebar">
          <span class="personality-inspector-savebar-hint">
            Wake entries have unsaved changes.
          </span>
          <button
            type="button"
            class="hub-nav-item"
            disabled={props.saving}
            onClick={props.onCancel}
          >
            Discard
          </button>
          <button
            type="button"
            class="hub-nav-item personality-inspector-save"
            disabled={props.saving}
            onClick={props.onSave}
          >
            Save
          </button>
        </div>
      </Show>
    </aside>
  );
};

const PersonalityDetail: Component<{
  instance: PersonalityInstanceTs;
  drafts: WakeEntryDraftTs[];
  tombstoning: string | null;
  confirming: boolean;
  onTombstone: () => void;
  onConfirm: () => void;
  onCancelConfirm: () => void;
  onSelectEntry: (index: number) => void;
  onAddEntry: () => void;
}> = (props) => (
  <div class="personality-inspector-section">
    <header class="personality-inspector-head">
      <h3>{props.instance.display_name}</h3>
      <span class={`personality-status ${props.instance.status}`}>
        {props.instance.status}
      </span>
    </header>
    <div class="personality-inspector-meta">
      <Mono>{props.instance.personality_instance_id}</Mono>
    </div>

    <Show when={props.instance.status === "needs_repair"}>
      <p class="proxima-error">Wake entries need repair.</p>
    </Show>

    <h4 class="personality-inspector-subhead">Wake entries</h4>
    <ul class="personality-inspector-entries">
      <For each={props.drafts}>
        {(entry, index) => (
          <li>
            <button
              type="button"
              class="personality-inspector-entry"
              onClick={() => props.onSelectEntry(index())}
              aria-label={`Edit ${entry.label || `wake entry ${index() + 1}`}`}
            >
              <span class="personality-inspector-entry-label">
                {entry.label || `entry ${index() + 1}`}
              </span>
              <Mono>{entry.trigger_id || "(no trigger)"}</Mono>
              <Show when={!entry.enabled}>
                <span class="personality-inspector-tag">disabled</span>
              </Show>
            </button>
          </li>
        )}
      </For>
      <Show when={props.drafts.length === 0}>
        <li class="personality-inspector-entries-empty">
          No wake entries yet.
        </li>
      </Show>
    </ul>
    <button
      type="button"
      class="hub-nav-item"
      onClick={props.onAddEntry}
    >
      Add WakeEntry
    </button>

    <div class="personality-inspector-danger">
      <Show
        when={!props.confirming}
        fallback={
          <div class="tombstone-confirm">
            <span>{`Tombstone ${props.instance.display_name}? Wakes stop; memories remain.`}</span>
            <button
              type="button"
              class="hub-nav-item danger"
              disabled={
                props.tombstoning === props.instance.personality_instance_id
              }
              onClick={props.onConfirm}
            >
              Confirm tombstone
            </button>
            <button
              type="button"
              class="hub-nav-item"
              onClick={props.onCancelConfirm}
            >
              Cancel
            </button>
          </div>
        }
      >
        <button
          type="button"
          class="hub-nav-item danger"
          onClick={props.onTombstone}
        >
          Tombstone
        </button>
      </Show>
    </div>
  </div>
);

const WakeEntryDetail: Component<{
  instance: PersonalityInstanceTs;
  draft: WakeEntryDraftTs;
  entryIndex: number;
  onUpdate: (mutate: (draft: WakeEntryDraftTs) => void) => void;
  onRemove: () => void;
}> = (props) => {
  const triggerOptions = createMemo(() =>
    schemaOptionsFor(props.draft.trigger_id),
  );

  return (
    <div class="personality-inspector-section">
      <header class="personality-inspector-head">
        <div>
          <span class="personality-inspector-eyebrow">
            {props.instance.display_name}
          </span>
          <h3>{props.draft.label || `Wake entry ${props.entryIndex + 1}`}</h3>
        </div>
      </header>

      <details class="personality-section" open>
        <summary>Identity</summary>
        <div class="personality-section-grid">
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
          <label class="personality-section-checkbox">
            <input
              type="checkbox"
              checked={props.draft.enabled}
              onChange={(event) =>
                props.onUpdate((draft) => {
                  draft.enabled = event.currentTarget.checked;
                })
              }
            />
            Enabled
          </label>
        </div>
      </details>

      <details class="personality-section" open>
        <summary>Trigger</summary>
        <p class="personality-section-hint">When does this entry wake?</p>
        <div class="personality-section-grid">
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
              when={props.draft.trigger_kind === "on_memory"}
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
                <For each={triggerOptions()}>
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
                  draft.probability_promille = clampInt(
                    event.currentTarget.value,
                    0,
                    1000,
                  );
                })
              }
            />
          </label>
        </div>
      </details>

      <details class="personality-section" open>
        <summary>Behavior</summary>
        <p class="personality-section-hint">
          What runs on each wake. Recipe weaves the per-trigger prose.
        </p>
        <div class="personality-section-grid">
          <label class="personality-section-grid-full">
            Recipe ref
            <input
              value={props.draft.recipe_ref}
              placeholder="user:default.yaml"
              onInput={(event) =>
                props.onUpdate((draft) => {
                  draft.recipe_ref = event.currentTarget.value;
                })
              }
            />
          </label>
          <label class="personality-section-grid-full">
            Substrate tool palette
            <input
              value={props.draft.substrate_tool_palette.join(",")}
              placeholder="comma,separated,tool,ids"
              onInput={(event) =>
                props.onUpdate((draft) => {
                  draft.substrate_tool_palette = splitPalette(
                    event.currentTarget.value,
                  );
                })
              }
            />
          </label>
          <label class="personality-section-grid-full">
            Workspace tool palette
            <input
              value={props.draft.workspace_tool_palette.join(",")}
              placeholder="comma,separated,tool,ids"
              onInput={(event) =>
                props.onUpdate((draft) => {
                  draft.workspace_tool_palette = splitPalette(
                    event.currentTarget.value,
                  );
                })
              }
            />
          </label>
        </div>
      </details>

      <details class="personality-section">
        <summary>Runtime</summary>
        <p class="personality-section-hint">How the wake invocation is dispatched.</p>
        <div class="personality-section-grid">
          <label>
            Execution mode
            <select
              value={props.draft.execution_mode}
              onChange={(event) =>
                props.onUpdate((draft) => {
                  draft.execution_mode = event.currentTarget.value as ExecutionModeTs;
                })
              }
            >
              <For each={EXECUTION_MODES}>
                {(mode) => <option value={mode}>{mode}</option>}
              </For>
            </select>
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
              placeholder="(default tier binding)"
              onInput={(event) =>
                props.onUpdate((draft) => {
                  const value = event.currentTarget.value.trim();
                  draft.inference_target_ref = value === "" ? null : value;
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
                  draft.max_rounds = clampInt(
                    event.currentTarget.value,
                    0,
                    1_000,
                  );
                })
              }
            />
          </label>
        </div>
      </details>

      <div class="personality-inspector-danger">
        <button
          type="button"
          class="hub-nav-item danger"
          onClick={props.onRemove}
        >
          Remove entry
        </button>
      </div>
    </div>
  );
};

const EdgeDetail: Component<{
  instance: PersonalityInstanceTs;
  schemaId: string;
  entry: typeof undefined | { label: string; trigger_id: string; enabled: boolean };
}> = (props) => (
  <div class="personality-inspector-section">
    <header class="personality-inspector-head">
      <span class="personality-inspector-eyebrow">trigger schema</span>
      <h3>{props.schemaId}</h3>
    </header>
    <p class="personality-inspector-meta">
      Wakes <strong>{props.instance.display_name}</strong>
      <Show when={props.entry}>
        {" "}
        on entry <Mono>{props.entry?.label || "(unlabeled)"}</Mono>.
      </Show>
    </p>
  </div>
);
