import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { sentinelOwner } from "../../graph-store";
import type { InferenceTargetTs } from "../../bindings";
import { InferenceTargetsSection } from "./inference-targets-section";

const owner = sentinelOwner();

const sampleTarget: InferenceTargetTs = {
  target_ref: "custom-mistral",
  config: {
    kind: "mistral_chat",
    base_url: "https://api.mistral.ai",
    model_id: "mistral-medium-latest",
    api_key_env: "MISTRAL_API_KEY",
    temperature: null,
    max_completion_tokens: null,
  },
  created_at: "2026-05-07T00:00:00Z",
  updated_at: "2026-05-07T00:00:00Z",
};

const client = () => ({
  registerInferenceTarget: vi.fn(async () => ({
    target_ref: "custom-target",
    idempotent_replay: false,
  })),
  removeInferenceTarget: vi.fn(async () => ({
    idempotent_replay: false,
  })),
  bindInferenceTier: vi.fn(async () => undefined),
});

describe("InferenceTargetsSection", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders custom non-preset targets", async () => {
    const c = client();

    render(() => (
      <InferenceTargetsSection
        client={c}
        owner={owner}
        targets={() => [sampleTarget]}
        refetchTargets={vi.fn()}
        onChanged={vi.fn()}
      />
    ));

    expect(await screen.findByText("custom-mistral")).toBeTruthy();
    expect(screen.getAllByText("Mistral Chat").length).toBeGreaterThan(0);
  });

  it("hides native preset rows from the custom targets table", async () => {
    const presetTarget: InferenceTargetTs = {
      ...sampleTarget,
      target_ref: "default-fast",
    };
    const c = client();

    render(() => (
      <InferenceTargetsSection
        client={c}
        owner={owner}
        targets={() => [presetTarget]}
        refetchTargets={vi.fn()}
        onChanged={vi.fn()}
      />
    ));

    expect(screen.getByText("mistral-medium-latest")).toBeTruthy();
    expect(screen.queryByText("Existing custom targets")).toBeNull();
  });

  it("registers a custom Mistral Chat target with blank chat fields as null", async () => {
    const c = client();

    render(() => (
      <InferenceTargetsSection
        client={c}
        owner={owner}
        targets={() => []}
        refetchTargets={vi.fn()}
        onChanged={vi.fn()}
      />
    ));

    fireEvent.input(screen.getByLabelText("Target ref"), {
      target: { value: "custom-fast" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^register$/i }));

    await waitFor(() => {
      expect(c.registerInferenceTarget).toHaveBeenCalledWith({
        owner,
        target_ref: "custom-fast",
        config: sampleTarget.config,
      });
    });
  });

  it("registers a custom OpenAI Chat target with chat-only numeric fields", async () => {
    const c = client();

    render(() => (
      <InferenceTargetsSection
        client={c}
        owner={owner}
        targets={() => []}
        refetchTargets={vi.fn()}
        onChanged={vi.fn()}
      />
    ));

    fireEvent.change(screen.getByLabelText("Kind"), {
      target: { value: "openai_chat" },
    });
    fireEvent.input(screen.getByLabelText("Target ref"), {
      target: { value: "custom-openai-chat" },
    });
    fireEvent.input(screen.getByLabelText("temperature"), {
      target: { value: "0.2" },
    });
    fireEvent.input(screen.getByLabelText("max_completion_tokens"), {
      target: { value: "4096" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^register$/i }));

    await waitFor(() => {
      expect(c.registerInferenceTarget).toHaveBeenCalledWith({
        owner,
        target_ref: "custom-openai-chat",
        config: {
          kind: "openai_chat",
          base_url: "https://api.openai.com",
          model_id: "gpt-5.3-codex-spark",
          api_key_env: "OPENAI_API_KEY",
          temperature: 0.2,
          max_completion_tokens: 4096,
        },
      });
    });
  });

  it("registers a custom OpenAI Responses target with blank reasoning as null", async () => {
    const c = client();

    render(() => (
      <InferenceTargetsSection
        client={c}
        owner={owner}
        targets={() => []}
        refetchTargets={vi.fn()}
        onChanged={vi.fn()}
      />
    ));

    fireEvent.change(screen.getByLabelText("Kind"), {
      target: { value: "openai_responses" },
    });
    fireEvent.input(screen.getByLabelText("Target ref"), {
      target: { value: "custom-responses" },
    });
    fireEvent.change(screen.getByLabelText("reasoning_effort"), {
      target: { value: "" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^register$/i }));

    await waitFor(() => {
      expect(c.registerInferenceTarget).toHaveBeenCalledWith({
        owner,
        target_ref: "custom-responses",
        config: {
          kind: "openai_responses",
          base_url: "https://api.openai.com",
          model_id: "gpt-5.3-codex-spark",
          api_key_env: "OPENAI_API_KEY",
          reasoning_effort: null,
        },
      });
    });
  });

  it("shows server-side typed errors verbatim on register", async () => {
    const c = {
      ...client(),
      registerInferenceTarget: vi.fn(async () => {
        throw new Error("target_ref_conflict: custom-fast");
      }),
    };

    render(() => (
      <InferenceTargetsSection
        client={c}
        owner={owner}
        targets={() => []}
        refetchTargets={vi.fn()}
        onChanged={vi.fn()}
      />
    ));

    fireEvent.input(screen.getByLabelText("Target ref"), {
      target: { value: "custom-fast" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^register$/i }));

    await waitFor(() => {
      expect(screen.getByText(/target_ref_conflict/)).toBeTruthy();
    });
  });
});
