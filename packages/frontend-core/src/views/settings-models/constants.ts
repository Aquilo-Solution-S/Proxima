import type { InferenceTargetConfigTs, ModelTierTs } from "../../bindings";

export const TIERS: ModelTierTs[] = ["fast", "standard", "deep"];

export const REASONING_EFFORTS = ["low", "medium", "high", "xhigh"] as const;

export type ReasoningEffort = (typeof REASONING_EFFORTS)[number];

export type InferenceTargetKind = InferenceTargetConfigTs["kind"];

export const TARGET_KIND_OPTIONS: {
  kind: InferenceTargetKind;
  label: string;
}[] = [
  { kind: "mistral_chat", label: "Mistral Chat" },
  { kind: "openai_chat", label: "OpenAI Chat" },
  { kind: "openai_responses", label: "OpenAI Responses" },
];

export const DEFAULT_TIER_PRESETS: ReadonlyArray<{
  tier: ModelTierTs;
  targetRef: string;
  label: string;
  config: InferenceTargetConfigTs;
}> = [
  {
    tier: "fast",
    targetRef: "default-fast",
    label: "Fast",
    config: {
      kind: "mistral_chat",
      base_url: "https://api.mistral.ai",
      model_id: "mistral-medium-latest",
      api_key_env: "MISTRAL_API_KEY",
      temperature: null,
      max_completion_tokens: null,
    },
  },
  {
    tier: "standard",
    targetRef: "default-standard",
    label: "Standard",
    config: {
      kind: "openai_responses",
      base_url: "https://api.openai.com",
      model_id: "gpt-5.3-codex-spark",
      api_key_env: "OPENAI_API_KEY",
      reasoning_effort: "medium",
    },
  },
  {
    tier: "deep",
    targetRef: "default-deep",
    label: "Deep",
    config: {
      kind: "openai_responses",
      base_url: "https://api.openai.com",
      model_id: "gpt-5.5",
      api_key_env: "OPENAI_API_KEY",
      reasoning_effort: "high",
    },
  },
] as const satisfies ReadonlyArray<{
  tier: ModelTierTs;
  targetRef: string;
  label: string;
  config: InferenceTargetConfigTs;
}>;

export const PRESET_TARGET_REFS = new Set(
  DEFAULT_TIER_PRESETS.map((preset) => preset.targetRef),
);

export const targetRefForTier = (tier: ModelTierTs): string =>
  DEFAULT_TIER_PRESETS.find((preset) => preset.tier === tier)?.targetRef ??
  `default-${tier}`;

export const defaultPresetForTier = (tier: ModelTierTs) => {
  const preset = DEFAULT_TIER_PRESETS.find((entry) => entry.tier === tier);
  if (!preset) throw new Error(`unknown tier: ${tier}`);
  return preset;
};

export const kindLabel = (kind: InferenceTargetKind): string => {
  switch (kind) {
    case "mistral_chat":
      return "Mistral Chat";
    case "openai_chat":
      return "OpenAI Chat";
    case "openai_responses":
      return "OpenAI Responses";
  }
};

export const configSummary = (config: InferenceTargetConfigTs): string => {
  switch (config.kind) {
    case "mistral_chat":
    case "openai_chat": {
      const details = [
        config.base_url,
        config.model_id,
        config.api_key_env,
        config.temperature === null ? null : `temp ${config.temperature}`,
        config.max_completion_tokens === null
          ? null
          : `max ${config.max_completion_tokens}`,
      ].filter(Boolean);
      return details.join(" / ");
    }
    case "openai_responses": {
      const details = [
        config.base_url,
        config.model_id,
        config.api_key_env,
        config.reasoning_effort ? `${config.reasoning_effort} reasoning` : null,
      ].filter(Boolean);
      return details.join(" / ");
    }
  }
};

export const configKey = (config: InferenceTargetConfigTs): string => {
  switch (config.kind) {
    case "mistral_chat":
    case "openai_chat":
      return JSON.stringify([
        config.kind,
        config.base_url,
        config.model_id,
        config.api_key_env,
        config.temperature ?? null,
        config.max_completion_tokens ?? null,
      ]);
    case "openai_responses":
      return JSON.stringify([
        config.kind,
        config.base_url,
        config.model_id,
        config.api_key_env,
        config.reasoning_effort ?? null,
      ]);
  }
};

export const sameConfig = (
  left: InferenceTargetConfigTs,
  right: InferenceTargetConfigTs,
): boolean => configKey(left) === configKey(right);

export const shortHash = (input: string): string => {
  let hash = 0x811c9dc5;
  for (let i = 0; i < input.length; i += 1) {
    hash ^= input.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(36);
};

export const safeRefPart = (input: string): string =>
  input
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 36) || "target";

export const cloneConfig = (
  config: InferenceTargetConfigTs,
): InferenceTargetConfigTs => {
  switch (config.kind) {
    case "mistral_chat":
      return { ...config, kind: "mistral_chat" };
    case "openai_chat":
      return { ...config, kind: "openai_chat" };
    case "openai_responses":
      return { ...config, kind: "openai_responses" };
  }
};

export const defaultConfigForKind = (
  kind: InferenceTargetKind,
): InferenceTargetConfigTs => {
  switch (kind) {
    case "mistral_chat":
      return cloneConfig(defaultPresetForTier("fast").config);
    case "openai_chat":
      return {
        kind: "openai_chat",
        base_url: "https://api.openai.com",
        model_id: "gpt-5.3-codex-spark",
        api_key_env: "OPENAI_API_KEY",
        temperature: null,
        max_completion_tokens: null,
      };
    case "openai_responses":
      return cloneConfig(defaultPresetForTier("standard").config);
  }
};

export const nullableString = (value: string): string | null => {
  const trimmed = value.trim();
  return trimmed.length === 0 ? null : trimmed;
};

export const nullableFloat = (value: string): number | null => {
  const trimmed = value.trim();
  if (trimmed.length === 0) return null;
  const parsed = Number.parseFloat(trimmed);
  return Number.isFinite(parsed) ? parsed : null;
};

export const nullableInt = (value: string): number | null => {
  const trimmed = value.trim();
  if (trimmed.length === 0) return null;
  const parsed = Number.parseInt(trimmed, 10);
  return Number.isFinite(parsed) ? parsed : null;
};

export interface TargetDraft {
  kind: InferenceTargetKind;
  baseUrl: string;
  modelId: string;
  apiKeyEnv: string;
  temperature: string;
  maxCompletionTokens: string;
  reasoningEffort: string;
}

export const draftFromConfig = (
  config: InferenceTargetConfigTs,
): TargetDraft => {
  switch (config.kind) {
    case "mistral_chat":
    case "openai_chat":
      return {
        kind: config.kind,
        baseUrl: config.base_url,
        modelId: config.model_id,
        apiKeyEnv: config.api_key_env,
        temperature:
          config.temperature === null ? "" : String(config.temperature),
        maxCompletionTokens:
          config.max_completion_tokens === null
            ? ""
            : String(config.max_completion_tokens),
        reasoningEffort: "",
      };
    case "openai_responses":
      return {
        kind: config.kind,
        baseUrl: config.base_url,
        modelId: config.model_id,
        apiKeyEnv: config.api_key_env,
        temperature: "",
        maxCompletionTokens: "",
        reasoningEffort: config.reasoning_effort ?? "",
      };
  }
};

export const draftForKind = (kind: InferenceTargetKind): TargetDraft =>
  draftFromConfig(defaultConfigForKind(kind));

export const configFromDraft = (draft: TargetDraft): InferenceTargetConfigTs => {
  switch (draft.kind) {
    case "mistral_chat":
      return {
        kind: "mistral_chat",
        base_url: draft.baseUrl.trim(),
        model_id: draft.modelId.trim(),
        api_key_env: draft.apiKeyEnv.trim(),
        temperature: nullableFloat(draft.temperature),
        max_completion_tokens: nullableInt(draft.maxCompletionTokens),
      };
    case "openai_chat":
      return {
        kind: "openai_chat",
        base_url: draft.baseUrl.trim(),
        model_id: draft.modelId.trim(),
        api_key_env: draft.apiKeyEnv.trim(),
        temperature: nullableFloat(draft.temperature),
        max_completion_tokens: nullableInt(draft.maxCompletionTokens),
      };
    case "openai_responses":
      return {
        kind: "openai_responses",
        base_url: draft.baseUrl.trim(),
        model_id: draft.modelId.trim(),
        api_key_env: draft.apiKeyEnv.trim(),
        reasoning_effort: nullableString(draft.reasoningEffort),
      };
  }
};

export const targetRefForCollision = (
  tier: ModelTierTs,
  config: InferenceTargetConfigTs,
): string => {
  switch (config.kind) {
    case "mistral_chat":
    case "openai_chat":
    case "openai_responses":
      return `${targetRefForTier(tier)}-${safeRefPart(config.kind)}-${safeRefPart(
        config.model_id,
      )}-${shortHash(configKey(config))}`;
  }
};
