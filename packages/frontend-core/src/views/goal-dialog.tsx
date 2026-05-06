import {
  For,
  Show,
  createMemo,
  createSignal,
  type Component,
} from "solid-js";
import { Portal } from "solid-js/web";
import {
  registeredGoalPayloadEditors,
  type RegisteredGoalPayloadEditor,
} from "../registry";
import { useGraph } from "../graph-store";
import type { Hub } from "../hub";
import { commands, type GoalDraft, type GoalRow } from "../bindings";

export interface GoalDialogProps {
  hub: Hub;
  /** Optional proposal being modified — pre-populates schema + payload. */
  proposal?: GoalRow;
  onClose: () => void;
  onAfterWrite: () => void;
}

const editorKey = (editor: RegisteredGoalPayloadEditor): string =>
  `${editor.schemaId}@${editor.schemaVersion}`;

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

  const onSchemaChange = (key: string) => {
    setSelectedKey(key);
    const editor = selected();
    if (editor !== null) setPayload(editor.defaults());
  };

  const submit = async (event: Event) => {
    event.preventDefault();
    const editor = selected();
    if (editor === null) return;
    const codec = props.hub.codecFor(editor.schemaId, editor.schemaVersion);
    if (codec === null) {
      setError("No codec registered for this schema");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const draft: GoalDraft = {
        owner: props.proposal?.owner ?? graph.state().owner,
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
              disabled={props.proposal !== undefined}
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
              onInput={(event) => setTitle(event.currentTarget.value)}
            />
          </label>
          <label class="goal-editor-row">
            <span>Text</span>
            <textarea
              rows={4}
              value={text()}
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
        <Show when={error() !== null}>
          <p class="goal-dialog-error">{error()}</p>
        </Show>
        <footer class="goal-dialog-actions">
          <button type="button" disabled={busy()} onClick={props.onClose}>
            Cancel
          </button>
          <button type="submit" disabled={busy() || selected() === null}>
            {props.proposal === undefined ? "Create" : "Accept"}
          </button>
        </footer>
      </form>
      </div>
    </Portal>
  );
};
