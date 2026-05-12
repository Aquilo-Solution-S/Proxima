import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  BindInferenceTierTs,
  DetectedHarnessTs,
  InferenceTargetTs,
  InferenceTierBindingTs,
  RegisterInferenceTargetTs,
  RemoveInferenceTargetTs,
} from "../../bindings";
import { sentinelOwner } from "../../graph-store";
import { GoosePresetCard } from "./goose-preset-card";

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => "/fake/goose"),
}));

const owner = sentinelOwner();

const detectedGoose = vi.fn(
  async (_name: string): Promise<DetectedHarnessTs | null> => ({
    path: "/fake/goose",
    version: "1.33.1",
  }),
);

const renderCard = (
  options: {
    detectLocalHarness?: (name: string) => Promise<DetectedHarnessTs | null>;
    targets?: InferenceTargetTs[];
    bindings?: InferenceTierBindingTs[];
  } = {},
) => {
  const detectLocalHarness = options.detectLocalHarness ?? detectedGoose;
  const targets = options.targets ?? [];
  const bindings = options.bindings ?? [];
  const client = {
    detectLocalHarness,
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
    <GoosePresetCard
      client={client}
      owner={owner}
      targets={() => targets}
      bindings={() => bindings}
      onChanged={onChanged}
    />
  ));

  return { client, onChanged };
};

const goosePillVisible = () => screen.findByText(/goose 1\.33\.1/i);

const gooseTarget = (
  targetRef: string,
  provider: string,
  model: string,
  reasoning?: string,
): InferenceTargetTs => ({
  target_ref: targetRef,
  config: {
    kind: "local_cli",
    command: "/fake/goose",
    profile: null,
    env_overrides: [
      ["GOOSE_PROVIDER", provider],
      ["GOOSE_MODEL", model],
      ...(reasoning
        ? ([["CHATGPT_CODEX_REASONING_EFFORT", reasoning]] as [string, string][])
        : []),
    ],
  },
  created_at: "2026-05-08T00:00:00Z",
  updated_at: "2026-05-08T00:00:00Z",
});

describe("GoosePresetCard", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("registers and binds the fast tier from its row editor", async () => {
    const { client, onChanged } = renderCard();

    await goosePillVisible();
    fireEvent.click(screen.getByRole("button", { name: "Configure fast" }));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(client.registerInferenceTarget).toHaveBeenCalledWith({
        owner,
        target_ref: "goose-fast",
        config: {
          kind: "local_cli",
          command: "/fake/goose",
          profile: null,
          env_overrides: [
            ["GOOSE_PROVIDER", "mistral"],
            ["GOOSE_MODEL", "mistral-medium-latest"],
          ],
        },
      });
    });
    expect(client.bindInferenceTier).toHaveBeenCalledWith({
      owner,
      tier: "fast",
      target_ref: "goose-fast",
    });
    await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(1));
  });

  it("emits reasoning effort for chatgpt codex presets", async () => {
    const { client } = renderCard();

    await goosePillVisible();
    fireEvent.click(screen.getByRole("button", { name: "Configure standard" }));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(client.registerInferenceTarget).toHaveBeenCalledWith({
        owner,
        target_ref: "goose-standard",
        config: {
          kind: "local_cli",
          command: "/fake/goose",
          profile: null,
          env_overrides: [
            ["GOOSE_PROVIDER", "chatgpt_codex"],
            ["GOOSE_MODEL", "gpt-5.3-codex-spark"],
            ["CHATGPT_CODEX_REASONING_EFFORT", "medium"],
          ],
        },
      });
    });
  });

  it("sets up all three proven tier defaults in order", async () => {
    const { client, onChanged } = renderCard();

    await goosePillVisible();
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
    ).toEqual(["goose-fast", "goose-standard", "goose-deep"]);
    expect(client.bindInferenceTier.mock.calls.map(([req]) => req)).toEqual([
      { owner, tier: "fast", target_ref: "goose-fast" },
      { owner, tier: "standard", target_ref: "goose-standard" },
      { owner, tier: "deep", target_ref: "goose-deep" },
    ]);
    expect(onChanged).toHaveBeenCalledTimes(1);
  });

  it("rebinds a tier to an existing matching Goose target instead of re-registering the fixed ref", async () => {
    const { client, onChanged } = renderCard({
      targets: [
        gooseTarget("goose-fast", "mistral", "mistral-medium-latest"),
        gooseTarget(
          "goose-standard",
          "chatgpt_codex",
          "gpt-5.3-codex-spark",
          "medium",
        ),
      ],
      bindings: [{ tier: "standard", target_ref: "goose-standard" }],
    });

    await goosePillVisible();
    fireEvent.click(screen.getByRole("button", { name: "Edit standard" }));
    fireEvent.change(screen.getByLabelText("Provider"), {
      target: { value: "mistral" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(client.bindInferenceTier).toHaveBeenCalledWith({
        owner,
        tier: "standard",
        target_ref: "goose-fast",
      });
    });
    expect(client.registerInferenceTarget).not.toHaveBeenCalled();
    await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(1));
  });

  it("creates a new Goose target ref when a tier fixed ref already exists with another body", async () => {
    const { client } = renderCard({
      targets: [
        gooseTarget(
          "goose-standard",
          "chatgpt_codex",
          "gpt-5.3-codex-spark",
          "medium",
        ),
      ],
      bindings: [{ tier: "standard", target_ref: "goose-standard" }],
    });

    await goosePillVisible();
    fireEvent.click(screen.getByRole("button", { name: "Edit standard" }));
    fireEvent.change(screen.getByLabelText("Provider"), {
      target: { value: "mistral" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(client.registerInferenceTarget).toHaveBeenCalledTimes(1);
    });
    const registered = client.registerInferenceTarget.mock.calls[0][0];
    expect(registered.target_ref).toMatch(
      /^goose-standard-mistral-mistral-medium-latest-/,
    );
    expect(registered.target_ref).not.toBe("goose-standard");
    expect(client.bindInferenceTier).toHaveBeenCalledWith({
      owner,
      tier: "standard",
      target_ref: registered.target_ref,
    });
  });

  it("surfaces missing goose detection without filling the path", async () => {
    renderCard({
      detectLocalHarness: vi.fn(
        async (_name: string): Promise<DetectedHarnessTs | null> => null,
      ),
    });

    expect(await screen.findByText("goose missing from PATH")).toBeTruthy();
    expect(screen.queryByText(/goose 1\.33\.1/i)).toBeNull();
  });

  it("renders configured rows with provider/model from env overrides and supports removal", async () => {
    const target: InferenceTargetTs = {
      target_ref: "goose-deep",
      config: {
        kind: "local_cli",
        command: "/fake/goose",
        profile: null,
        env_overrides: [
          ["GOOSE_PROVIDER", "chatgpt_codex"],
          ["GOOSE_MODEL", "gpt-5.5"],
          ["CHATGPT_CODEX_REASONING_EFFORT", "high"],
        ],
      },
      created_at: "2026-05-08T00:00:00Z",
      updated_at: "2026-05-08T00:00:00Z",
    };
    const { client, onChanged } = renderCard({ targets: [target] });

    await goosePillVisible();
    expect(screen.getByText("ChatGPT Codex")).toBeTruthy();
    expect(screen.getByText("GPT-5.5")).toBeTruthy();
    expect(screen.getByText("high reasoning")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Remove deep" }));
    await waitFor(() => {
      expect(client.removeInferenceTarget).toHaveBeenCalledWith({
        owner,
        target_ref: "goose-deep",
      });
    });
    await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(1));
  });
});
