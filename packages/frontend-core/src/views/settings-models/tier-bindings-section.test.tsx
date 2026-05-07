import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { sentinelOwner } from "../../graph-store";
import type { InferenceTargetTs, InferenceTierBindingTs } from "../../bindings";
import { TierBindingsSection } from "./tier-bindings-section";

const owner = sentinelOwner();

const target = (
  targetRef: string,
  command = targetRef,
): InferenceTargetTs => ({
  target_ref: targetRef,
  config: {
    kind: "local_cli",
    command,
    profile: null,
    env_overrides: [],
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
      { tier: "fast", target_ref: "local-goose" },
    ];
    const client = {
      listInferenceTargets: vi.fn(async () => [
        target("local-goose", "goose"),
        target("remote-claude", "claude"),
      ]),
      listInferenceTierBindings: vi.fn(async () => bindings),
      bindInferenceTier: vi.fn(async () => undefined),
    };

    render(() => <TierBindingsSection client={client} owner={owner} />);

    await waitFor(() =>
      expect(screen.getAllByRole("combobox")).toHaveLength(3),
    );
    const fastSelect = screen.getAllByRole("combobox")[0] as HTMLSelectElement;
    await waitFor(() => expect(fastSelect.value).toBe("local-goose"));

    fireEvent.change(fastSelect, {
      target: { value: "remote-claude" },
    });

    await waitFor(() => {
      expect(client.bindInferenceTier).toHaveBeenCalledWith({
        owner,
        tier: "fast",
        target_ref: "remote-claude",
      });
    });
  });

  it("shows server-side typed errors verbatim on bind", async () => {
    const client = {
      listInferenceTargets: vi.fn(async () => [
        target("local-goose", "goose"),
        target("missing-target", "missing"),
      ]),
      listInferenceTierBindings: vi.fn(async () => [
        { tier: "fast" as const, target_ref: "local-goose" },
      ]),
      bindInferenceTier: vi.fn(async () => {
        throw { code: "InferenceTargetMissing", message: "missing target" };
      }),
    };

    render(() => <TierBindingsSection client={client} owner={owner} />);

    const fastSelect = (await screen.findAllByRole("combobox"))[0] as HTMLSelectElement;
    await waitFor(() => expect(fastSelect.value).toBe("local-goose"));
    fireEvent.change(fastSelect, {
      target: { value: "missing-target" },
    });

    await waitFor(() => expect(client.bindInferenceTier).toHaveBeenCalled());
    expect(
      await screen.findByText("InferenceTargetMissing: missing target"),
    ).toBeTruthy();
  });
});
