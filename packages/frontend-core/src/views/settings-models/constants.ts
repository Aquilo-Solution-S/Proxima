import type { InferenceTargetConfigTs, ModelTierTs } from "../../bindings";

export const TIERS: ModelTierTs[] = ["fast", "standard", "deep"];

export const REASONING_EFFORTS = ["low", "medium", "high", "xhigh"] as const;
export const MISTRAL_REASONING_EFFORTS = ["none", "high"] as const;

export type ReasoningEffort = (typeof REASONING_EFFORTS)[number];
export type MistralReasoningEffort =
  (typeof MISTRAL_REASONING_EFFORTS)[number];

export type InferenceTargetKind = InferenceTargetConfigTs["kind"];

export const TARGET_KIND_OPTIONS: {
  kind: InferenceTargetKind;
  label: string;
}[] = [
  { kind: "mistral_chat", label: "Mistral Chat" },
  { kind: "openai_chat", label: "OpenAI Chat" },
  { kind: "openai_responses", label: "OpenAI Responses" },
  { kind: "chatgpt_codex", label: "ChatGPT (subscription)" },
];

export const kindLabel = (kind: InferenceTargetKind): string => {
  switch (kind) {
    case "mistral_chat":
      return "Mistral Chat";
    case "openai_chat":
      return "OpenAI Chat";
    case "openai_responses":
      return "OpenAI Responses";
    case "chatgpt_codex":
      return "ChatGPT (subscription)";
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
        config.context_window_tokens === null
          ? null
          : `ctx ${config.context_window_tokens}`,
        config.kind === "mistral_chat" && config.reasoning_effort
          ? `${config.reasoning_effort} reasoning`
          : null,
      ].filter(Boolean);
      return details.join(" / ");
    }
    case "openai_responses": {
      const details = [
        config.base_url,
        config.model_id,
        config.api_key_env,
        config.context_window_tokens === null
          ? null
          : `ctx ${config.context_window_tokens}`,
        config.reasoning_effort ? `${config.reasoning_effort} reasoning` : null,
      ].filter(Boolean);
      return details.join(" / ");
    }
    case "chatgpt_codex": {
      const details = [
        config.base_url,
        config.model_id,
        config.context_window_tokens === null
          ? null
          : `ctx ${config.context_window_tokens}`,
        config.reasoning_effort ? `${config.reasoning_effort} reasoning` : null,
      ].filter(Boolean);
      return details.join(" / ");
    }
  }
};

export const configKey = (config: InferenceTargetConfigTs): string => {
  switch (config.kind) {
    case "mistral_chat":
      return JSON.stringify([
        config.kind,
        config.base_url,
        config.model_id,
        config.api_key_env,
        config.temperature ?? null,
        config.max_completion_tokens ?? null,
        config.reasoning_effort ?? null,
        config.context_window_tokens ?? null,
      ]);
    case "openai_chat":
      return JSON.stringify([
        config.kind,
        config.base_url,
        config.model_id,
        config.api_key_env,
        config.temperature ?? null,
        config.max_completion_tokens ?? null,
        config.context_window_tokens ?? null,
      ]);
    case "openai_responses":
      return JSON.stringify([
        config.kind,
        config.base_url,
        config.model_id,
        config.api_key_env,
        config.reasoning_effort ?? null,
        config.context_window_tokens ?? null,
      ]);
    case "chatgpt_codex":
      return JSON.stringify([
        config.kind,
        config.base_url,
        config.model_id,
        config.reasoning_effort ?? null,
        config.context_window_tokens ?? null,
      ]);
  }
};

export const sameConfig = (
  left: InferenceTargetConfigTs,
  right: InferenceTargetConfigTs,
): boolean => configKey(left) === configKey(right);

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
    case "chatgpt_codex":
      return { ...config, kind: "chatgpt_codex" };
  }
};

export const defaultConfigForKind = (
  kind: InferenceTargetKind,
): InferenceTargetConfigTs => {
  const placeholder = KIND_PLACEHOLDERS[kind];
  switch (kind) {
    case "mistral_chat":
      return {
        kind: "mistral_chat",
        base_url: placeholder.baseUrl,
        model_id: "",
        api_key_env: placeholder.apiKeyEnv,
        temperature: null,
        max_completion_tokens: null,
        reasoning_effort: null,
        context_window_tokens: null,
      };
    case "openai_chat":
      return {
        kind: "openai_chat",
        base_url: placeholder.baseUrl,
        model_id: "",
        api_key_env: placeholder.apiKeyEnv,
        temperature: null,
        max_completion_tokens: null,
        context_window_tokens: null,
      };
    case "openai_responses":
      return {
        kind: "openai_responses",
        base_url: placeholder.baseUrl,
        model_id: "",
        api_key_env: placeholder.apiKeyEnv,
        reasoning_effort: null,
        context_window_tokens: null,
      };
    case "chatgpt_codex":
      return {
        kind: "chatgpt_codex",
        base_url: placeholder.baseUrl,
        model_id: "gpt-5.3-codex",
        reasoning_effort: null,
        context_window_tokens: null,
      };
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
  contextWindowTokens: string;
}

export const draftFromConfig = (
  config: InferenceTargetConfigTs,
): TargetDraft => {
  switch (config.kind) {
    case "mistral_chat":
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
        reasoningEffort: config.reasoning_effort ?? "",
        contextWindowTokens:
          config.context_window_tokens === null
            ? ""
            : String(config.context_window_tokens),
      };
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
        contextWindowTokens:
          config.context_window_tokens === null
            ? ""
            : String(config.context_window_tokens),
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
        contextWindowTokens:
          config.context_window_tokens === null
            ? ""
            : String(config.context_window_tokens),
      };
    case "chatgpt_codex":
      return {
        kind: config.kind,
        baseUrl: config.base_url,
        modelId: config.model_id,
        apiKeyEnv: "",
        temperature: "",
        maxCompletionTokens: "",
        reasoningEffort: config.reasoning_effort ?? "",
        contextWindowTokens:
          config.context_window_tokens === null
            ? ""
            : String(config.context_window_tokens),
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
        reasoning_effort: nullableString(draft.reasoningEffort),
        context_window_tokens: nullableInt(draft.contextWindowTokens),
      };
    case "openai_chat":
      return {
        kind: "openai_chat",
        base_url: draft.baseUrl.trim(),
        model_id: draft.modelId.trim(),
        api_key_env: draft.apiKeyEnv.trim(),
        temperature: nullableFloat(draft.temperature),
        max_completion_tokens: nullableInt(draft.maxCompletionTokens),
        context_window_tokens: nullableInt(draft.contextWindowTokens),
      };
    case "openai_responses":
      return {
        kind: "openai_responses",
        base_url: draft.baseUrl.trim(),
        model_id: draft.modelId.trim(),
        api_key_env: draft.apiKeyEnv.trim(),
        reasoning_effort: nullableString(draft.reasoningEffort),
        context_window_tokens: nullableInt(draft.contextWindowTokens),
      };
    case "chatgpt_codex":
      return {
        kind: "chatgpt_codex",
        base_url: draft.baseUrl.trim(),
        model_id: draft.modelId.trim(),
        reasoning_effort: nullableString(draft.reasoningEffort),
        context_window_tokens: nullableInt(draft.contextWindowTokens),
      };
  }
};

export interface KindPlaceholder {
  baseUrl: string;
  apiKeyEnv: string;
}

export const KIND_PLACEHOLDERS: Record<InferenceTargetKind, KindPlaceholder> = {
  mistral_chat: {
    baseUrl: "https://api.mistral.ai",
    apiKeyEnv: "MISTRAL_API_KEY",
  },
  openai_chat: {
    baseUrl: "https://api.openai.com",
    apiKeyEnv: "OPENAI_API_KEY",
  },
  openai_responses: {
    baseUrl: "https://api.openai.com",
    apiKeyEnv: "OPENAI_API_KEY",
  },
  chatgpt_codex: {
    baseUrl: "https://chatgpt.com/backend-api/codex",
    apiKeyEnv: "",
  },
};
