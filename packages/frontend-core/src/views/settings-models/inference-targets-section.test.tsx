import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { sentinelOwner } from "../../graph-store";
import type { InferenceTargetTs } from "../../bindings";
import { InferenceTargetsSection } from "./inference-targets-section";

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => "/usr/local/bin/goose"),
}));

const owner = sentinelOwner();

const sampleTarget: InferenceTargetTs = {
  target_ref: "local-goose",
  config: {
    kind: "local_cli",
    command: "goose",
    profile: null,
    env_overrides: [],
  },
  created_at: "2026-05-07T00:00:00Z",
  updated_at: "2026-05-07T00:00:00Z",
};

describe("InferenceTargetsSection", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders custom (non-preset) targets", async () => {
    const client = {
      detectLocalHarness: vi.fn(async () => null),
      registerInferenceTarget: vi.fn(),
      removeInferenceTarget: vi.fn(),
      bindInferenceTier: vi.fn(),
    };

    render(() => (
      <InferenceTargetsSection
        client={client}
        owner={owner}
        targets={() => [sampleTarget]}
        refetchTargets={vi.fn()}
        onChanged={vi.fn()}
      />
    ));

    expect(await screen.findByText("local-goose")).toBeTruthy();
    expect(screen.getAllByText("Local CLI").length).toBeGreaterThan(0);
  });

  it("hides preset goose-{tier} rows from the custom targets table", async () => {
    const presetTarget: InferenceTargetTs = {
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
      created_at: "2026-05-08T00:00:00Z",
      updated_at: "2026-05-08T00:00:00Z",
    };
    const client = {
      detectLocalHarness: vi.fn(async () => null),
      registerInferenceTarget: vi.fn(),
      removeInferenceTarget: vi.fn(),
      bindInferenceTier: vi.fn(),
    };

    render(() => (
      <InferenceTargetsSection
        client={client}
        owner={owner}
        targets={() => [presetTarget]}
        refetchTargets={vi.fn()}
        onChanged={vi.fn()}
      />
    ));

    await waitFor(() =>
      expect(screen.getByText("Mistral Medium (latest)")).toBeTruthy(),
    );
    expect(screen.queryByText("Existing custom targets")).toBeNull();
  });

  it("shows server-side typed errors verbatim on register", async () => {
    const client = {
      detectLocalHarness: vi.fn(async () => null),
      registerInferenceTarget: vi.fn(async () => {
        throw new Error("target_ref_conflict: local-goose");
      }),
      removeInferenceTarget: vi.fn(),
      bindInferenceTier: vi.fn(),
    };

    render(() => (
      <InferenceTargetsSection
        client={client}
        owner={owner}
        targets={() => []}
        refetchTargets={vi.fn()}
        onChanged={vi.fn()}
      />
    ));

    fireEvent.input(screen.getByLabelText("Target path"), {
      target: { value: "/usr/local/bin/goose" },
    });
    fireEvent.input(screen.getByLabelText("System Prompt"), {
      target: { value: "Stay inside the Proxima MCP surface." },
    });
    fireEvent.click(screen.getByRole("button", { name: /^register$/i }));

    await waitFor(() => {
      expect(screen.getByText(/target_ref_conflict/)).toBeTruthy();
    });
    expect(client.registerInferenceTarget).toHaveBeenCalledWith({
      owner,
      target_ref: "/usr/local/bin/goose",
      config: {
        kind: "local_cli",
        command: "/usr/local/bin/goose",
        profile: null,
        env_overrides: [
          ["PROXIMA_SYSTEM_PROMPT", "Stay inside the Proxima MCP surface."],
        ],
      },
    });
  });

  it("can pick a local target path from the platform dialog", async () => {
    const client = {
      detectLocalHarness: vi.fn(async () => null),
      registerInferenceTarget: vi.fn(),
      removeInferenceTarget: vi.fn(),
      bindInferenceTier: vi.fn(),
    };

    render(() => (
      <InferenceTargetsSection
        client={client}
        owner={owner}
        targets={() => []}
        refetchTargets={vi.fn()}
        onChanged={vi.fn()}
      />
    ));

    fireEvent.click(screen.getByRole("button", { name: /^select$/i }));

    await waitFor(() => {
      expect((screen.getByLabelText("Target path") as HTMLInputElement).value).toBe(
        "/usr/local/bin/goose",
      );
    });
  });
});
