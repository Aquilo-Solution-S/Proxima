import {
  For,
  Show,
  createMemo,
  createSignal,
  type Component,
} from "solid-js";
import type {
  AuthoredByTs,
  ExecutionModeTs,
  McpToolTs,
  ModelTierTs,
  PersonalityInstanceTs,
  RelationTs,
  TriggerKindTs,
  WakeEntryDraftTs,
  WakeInvocationTs,
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

const isGoalLifecycleTrigger = (draft: WakeEntryDraftTs): boolean =>
  draft.trigger_kind === "on_memory" &&
  draft.trigger_id === "proxima-goal/goal-activated-v1";

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
  mcpTools: McpToolTs[] | null;
  workspaceTools: WorkspaceToolTs[] | null;
  relations: RelationTs[] | null;
  toolsError: string | null;
  wakeInvocations: WakeInvocationTs[] | null;
  wakeInvocationsLoading: boolean;
  wakeInvocationsError: string | null;
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
              wakeInvocations={props.wakeInvocations}
              wakeInvocationsLoading={props.wakeInvocationsLoading}
              wakeInvocationsError={props.wakeInvocationsError}
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
            mcpTools={props.mcpTools}
            workspaceTools={props.workspaceTools}
            relations={props.relations}
            toolsError={props.toolsError}
            wakeInvocations={props.wakeInvocations}
            wakeInvocationsLoading={props.wakeInvocationsLoading}
            wakeInvocationsError={props.wakeInvocationsError}
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
  wakeInvocations: WakeInvocationTs[] | null;
  wakeInvocationsLoading: boolean;
  wakeInvocationsError: string | null;
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

    <WakeInvocationPanel
      invocations={props.wakeInvocations}
      loading={props.wakeInvocationsLoading}
      error={props.wakeInvocationsError}
    />

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
  mcpTools: McpToolTs[] | null;
  workspaceTools: WorkspaceToolTs[] | null;
  relations: RelationTs[] | null;
  toolsError: string | null;
  onUpdate: (mutate: (draft: WakeEntryDraftTs) => void) => void;
  onRemove: () => void;
  wakeInvocations: WakeInvocationTs[] | null;
  wakeInvocationsLoading: boolean;
  wakeInvocationsError: string | null;
}> = (props) => {
  const memoryOptions = createMemo<SchemaPickerOption[]>(() =>
    schemaOptionsFor(props.draft.trigger_id).map((option) => ({
      id: option.schemaId,
      group: option.flavor,
    })),
  );
  const edgeOptions = createMemo<SchemaPickerOption[] | null>(() => {
    if (!props.relations) return null;
    const known = new Set(props.relations.map((r) => r.relation_id));
    const base: SchemaPickerOption[] = props.relations.map((r) => ({
      id: r.relation_id,
      group: r.flavor_id || "core",
      detail: r.typed ? "typed" : "substrate",
    }));
    if (
      props.draft.trigger_id.trim() !== "" &&
      !known.has(props.draft.trigger_id)
    ) {
      base.unshift({
        id: props.draft.trigger_id,
        group: "stored config",
        orphan: true,
      });
    }
    return base;
  });

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
          <Show
            when={props.draft.trigger_kind === "on_memory"}
            fallback={
              <SchemaPickerSelect
                label="Trigger id"
                hint="Pick a relation schema for this edge trigger."
                selected={props.draft.trigger_id}
                options={edgeOptions()}
                error={props.toolsError}
                loadingLabel="Loading relations..."
                emptyLabel="No relations registered in this build."
                onChange={(value) =>
                  props.onUpdate((draft) => {
                    draft.trigger_id = value;
                  })
                }
              />
            }
          >
            <SchemaPickerSelect
              label="Trigger id"
              hint="Pick a memory schema for this trigger."
              selected={props.draft.trigger_id}
              options={memoryOptions()}
              error={null}
              loadingLabel="Loading schemas..."
              emptyLabel="No memory schemas registered in this build."
              onChange={(value) =>
                props.onUpdate((draft) => {
                  draft.trigger_id = value;
                })
              }
            />
          </Show>
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
          <Show when={isGoalLifecycleTrigger(props.draft)}>
            <label class="personality-section-checkbox">
              <input
                type="checkbox"
                checked={props.draft.goal_scope === "trigger_goal_assigned"}
                onChange={(event) =>
                  props.onUpdate((draft) => {
                    draft.goal_scope = event.currentTarget.checked
                      ? "trigger_goal_assigned"
                      : "none";
                  })
                }
              />
              Requires assigned goal
            </label>
          </Show>
        </div>
      </details>

      <details class="personality-section" open>
        <summary>Behavior</summary>
        <p class="personality-section-hint">
          What runs on each wake.
        </p>
        <div class="personality-section-grid">
          <label class="personality-section-grid-full">
            Instructions
            <textarea
              rows="7"
              value={props.draft.instructions}
              onInput={(event) =>
                props.onUpdate((draft) => {
                  draft.instructions = event.currentTarget.value;
                })
              }
            />
          </label>
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

      <WakeInvocationPanel
        invocations={props.wakeInvocations}
        loading={props.wakeInvocationsLoading}
        error={props.wakeInvocationsError}
      />

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

const WakeInvocationPanel: Component<{
  invocations: WakeInvocationTs[] | null;
  loading: boolean;
  error: string | null;
}> = (props) => (
  <details class="personality-section personality-invocations">
    <summary>Wake invocations</summary>
    <Show when={props.error}>
      {(message) => (
        <p class="proxima-error personality-invocations-error" role="alert">
          {message()}
        </p>
      )}
    </Show>
    <Show when={props.loading}>
      <p class="personality-invocations-empty">Loading invocations.</p>
    </Show>
    <Show when={!props.loading && props.invocations?.length === 0}>
      <p class="personality-invocations-empty">No wake invocations recorded.</p>
    </Show>
    <div class="personality-invocation-list">
      <For each={props.invocations ?? []}>
        {(invocation) => (
          <details class="personality-invocation" open={invocation.status === "failed"}>
            <summary>
              <span class={`personality-invocation-status ${invocation.status}`}>
                {invocation.status}
              </span>
              <span class="personality-invocation-title">
                {invocation.wake_entry_label}
              </span>
              <Mono>{shortId(invocation.change_event_seq)}</Mono>
            </summary>
            <div class="personality-invocation-body">
              <dl class="personality-invocation-meta">
                <div>
                  <dt>Started</dt>
                  <dd>{invocation.started_at}</dd>
                </div>
                <div>
                  <dt>Duration</dt>
                  <dd>{formatDuration(invocation.duration_ms)}</dd>
                </div>
                <div>
                  <dt>Exit</dt>
                  <dd>{invocation.exit_code ?? "-"}</dd>
                </div>
                <div>
                  <dt>Turns</dt>
                  <dd>{invocation.turn_count}</dd>
                </div>
              </dl>
              <Show when={invocation.failure_reason}>
                {(message) => (
                  <pre class="personality-invocation-output">{message()}</pre>
                )}
              </Show>
              <InvocationOutput
                label="stdout"
                value={invocation.stdout_tail}
                truncated={invocation.stdout_truncated}
              />
              <InvocationOutput
                label="stderr"
                value={invocation.stderr_tail}
                truncated={invocation.stderr_truncated}
              />
              <Show when={invocation.logs.length > 0}>
                <div class="personality-invocation-tools">
                  <h5>Tool calls</h5>
                  <For each={invocation.logs}>
                    {(log) => (
                      <div class="personality-invocation-tool">
                        <span>{log.tool_id ?? log.phase}</span>
                        <span class={`personality-invocation-status ${log.status}`}>
                          {log.status}
                        </span>
                        <span>{formatDuration(log.duration_ms)}</span>
                        <Show when={log.message_tail}>
                          {(message) => (
                            <pre class="personality-invocation-output">
                              {message()}
                            </pre>
                          )}
                        </Show>
                      </div>
                    )}
                  </For>
                </div>
              </Show>
            </div>
          </details>
        )}
      </For>
    </div>
  </details>
);

const InvocationOutput: Component<{
  label: string;
  value: string | null;
  truncated: boolean;
}> = (props) => (
  <Show when={props.value}>
    {(value) => (
      <div>
        <div class="personality-invocation-output-label">
          <span>{props.label}</span>
          <Show when={props.truncated}>
            <span>truncated</span>
          </Show>
        </div>
        <pre class="personality-invocation-output">{value()}</pre>
      </div>
    )}
  </Show>
);

const shortId = (value: string): string => value.slice(0, 8);

const formatDuration = (durationMs: number | null): string => {
  if (durationMs === null) return "-";
  if (durationMs < 1000) return `${durationMs} ms`;
  return `${(durationMs / 1000).toFixed(1)} s`;
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

type SchemaPickerOption = {
  id: string;
  group: string;
  detail?: string | null;
  orphan?: boolean;
};

const SchemaPickerSelect: Component<{
  label: string;
  hint: string;
  selected: string;
  options: SchemaPickerOption[] | null;
  error: string | null;
  loadingLabel: string;
  emptyLabel: string;
  onChange: (id: string) => void;
}> = (props) => {
  const [open, setOpen] = createSignal(false);
  const [pending, setPending] = createSignal<string>("");
  const [query, setQuery] = createSignal("");
  const summary = createMemo(() =>
    props.selected.trim() === "" ? "Select schema" : props.selected,
  );
  const filteredOptions = createMemo(() => {
    const needle = query().trim().toLowerCase();
    const options = props.options ?? [];
    if (!needle) return options;
    return options.filter((option) =>
      `${option.id} ${option.group} ${option.detail ?? ""}`
        .toLowerCase()
        .includes(needle),
    );
  });
  const groups = createMemo(() => {
    const entries = new Map<string, SchemaPickerOption[]>();
    for (const option of filteredOptions()) {
      entries.set(option.group, [...(entries.get(option.group) ?? []), option]);
    }
    return Array.from(entries, ([group, options]) => ({ group, options }));
  });
  const openDialog = () => {
    setPending(props.selected);
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
                      aria-label="Close picker"
                      onClick={() => setOpen(false)}
                    >
                      x
                    </button>
                  </header>
                  <div class="personality-tool-dialog-toolbar">
                    <input
                      value={query()}
                      placeholder="Search schemas"
                      aria-label="Search schemas"
                      onInput={(event) => setQuery(event.currentTarget.value)}
                    />
                    <button
                      type="button"
                      class="hub-nav-item"
                      onClick={() => setPending("")}
                    >
                      Clear
                    </button>
                  </div>
                  <div class="personality-tool-dialog-list">
                    <Show
                      when={groups().length > 0}
                      fallback={
                        <p class="personality-tool-picker-empty">
                          No schemas match the current search.
                        </p>
                      }
                    >
                      <For each={groups()}>
                        {(group) => (
                          <section class="personality-tool-group">
                            <h5>{group.group}</h5>
                            <ul class="personality-tool-list" role="list">
                              <For each={group.options}>
                                {(option) => (
                                  <li>
                                    <label
                                      classList={{
                                        "personality-tool-row": true,
                                        "personality-tool-row-orphan": Boolean(
                                          option.orphan,
                                        ),
                                        "is-selected": pending() === option.id,
                                      }}
                                    >
                                      <input
                                        type="radio"
                                        name={`schema-picker-${props.label}`}
                                        checked={pending() === option.id}
                                        onChange={() => setPending(option.id)}
                                      />
                                      <span class="personality-tool-row-main">
                                        <span class="personality-tool-row-short">
                                          {toolShortName(option.id)}
                                        </span>
                                        <span class="personality-tool-row-id">
                                          <Mono>{option.id}</Mono>
                                        </span>
                                      </span>
                                      <Show when={option.detail}>
                                        <span class="personality-tool-row-desc">
                                          {option.detail}
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
                    <Show when={pending()} fallback={<span>None selected</span>}>
                      <span>Selected: <Mono>{pending()}</Mono></span>
                    </Show>
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
