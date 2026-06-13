import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { sentinelOwner } from "../../graph-store";
import { RegisterModelModal } from "./register-model-modal";

const owner = sentinelOwner();

const baseClient = () => ({
  registerInferenceTarget: vi.fn(async () => ({
    target_ref: "new-target",
    idempotent_replay: false,
  })),
});

describe("RegisterModelModal", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("pre-fills mistral defaults when the modal opens with mistral_chat", () => {
    render(() => (
      <RegisterModelModal
        client={baseClient()}
        owner={owner}
        existingRefs={[]}
        onClose={vi.fn()}
        onRegistered={vi.fn()}
      />
    ));
    const baseUrl = screen.getByLabelText("base_url") as HTMLInputElement;
    const env = screen.getByLabelText("api_key_env") as HTMLInputElement;
    const contextWindow = screen.getByLabelText(
      "context_window_tokens",
    ) as HTMLInputElement;
    expect(baseUrl.value).toBe("https://api.mistral.ai");
    expect(env.value).toBe("MISTRAL_API_KEY");
    expect(contextWindow.value).toBe("");
  });

  it("switches placeholders when the user picks openai_chat", () => {
    render(() => (
      <RegisterModelModal
        client={baseClient()}
        owner={owner}
        existingRefs={[]}
        onClose={vi.fn()}
        onRegistered={vi.fn()}
      />
    ));
    const kind = screen.getByLabelText("Kind") as HTMLSelectElement;
    fireEvent.change(kind, { target: { value: "openai_chat" } });
    const baseUrl = screen.getByLabelText("base_url") as HTMLInputElement;
    expect(baseUrl.value).toBe("https://api.openai.com");
  });

  it("shows a duplicate-ref error when the user enters an existing target_ref", async () => {
    render(() => (
      <RegisterModelModal
        client={baseClient()}
        owner={owner}
        existingRefs={["taken-ref"]}
        onClose={vi.fn()}
        onRegistered={vi.fn()}
      />
    ));
    const ref = screen.getByLabelText("Target ref") as HTMLInputElement;
    fireEvent.input(ref, { target: { value: "taken-ref" } });
    const modelId = screen.getByLabelText("model_id") as HTMLInputElement;
    fireEvent.input(modelId, { target: { value: "any-model" } });
    fireEvent.click(screen.getByRole("button", { name: /register/i }));
    expect(
      await screen.findByText(/target with this ref already exists/i),
    ).toBeTruthy();
  });

  it("submits a valid form and calls registerInferenceTarget", async () => {
    const c = baseClient();
    const onRegistered = vi.fn();
    const onClose = vi.fn();
    render(() => (
      <RegisterModelModal
        client={c}
        owner={owner}
        existingRefs={[]}
        onClose={onClose}
        onRegistered={onRegistered}
      />
    ));
    fireEvent.input(screen.getByLabelText("Target ref"), {
      target: { value: "my-new" },
    });
    fireEvent.input(screen.getByLabelText("model_id"), {
      target: { value: "mistral-medium-latest" },
    });
    fireEvent.input(screen.getByLabelText("context_window_tokens"), {
      target: { value: "128000" },
    });
    fireEvent.click(screen.getByRole("button", { name: /register/i }));
    await waitFor(() =>
      expect(c.registerInferenceTarget).toHaveBeenCalledWith({
        principal: owner.principal,
        target_ref: "my-new",
        config: expect.objectContaining({
          kind: "mistral_chat",
          base_url: "https://api.mistral.ai",
          model_id: "mistral-medium-latest",
          api_key_env: "MISTRAL_API_KEY",
          context_window_tokens: 128000,
        }),
      }),
    );
    expect(onRegistered).toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });
});
