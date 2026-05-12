import { open } from "@tauri-apps/plugin-dialog";
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
import { GoosePresetCard } from "./goose-preset-card";
import { SYSTEM_PROMPT_ENV, TIERS, decodeGooseConfig } from "./constants";
import { TierBindingsSection } from "./tier-bindings-section";

interface Props {
  client: Pick<
    EngineClient,
    | "detectLocalHarness"
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

const systemPromptFromConfig = (
  config: InferenceTargetConfigTs,
): string | undefined => {
  if (config.kind !== "local_cli") return undefined;
  return config.env_overrides.find(([key]) => key === SYSTEM_PROMPT_ENV)?.[1];
};

const configSummary = (target: InferenceTargetTs): string => {
  switch (target.config.kind) {
    case "local_cli": {
      const decoded = decodeGooseConfig(target.config);
      if (decoded?.provider && decoded.model) {
        const reasoning = decoded.reasoning ? ` · ${decoded.reasoning}` : "";
        return `${decoded.provider} / ${decoded.model}${reasoning}`;
      }
      const prompt = systemPromptFromConfig(target.config);
      return prompt ? "system prompt set" : "no system prompt";
    }
    case "remote_model":
      return `${target.config.vendor} / ${target.config.model_id}`;
  }
};

const kindLabel = (kind: InferenceTargetConfigTs["kind"]): string => {
  switch (kind) {
    case "local_cli":
      return "Local CLI";
    case "remote_model":
      return "Remote model";
  }
};

export const InferenceTargetsSection: Component<Props> = (props) => {
  const owner = () => props.owner ?? sentinelOwner();
  const [targetRef, setTargetRef] = createSignal("");
  const [kind, setKind] = createSignal<InferenceTargetConfigTs["kind"]>("local_cli");
  const [systemPrompt, setSystemPrompt] = createSignal("");
  const [vendor, setVendor] = createSignal("");
  const [dialect, setDialect] = createSignal("");
  const [modelId, setModelId] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);

  const presetRefs = new Set(TIERS.map((tier) => `goose-${tier}`));
  const customTargets = createMemo(() =>
    (props.targets() ?? []).filter((t) => !presetRefs.has(t.target_ref)),
  );

  const clearForm = () => {
    setTargetRef("");
    setSystemPrompt("");
    setVendor("");
    setDialect("");
    setModelId("");
  };

  const chooseTargetPath = async () => {
    setError(null);
    try {
      const selected = await open({
        directory: false,
        multiple: false,
        title: "Select inference target",
      });
      if (typeof selected === "string") setTargetRef(selected);
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const submit = async (event: Event) => {
    event.preventDefault();
    setError(null);
    const prompt = systemPrompt().trim();
    const config: InferenceTargetConfigTs =
      kind() === "local_cli"
        ? {
            kind: "local_cli",
            command: targetRef(),
            profile: null,
            env_overrides: prompt.length > 0 ? [[SYSTEM_PROMPT_ENV, prompt]] : [],
          }
        : {
            kind: "remote_model",
            vendor: vendor(),
            dialect: dialect(),
            model_id: modelId(),
            credentials_ref: null,
          };
    try {
      await props.client.registerInferenceTarget({
        owner: owner(),
        target_ref: targetRef(),
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

      <GoosePresetCard
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
              Override which target a tier resolves to. Defaults bind each
              tier to its <span class="proxima-mono">goose-&lt;tier&gt;</span>{" "}
              preset.
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
            <label for="inference-target-ref">Target path</label>
            <div class="proxima-models-path-row">
              <input
                id="inference-target-ref"
                type="text"
                value={targetRef()}
                onInput={(event) => setTargetRef(event.currentTarget.value)}
              />
              <button
                type="button"
                class="proxima-btn"
                onClick={() => void chooseTargetPath()}
              >
                select
              </button>
            </div>

            <label for="inference-target-kind">Kind</label>
            <select
              id="inference-target-kind"
              value={kind()}
              onChange={(event) =>
                setKind(event.currentTarget.value as InferenceTargetConfigTs["kind"])
              }
            >
              <option value="local_cli">Local CLI</option>
              <option value="remote_model">Remote model</option>
            </select>

            <Show when={kind() === "local_cli"}>
              <label for="inference-target-system-prompt">System Prompt</label>
              <textarea
                id="inference-target-system-prompt"
                rows={5}
                value={systemPrompt()}
                onInput={(event) => setSystemPrompt(event.currentTarget.value)}
              />
            </Show>

            <Show when={kind() === "remote_model"}>
              <label for="inference-target-vendor">vendor</label>
              <input
                id="inference-target-vendor"
                type="text"
                value={vendor()}
                onInput={(event) => setVendor(event.currentTarget.value)}
              />

              <label for="inference-target-dialect">dialect</label>
              <input
                id="inference-target-dialect"
                type="text"
                value={dialect()}
                onInput={(event) => setDialect(event.currentTarget.value)}
              />

              <label for="inference-target-model-id">model_id</label>
              <input
                id="inference-target-model-id"
                type="text"
                value={modelId()}
                onInput={(event) => setModelId(event.currentTarget.value)}
              />
            </Show>
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
