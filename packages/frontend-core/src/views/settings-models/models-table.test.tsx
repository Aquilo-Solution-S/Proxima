import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  InferenceTargetTs,
  InferenceTierBindingTs,
} from "../../bindings";
import type { EngineClient } from "../../client";
import { sentinelOwner } from "../../graph-store";
import { ModelsTable } from "./models-table";

const owner = sentinelOwner();

const fastTarget: InferenceTargetTs = {
  target_ref: "my-fast",
  config: {
    kind: "mistral_chat",
    base_url: "https://api.mistral.ai",
    model_id: "mistral-medium-latest",
    api_key_env: "MISTRAL_API_KEY",
    temperature: null,
    max_completion_tokens: null,
    reasoning_effort: null,
    context_window_tokens: null,
  },
  created_at: "2026-05-12T00:00:00Z",
  updated_at: "2026-05-12T00:00:00Z",
};

const deepTarget: InferenceTargetTs = {
  ...fastTarget,
  target_ref: "my-deep",
  config: {
    kind: "openai_responses",
    base_url: "https://api.openai.com",
    model_id: "gpt-5.5",
    api_key_env: "OPENAI_API_KEY",
    reasoning_effort: "high",
    context_window_tokens: null,
  },
};

type ModelsTableClient = Pick<
  EngineClient,
  | "registerInferenceTarget"
  | "removeInferenceTarget"
  | "bindInferenceTier"
  | "inferenceEnvStatus"
  | "codexAuthStatus"
  | "testInferenceTarget"
>;

const baseClient = (): ModelsTableClient => ({
  registerInferenceTarget: vi.fn(async () => ({
    target_ref: "x",
    idempotent_replay: false,
  })),
  removeInferenceTarget: vi.fn(async () => ({ idempotent_replay: false })),
  bindInferenceTier: vi.fn(async () => undefined),
  inferenceEnvStatus: vi.fn(async (_req: { env_var: string }) => ({ present: true })),
  codexAuthStatus: vi.fn(async () => ({
    auth_json_present: true,
    access_token_present: true,
  })),
  testInferenceTarget: vi.fn(async () => ({
    ok: true,
    latency_ms: 1,
    error_code: null,
    error_message: null,
  })),
});

describe("ModelsTable", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders an empty state when no models are registered", () => {
    render(() => (
      <ModelsTable
        client={baseClient()}
        owner={owner}
        targets={() => []}
        bindings={() => []}
        refetchTargets={vi.fn()}
        refetchBindings={vi.fn()}
      />
    ));
    expect(screen.getByText(/no models registered/i)).toBeTruthy();
  });

  it("renders a row per registered model", () => {
    render(() => (
      <ModelsTable
        client={baseClient()}
        owner={owner}
        targets={() => [fastTarget, deepTarget]}
        bindings={() => []}
        refetchTargets={vi.fn()}
        refetchBindings={vi.fn()}
      />
    ));
    expect(screen.getByText("my-fast")).toBeTruthy();
    expect(screen.getByText("my-deep")).toBeTruthy();
    expect(screen.getByText("mistral-medium-latest")).toBeTruthy();
    expect(screen.getByText("gpt-5.5")).toBeTruthy();
  });

  it("marks the filled tier radio on the bound row", () => {
    const bindings: InferenceTierBindingTs[] = [
      { tier: "fast", target_ref: "my-fast" },
      { tier: "deep", target_ref: "my-deep" },
    ];
    render(() => (
      <ModelsTable
        client={baseClient()}
        owner={owner}
        targets={() => [fastTarget, deepTarget]}
        bindings={() => bindings}
        refetchTargets={vi.fn()}
        refetchBindings={vi.fn()}
      />
    ));
    const radio = screen.getByLabelText(
      "bind tier fast to my-fast",
    ) as HTMLInputElement;
    expect(radio.checked).toBe(true);
  });

  it("calls bindInferenceTier when an empty radio is clicked", async () => {
    const c = baseClient();
    const refetchBindings = vi.fn();
    render(() => (
      <ModelsTable
        client={c}
        owner={owner}
        targets={() => [fastTarget, deepTarget]}
        bindings={() => [{ tier: "standard", target_ref: "my-fast" }]}
        refetchTargets={vi.fn()}
        refetchBindings={refetchBindings}
      />
    ));
    const radio = screen.getByLabelText("bind tier standard to my-deep");
    fireEvent.click(radio);
    await waitFor(() =>
      expect(c.bindInferenceTier).toHaveBeenCalledWith({
        principal: owner.principal,
        tier: "standard",
        target_ref: "my-deep",
      }),
    );
    expect(refetchBindings).toHaveBeenCalled();
  });

  it("does not call bindInferenceTier when an already-filled radio is clicked", async () => {
    const c = baseClient();
    render(() => (
      <ModelsTable
        client={c}
        owner={owner}
        targets={() => [fastTarget]}
        bindings={() => [{ tier: "fast", target_ref: "my-fast" }]}
        refetchTargets={vi.fn()}
        refetchBindings={vi.fn()}
      />
    ));
    const radio = screen.getByLabelText("bind tier fast to my-fast");
    fireEvent.click(radio);
    await new Promise((r) => setTimeout(r, 0));
    expect(c.bindInferenceTier).not.toHaveBeenCalled();
  });

  it("renders the current-tier header strip from bindings", () => {
    render(() => (
      <ModelsTable
        client={baseClient()}
        owner={owner}
        targets={() => [fastTarget, deepTarget]}
        bindings={() => [
          { tier: "fast", target_ref: "my-fast" },
          { tier: "standard", target_ref: "my-fast" },
          { tier: "deep", target_ref: "my-deep" },
        ]}
        refetchTargets={vi.fn()}
        refetchBindings={vi.fn()}
      />
    ));
    expect(screen.getByTestId("tier-summary-fast").textContent).toContain(
      "my-fast",
    );
    expect(screen.getByTestId("tier-summary-standard").textContent).toContain(
      "my-fast",
    );
    expect(screen.getByTestId("tier-summary-deep").textContent).toContain(
      "my-deep",
    );
  });

  it("renders (none) for unbound tiers in the header", () => {
    render(() => (
      <ModelsTable
        client={baseClient()}
        owner={owner}
        targets={() => []}
        bindings={() => []}
        refetchTargets={vi.fn()}
        refetchBindings={vi.fn()}
      />
    ));
    for (const tier of ["fast", "standard", "deep"]) {
      const cell = screen.getByTestId(`tier-summary-${tier}`);
      expect(cell.textContent).toContain(`${tier} →`);
      expect(cell.textContent).toContain("(none)");
    }
  });

  it("disables remove on rows that own a tier binding", () => {
    render(() => (
      <ModelsTable
        client={baseClient()}
        owner={owner}
        targets={() => [fastTarget]}
        bindings={() => [{ tier: "fast", target_ref: "my-fast" }]}
        refetchTargets={vi.fn()}
        refetchBindings={vi.fn()}
      />
    ));
    const remove = screen.getByRole("button", {
      name: /remove my-fast/i,
    }) as HTMLButtonElement;
    expect(remove.disabled).toBe(true);
  });

  it("renders API key health pill from inferenceEnvStatus", async () => {
    const c = baseClient();
    c.inferenceEnvStatus = vi.fn(async (req: { env_var: string }) => ({
      present: req.env_var === "MISTRAL_API_KEY",
    }));
    render(() => (
      <ModelsTable
        client={c}
        owner={owner}
        targets={() => [fastTarget, deepTarget]}
        bindings={() => []}
        refetchTargets={vi.fn()}
        refetchBindings={vi.fn()}
      />
    ));
    await waitFor(() => expect(c.inferenceEnvStatus).toHaveBeenCalled());
    expect(
      await screen.findByLabelText("key status for my-fast: set"),
    ).toBeTruthy();
    expect(
      await screen.findByLabelText("key status for my-deep: missing"),
    ).toBeTruthy();
  });

  it("renders ok result after Test connection succeeds", async () => {
    const c = baseClient();
    c.testInferenceTarget = vi.fn(async () => ({
      ok: true,
      latency_ms: 213,
      error_code: null,
      error_message: null,
    }));
    render(() => (
      <ModelsTable
        client={c}
        owner={owner}
        targets={() => [fastTarget]}
        bindings={() => []}
        refetchTargets={vi.fn()}
        refetchBindings={vi.fn()}
      />
    ));
    fireEvent.click(screen.getByRole("button", { name: /test my-fast/i }));
    await waitFor(() =>
      expect(c.testInferenceTarget).toHaveBeenCalledWith({
        principal: owner.principal,
        target_ref: "my-fast",
      }),
    );
    expect(await screen.findByText(/tested ok/i)).toBeTruthy();
    expect(screen.getByText(/213ms/i)).toBeTruthy();
  });

  it("renders error result after Test connection fails", async () => {
    const c = baseClient();
    c.testInferenceTarget = vi.fn(async () => ({
      ok: false,
      latency_ms: 12,
      error_code: "http_401",
      error_message: "Unauthorized",
    }));
    render(() => (
      <ModelsTable
        client={c}
        owner={owner}
        targets={() => [fastTarget]}
        bindings={() => []}
        refetchTargets={vi.fn()}
        refetchBindings={vi.fn()}
      />
    ));
    fireEvent.click(screen.getByRole("button", { name: /test my-fast/i }));
    expect(await screen.findByText(/failed/i)).toBeTruthy();
    expect(screen.getByText(/http_401/i)).toBeTruthy();
  });
});
