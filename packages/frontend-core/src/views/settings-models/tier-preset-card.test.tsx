import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  BindInferenceTierTs,
  InferenceTargetConfigTs,
  InferenceTargetTs,
  InferenceTierBindingTs,
  RegisterInferenceTargetTs,
  RemoveInferenceTargetTs,
} from "../../bindings";
import { sentinelOwner } from "../../graph-store";
import { TierPresetCard } from "./tier-preset-card";

const owner = sentinelOwner();

const renderCard = (
  options: {
    targets?: InferenceTargetTs[];
    bindings?: InferenceTierBindingTs[];
  } = {},
) => {
  const targets = options.targets ?? [];
  const bindings = options.bindings ?? [];
  const client = {
    registerInferenceTarget: vi.fn(
      async (req: RegisterInferenceTargetTs) => ({
        target_ref: req.target_ref,
        idempotent_replay: false,
      }),
    ),
    removeInferenceTarget: vi.fn(
      async (_req: RemoveInferenceTargetTs) => ({
        idempotent_replay: false,
      }),
    ),
    bindInferenceTier: vi.fn(async (_req: BindInferenceTierTs) => undefined),
  };
  const onChanged = vi.fn();

  render(() => (
    <TierPresetCard
      client={client}
      owner={owner}
      targets={() => targets}
      bindings={() => bindings}
      onChanged={onChanged}
    />
  ));

  return { client, onChanged };
};

const target = (
  targetRef: string,
  config: InferenceTargetConfigTs,
): InferenceTargetTs => ({
  target_ref: targetRef,
  config,
  created_at: "2026-05-08T00:00:00Z",
  updated_at: "2026-05-08T00:00:00Z",
});

const mistralConfig: InferenceTargetConfigTs = {
  kind: "mistral_chat",
  base_url: "https://api.mistral.ai",
  model_id: "mistral-medium-latest",
  api_key_env: "MISTRAL_API_KEY",
  temperature: null,
  max_completion_tokens: null,
};

const standardConfig: InferenceTargetConfigTs = {
  kind: "openai_responses",
  base_url: "https://api.openai.com",
  model_id: "gpt-5.3-codex-spark",
  api_key_env: "OPENAI_API_KEY",
  reasoning_effort: "medium",
};

const deepConfig: InferenceTargetConfigTs = {
  kind: "openai_responses",
  base_url: "https://api.openai.com",
  model_id: "gpt-5.5",
  api_key_env: "OPENAI_API_KEY",
  reasoning_effort: "high",
};

describe("TierPresetCard", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("registers and binds the fast tier from its row editor", async () => {
    const { client, onChanged } = renderCard();

    fireEvent.click(screen.getByRole("button", { name: "Configure fast" }));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(client.registerInferenceTarget).toHaveBeenCalledWith({
        owner,
        target_ref: "default-fast",
        config: mistralConfig,
      });
    });
    expect(client.bindInferenceTier).toHaveBeenCalledWith({
      owner,
      tier: "fast",
      target_ref: "default-fast",
    });
    await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(1));
  });

  it("sets up all three native tier defaults in order", async () => {
    const { client, onChanged } = renderCard();

    fireEvent.click(
      screen.getByRole("button", {
        name: "Set up all 3 tiers (recommended)",
      }),
    );

    await waitFor(() =>
      expect(client.registerInferenceTarget).toHaveBeenCalledTimes(3),
    );
    expect(
      client.registerInferenceTarget.mock.calls.map(([req]) => req.target_ref),
    ).toEqual(["default-fast", "default-standard", "default-deep"]);
    expect(client.registerInferenceTarget.mock.calls.map(([req]) => req.config)).toEqual([
      mistralConfig,
      standardConfig,
      deepConfig,
    ]);
    expect(client.bindInferenceTier.mock.calls.map(([req]) => req)).toEqual([
      { owner, tier: "fast", target_ref: "default-fast" },
      { owner, tier: "standard", target_ref: "default-standard" },
      { owner, tier: "deep", target_ref: "default-deep" },
    ]);
    expect(onChanged).toHaveBeenCalledTimes(1);
  });

  it("rebinds a tier to an existing matching target instead of re-registering the fixed ref", async () => {
    const { client, onChanged } = renderCard({
      targets: [target("shared-standard", standardConfig)],
    });

    fireEvent.click(screen.getByRole("button", { name: "Configure standard" }));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(client.bindInferenceTier).toHaveBeenCalledWith({
        owner,
        tier: "standard",
        target_ref: "shared-standard",
      });
    });
    expect(client.registerInferenceTarget).not.toHaveBeenCalled();
    await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(1));
  });

  it("creates a hashed target ref when a tier fixed ref already exists with another body", async () => {
    const { client } = renderCard({
      targets: [
        target("default-standard", {
          ...standardConfig,
          model_id: "other-model",
        }),
      ],
      bindings: [{ tier: "standard", target_ref: "default-standard" }],
    });

    fireEvent.click(
      screen.getByRole("button", { name: "Fill missing tiers from defaults" }),
    );

    await waitFor(() => {
      expect(client.registerInferenceTarget).toHaveBeenCalledTimes(3);
    });
    const registered = client.registerInferenceTarget.mock.calls
      .map(([req]) => req)
      .find((req) => req.config.model_id === "gpt-5.3-codex-spark")!;
    expect(registered.target_ref).toMatch(
      /^default-standard-openai-responses-gpt-5-3-codex-spark-/,
    );
    expect(registered.target_ref).not.toBe("default-standard");
    expect(client.bindInferenceTier).toHaveBeenCalledWith({
      owner,
      tier: "standard",
      target_ref: registered.target_ref,
    });
  });

  it("renders configured rows and supports removal", async () => {
    const { client, onChanged } = renderCard({
      targets: [target("default-deep", deepConfig)],
    });

    expect(screen.getByText("OpenAI Responses")).toBeTruthy();
    expect(screen.getByText("gpt-5.5")).toBeTruthy();
    expect(screen.getByText("high reasoning")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Remove deep" }));
    await waitFor(() => {
      expect(client.removeInferenceTarget).toHaveBeenCalledWith({
        owner,
        target_ref: "default-deep",
      });
    });
    await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(1));
  });
});
