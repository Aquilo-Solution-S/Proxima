import { cleanup, render, screen, waitFor } from "@solidjs/testing-library";
import { createResource, type Component } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { LlmCaps, LlmModelRecord, TierBindings } from "../../bindings";
import { TierBindingsSection } from "./tier-bindings-section";

const mocks = vi.hoisted(() => ({
  tierBind: vi.fn(),
  tierUnbind: vi.fn(),
  tierRequires: vi.fn(),
}));

vi.mock("../../bindings", () => ({
  commands: {
    tierBind: mocks.tierBind,
    tierUnbind: mocks.tierUnbind,
    tierRequires: mocks.tierRequires,
  },
}));

const ok = <T,>(data: T) => Promise.resolve({ status: "ok" as const, data });

const emptyCaps: LlmCaps = {
  tool_use: false,
  json_mode: false,
  long_context: false,
  vision: false,
};

const model = (
  overrides: Partial<LlmModelRecord> = {},
): LlmModelRecord => ({
  vendor: "ollama",
  model_id: "granite4.1:8b",
  dialect: "openai",
  base_url: "http://localhost:11434",
  caps: {
    tool_use: true,
    json_mode: true,
    long_context: false,
    vision: false,
  },
  secret_ref: null,
  ...overrides,
});

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
};

describe("TierBindingsSection", () => {
  beforeEach(() => {
    mocks.tierBind.mockResolvedValue(ok(null));
    mocks.tierUnbind.mockResolvedValue(ok(false));
    mocks.tierRequires.mockResolvedValue(ok(emptyCaps));
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("selects the bound tier model when options load after bindings", async () => {
    const models = deferred<LlmModelRecord[]>();
    const bindingsData: TierBindings = {
      fast: { vendor: "ollama", model_id: "granite4.1:8b" },
    };
    const Harness: Component = () => {
      const [bindings] = createResource(async () => bindingsData);
      const [llmModels] = createResource(async () => models.promise);
      return (
        <TierBindingsSection
          bindings={bindings}
          llmModels={llmModels}
          onChange={() => undefined}
        />
      );
    };

    render(() => <Harness />);

    await waitFor(() =>
      expect(screen.getAllByRole("combobox")).toHaveLength(3),
    );
    const fastSelect = screen.getAllByRole("combobox")[0] as HTMLSelectElement;
    expect(fastSelect.value).toBe("");

    models.resolve([model()]);

    await waitFor(() =>
      expect(fastSelect.value).toBe("ollama|granite4.1:8b"),
    );
  });
});
