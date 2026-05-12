import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { sentinelOwner } from "../../graph-store";
import type { InferenceTargetTs, InferenceTierBindingTs } from "../../bindings";
import { TierBindingsSection } from "./tier-bindings-section";

const owner = sentinelOwner();

const target = (
  targetRef: string,
): InferenceTargetTs => ({
  target_ref: targetRef,
  config: {
    kind: "mistral_chat",
    base_url: "https://api.mistral.ai",
    model_id: targetRef,
    api_key_env: "MISTRAL_API_KEY",
    temperature: null,
    max_completion_tokens: null,
  },
  created_at: "2026-05-07T00:00:00Z",
  updated_at: "2026-05-07T00:00:00Z",
});

describe("TierBindingsSection", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("binds a tier to a selected inference target", async () => {
    const bindings: InferenceTierBindingTs[] = [
      { tier: "fast", target_ref: "local-mistral" },
    ];
    const client = {
      bindInferenceTier: vi.fn(async () => undefined),
    };

    render(() => (
      <TierBindingsSection
        client={client}
        owner={owner}
        targets={() => [
          target("local-mistral"),
          target("remote-openai"),
        ]}
        bindings={() => bindings}
        refetchBindings={vi.fn()}
      />
    ));

    await waitFor(() =>
      expect(screen.getAllByRole("combobox")).toHaveLength(3),
    );
    const fastSelect = screen.getAllByRole("combobox")[0] as HTMLSelectElement;
    await waitFor(() => expect(fastSelect.value).toBe("local-mistral"));

    fireEvent.change(fastSelect, {
      target: { value: "remote-openai" },
    });

    await waitFor(() => {
      expect(client.bindInferenceTier).toHaveBeenCalledWith({
        owner,
        tier: "fast",
        target_ref: "remote-openai",
      });
    });
  });

  it("shows server-side typed errors verbatim on bind", async () => {
    const client = {
      bindInferenceTier: vi.fn(async () => {
        throw { code: "InferenceTargetMissing", message: "missing target" };
      }),
    };

    render(() => (
      <TierBindingsSection
        client={client}
        owner={owner}
        targets={() => [
          target("local-mistral"),
          target("missing-target"),
        ]}
        bindings={() => [{ tier: "fast" as const, target_ref: "local-mistral" }]}
        refetchBindings={vi.fn()}
      />
    ));

    const fastSelect = (await screen.findAllByRole("combobox"))[0] as HTMLSelectElement;
    await waitFor(() => expect(fastSelect.value).toBe("local-mistral"));
    fireEvent.change(fastSelect, {
      target: { value: "missing-target" },
    });

    await waitFor(() => expect(client.bindInferenceTier).toHaveBeenCalled());
    expect(
      await screen.findByText("InferenceTargetMissing: missing target"),
    ).toBeTruthy();
  });
});
