import { For, Show, type Component } from "solid-js";
import type { TargetDraft } from "./constants";

export const TargetDraftEditor: Component<{
  draft: TargetDraft;
  onUpdate: (patch: Partial<TargetDraft>) => void;
  idPrefix?: string;
}> = (props) => {
  const id = (suffix: string) =>
    `${props.idPrefix ?? "register-target"}-${suffix}`;
  return (
    <div class="proxima-target-editor-grid">
      <label for={id("base-url")}>base_url</label>
      <input
        id={id("base-url")}
        value={props.draft.baseUrl}
        onInput={(event) =>
          props.onUpdate({ baseUrl: event.currentTarget.value })
        }
      />

      <label for={id("model-id")}>model_id</label>
      <input
        id={id("model-id")}
        value={props.draft.modelId}
        onInput={(event) =>
          props.onUpdate({ modelId: event.currentTarget.value })
        }
      />

      <label for={id("api-key-env")}>api_key_env</label>
      <input
        id={id("api-key-env")}
        value={props.draft.apiKeyEnv}
        onInput={(event) =>
          props.onUpdate({ apiKeyEnv: event.currentTarget.value })
        }
      />

      <Show when={props.draft.kind !== "openai_responses"}>
        <label for={id("temperature")}>temperature</label>
        <input
          id={id("temperature")}
          type="number"
          step="0.01"
          value={props.draft.temperature}
          onInput={(event) =>
            props.onUpdate({ temperature: event.currentTarget.value })
          }
        />

        <label for={id("max-completion-tokens")}>max_completion_tokens</label>
        <input
          id={id("max-completion-tokens")}
          type="number"
          min="1"
          step="1"
          value={props.draft.maxCompletionTokens}
          onInput={(event) =>
            props.onUpdate({ maxCompletionTokens: event.currentTarget.value })
          }
        />
      </Show>

      <Show when={props.draft.kind === "openai_responses"}>
        <label for={id("reasoning-effort")}>reasoning_effort</label>
        <select
          id={id("reasoning-effort")}
          value={props.draft.reasoningEffort}
          onChange={(event) =>
            props.onUpdate({ reasoningEffort: event.currentTarget.value })
          }
        >
          <option value="">(default)</option>
          <For each={["low", "medium", "high", "xhigh"]}>
            {(effort) => <option value={effort}>{effort}</option>}
          </For>
        </select>
      </Show>
    </div>
  );
};
