import { For, Show, createSignal, type Component } from "solid-js";
import type { Owner } from "../../bindings";
import type { EngineClient } from "../../client";
import {
  TARGET_KIND_OPTIONS,
  configFromDraft,
  draftForKind,
  type InferenceTargetKind,
  type TargetDraft,
} from "./constants";
import { TargetDraftEditor } from "./target-draft-editor";

interface Props {
  client: Pick<EngineClient, "registerInferenceTarget">;
  owner: Owner;
  existingRefs: string[];
  onClose: () => void;
  onRegistered: () => void;
}

const errorMessage = (err: unknown): string => {
  if (err && typeof err === "object") {
    if ("code" in err && "message" in err) {
      return `${String((err as { code: unknown }).code)}: ${String((err as { message: unknown }).message)}`;
    }
    if ("message" in err && typeof (err as { message: unknown }).message === "string") {
      return (err as { message: string }).message;
    }
  }
  return String(err);
};

export const RegisterModelModal: Component<Props> = (props) => {
  const [targetRef, setTargetRef] = createSignal("");
  const [draft, setDraft] = createSignal<TargetDraft>(
    draftForKind("mistral_chat"),
  );
  const [error, setError] = createSignal<string | null>(null);

  const switchKind = (kind: InferenceTargetKind) => {
    setDraft(draftForKind(kind));
  };

  const updateDraft = (patch: Partial<TargetDraft>) =>
    setDraft({ ...draft(), ...patch });

  const submit = async (event: Event) => {
    event.preventDefault();
    setError(null);
    const ref = targetRef().trim();
    if (ref.length === 0) {
      setError("Target ref is required.");
      return;
    }
    if (props.existingRefs.includes(ref)) {
      setError("A target with this ref already exists.");
      return;
    }
    try {
      await props.client.registerInferenceTarget({
        principal: props.owner.principal,
        target_ref: ref,
        config: configFromDraft(draft()),
      });
      props.onRegistered();
      props.onClose();
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  return (
    <div class="proxima-modal-backdrop" role="dialog" aria-modal="true">
      <div class="proxima-modal proxima-register-model-modal">
        <header class="proxima-modal-head">
          <h3>Register model</h3>
          <button
            type="button"
            class="proxima-btn-link"
            aria-label="close"
            onClick={props.onClose}
          >
            close
          </button>
        </header>

        <form onSubmit={(event) => void submit(event)}>
          <div class="proxima-target-editor-grid">
            <label for="register-target-ref">Target ref</label>
            <input
              id="register-target-ref"
              type="text"
              value={targetRef()}
              onInput={(event) => setTargetRef(event.currentTarget.value)}
            />

            <label for="register-kind">Kind</label>
            <select
              id="register-kind"
              value={draft().kind}
              onChange={(event) =>
                switchKind(event.currentTarget.value as InferenceTargetKind)
              }
            >
              <For each={TARGET_KIND_OPTIONS}>
                {(option) => (
                  <option value={option.kind}>{option.label}</option>
                )}
              </For>
            </select>
          </div>

          <TargetDraftEditor draft={draft()} onUpdate={updateDraft} />

          <Show when={error()}>
            {(message) => (
              <p class="proxima-error" role="alert">
                {message()}
              </p>
            )}
          </Show>

          <div class="proxima-modal-actions">
            <button
              type="button"
              class="proxima-btn"
              onClick={props.onClose}
            >
              cancel
            </button>
            <button type="submit" class="proxima-btn proxima-btn-primary">
              register
            </button>
          </div>
        </form>
      </div>
    </div>
  );
};
