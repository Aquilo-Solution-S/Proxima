import "./goal-dialog.css";

import {
  For,
  Show,
  createMemo,
  createSignal,
  onMount,
  type Component,
} from "solid-js";
import { Portal } from "solid-js/web";
import {
  registeredGoalPayloadEditors,
  type RegisteredGoalPayloadEditor,
} from "../registry";
import { useGraph } from "../graph-store";
import type { Hub } from "../hub";
import {
  commands,
  type GoalDraft,
  type GoalRow,
  type Owner,
  type PersonalityInstanceTs,
} from "../bindings";

export interface GoalDialogProps {
  hub: Hub;
  /** Optional proposal being modified — pre-populates schema + payload. */
  proposal?: GoalRow;
  assignmentMode?: "goal-reactive";
  onClose: () => void;
  onAfterWrite: () => void;
}

type GoalReactivePersonality = {
  id: string;
  rootId: string;
  label: string;
};

const editorKey = (editor: RegisteredGoalPayloadEditor): string =>
  `${editor.schemaId}@${editor.schemaVersion}`;

const commandErrorMessage = (raw: unknown): string => {
  if (typeof raw === "object" && raw !== null && "message" in raw) {
    return String((raw as { message: unknown }).message);
  }
  return typeof raw === "string" ? raw : "command failed";
};

const goalReactivePersonalities = (
  instances: PersonalityInstanceTs[],
): GoalReactivePersonality[] =>
  instances
    .filter(
      (instance) =>
        instance.status === "active" &&
        instance.wake_entries.some(
          (entry) =>
            entry.enabled &&
            entry.trigger_kind === "on_memory" &&
            entry.trigger_id === "proxima-goal/goal-activated-v1" &&
            entry.goal_scope === "trigger_goal_assigned",
        ),
    )
    .map((instance) => ({
      id: instance.personality_instance_id,
      rootId: instance.current_root_perspective_memory_id,
      label: instance.display_name,
    }));

const decodeProposalPayload = (
  hub: Hub,
  goal: GoalRow,
  fallback: unknown,
): unknown => {
  const codec = hub.codecFor(goal.schema_id, goal.schema_version);
  if (codec === null) return fallback;
  try {
    return codec.decode(new Uint8Array(goal.payload));
  } catch {
    return fallback;
  }
};

export const GoalDialog: Component<GoalDialogProps> = (props) => {
  const graph = useGraph();
  const editors = registeredGoalPayloadEditors();
  const assignmentEnabled = () =>
    props.assignmentMode === "goal-reactive" && props.proposal === undefined;

  const initialEditor = (): RegisteredGoalPayloadEditor | null => {
    if (props.proposal !== undefined) {
      const match = editors.find(
        (e) =>
          e.schemaId === props.proposal!.schema_id &&
          e.schemaVersion === props.proposal!.schema_version,
      );
      if (match !== undefined) return match;
    }
    return editors[0] ?? null;
  };

  const [selectedKey, setSelectedKey] = createSignal<string | null>(
    initialEditor() === null ? null : editorKey(initialEditor()!),
  );
  const selected = createMemo<RegisteredGoalPayloadEditor | null>(() => {
    const key = selectedKey();
    if (key === null) return null;
    return editors.find((e) => editorKey(e) === key) ?? null;
  });

  const initialPayload = (): unknown => {
    const editor = selected();
    if (editor === null) return null;
    if (props.proposal !== undefined) {
      return decodeProposalPayload(props.hub, props.proposal, editor.defaults());
    }
    return editor.defaults();
  };

  const [payload, setPayload] = createSignal<unknown>(initialPayload());
  const [title, setTitle] = createSignal(props.proposal?.title ?? "");
  const [text, setText] = createSignal(props.proposal?.text ?? "");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [assignable, setAssignable] = createSignal<GoalReactivePersonality[]>(
    [],
  );
  const [selectedPersonalityIds, setSelectedPersonalityIds] = createSignal<
    string[]
  >([]);
  const [personalitiesLoading, setPersonalitiesLoading] = createSignal(false);
  const [createdGoalId, setCreatedGoalId] = createSignal<string | null>(null);
  const [failedAssignmentIds, setFailedAssignmentIds] = createSignal<string[]>(
    [],
  );
  const [assignmentLoadError, setAssignmentLoadError] = createSignal<
    string | null
  >(null);

  onMount(() => {
    if (!assignmentEnabled()) return;
    const owner = graph.state().owner;
    setPersonalitiesLoading(true);
    setAssignmentLoadError(null);
    void commands
      .listPersonalityInstances({
        principal: owner.principal,
        include_tombstoned: false,
      })
      .then((result) => {
        if (result.status === "error") {
          setAssignable([]);
          setSelectedPersonalityIds([]);
          setAssignmentLoadError(commandErrorMessage(result.error));
          return;
        }
        const candidates = goalReactivePersonalities(result.data);
        setAssignable(candidates);
        setSelectedPersonalityIds(candidates.map((candidate) => candidate.id));
      })
      .catch((loadError: unknown) => {
        setAssignable([]);
        setSelectedPersonalityIds([]);
        setAssignmentLoadError(
          loadError instanceof Error
            ? loadError.message
            : "Failed to load personalities",
        );
      })
      .finally(() => setPersonalitiesLoading(false));
  });

  const targetLabel = (id: string): string =>
    assignable().find((candidate) => candidate.id === id)?.label ?? id;

  const togglePersonality = (id: string, checked: boolean) => {
    if (createdGoalId() !== null) return;
    setSelectedPersonalityIds((current) => {
      if (checked) return current.includes(id) ? current : [...current, id];
      return current.filter((entry) => entry !== id);
    });
  };

  const onSchemaChange = (key: string) => {
    setSelectedKey(key);
    const editor = selected();
    if (editor !== null) setPayload(editor.defaults());
  };

  const assignGoal = async (
    owner: Owner,
    goalId: string,
    targetIds: string[],
  ): Promise<string[]> => {
    const failed: string[] = [];
    for (const targetId of targetIds) {
      const result = await commands.goalReactivate({
        principal: owner.principal,
        goal_id: goalId,
        target_personality_id: targetId,
      });
      if (result.status === "error") failed.push(targetId);
    }
    return failed;
  };

  const submit = async (event: Event) => {
    event.preventDefault();
    const editor = selected();
    if (editor === null) return;
    if (assignmentEnabled()) {
      if (personalitiesLoading()) return;
      if (assignable().length === 0) {
        setError("No goal-reactive personality is configured.");
        return;
      }
      if (createdGoalId() === null && selectedPersonalityIds().length === 0) {
        setError("Select at least one personality.");
        return;
      }
      if (createdGoalId() !== null && failedAssignmentIds().length === 0) {
        props.onAfterWrite();
        props.onClose();
        return;
      }
    }
    const codec = props.hub.codecFor(editor.schemaId, editor.schemaVersion);
    if (codec === null) {
      setError("No codec registered for this schema");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const owner = props.proposal?.owner ?? graph.state().owner;
      let goalId = createdGoalId();
      if (goalId === null) {
        const draft: GoalDraft = {
          principal: owner.principal,
          schema_id: editor.schemaId,
          schema_version: editor.schemaVersion,
          title: title(),
          text: text(),
          payload: Array.from(codec.encode(payload())),
          state: "Active",
          parent_goal_ids: props.proposal?.parent_goal_ids ?? [],
          supersedes_goal_id: props.proposal?.id ?? null,
          authorship: "User",
          request_id: `goal-dialog:${props.proposal?.id ?? "new"}:${Date.now()}`,
        };
        const result = await commands.goalWrite(draft);
        if (result.status === "error") {
          setError(JSON.stringify(result.error));
          return;
        }
        goalId = result.data.goal_id;
        setCreatedGoalId(goalId);
      }
      if (assignmentEnabled()) {
        const targets =
          failedAssignmentIds().length > 0
            ? failedAssignmentIds()
            : selectedPersonalityIds();
        const failed = await assignGoal(owner, goalId, targets);
        setFailedAssignmentIds(failed);
        if (failed.length > 0) {
          setError(
            `Goal created, assignment failed for: ${failed
              .map(targetLabel)
              .join(", ")}`,
          );
          return;
        }
      }
      props.onAfterWrite();
      props.onClose();
    } finally {
      setBusy(false);
    }
  };

  return (
    <Portal>
      <div
        class="goal-dialog-overlay"
        role="dialog"
        aria-modal="true"
        aria-label="Goal editor"
      >
      <form class="goal-dialog" onSubmit={submit}>
        <header class="goal-dialog-head">
          <h2>{props.proposal === undefined ? "New goal" : "Modify proposal"}</h2>
          <button
            type="button"
            class="goal-dialog-close"
            aria-label="Close goal dialog"
            onClick={props.onClose}
          >
            ×
          </button>
        </header>
        <Show
          when={editors.length > 0}
          fallback={
            <p class="proxima-dim">No goal payload editors registered.</p>
          }
        >
          <label class="goal-dialog-row">
            <span>Type</span>
            <select
              value={selectedKey() ?? ""}
              onChange={(event) => onSchemaChange(event.currentTarget.value)}
              disabled={props.proposal !== undefined || createdGoalId() !== null}
            >
              <For each={editors}>
                {(editor) => (
                  <option value={editorKey(editor)}>{editor.label}</option>
                )}
              </For>
            </select>
          </label>
          <label class="goal-editor-row">
            <span>Title</span>
            <input
              type="text"
              value={title()}
              disabled={createdGoalId() !== null}
              onInput={(event) => setTitle(event.currentTarget.value)}
            />
          </label>
          <label class="goal-editor-row">
            <span>Text</span>
            <textarea
              rows={4}
              value={text()}
              disabled={createdGoalId() !== null}
              onInput={(event) => setText(event.currentTarget.value)}
            />
          </label>
          <Show when={selected()} keyed>
            {(editor) => {
              const Editor = editor.component;
              return (
                <div class="goal-dialog-editor">
                  <Editor payload={payload()} onChange={setPayload} />
                </div>
              );
            }}
          </Show>
        </Show>
        <Show when={assignmentEnabled()}>
          <section class="goal-dialog-assignments">
            <div class="goal-dialog-section-head">Assign to</div>
            <Show
              when={!personalitiesLoading()}
              fallback={
                <p class="goal-dialog-empty">Loading goal-reactive personalities.</p>
              }
            >
              <Show when={assignmentLoadError() !== null}>
                <p class="goal-dialog-error">{assignmentLoadError()}</p>
              </Show>
              <Show
                when={assignable().length > 0}
                fallback={
                  <p class="goal-dialog-empty">
                    No goal-reactive personality is configured.
                  </p>
                }
              >
                <div class="goal-dialog-assignment-list">
                  <For each={assignable()}>
                    {(candidate) => (
                      <label
                        class="goal-dialog-assignment-row"
                        classList={{
                          failed: failedAssignmentIds().includes(candidate.id),
                        }}
                      >
                        <input
                          type="checkbox"
                          checked={selectedPersonalityIds().includes(candidate.id)}
                          disabled={createdGoalId() !== null}
                          onChange={(event) =>
                            togglePersonality(
                              candidate.id,
                              event.currentTarget.checked,
                            )
                          }
                        />
                        <span>
                          <strong>{candidate.label}</strong>
                          <em>{candidate.rootId}</em>
                        </span>
                      </label>
                    )}
                  </For>
                </div>
              </Show>
            </Show>
          </section>
        </Show>
        <Show when={error() !== null}>
          <p class="goal-dialog-error">{error()}</p>
        </Show>
        <footer class="goal-dialog-actions">
          <button type="button" disabled={busy()} onClick={props.onClose}>
            Cancel
          </button>
          <button
            type="submit"
            disabled={
              busy() ||
              selected() === null ||
              (assignmentEnabled() &&
                (personalitiesLoading() ||
                  assignable().length === 0 ||
                  (createdGoalId() === null &&
                    selectedPersonalityIds().length === 0)))
            }
          >
            {createdGoalId() !== null
              ? "Retry"
              : props.proposal === undefined
                ? "Create"
                : "Accept"}
          </button>
        </footer>
      </form>
      </div>
    </Portal>
  );
};
