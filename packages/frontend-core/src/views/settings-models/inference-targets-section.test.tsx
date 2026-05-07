import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { sentinelOwner } from "../../graph-store";
import type { InferenceTargetTs } from "../../bindings";
import { InferenceTargetsSection } from "./inference-targets-section";

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

  it("renders existing targets", async () => {
    const client = {
      listInferenceTargets: vi.fn(async () => [sampleTarget]),
      registerInferenceTarget: vi.fn(),
      removeInferenceTarget: vi.fn(),
    };

    render(() => <InferenceTargetsSection client={client} owner={owner} />);

    expect(await screen.findByText("local-goose")).toBeTruthy();
    expect(screen.getAllByText("local_cli").length).toBeGreaterThan(0);
  });

  it("shows server-side typed errors verbatim on register", async () => {
    const client = {
      listInferenceTargets: vi.fn(async () => []),
      registerInferenceTarget: vi.fn(async () => {
        throw new Error("target_ref_conflict: local-goose");
      }),
      removeInferenceTarget: vi.fn(),
    };

    render(() => <InferenceTargetsSection client={client} owner={owner} />);

    fireEvent.input(screen.getByLabelText("target_ref"), {
      target: { value: "local-goose" },
    });
    fireEvent.input(screen.getByLabelText("command"), {
      target: { value: "goose" },
    });
    fireEvent.click(screen.getByRole("button", { name: /register/i }));

    await waitFor(() => {
      expect(screen.getByText(/target_ref_conflict/)).toBeTruthy();
    });
    expect(client.registerInferenceTarget).toHaveBeenCalledWith({
      owner,
      target_ref: "local-goose",
      config: {
        kind: "local_cli",
        command: "goose",
        profile: null,
        env_overrides: [],
      },
    });
  });
});
