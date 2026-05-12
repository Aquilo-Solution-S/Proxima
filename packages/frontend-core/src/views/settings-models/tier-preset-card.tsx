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
  ModelTierTs,
  Owner,
} from "../../bindings";
import type { EngineClient } from "../../client";
import {
  DEFAULT_TIER_PRESETS,
  TIERS,
  configFromDraft,
  defaultPresetForTier,
  draftFromConfig,
  kindLabel,
  sameConfig,
  targetRefForCollision,
  targetRefForTier,
  type TargetDraft,
} from "./constants";

interface Props {
  client: Pick<
    EngineClient,
    "registerInferenceTarget" | "removeInferenceTarget" | "bindInferenceTier"
  >;
  owner: Owner;
  targets: Accessor<InferenceTargetTs[] | undefined>;
  bindings?: Accessor<InferenceTierBindingTs[] | undefined>;
  onChanged: () => void;
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

const draftForTier = (tier: ModelTierTs): TargetDraft =>
  draftFromConfig(defaultPresetForTier(tier).config);

export const TierPresetCard: Component<Props> = (props) => {
  const [error, setError] = createSignal<string | null>(null);
  const [editingTier, setEditingTier] = createSignal<ModelTierTs | null>(null);
  const [draft, setDraft] = createSignal<TargetDraft>(draftForTier("fast"));

  const targetByRef = createMemo(() => {
    const map = new Map<string, InferenceTargetTs>();
    for (const target of props.targets() ?? []) map.set(target.target_ref, target);
    return map;
  });

  const boundTargetRef = (tier: ModelTierTs) =>
    props.bindings?.()?.find((binding) => binding.tier === tier)?.target_ref ??
    targetRefForTier(tier);

  const targetForTier = (tier: ModelTierTs) =>
    targetByRef().get(boundTargetRef(tier)) ??
    targetByRef().get(targetRefForTier(tier));

  const reasoningEffort = (target: InferenceTargetTs): string | null =>
    target.config.kind === "openai_responses"
      ? target.config.reasoning_effort
      : null;

  const configuredCount = () =>
    TIERS.filter((tier) => targetForTier(tier) !== undefined).length;

  const beginEdit = (tier: ModelTierTs) => {
    setError(null);
    setDraft(
      targetForTier(tier)
        ? draftFromConfig(targetForTier(tier)!.config)
        : draftForTier(tier),
    );
    setEditingTier(tier);
  };

  const cancelEdit = () => {
    setError(null);
    setEditingTier(null);
  };

  const updateDraft = (patch: Partial<TargetDraft>) =>
    setDraft({ ...draft(), ...patch });

  const persistTier = async (
    tier: ModelTierTs,
    config: InferenceTargetConfigTs,
  ) => {
    const existing = (props.targets() ?? []).find((target) =>
      sameConfig(target.config, config),
    );
    let targetRef = existing?.target_ref;
    if (!targetRef) {
      const defaultRef = targetRefForTier(tier);
      const defaultTarget = targetByRef().get(defaultRef);
      targetRef =
        !defaultTarget || sameConfig(defaultTarget.config, config)
          ? defaultRef
          : targetRefForCollision(tier, config);
      await props.client.registerInferenceTarget({
        owner: props.owner,
        target_ref: targetRef,
        config,
      });
    }
    await props.client.bindInferenceTier({
      owner: props.owner,
      tier,
      target_ref: targetRef,
    });
  };

  const saveCurrent = async () => {
    const tier = editingTier();
    if (!tier) return;
    setError(null);
    try {
      await persistTier(tier, configFromDraft(draft()));
      setEditingTier(null);
      props.onChanged();
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const removeTier = async (tier: ModelTierTs) => {
    setError(null);
    try {
      await props.client.removeInferenceTarget({
        owner: props.owner,
        target_ref: boundTargetRef(tier),
      });
      if (editingTier() === tier) setEditingTier(null);
      props.onChanged();
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const submitDefaults = async () => {
    setError(null);
    try {
      for (const preset of DEFAULT_TIER_PRESETS) {
        await persistTier(preset.tier, preset.config);
      }
      setEditingTier(null);
      props.onChanged();
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  return (
    <section class="proxima-tier-panel" aria-labelledby="native-tiers-title">
      <header class="proxima-tier-panel-head">
        <h3 id="native-tiers-title" class="proxima-tier-panel-title">
          Native tiers
        </h3>
        <span class="proxima-tier-status">
          {configuredCount()} / {TIERS.length} configured
        </span>
      </header>

      <Show when={error()}>
        {(message) => (
          <p class="proxima-error" role="alert">
            {message()}
          </p>
        )}
      </Show>

      <ul class="proxima-tier-list">
        <For each={TIERS}>
          {(tier) => {
            const target = () => targetForTier(tier);
            const isEditing = () => editingTier() === tier;
            const editorId = `native-${tier}-editor`;
            return (
              <li
                class="proxima-tier-row"
                classList={{
                  "is-editing": isEditing(),
                  "is-empty": !target(),
                }}
              >
                <div class="proxima-tier-summary">
                  <span class="proxima-tier-label">{tier}</span>
                  <div class="proxima-tier-detail">
                    <Show
                      when={target()}
                      fallback={<span class="proxima-dim">not configured</span>}
                    >
                      {(row) => (
                        <>
                          <span class="proxima-tier-model">
                            <span>{kindLabel(row().config.kind)}</span>
                            <span class="proxima-tier-sep">/</span>
                            <span class="proxima-mono">
                              {row().config.model_id}
                            </span>
                          </span>
                          <span class="proxima-target-tag">
                            {row().target_ref}
                          </span>
                          <Show
                            when={reasoningEffort(row())}
                          >
                            {(reasoning) => (
                              <span class="proxima-target-tag">
                                {reasoning()} reasoning
                              </span>
                            )}
                          </Show>
                        </>
                      )}
                    </Show>
                  </div>
                  <div class="proxima-tier-actions">
                    <Show
                      when={isEditing()}
                      fallback={
                        <>
                          <button
                            type="button"
                            class="proxima-btn-link"
                            aria-label={`${target() ? "Edit" : "Configure"} ${tier}`}
                            aria-expanded={false}
                            aria-controls={editorId}
                            onClick={() => beginEdit(tier)}
                          >
                            {target() ? "Edit" : "Configure"}
                          </button>
                          <Show when={target()}>
                            <button
                              type="button"
                              class="proxima-btn-link proxima-btn-link-danger"
                              aria-label={`Remove ${tier}`}
                              onClick={() => void removeTier(tier)}
                            >
                              Remove
                            </button>
                          </Show>
                        </>
                      }
                    >
                      <button
                        type="button"
                        class="proxima-btn-link"
                        aria-label={`Cancel ${tier}`}
                        onClick={cancelEdit}
                      >
                        Cancel
                      </button>
                    </Show>
                  </div>
                </div>

                <Show when={isEditing()}>
                  <div id={editorId} class="proxima-tier-editor">
                    <TargetDraftEditor
                      draft={draft()}
                      onUpdate={updateDraft}
                      idPrefix={`native-${tier}-target`}
                    />
                    <div class="proxima-tier-editor-actions">
                      <button
                        type="button"
                        class="proxima-btn proxima-btn-primary"
                        onClick={() => void saveCurrent()}
                      >
                        Save
                      </button>
                      <button
                        type="button"
                        class="proxima-btn"
                        onClick={cancelEdit}
                      >
                        Cancel
                      </button>
                    </div>
                  </div>
                </Show>
              </li>
            );
          }}
        </For>
      </ul>

      <Show when={configuredCount() < 3}>
        <div class="proxima-tier-cta">
          <button
            type="button"
            classList={{
              "proxima-btn": true,
              "proxima-btn-primary": configuredCount() === 0,
            }}
            onClick={() => void submitDefaults()}
          >
            {configuredCount() === 0
              ? "Set up all 3 tiers (recommended)"
              : "Fill missing tiers from defaults"}
          </button>
        </div>
      </Show>
    </section>
  );
};

export const TargetDraftEditor: Component<{
  draft: TargetDraft;
  onUpdate: (patch: Partial<TargetDraft>) => void;
  idPrefix?: string;
}> = (props) => {
  const id = (suffix: string) => `${props.idPrefix ?? "native-target"}-${suffix}`;
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
      onInput={(event) => props.onUpdate({ modelId: event.currentTarget.value })}
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

      <label for={id("max-completion-tokens")}>
        max_completion_tokens
      </label>
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
