import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  InstantiatePersonalityOutcomeTs,
  McpToolTs,
  PersonalityInstanceTs,
  RelationTs,
  SetWakeEntriesOutcomeTs,
  TombstonePersonalityOutcomeTs,
  WakeEntryTs,
  WakeInvocationTs,
} from "../bindings";
import { sentinelOwner } from "../graph-store";
import {
  clearRegistriesForTests,
  registerPayloadRenderer,
  registerPersonalityType,
} from "../registry";
import {
  PersonalitiesView,
  type PersonalityCommandClient,
} from "./personalities";

const owner = sentinelOwner();

const ok = <T,>(data: T) => Promise.resolve({ status: "ok" as const, data });

const wakeEntry = (overrides: Partial<WakeEntryTs> = {}): WakeEntryTs => ({
  wake_entry_id: "11111111-1111-7111-8111-111111111111",
  trigger_kind: "on_memory",
  trigger_id: "proxima-code/commit-summary-v1",
  label: "react-to-commit",
  enabled: true,
  execution_mode: "substrate_only",
  authored_by: "any",
  probability_promille: 1000,
  goal_scope: "none",
  instructions: "",
  model_tier: "standard",
  inference_target_ref: null,
  substrate_tool_palette: [],
  required_produced_schema_ids: [],
  max_rounds: 4,
  disabled_reason: null,
  ...overrides,
});

const instance = (
  overrides: Partial<PersonalityInstanceTs> = {},
): PersonalityInstanceTs => ({
  owner,
  personality_instance_id: "018f0000-0000-7000-8000-000000000001",
  current_root_perspective_memory_id: "018f0000-0000-7000-8000-000000000101",
  display_name: "Engineer",
  status: "active",
  wake_entries: [wakeEntry()],
  ...overrides,
});

const wakeInvocation = (
  overrides: Partial<WakeInvocationTs> = {},
): WakeInvocationTs => ({
  personality_instance_id: "018f0000-0000-7000-8000-000000000001",
  wake_entry_id: "11111111-1111-7111-8111-111111111111",
  wake_entry_label: "react-to-commit",
  change_event_seq: "22222222-2222-7222-8222-222222222222",
  status: "failed",
  started_at: "2026-05-09 07:38:46 UTC",
  finished_at: "2026-05-09 07:38:54 UTC",
  turn_count: 1,
  cost_usd: 0,
  resolved_inference_target_ref: null,
  failure_reason: "Error: harness failed",
  exit_code: 1,
  duration_ms: 8120,
  stdout_tail: "stdout tail",
  stderr_tail: "stderr tail",
  stdout_truncated: false,
  stderr_truncated: true,
  logs: [
    {
      log_seq: 1,
      at: "2026-05-09 07:38:47 UTC",
      phase: "tool_call",
      tool_id: "proxima-agent-memory/proxima_derive",
      status: "failed",
      duration_ms: 120,
      message_tail: "tool failed",
    },
  ],
  ...overrides,
});

const mockClient = (
  initial: PersonalityInstanceTs[],
  afterRefresh: PersonalityInstanceTs[] = initial,
  invocations: WakeInvocationTs[] = [],
) => {
  const listPersonalityInstances = vi
    .fn()
    .mockResolvedValueOnce({ status: "ok", data: initial })
    .mockResolvedValue({ status: "ok", data: afterRefresh });
  const instantiatePersonality = vi.fn((_) =>
    ok<InstantiatePersonalityOutcomeTs>({
      instance_id: "018f0000-0000-7000-8000-000000000099",
    }),
  );
  const setWakeEntries = vi.fn((_) =>
    ok<SetWakeEntriesOutcomeTs>({ active_entries: 1 }),
  );
  const tombstonePersonality = vi.fn((_) =>
    ok<TombstonePersonalityOutcomeTs>({
      status: "tombstoned",
      idempotent_replay: false,
    }),
  );
  const listMcpTools = vi.fn(() =>
    ok<McpToolTs[]>([
      {
        name: "core/fetch_memory",
        description: "Fetch one memory by id",
        flavor_id: "core",
      },
      {
        name: "core/emit_abstraction",
        description: "Emit one Abstraction memory",
        flavor_id: "core",
      },
      {
        name: "proxima-code/code_search_chunks",
        description: "Semantic search over indexed code chunks",
        flavor_id: "proxima-code",
      },
      {
        name: "proxima-code/code_open_file_revision",
        description: "Open a file revision by id",
        flavor_id: "proxima-code",
      },
    ]),
  );
  const listRelations = vi.fn(() =>
    ok<RelationTs[]>([
      {
        relation_id: "core/inspires",
        flavor_id: "core",
        class: "AbductiveOperator",
        typed: false,
      },
      {
        relation_id: "proxima-code/calls",
        flavor_id: "proxima-code",
        class: "Substrate",
        typed: false,
      },
    ]),
  );
  const wakeEntryProduces = vi.fn((_palette: string[]) =>
    ok({ schema_ids: [] as string[], relation_ids: [] as string[] }),
  );
  const listWakeInvocations = vi.fn((_) => ok<WakeInvocationTs[]>(invocations));

  return {
    client: {
      listPersonalityInstances,
      instantiatePersonality,
      setWakeEntries,
      tombstonePersonality,
      listMcpTools,
      listRelations,
      wakeEntryProduces,
      listWakeInvocations,
    } satisfies PersonalityCommandClient,
    listPersonalityInstances,
    instantiatePersonality,
    setWakeEntries,
    tombstonePersonality,
    listMcpTools,
    listRelations,
    wakeEntryProduces,
    listWakeInvocations,
  };
};

const selectPersonality = async (displayName: string) => {
  const card = await waitFor(() => {
    const node = screen.getByText(displayName).closest("article");
    if (!node) throw new Error(`personality node not rendered: ${displayName}`);
    return node;
  });
  fireEvent.click(card);
};

const selectEntry = async (label: string) => {
  await waitFor(() => {
    const button = screen.getByRole("button", { name: new RegExp(`^Edit ${label}`) });
    fireEvent.click(button);
  });
};

describe("PersonalitiesView", () => {
  beforeEach(() => {
    registerPayloadRenderer({
      schemaId: "proxima-code/commit-summary-v1",
      schemaVersion: 1,
      flavor: "proxima-code",
      renderer: { render: () => null },
    });
    registerPayloadRenderer({
      schemaId: "proxima-code/code-chunk-v1",
      schemaVersion: 1,
      flavor: "proxima-code",
      renderer: { render: () => null },
    });
    registerPersonalityType({
      typeId: "proxima-code/engineer-v1",
      flavor: "proxima-code",
      label: "Engineer",
      purpose: "Develop perspectives on code changes.",
    });
  });

  afterEach(() => {
    cleanup();
    clearRegistriesForTests();
    vi.restoreAllMocks();
  });

  it("renders one node per instance returned by ListPersonalityInstances", async () => {
    const { client } = mockClient([
      instance({ display_name: "Engineer A" }),
      instance({
        display_name: "Engineer B",
        personality_instance_id: "018f0000-0000-7000-8000-000000000002",
      }),
    ]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    expect(await screen.findAllByTestId("personality-card")).toHaveLength(2);
    expect(screen.getByText("Engineer A")).toBeTruthy();
    expect(screen.getByText("Engineer B")).toBeTruthy();
  });

  it("renders instance metadata without a FlavorDescriptor lookup", async () => {
    const { client } = mockClient([instance()]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    const chip = await screen.findByTestId("personality-flavor-chip");
    expect(chip.textContent).toContain("Instance");
  });

  it("zooms the personality canvas from controls and wheel input", async () => {
    const { client } = mockClient([instance()]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    const stage = await screen.findByTestId("personality-canvas-stage");
    const stageScale = (): number => {
      const match = /scale\(([0-9.]+)\)/.exec(stage.getAttribute("style") ?? "");
      return match ? Number(match[1]) : NaN;
    };
    expect(stageScale()).toBeCloseTo(0.9, 5);

    const zoomIn = screen.getByRole("button", { name: "Zoom in" });
    fireEvent.pointerDown(zoomIn, { button: 0, pointerId: 1 });
    fireEvent.pointerUp(zoomIn, { button: 0, pointerId: 1 });
    fireEvent.click(zoomIn);
    await waitFor(() => {
      expect(stageScale()).toBeCloseTo(0.9 * 1.12, 5);
    });

    fireEvent.wheel(stage.closest(".personality-canvas")!, { deltaY: -120 });
    await waitFor(() => {
      expect(stageScale()).toBeCloseTo(0.9 * 1.12 * 1.08, 5);
    });
  });

  it("renders wake invocation diagnostics for the selected personality", async () => {
    const { client, listWakeInvocations } = mockClient(
      [instance()],
      [instance()],
      [wakeInvocation()],
    );

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");

    await waitFor(() => {
      expect(listWakeInvocations).toHaveBeenCalledWith({
        owner,
        personality_instance_id: "018f0000-0000-7000-8000-000000000001",
        wake_entry_id: null,
        triggering_memory_id: null,
        change_event_seq: null,
        limit: 20,
      });
    });
    const invocationsSummary = await screen.findByText("Wake invocations");
    const invocationsSection = invocationsSummary.closest("details");
    expect(invocationsSection?.hasAttribute("open")).toBe(false);

    fireEvent.click(invocationsSummary);
    await waitFor(() => {
      expect(invocationsSection?.hasAttribute("open")).toBe(true);
    });
    expect(screen.getByText("Error: harness failed")).toBeTruthy();
    expect(screen.getByText("stderr tail")).toBeTruthy();
    expect(screen.getByText("proxima-agent-memory/proxima_derive")).toBeTruthy();
  });

  it("creates a personality through the create dialog", async () => {
    const alice = instance({
      display_name: "Alice",
      personality_instance_id: "018f0000-0000-7000-8000-000000000003",
    });
    const { client, instantiatePersonality } = mockClient([], [alice]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    const openButton = await screen.findByRole("button", {
      name: "Create new Personality",
    });
    await waitFor(() => {
      expect(openButton.hasAttribute("disabled")).toBe(false);
    });
    fireEvent.click(openButton);

    expect(await screen.findByRole("dialog")).toBeTruthy();
    fireEvent.input(screen.getByLabelText("Display name"), {
      target: { value: "Alice" },
    });
    fireEvent.input(screen.getByLabelText("Purpose"), {
      target: { value: "Test" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() => {
      expect(instantiatePersonality).toHaveBeenCalledWith({
        owner,
        display_name: "Alice",
        purpose: "Test",
      });
    });
    expect(await screen.findByText("Alice")).toBeTruthy();
  });

  it("renders an existing WakeEntry's fields when its chip is selected", async () => {
    const { client } = mockClient([instance()]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    expect(screen.getByDisplayValue("react-to-commit")).toBeTruthy();
    expect(screen.getByLabelText("Instructions")).toBeTruthy();
    expect(
      screen.getByRole("button", {
        name: /^Trigger id: proxima-code\/commit-summary-v1$/,
      }),
    ).toBeTruthy();
    expect(screen.getByRole("option", { name: "On memory" })).toHaveProperty(
      "value",
      "on_memory",
    );
    expect(screen.getByRole("option", { name: "On edge" })).toHaveProperty(
      "value",
      "on_edge",
    );
    expect(screen.getByRole("option", { name: "Self author" })).toHaveProperty(
      "value",
      "self_author",
    );
    expect(
      screen.getByRole("option", { name: "Substrate only" }),
    ).toHaveProperty("value", "substrate_only");
    expect(screen.getByRole("option", { name: "Standard" })).toHaveProperty(
      "value",
      "standard",
    );
  });

  it("does not publish text draft edits on every keystroke", async () => {
    const { client } = mockClient([instance()]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    const label = screen.getByDisplayValue("react-to-commit");
    fireEvent.input(label, {
      target: { value: "react-to-commit-edited" },
    });

    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();

    fireEvent.change(label, {
      target: { value: "react-to-commit-edited" },
    });

    expect(screen.getByRole("button", { name: "Save" })).toBeTruthy();
  });

  it("calls setWakeEntries with the full edited list when Save is clicked", async () => {
    const row = instance();
    const updated = instance({
      wake_entries: [wakeEntry({ label: "react-to-commit-edited" })],
    });
    const { client, setWakeEntries } = mockClient([row], [updated]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    fireEvent.change(screen.getByDisplayValue("react-to-commit"), {
      target: { value: "react-to-commit-edited" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(setWakeEntries).toHaveBeenCalledWith({
        owner,
        personality_instance_id: row.personality_instance_id,
        entries: [
          {
            trigger_kind: "on_memory",
            trigger_id: "proxima-code/commit-summary-v1",
            label: "react-to-commit-edited",
            enabled: true,
            execution_mode: "substrate_only",
            authored_by: "any",
            probability_promille: 1000,
            goal_scope: "none",
            instructions: "",
            model_tier: "standard",
            inference_target_ref: null,
            substrate_tool_palette: [],
            required_produced_schema_ids: [],
            max_rounds: 4,
          },
        ],
      });
    });
  });

  it("renders registry-backed trigger options for OnMemory entries", async () => {
    const row = instance();
    const updated = instance({
      wake_entries: [
        wakeEntry({
          trigger_id: "proxima-code/code-chunk-v1",
        }),
      ],
    });
    const { client, setWakeEntries } = mockClient([row], [updated]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    const triggerButton = screen.getByRole("button", {
      name: /^Trigger id: proxima-code\/commit-summary-v1$/,
    });
    fireEvent.click(triggerButton);

    const radio = await waitFor(() => {
      const dialog = screen.getByRole("dialog", { name: "Trigger id" });
      const inputs = dialog.querySelectorAll<HTMLInputElement>(
        'input[type="radio"]',
      );
      const target = Array.from(inputs).find((input) =>
        input
          .closest(".personality-tool-row")
          ?.textContent?.includes("proxima-code/code-chunk-v1"),
      );
      if (!target) throw new Error("radio for proxima-code/code-chunk-v1 not found");
      return target;
    });
    fireEvent.click(radio);
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(setWakeEntries).toHaveBeenCalled();
      expect(setWakeEntries.mock.calls[0][0].entries[0].trigger_id).toBe(
        "proxima-code/code-chunk-v1",
      );
    });
  });

  it("displays server-side typed errors verbatim on save failure", async () => {
    const row = instance();
    const { client, setWakeEntries } = mockClient([row]);
    setWakeEntries.mockResolvedValueOnce({
      status: "error",
      error: {
        code: "WakeEntryInvalid",
        message: "parse error at line 3",
        request_id: "req-1",
      },
    } as never);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    fireEvent.change(screen.getByDisplayValue("react-to-commit"), {
      target: { value: "react-to-commit-touched" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(
      await screen.findByText("WakeEntryInvalid: parse error at line 3"),
    ).toBeTruthy();
  });

  it("tombstones a personality from the inspector after inline confirmation", async () => {
    const row = instance({ display_name: "Alice" });
    const { client, tombstonePersonality } = mockClient([row], []);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Alice");
    fireEvent.click(screen.getByRole("button", { name: "Tombstone" }));
    expect(
      screen.getByText("Tombstone Alice? Wakes stop; memories remain."),
    ).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Confirm tombstone" }));

    await waitFor(() => {
      expect(tombstonePersonality).toHaveBeenCalledWith({
        owner,
        personality_instance_id: row.personality_instance_id,
      });
    });
    await waitFor(() => expect(screen.queryByText("Alice")).toBeNull());
  });

  it("cancels tombstone confirmation without calling the command", async () => {
    const row = instance({ display_name: "Alice" });
    const { client, tombstonePersonality } = mockClient([row]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Alice");
    fireEvent.click(screen.getByRole("button", { name: "Tombstone" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(tombstonePersonality).not.toHaveBeenCalled();
    expect(screen.getAllByText("Alice").length).toBeGreaterThan(0);
  });

  it("keeps the row and shows an error when tombstone fails", async () => {
    const row = instance({ display_name: "Alice" });
    const { client, tombstonePersonality } = mockClient([row]);
    tombstonePersonality.mockResolvedValueOnce({
      status: "error",
      error: { code: "internal", message: "db unavailable", request_id: null },
    } as never);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Alice");
    fireEvent.click(screen.getByRole("button", { name: "Tombstone" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm tombstone" }));

    expect(await screen.findByText("internal: db unavailable")).toBeTruthy();
    expect(screen.getAllByText("Alice").length).toBeGreaterThan(0);
  });

  it("surfaces needs_repair status and presents an empty entries list", async () => {
    const { client } = mockClient([
      instance({
        status: "needs_repair",
        wake_entries: [wakeEntry()],
      }),
    ]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    expect(await screen.findByText("Wake entries need repair.")).toBeTruthy();
    expect(screen.getByText("No wake entries yet.")).toBeTruthy();
  });

  it("edits wake entry instructions and sends them on save", async () => {
    const row = instance();
    const { client, setWakeEntries } = mockClient([row]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    const textarea = screen.getByLabelText("Instructions") as HTMLTextAreaElement;
    fireEvent.input(textarea, {
      target: { value: "Summarize the triggering commit and decide next action." },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(setWakeEntries).toHaveBeenCalled();
      expect(setWakeEntries.mock.calls[0][0].entries[0].instructions).toBe(
        "Summarize the triggering commit and decide next action.",
      );
      const removedField = ["recipe", "ref"].join("_");
      expect(removedField in setWakeEntries.mock.calls[0][0].entries[0]).toBe(false);
    });
  });

  it("renders substrate tools from listMcpTools and toggles selection on save", async () => {
    const row = instance();
    const { client, setWakeEntries } = mockClient([row]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    fireEvent.click(
      await screen.findByRole("button", { name: /Substrate tool palette/ }),
    );
    const checkbox = await waitFor(() => {
      const node = screen.getByRole("checkbox", {
        name: /core\/emit_abstraction/,
      });
      if (!node) throw new Error("substrate tool checkbox not rendered");
      return node as HTMLInputElement;
    });

    expect(checkbox.checked).toBe(false);
    fireEvent.click(checkbox);
    expect(checkbox.checked).toBe(true);
    expect(screen.getByText("core")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(setWakeEntries).toHaveBeenCalled();
      const sent = setWakeEntries.mock.calls[0][0].entries[0];
      expect(sent.substrate_tool_palette).toEqual(["core/emit_abstraction"]);
    });
  });

  it("renders only the substrate tool palette for v1 wake entries", async () => {
    const row = instance();
    const { client } = mockClient([row]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    await waitFor(() => {
      expect(
        screen.getByRole("button", { name: /Substrate tool palette/ }),
      ).toBeTruthy();
    });

    expect(screen.queryByText("Workspace tool palette")).toBeNull();
  });

  it("preserves inspector scroll position when runtime fields change", async () => {
    const row = instance();
    const { client } = mockClient([row]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    const section = await waitFor(() => {
      const node = document.querySelector(".personality-inspector-section");
      if (!(node instanceof HTMLElement)) {
        throw new Error("inspector section not rendered");
      }
      return node;
    });
    section.scrollTop = 160;

    const runtimeSummary = screen.getByText("Runtime");
    fireEvent.click(runtimeSummary);

    const modelTierSelect = screen.getByDisplayValue("Standard");
    fireEvent.change(modelTierSelect, { target: { value: "deep" } });

    await waitFor(() => {
      expect(section.scrollTop).toBe(160);
    });
  });

  it("renders a relation node when a wake entry's palette includes core/create_edge", async () => {
    const { client, wakeEntryProduces } = mockClient([
      instance({
        wake_entries: [
          wakeEntry({ substrate_tool_palette: ["core/create_edge"] }),
        ],
      }),
    ]);

    wakeEntryProduces.mockImplementation(async (palette: string[]) => {
      if (palette.length === 1 && palette[0] === "core/create_edge") {
        return {
          status: "ok" as const,
          data: { schema_ids: [], relation_ids: ["core/derived-from"] },
        };
      }
      return {
        status: "ok" as const,
        data: { schema_ids: [], relation_ids: [] },
      };
    });

    render(() => <PersonalitiesView client={client} owner={owner} />);

    const relationNode = await waitFor(() => {
      const node = document.querySelector(
        '[data-canvas-node="relation"]',
      );
      if (!node) throw new Error("relation node not rendered");
      return node;
    });
    expect(relationNode.textContent).toContain("core/derived-from");
  });

  it("renders a produces edge with the is-produces class for emit_abstraction palettes", async () => {
    const { client, wakeEntryProduces } = mockClient([
      instance({
        wake_entries: [
          wakeEntry({ substrate_tool_palette: ["core/emit_abstraction"] }),
        ],
      }),
    ]);

    wakeEntryProduces.mockImplementation(async (palette: string[]) => {
      if (palette.length === 1 && palette[0] === "core/emit_abstraction") {
        return {
          status: "ok" as const,
          data: {
            schema_ids: ["proxima-code/commit-summary-v1"],
            relation_ids: [],
          },
        };
      }
      return {
        status: "ok" as const,
        data: { schema_ids: [], relation_ids: [] },
      };
    });

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await waitFor(() => {
      const producesEdges = document.querySelectorAll(
        ".personality-edge.is-produces",
      );
      if (producesEdges.length === 0) {
        throw new Error("no produces edges rendered");
      }
    });
  });

  it("calls wakeEntryProduces once per distinct substrate palette key", async () => {
    const { client, wakeEntryProduces } = mockClient([
      instance({
        personality_instance_id: "018f0000-0000-7000-8000-000000000001",
        wake_entries: [
          wakeEntry({
            wake_entry_id: "aaaaaaaa-aaaa-7aaa-8aaa-aaaaaaaaaaaa",
            substrate_tool_palette: ["core/emit_abstraction"],
          }),
          wakeEntry({
            wake_entry_id: "bbbbbbbb-bbbb-7bbb-8bbb-bbbbbbbbbbbb",
            substrate_tool_palette: ["core/emit_abstraction"], // same palette
          }),
          wakeEntry({
            wake_entry_id: "cccccccc-cccc-7ccc-8ccc-cccccccccccc",
            substrate_tool_palette: ["core/create_edge"], // different palette
          }),
        ],
      }),
    ]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await waitFor(() => {
      // Two distinct palette keys: ["core/emit_abstraction"] and ["core/create_edge"]
      expect(wakeEntryProduces).toHaveBeenCalledTimes(2);
    });
  });
});
