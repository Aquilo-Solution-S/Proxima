import { open } from "@tauri-apps/plugin-dialog";
import {
  For,
  Show,
  createMemo,
  createSignal,
  onMount,
  type Accessor,
  type Component,
} from "solid-js";
import type { InferenceTargetTs, ModelTierTs, Owner } from "../../bindings";
import type { EngineClient } from "../../client";
import {
  DEFAULT_TIER_PRESETS,
  GOOSE_MODELS,
  GOOSE_PROVIDERS,
  REASONING_EFFORTS,
  TIERS,
  decodeGooseConfig,
  modelLabel,
  providerLabel,
  type GooseProvider,
  type GooseReasoningEffort,
} from "./constants";

type DetectStatus = "idle" | "detecting" | "found" | "missing";

interface Props {
  client: Pick<
    EngineClient,
    | "detectLocalHarness"
    | "registerInferenceTarget"
    | "removeInferenceTarget"
    | "bindInferenceTier"
  >;
  owner: Owner;
  targets: Accessor<InferenceTargetTs[] | undefined>;
  onChanged: () => void;
}

interface Draft {
  provider: GooseProvider;
  model: string;
  reasoning: GooseReasoningEffort;
  systemPrompt: string;
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

const targetRefForTier = (tier: ModelTierTs): string => `goose-${tier}`;

const buildEnvOverrides = (
  provider: GooseProvider,
  model: string,
  reasoning: GooseReasoningEffort,
  systemPrompt: string,
): [string, string][] => {
  const env: [string, string][] = [
    ["GOOSE_PROVIDER", provider],
    ["GOOSE_MODEL", model],
  ];
  if (provider === "chatgpt_codex") {
    env.push(["CHATGPT_CODEX_REASONING_EFFORT", reasoning]);
  }
  const prompt = systemPrompt.trim();
  if (prompt.length > 0) env.push(["PROXIMA_SYSTEM_PROMPT", prompt]);
  return env;
};

const draftFromDefaults = (tier: ModelTierTs): Draft => {
  const preset = DEFAULT_TIER_PRESETS.find((p) => p.tier === tier);
  return {
    provider: (preset?.provider ?? "mistral") as GooseProvider,
    model: preset?.model ?? GOOSE_MODELS.mistral[0].id,
    reasoning: (preset?.reasoning ?? "medium") as GooseReasoningEffort,
    systemPrompt: "",
  };
};

export const GoosePresetCard: Component<Props> = (props) => {
  const [goosePath, setGoosePath] = createSignal("");
  const [detectStatus, setDetectStatus] = createSignal<DetectStatus>("idle");
  const [detectedVersion, setDetectedVersion] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const [editingTier, setEditingTier] = createSignal<ModelTierTs | null>(null);
  const [draft, setDraft] = createSignal<Draft>(draftFromDefaults("fast"));

  const targetByTier = createMemo(() => {
    const list = props.targets() ?? [];
    const map = new Map<ModelTierTs, InferenceTargetTs>();
    for (const tier of TIERS) {
      const found = list.find((x) => x.target_ref === targetRefForTier(tier));
      if (found) map.set(tier, found);
    }
    return map;
  });

  const decodedFor = (tier: ModelTierTs) => {
    const target = targetByTier().get(tier);
    return target ? decodeGooseConfig(target.config) : null;
  };

  const configuredCount = () => targetByTier().size;

  const detectGoose = async () => {
    setError(null);
    setDetectStatus("detecting");
    try {
      const detected = await props.client.detectLocalHarness("goose");
      if (detected) {
        setGoosePath(detected.path);
        setDetectedVersion(detected.version);
        setDetectStatus("found");
      } else {
        setDetectedVersion("");
        setDetectStatus("missing");
      }
    } catch (err) {
      setDetectStatus("missing");
      setDetectedVersion("");
      setError(errorMessage(err));
    }
  };

  onMount(() => {
    void detectGoose();
  });

  const chooseGoosePath = async () => {
    setError(null);
    try {
      const selected = await open({
        directory: false,
        multiple: false,
        title: "Select goose binary",
      });
      if (typeof selected === "string") {
        setGoosePath(selected);
        setDetectStatus("found");
      }
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const beginEdit = (tier: ModelTierTs) => {
    setError(null);
    const decoded = decodedFor(tier);
    setDraft(
      decoded
        ? {
            provider: (decoded.provider ?? "mistral") as GooseProvider,
            model: decoded.model ?? GOOSE_MODELS.mistral[0].id,
            reasoning: (decoded.reasoning ?? "medium") as GooseReasoningEffort,
            systemPrompt: decoded.systemPrompt ?? "",
          }
        : draftFromDefaults(tier),
    );
    setEditingTier(tier);
  };

  const cancelEdit = () => {
    setError(null);
    setEditingTier(null);
  };

  const updateDraft = (patch: Partial<Draft>) =>
    setDraft({ ...draft(), ...patch });

  const switchProvider = (next: GooseProvider) => {
    const list = GOOSE_MODELS[next];
    const stillValid = list.some((m) => m.id === draft().model);
    setDraft({
      ...draft(),
      provider: next,
      model: stillValid ? draft().model : list[0].id,
    });
  };

  const persistTier = async (
    tier: ModelTierTs,
    provider: GooseProvider,
    model: string,
    reasoning: GooseReasoningEffort,
    systemPrompt: string,
  ) => {
    const command = goosePath().trim();
    if (!command) throw new Error("goose path is required");
    const targetRef = targetRefForTier(tier);
    await props.client.registerInferenceTarget({
      owner: props.owner,
      target_ref: targetRef,
      config: {
        kind: "local_cli",
        command,
        profile: null,
        env_overrides: buildEnvOverrides(provider, model, reasoning, systemPrompt),
      },
    });
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
    const d = draft();
    try {
      await persistTier(tier, d.provider, d.model, d.reasoning, d.systemPrompt);
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
        target_ref: targetRefForTier(tier),
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
        await persistTier(
          preset.tier,
          preset.provider as GooseProvider,
          preset.model,
          (preset.reasoning ?? "medium") as GooseReasoningEffort,
          "",
        );
      }
      setEditingTier(null);
      props.onChanged();
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  return (
    <section class="proxima-goose-panel" aria-labelledby="goose-tiers-title">
      <header class="proxima-goose-panel-head">
        <h3 id="goose-tiers-title" class="proxima-goose-panel-title">
          Goose tiers
        </h3>
        <div class="proxima-goose-status">
          <Show when={detectStatus() === "idle"}>
            <span class="proxima-dim">checking goose…</span>
          </Show>
          <Show when={detectStatus() === "detecting"}>
            <span class="proxima-dim">detecting…</span>
          </Show>
          <Show when={detectStatus() === "found"}>
            <span class="proxima-goose-pill proxima-goose-pill-ok">
              goose {detectedVersion() || "detected"}
            </span>
            <span class="proxima-mono proxima-dim proxima-goose-path" title={goosePath()}>
              {goosePath()}
            </span>
            <button
              type="button"
              class="proxima-btn-link"
              onClick={() => void chooseGoosePath()}
            >
              change
            </button>
          </Show>
          <Show when={detectStatus() === "missing"}>
            <span class="proxima-goose-pill proxima-goose-pill-warn">
              goose missing from PATH
            </span>
            <button
              type="button"
              class="proxima-btn-link"
              onClick={() => void detectGoose()}
            >
              retry
            </button>
            <button
              type="button"
              class="proxima-btn-link"
              onClick={() => void chooseGoosePath()}
            >
              browse
            </button>
          </Show>
        </div>
      </header>

      <Show when={error()}>
        {(message) => (
          <p class="proxima-error" role="alert">
            {message()}
          </p>
        )}
      </Show>

      <ul class="proxima-goose-tiers">
        <For each={TIERS}>
          {(tier) => {
            const decoded = () => decodedFor(tier);
            const isEditing = () => editingTier() === tier;
            const editorId = `goose-${tier}-editor`;
            return (
              <li
                class="proxima-goose-tier"
                classList={{
                  "is-editing": isEditing(),
                  "is-empty": !decoded(),
                }}
              >
                <div class="proxima-goose-tier-summary">
                  <span class="proxima-goose-tier-label">{tier}</span>
                  <div class="proxima-goose-tier-detail">
                    <Show
                      when={decoded()}
                      fallback={<span class="proxima-dim">not configured</span>}
                    >
                      {(d) => (
                        <>
                          <span class="proxima-goose-tier-model">
                            <span>{providerLabel(d().provider)}</span>
                            <span class="proxima-goose-tier-sep">/</span>
                            <span class="proxima-mono">
                              {modelLabel(d().provider, d().model)}
                            </span>
                          </span>
                          <Show when={d().reasoning}>
                            {(r) => (
                              <span class="proxima-goose-tag">{r()} reasoning</span>
                            )}
                          </Show>
                          <Show when={d().systemPrompt}>
                            <span class="proxima-goose-tag proxima-goose-tag-soft">
                              system prompt
                            </span>
                          </Show>
                        </>
                      )}
                    </Show>
                  </div>
                  <div class="proxima-goose-tier-actions">
                    <Show
                      when={isEditing()}
                      fallback={
                        <>
                          <button
                            type="button"
                            class="proxima-btn-link"
                            aria-label={`${decoded() ? "Edit" : "Configure"} ${tier}`}
                            aria-expanded={false}
                            aria-controls={editorId}
                            onClick={() => beginEdit(tier)}
                          >
                            {decoded() ? "Edit" : "Configure"}
                          </button>
                          <Show when={decoded()}>
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
                  <div id={editorId} class="proxima-goose-tier-editor">
                    <div class="proxima-goose-editor-grid">
                      <label for={`goose-${tier}-provider`}>Provider</label>
                      <select
                        id={`goose-${tier}-provider`}
                        value={draft().provider}
                        onChange={(event) =>
                          switchProvider(event.currentTarget.value as GooseProvider)
                        }
                      >
                        <For each={GOOSE_PROVIDERS}>
                          {(entry) => (
                            <option value={entry.id}>{entry.label}</option>
                          )}
                        </For>
                      </select>

                      <label for={`goose-${tier}-model`}>Model</label>
                      <select
                        id={`goose-${tier}-model`}
                        value={draft().model}
                        onChange={(event) =>
                          updateDraft({ model: event.currentTarget.value })
                        }
                      >
                        <For each={GOOSE_MODELS[draft().provider]}>
                          {(entry) => (
                            <option value={entry.id}>{entry.label}</option>
                          )}
                        </For>
                      </select>

                      <Show when={draft().provider === "chatgpt_codex"}>
                        <label for={`goose-${tier}-reasoning`}>Reasoning</label>
                        <select
                          id={`goose-${tier}-reasoning`}
                          value={draft().reasoning}
                          onChange={(event) =>
                            updateDraft({
                              reasoning: event.currentTarget.value as GooseReasoningEffort,
                            })
                          }
                        >
                          <For each={REASONING_EFFORTS}>
                            {(effort) => <option value={effort}>{effort}</option>}
                          </For>
                        </select>
                      </Show>

                      <label for={`goose-${tier}-prompt`}>System prompt</label>
                      <textarea
                        id={`goose-${tier}-prompt`}
                        rows={4}
                        value={draft().systemPrompt}
                        onInput={(event) =>
                          updateDraft({ systemPrompt: event.currentTarget.value })
                        }
                        placeholder="optional · e.g., 'Stay inside the Proxima MCP surface.'"
                      />
                    </div>
                    <div class="proxima-goose-editor-actions">
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
        <div class="proxima-goose-cta">
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
