import type { InferenceTargetConfigTs, ModelTierTs } from "../../bindings";

export const TIERS: ModelTierTs[] = ["fast", "standard", "deep"];

export const SYSTEM_PROMPT_ENV = "PROXIMA_SYSTEM_PROMPT";

export const GOOSE_PROVIDERS = [
  { id: "mistral", label: "Mistral" },
  { id: "chatgpt_codex", label: "ChatGPT Codex" },
] as const;

export const GOOSE_MODELS = {
  mistral: [
    { id: "mistral-medium-latest", label: "Mistral Medium (latest)" },
  ],
  chatgpt_codex: [
    { id: "gpt-5.3-codex-spark", label: "GPT-5.3 Codex Spark" },
    { id: "gpt-5.5", label: "GPT-5.5" },
  ],
} as const;

export const REASONING_EFFORTS = ["low", "medium", "high", "xhigh"] as const;

export const DEFAULT_TIER_PRESETS = [
  {
    tier: "fast",
    provider: "mistral",
    model: "mistral-medium-latest",
    reasoning: null,
  },
  {
    tier: "standard",
    provider: "chatgpt_codex",
    model: "gpt-5.3-codex-spark",
    reasoning: "medium",
  },
  {
    tier: "deep",
    provider: "chatgpt_codex",
    model: "gpt-5.5",
    reasoning: "high",
  },
] as const;

export type GooseProvider = (typeof GOOSE_PROVIDERS)[number]["id"];
export type GooseReasoningEffort = (typeof REASONING_EFFORTS)[number];

export const providerLabel = (id: string | undefined): string => {
  if (!id) return "";
  return GOOSE_PROVIDERS.find((p) => p.id === id)?.label ?? id;
};

export const modelLabel = (
  provider: string | undefined,
  id: string | undefined,
): string => {
  if (!id) return "";
  if (!provider) return id;
  const list = (GOOSE_MODELS as Record<string, ReadonlyArray<{ id: string; label: string }>>)[
    provider
  ];
  return list?.find((m) => m.id === id)?.label ?? id;
};

export interface DecodedGooseConfig {
  command: string;
  provider: string | undefined;
  model: string | undefined;
  reasoning: string | undefined;
  systemPrompt: string | undefined;
}

export const decodeGooseConfig = (
  config: InferenceTargetConfigTs,
): DecodedGooseConfig | null => {
  if (config.kind !== "local_cli") return null;
  const map = new Map(config.env_overrides);
  return {
    command: config.command,
    provider: map.get("GOOSE_PROVIDER"),
    model: map.get("GOOSE_MODEL"),
    reasoning: map.get("CHATGPT_CODEX_REASONING_EFFORT"),
    systemPrompt: map.get(SYSTEM_PROMPT_ENV),
  };
};
