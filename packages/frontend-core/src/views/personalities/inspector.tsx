import {
  For,
  Show,
  createMemo,
  createSignal,
  type Component,
} from "solid-js";
import type {
  AuthoredByTs,
  BundledRecipeTs,
  ExecutionModeTs,
  McpToolTs,
  ModelTierTs,
  OwnerRecipesListingTs,
  PersonalityInstanceTs,
  TriggerKindTs,
  WakeEntryDraftTs,
  WorkspaceToolTs,
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

const triggerKindLabel = (kind: TriggerKindTs): string => {
  switch (kind) {
    case "on_memory":
      return "On memory";
    case "on_edge":
      return "On edge";
  }
};

const authoredByLabel = (author: AuthoredByTs): string => {
  switch (author) {
    case "any":
      return "Any";
    case "self_author":
      return "Self author";
    case "other":
      return "Other";
  }
};

const executionModeLabel = (mode: ExecutionModeTs): string => {
  switch (mode) {
    case "substrate_only":
      return "Substrate only";
    case "workspace":
      return "Workspace";
  }
};

const modelTierLabel = (tier: ModelTierTs): string => {
  switch (tier) {
    case "fast":
      return "Fast";
    case "standard":
      return "Standard";
    case "deep":
      return "Deep";
  }
};

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
  recipes: OwnerRecipesListingTs | null;
  bundledRecipes: BundledRecipeTs[] | null;
  recipesError: string | null;
  mcpTools: McpToolTs[] | null;
  workspaceTools: WorkspaceToolTs[] | null;
  toolsError: string | null;
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
  onRefreshRecipes: () => void;
  onRevealRecipesFolder: (path: string) => void;
}

const findInstance = (
  instances: PersonalityInstanceTs[],
  id: string,
): PersonalityInstanceTs | undefined =>
  instances.find((row) => row.personality_instance_id === id);

export const Inspector: Component<InspectorProps> = (props) => {
  let inspectorRef: HTMLElement | undefined;

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

  const currentScrollSection = (): HTMLElement | null =>
    inspectorRef?.querySelector(".personality-inspector-section") ?? null;

  const restoreScrollTop = (scrollTop: number) => {
    queueMicrotask(() => {
      const section = currentScrollSection();
      if (section) section.scrollTop = scrollTop;
      requestAnimationFrame(() => {
        const nextSection = currentScrollSection();
        if (nextSection) nextSection.scrollTop = scrollTop;
      });
    });
  };

  return (
    <aside
      ref={inspectorRef}
      class="personality-inspector"
      aria-label="Inspector"
    >
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
            recipes={props.recipes}
            bundledRecipes={props.bundledRecipes}
            recipesError={props.recipesError}
            mcpTools={props.mcpTools}
            workspaceTools={props.workspaceTools}
            toolsError={props.toolsError}
            onUpdate={(mutate) => {
              const sel = props.selection;
              if (sel?.kind !== "wake_entry") return;
              const scrollTop = currentScrollSection()?.scrollTop ?? 0;
              props.onUpdateEntry(sel.instance_id, sel.entry_index, mutate);
              restoreScrollTop(scrollTop);
            }}
            onRemove={() => {
              const sel = props.selection;
              if (sel?.kind !== "wake_entry") return;
              props.onRemoveEntry(sel.instance_id, sel.entry_index);
            }}
            onRefreshRecipes={props.onRefreshRecipes}
            onRevealRecipesFolder={props.onRevealRecipesFolder}
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
  recipes: OwnerRecipesListingTs | null;
  bundledRecipes: BundledRecipeTs[] | null;
  recipesError: string | null;
  mcpTools: McpToolTs[] | null;
  workspaceTools: WorkspaceToolTs[] | null;
  toolsError: string | null;
  onUpdate: (mutate: (draft: WakeEntryDraftTs) => void) => void;
  onRemove: () => void;
  onRefreshRecipes: () => void;
  onRevealRecipesFolder: (path: string) => void;
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
              onChange={(event) =>
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
                {(kind) => <option value={kind}>{triggerKindLabel(kind)}</option>}
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
                  onChange={(event) =>
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
                {(author) => (
                  <option value={author}>{authoredByLabel(author)}</option>
                )}
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
              onChange={(event) =>
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
          <RecipePicker
            recipeRef={props.draft.recipe_ref}
            recipes={props.recipes}
            bundledRecipes={props.bundledRecipes}
            recipesError={props.recipesError}
            onChange={(value) =>
              props.onUpdate((draft) => {
                draft.recipe_ref = value;
              })
            }
            onRefresh={props.onRefreshRecipes}
            onReveal={props.onRevealRecipesFolder}
          />
          <SubstrateToolPicker
            selected={props.draft.substrate_tool_palette}
            tools={props.mcpTools}
            error={props.toolsError}
            onChange={(toolIds) =>
              props.onUpdate((draft) => {
                draft.substrate_tool_palette = toolIds;
              })
            }
          />
          <Show when={props.draft.execution_mode === "workspace"}>
            <WorkspaceToolPicker
              selected={props.draft.workspace_tool_palette}
              tools={props.workspaceTools}
              error={props.toolsError}
              onChange={(toolIds) =>
                props.onUpdate((draft) => {
                  draft.workspace_tool_palette = toolIds;
                })
              }
            />
          </Show>
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
                {(mode) => (
                  <option value={mode}>{executionModeLabel(mode)}</option>
                )}
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
                {(tier) => <option value={tier}>{modelTierLabel(tier)}</option>}
              </For>
            </select>
          </label>
          <label>
            Inference target ref
            <input
              value={props.draft.inference_target_ref ?? ""}
              placeholder="(default tier binding)"
              onChange={(event) =>
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
              onChange={(event) =>
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

const USER_PREFIX = "user:";
const BUNDLED_PREFIX = "bundled:";

type RecipeTab = "user" | "bundled";

const tabFromRef = (recipeRef: string): RecipeTab =>
  recipeRef.startsWith(BUNDLED_PREFIX) ? "bundled" : "user";

const RecipePicker: Component<{
  recipeRef: string;
  recipes: OwnerRecipesListingTs | null;
  bundledRecipes: BundledRecipeTs[] | null;
  recipesError: string | null;
  onChange: (value: string) => void;
  onRefresh: () => void;
  onReveal: (path: string) => void;
}> = (props) => {
  const [activeTab, setActiveTab] = createSignal<RecipeTab>(
    tabFromRef(props.recipeRef),
  );

  const userOptions = createMemo<{ value: string; label: string }[]>(() => {
    const listing = props.recipes;
    if (!listing) return [];
    return listing.recipes.map((recipe) => ({
      value: `${USER_PREFIX}${recipe.filename}`,
      label: recipe.filename,
    }));
  });

  const bundledOptions = createMemo<{ value: string; label: string }[]>(() => {
    const list = props.bundledRecipes;
    if (!list) return [];
    return list.map((recipe) => ({
      value: `${BUNDLED_PREFIX}${recipe.slug}`,
      label: recipe.slug,
    }));
  });

  const userOrphan = createMemo<{ value: string; label: string } | null>(() => {
    const ref = props.recipeRef;
    if (!ref.startsWith(USER_PREFIX)) return null;
    if (userOptions().some((opt) => opt.value === ref)) return null;
    return { value: ref, label: `${ref.slice(USER_PREFIX.length)} (missing)` };
  });

  const bundledOrphan = createMemo<{ value: string; label: string } | null>(
    () => {
      const ref = props.recipeRef;
      if (!ref.startsWith(BUNDLED_PREFIX)) return null;
      if (bundledOptions().some((opt) => opt.value === ref)) return null;
      return {
        value: ref,
        label: `${ref.slice(BUNDLED_PREFIX.length)} (unknown)`,
      };
    },
  );

  const otherOrphan = createMemo<{ value: string; label: string } | null>(
    () => {
      const ref = props.recipeRef;
      if (!ref) return null;
      if (ref.startsWith(USER_PREFIX) || ref.startsWith(BUNDLED_PREFIX))
        return null;
      return { value: ref, label: `${ref} (unrecognized)` };
    },
  );

  const userLoading = createMemo(() => props.recipes === null);
  const bundledLoading = createMemo(() => props.bundledRecipes === null);
  const userEmpty = createMemo(
    () => props.recipes !== null && props.recipes.recipes.length === 0,
  );
  const bundledEmpty = createMemo(
    () => props.bundledRecipes !== null && props.bundledRecipes.length === 0,
  );
  const folderPath = createMemo(() => props.recipes?.root_path ?? "");

  const currentValue = createMemo(() => {
    const tab = activeTab();
    const ref = props.recipeRef;
    if (tab === "user") {
      if (ref.startsWith(USER_PREFIX)) return ref;
      return "";
    }
    if (ref.startsWith(BUNDLED_PREFIX)) return ref;
    return "";
  });

  return (
    <div class="personality-section-grid-full personality-recipe-picker">
      <div class="personality-recipe-picker-tabs" role="tablist" aria-label="Recipe source">
        <button
          type="button"
          role="tab"
          aria-selected={activeTab() === "user"}
          class={`personality-recipe-picker-tab${
            activeTab() === "user" ? " is-active" : ""
          }`}
          onClick={() => setActiveTab("user")}
        >
          Private
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={activeTab() === "bundled"}
          class={`personality-recipe-picker-tab${
            activeTab() === "bundled" ? " is-active" : ""
          }`}
          onClick={() => setActiveTab("bundled")}
        >
          Bundled
        </button>
      </div>

      <Show when={activeTab() === "user"}>
        <label>
          Recipe
          <div class="personality-recipe-picker-row">
            <select
              value={currentValue()}
              disabled={userLoading()}
              aria-label="Recipe"
              onChange={(event) => props.onChange(event.currentTarget.value)}
            >
              <Show when={!currentValue()}>
                <option value="" disabled>
                  {userLoading() ? "Loading recipes…" : "Select a recipe"}
                </option>
              </Show>
              <Show when={userOrphan()}>
                {(orphan) => (
                  <option value={orphan().value}>{orphan().label}</option>
                )}
              </Show>
              <Show when={activeTab() === "user" ? otherOrphan() : null}>
                {(orphan) => (
                  <option value={orphan().value}>{orphan().label}</option>
                )}
              </Show>
              <For each={userOptions()}>
                {(option) => (
                  <option value={option.value}>{option.label}</option>
                )}
              </For>
            </select>
            <button
              type="button"
              class="hub-nav-item"
              onClick={props.onRefresh}
              aria-label="Refresh recipes"
              title="Refresh"
            >
              ↻
            </button>
          </div>
        </label>
        <p class="personality-recipe-picker-hint">
          <Show
            when={folderPath()}
            fallback={<span>Loading recipes folder…</span>}
          >
            <span>
              Recipes folder: <Mono>{folderPath()}</Mono>
            </span>
            <button
              type="button"
              class="personality-recipe-picker-link"
              onClick={() => props.onReveal(folderPath())}
            >
              Reveal
            </button>
          </Show>
        </p>
        <Show when={userEmpty()}>
          <p class="personality-recipe-picker-empty">
            No recipes found. Drop a *.yaml in the folder above, then refresh.
          </p>
        </Show>
      </Show>

      <Show when={activeTab() === "bundled"}>
        <label>
          Recipe
          <div class="personality-recipe-picker-row">
            <select
              value={currentValue()}
              disabled={bundledLoading()}
              aria-label="Recipe"
              onChange={(event) => props.onChange(event.currentTarget.value)}
            >
              <Show when={!currentValue()}>
                <option value="" disabled>
                  {bundledLoading()
                    ? "Loading bundled recipes…"
                    : "Select a bundled recipe"}
                </option>
              </Show>
              <Show when={bundledOrphan()}>
                {(orphan) => (
                  <option value={orphan().value}>{orphan().label}</option>
                )}
              </Show>
              <For each={bundledOptions()}>
                {(option) => (
                  <option value={option.value}>{option.label}</option>
                )}
              </For>
            </select>
          </div>
        </label>
        <p class="personality-recipe-picker-hint">
          Ships with the app — read-only. Copy a bundled file into the
          recipes folder to customize.
        </p>
        <Show when={bundledEmpty()}>
          <p class="personality-recipe-picker-empty">
            No bundled recipes registered.
          </p>
        </Show>
      </Show>

      <Show when={props.recipesError}>
        {(message) => (
          <p class="proxima-error" role="alert">
            {message()}
          </p>
        )}
      </Show>
    </div>
  );
};

const SubstrateToolPicker: Component<{
  selected: string[];
  tools: McpToolTs[] | null;
  error: string | null;
  onChange: (toolIds: string[]) => void;
}> = (props) => {
  const options = createMemo<ToolPaletteOption[] | null>(() => {
    if (!props.tools) return null;
    const known = new Set((props.tools ?? []).map((t) => t.name));
    return [
      ...props.tools.map((tool) => ({
        id: tool.name,
        description: tool.description,
      })),
      ...props.selected
        .filter((id) => !known.has(id))
        .map((id) => ({
          id,
          description: "(unknown)",
          orphan: true,
        })),
    ];
  });

  return (
    <ToolPaletteSelect
      label="Substrate tool palette"
      hint="Engine-hosted MCP tools registered at compile time."
      selected={props.selected}
      options={options()}
      error={props.error}
      loadingLabel="Loading tools..."
      emptyLabel="No MCP tools registered in this build."
      onChange={props.onChange}
    />
  );
};

const WorkspaceToolPicker: Component<{
  selected: string[];
  tools: WorkspaceToolTs[] | null;
  error: string | null;
  onChange: (toolIds: string[]) => void;
}> = (props) => {
  const options = createMemo<ToolPaletteOption[] | null>(() => {
    if (!props.tools) return null;
    const known = new Set((props.tools ?? []).map((t) => t.id));
    return [
      ...props.tools.map((tool) => ({
        id: tool.id,
        description: tool.description,
      })),
      ...props.selected
        .filter((id) => !known.has(id))
        .map((id) => ({
          id,
          description: "(unknown)",
          orphan: true,
        })),
    ];
  });

  return (
    <ToolPaletteSelect
      label="Workspace tool palette"
      hint="Filesystem and process tools available when running in workspace mode."
      selected={props.selected}
      options={options()}
      error={props.error}
      loadingLabel="Loading tools..."
      emptyLabel="No workspace tools registered in this build."
      onChange={props.onChange}
    />
  );
};

type ToolPaletteOption = {
  id: string;
  description?: string | null;
  orphan?: boolean;
};

const toggleToolSelection = (selected: string[], toolId: string): string[] => {
  if (selected.includes(toolId)) return selected.filter((id) => id !== toolId);
  return [...selected, toolId];
};

const toolShortName = (id: string): string => {
  const parts = id.split("/");
  return parts[parts.length - 1] ?? id;
};

const toolGroupName = (id: string): string => id.split("/")[0] ?? "tools";

const ToolPaletteSelect: Component<{
  label: string;
  hint: string;
  selected: string[];
  options: ToolPaletteOption[] | null;
  error: string | null;
  loadingLabel: string;
  emptyLabel: string;
  onChange: (toolIds: string[]) => void;
}> = (props) => {
  const [open, setOpen] = createSignal(false);
  const [pending, setPending] = createSignal<string[]>([]);
  const [query, setQuery] = createSignal("");
  const summary = createMemo(() => {
    if (props.selected.length === 0) return "Select tools";
    if (props.selected.length === 1) return toolShortName(props.selected[0]);
    return `${props.selected.length} selected`;
  });
  const filteredOptions = createMemo(() => {
    const needle = query().trim().toLowerCase();
    const options = props.options ?? [];
    if (!needle) return options;
    return options.filter((tool) =>
      `${tool.id} ${tool.description ?? ""}`.toLowerCase().includes(needle),
    );
  });
  const groups = createMemo(() => {
    const entries = new Map<string, ToolPaletteOption[]>();
    for (const option of filteredOptions()) {
      const group = toolGroupName(option.id);
      entries.set(group, [...(entries.get(group) ?? []), option]);
    }
    return Array.from(entries, ([group, tools]) => ({ group, tools }));
  });
  const openDialog = () => {
    setPending([...props.selected]);
    setQuery("");
    setOpen(true);
  };
  const apply = () => {
    props.onChange(pending());
    setOpen(false);
  };

  return (
    <div class="personality-section-grid-full personality-tool-picker">
      <div class="personality-tool-picker-head">
        <span class="personality-tool-picker-label">{props.label}</span>
        <span class="personality-tool-picker-hint">{props.hint}</span>
      </div>
      <Show when={props.error}>
        {(message) => (
          <p class="proxima-error" role="alert">
            {message()}
          </p>
        )}
      </Show>
      <Show
        when={props.options}
        fallback={
          <p class="personality-tool-picker-empty">{props.loadingLabel}</p>
        }
      >
        {(options) => (
          <Show
            when={options().length > 0}
            fallback={
              <p class="personality-tool-picker-empty">{props.emptyLabel}</p>
            }
          >
            <button
              type="button"
              class="personality-tool-configure-trigger"
              aria-label={`${props.label}: ${summary()}`}
              onClick={openDialog}
            >
              <span class="personality-tool-configure-summary">
                {summary()}
              </span>
              <span class="personality-tool-configure-action">Configure</span>
            </button>
            <Show when={open()}>
              <div class="personality-tool-dialog-backdrop">
                <div
                  class="personality-tool-dialog"
                  role="dialog"
                  aria-modal="true"
                  aria-label={props.label}
                >
                  <header class="personality-tool-dialog-head">
                    <div>
                      <h4>{props.label}</h4>
                      <p>{props.hint}</p>
                    </div>
                    <button
                      type="button"
                      class="personality-tool-dialog-close"
                      aria-label="Close tool palette"
                      onClick={() => setOpen(false)}
                    >
                      x
                    </button>
                  </header>
                  <div class="personality-tool-dialog-toolbar">
                    <input
                      value={query()}
                      placeholder="Search tools"
                      aria-label="Search tools"
                      onInput={(event) => setQuery(event.currentTarget.value)}
                    />
                    <button
                      type="button"
                      class="hub-nav-item"
                      onClick={() => setPending([])}
                    >
                      Clear all
                    </button>
                  </div>
                  <div class="personality-tool-dialog-list">
                    <Show
                      when={groups().length > 0}
                      fallback={
                        <p class="personality-tool-picker-empty">
                          No tools match the current search.
                        </p>
                      }
                    >
                      <For each={groups()}>
                        {(group) => (
                          <section class="personality-tool-group">
                            <h5>{group.group}</h5>
                            <ul class="personality-tool-list" role="list">
                              <For each={group.tools}>
                                {(tool) => (
                                  <li>
                                    <label
                                      classList={{
                                        "personality-tool-row": true,
                                        "personality-tool-row-orphan": Boolean(
                                          tool.orphan,
                                        ),
                                        "is-selected": pending().includes(
                                          tool.id,
                                        ),
                                      }}
                                    >
                                      <input
                                        type="checkbox"
                                        checked={pending().includes(tool.id)}
                                        onChange={() =>
                                          setPending((current) =>
                                            toggleToolSelection(current, tool.id),
                                          )
                                        }
                                      />
                                      <span class="personality-tool-row-main">
                                        <span class="personality-tool-row-short">
                                          {toolShortName(tool.id)}
                                        </span>
                                        <span class="personality-tool-row-id">
                                          <Mono>{tool.id}</Mono>
                                        </span>
                                      </span>
                                      <Show when={tool.description}>
                                        <span class="personality-tool-row-desc">
                                          {tool.description}
                                        </span>
                                      </Show>
                                    </label>
                                  </li>
                                )}
                              </For>
                            </ul>
                          </section>
                        )}
                      </For>
                    </Show>
                  </div>
                  <footer class="personality-tool-dialog-actions">
                    <span>{pending().length} selected</span>
                    <button
                      type="button"
                      class="hub-nav-item"
                      onClick={() => setOpen(false)}
                    >
                      Cancel
                    </button>
                    <button
                      type="button"
                      class="hub-nav-item personality-inspector-save"
                      onClick={apply}
                    >
                      Apply
                    </button>
                  </footer>
                </div>
              </div>
            </Show>
          </Show>
        )}
      </Show>
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
