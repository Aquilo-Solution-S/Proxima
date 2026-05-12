import {
  For,
  Show,
  createMemo,
  createSignal,
  type Accessor,
  type Component,
} from "solid-js";
import type {
  InferenceTargetConfigTs,
  InferenceTargetTs,
  InferenceTierBindingTs,
  Owner,
} from "../../bindings";
import type { EngineClient } from "../../client";
import { sentinelOwner } from "../../graph-store";
import {
  PRESET_TARGET_REFS,
  TARGET_KIND_OPTIONS,
  configFromDraft,
  configSummary as targetConfigSummary,
  draftForKind,
  kindLabel,
  type InferenceTargetKind,
  type TargetDraft,
} from "./constants";
import { TargetDraftEditor, TierPresetCard } from "./tier-preset-card";
import { TierBindingsSection } from "./tier-bindings-section";

interface Props {
  client: Pick<
    EngineClient,
    | "registerInferenceTarget"
    | "removeInferenceTarget"
    | "bindInferenceTier"
  >;
  targets: Accessor<InferenceTargetTs[] | undefined>;
  refetchTargets: () => void;
  onChanged: () => void;
  owner?: Owner;
  bindings?: Accessor<InferenceTierBindingTs[] | undefined>;
  refetchBindings?: () => void;
}

const errorMessage = (err: unknown): string => {
  if (err && typeof err === "object") {
    if ("code" in err && "message" in err) {
      const code = (err as { code: unknown }).code;
      const message = (err as { message: unknown }).message;
      return `${String(code)}: ${String(message)}`;
    }
    if ("message" in err) {
      const message = (err as { message: unknown }).message;
      if (typeof message === "string") return message;
    }
  }
  return String(err);
};

const configSummary = (target: InferenceTargetTs): string => {
  return targetConfigSummary(target.config);
};

export const InferenceTargetsSection: Component<Props> = (props) => {
  const owner = () => props.owner ?? sentinelOwner();
  const [targetRef, setTargetRef] = createSignal("");
  const [draft, setDraft] = createSignal<TargetDraft>(draftForKind("mistral_chat"));
  const [error, setError] = createSignal<string | null>(null);

  const customTargets = createMemo(() =>
    (props.targets() ?? []).filter((t) => !PRESET_TARGET_REFS.has(t.target_ref)),
  );

  const clearForm = () => {
    setTargetRef("");
    setDraft(draftForKind("mistral_chat"));
  };

  const switchKind = (kind: InferenceTargetKind) => {
    setDraft(draftForKind(kind));
  };

  const updateDraft = (patch: Partial<TargetDraft>) =>
    setDraft({ ...draft(), ...patch });

  const submit = async (event: Event) => {
    event.preventDefault();
    setError(null);
    const ref = targetRef().trim();
    const config: InferenceTargetConfigTs = configFromDraft(draft());
    try {
      await props.client.registerInferenceTarget({
        owner: owner(),
        target_ref: ref,
        config,
      });
      clearForm();
      props.refetchTargets();
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const remove = async (targetRefToRemove: string) => {
    setError(null);
    try {
      await props.client.removeInferenceTarget({
        owner: owner(),
        target_ref: targetRefToRemove,
      });
      props.refetchTargets();
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  return (
    <section>
      <h2>Inference targets</h2>
      <Show when={error()}>
        {(message) => (
          <p class="proxima-error" role="alert">
            {message()}
          </p>
        )}
      </Show>

      <TierPresetCard
        client={props.client}
        owner={owner()}
        targets={props.targets}
        bindings={props.bindings}
        onChanged={props.onChanged}
      />

      <details class="proxima-models-form">
        <summary>Custom target (advanced)</summary>

        <Show when={customTargets().length > 0}>
          <h4 class="proxima-models-subhead">Existing custom targets</h4>
          <table class="proxima-models-table">
            <thead>
              <tr>
                <th>Target</th>
                <th>Kind</th>
                <th>Details</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              <For each={customTargets()}>
                {(target) => (
                  <tr>
                    <td>
                      <span class="proxima-mono">{target.target_ref}</span>
                    </td>
                    <td>{kindLabel(target.config.kind)}</td>
                    <td>
                      <code>{configSummary(target)}</code>
                    </td>
                    <td>
                      <button
                        type="button"
                        class="proxima-btn proxima-btn-danger"
                        onClick={() => void remove(target.target_ref)}
                      >
                        remove
                      </button>
                    </td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>

          <Show when={props.bindings && props.refetchBindings}>
            <h4 class="proxima-models-subhead">Tier bindings</h4>
            <p class="proxima-dim proxima-models-helper">
              Override which target a tier resolves to. Defaults bind each tier
              to its native preset target.
            </p>
            <TierBindingsSection
              client={props.client}
              owner={owner()}
              targets={props.targets}
              bindings={props.bindings!}
              refetchBindings={props.refetchBindings!}
              embedded
            />
          </Show>
        </Show>

        <h4 class="proxima-models-subhead">Register custom target</h4>
        <form onSubmit={(event) => void submit(event)}>
          <div class="proxima-models-form-grid">
            <label for="inference-target-ref">Target ref</label>
            <input
              id="inference-target-ref"
              type="text"
              value={targetRef()}
              onInput={(event) => setTargetRef(event.currentTarget.value)}
            />

            <label for="inference-target-kind">Kind</label>
            <select
              id="inference-target-kind"
              value={draft().kind}
              onChange={(event) =>
                switchKind(event.currentTarget.value as InferenceTargetKind)
              }
            >
              <For each={TARGET_KIND_OPTIONS}>
                {(option) => <option value={option.kind}>{option.label}</option>}
              </For>
            </select>

            <TargetDraftEditor
              draft={draft()}
              onUpdate={updateDraft}
              idPrefix="custom-target"
            />
          </div>
          <div class="proxima-models-form-actions">
            <button type="submit" class="proxima-btn">
              register
            </button>
          </div>
        </form>
      </details>
    </section>
  );
};
