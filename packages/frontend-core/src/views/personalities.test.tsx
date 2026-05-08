import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  BundledRecipeTs,
  InstantiatePersonalityOutcomeTs,
  McpToolTs,
  OwnerRecipesListingTs,
  PersonalityInstanceTs,
  SetWakeEntriesOutcomeTs,
  TombstonePersonalityOutcomeTs,
  WakeEntryTs,
  WorkspaceToolTs,
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
  recipe_ref: "user:default.yaml",
  model_tier: "standard",
  inference_target_ref: null,
  substrate_tool_palette: [],
  workspace_tool_palette: [],
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

const mockClient = (
  initial: PersonalityInstanceTs[],
  afterRefresh: PersonalityInstanceTs[] = initial,
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
  const listOwnerRecipes = vi.fn((_) =>
    ok<OwnerRecipesListingTs>({
      root_path: "/tmp/recipes",
      recipes: [{ filename: "default.yaml", modified_at: null }],
    }),
  );
  const listBundledRecipes = vi.fn(() =>
    ok<BundledRecipeTs[]>([]),
  );
  const listMcpTools = vi.fn(() =>
    ok<McpToolTs[]>([
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
  const listWorkspaceTools = vi.fn(() =>
    ok<WorkspaceToolTs[]>([
      { id: "proxima-workspace/shell", description: "Run shell commands" },
      {
        id: "proxima-workspace/text_editor",
        description: "View and edit files",
      },
      { id: "proxima-workspace/list_files", description: "List directories" },
    ]),
  );

  return {
    client: {
      listPersonalityInstances,
      instantiatePersonality,
      setWakeEntries,
      tombstonePersonality,
      listOwnerRecipes,
      listBundledRecipes,
      listMcpTools,
      listWorkspaceTools,
    } satisfies PersonalityCommandClient,
    listPersonalityInstances,
    instantiatePersonality,
    setWakeEntries,
    tombstonePersonality,
    listOwnerRecipes,
    listBundledRecipes,
    listMcpTools,
    listWorkspaceTools,
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
    const recipeSelect = screen.getByLabelText("Recipe") as HTMLSelectElement;
    await waitFor(() => {
      expect(recipeSelect.value).toBe("user:default.yaml");
    });
    expect(screen.getByLabelText("Trigger id")).toHaveProperty(
      "value",
      "proxima-code/commit-summary-v1",
    );
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
            recipe_ref: "user:default.yaml",
            model_tier: "standard",
            inference_target_ref: null,
            substrate_tool_palette: [],
            workspace_tool_palette: [],
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

    const trigger = screen.getByLabelText("Trigger id");
    expect(trigger.tagName).toBe("SELECT");
    fireEvent.change(trigger, {
      target: { value: "proxima-code/code-chunk-v1" },
    });
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
        code: "RecipeInvalid",
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
      await screen.findByText("RecipeInvalid: parse error at line 3"),
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

  it("lists user recipes from listOwnerRecipes and writes user:<filename> on save", async () => {
    const row = instance();
    const { client, setWakeEntries, listOwnerRecipes } = mockClient([row]);
    listOwnerRecipes.mockResolvedValue({
      status: "ok",
      data: {
        root_path: "/tmp/recipes/owner",
        recipes: [
          { filename: "default.yaml", modified_at: null },
          { filename: "review.yaml", modified_at: null },
        ],
      },
    });

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    const select = await waitFor(() => {
      const node = screen.getByLabelText("Recipe") as HTMLSelectElement;
      const labels = Array.from(node.options).map((opt) => opt.textContent);
      if (!labels.includes("review.yaml")) {
        throw new Error(`recipes not loaded yet, got ${labels.join(",")}`);
      }
      return node;
    });
    fireEvent.change(select, { target: { value: "user:review.yaml" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(setWakeEntries).toHaveBeenCalled();
      expect(setWakeEntries.mock.calls[0][0].entries[0].recipe_ref).toBe(
        "user:review.yaml",
      );
    });
  });

  it("shows an empty-folder hint when the recipes folder has no yaml", async () => {
    const row = instance({
      wake_entries: [wakeEntry({ recipe_ref: "" })],
    });
    const { client, listOwnerRecipes } = mockClient([row]);
    listOwnerRecipes.mockResolvedValue({
      status: "ok",
      data: { root_path: "/tmp/recipes/owner", recipes: [] },
    });

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    expect(
      await screen.findByText(/No recipes found/),
    ).toBeTruthy();
    expect(screen.getByText("/tmp/recipes/owner")).toBeTruthy();
  });

  it("renders an orphan badge when recipe_ref points at a missing file", async () => {
    const row = instance({
      wake_entries: [wakeEntry({ recipe_ref: "user:gone.yaml" })],
    });
    const { client, listOwnerRecipes } = mockClient([row]);
    listOwnerRecipes.mockResolvedValue({
      status: "ok",
      data: {
        root_path: "/tmp/recipes/owner",
        recipes: [{ filename: "default.yaml", modified_at: null }],
      },
    });

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    await waitFor(() => {
      const select = screen.getByLabelText("Recipe") as HTMLSelectElement;
      const selected = select.options[select.selectedIndex];
      expect(selected.textContent).toBe("gone.yaml (missing)");
    });
  });

  it("opens the bundled tab when recipe_ref starts with bundled:", async () => {
    const row = instance({
      wake_entries: [wakeEntry({ recipe_ref: "bundled:proxima-code/engineer" })],
    });
    const { client, listBundledRecipes } = mockClient([row]);
    listBundledRecipes.mockResolvedValue({
      status: "ok",
      data: [
        { slug: "proxima-code/engineer", flavor_id: "proxima-code" },
        { slug: "proxima-code/commit_summary", flavor_id: "proxima-code" },
      ],
    });

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    const bundledTab = await screen.findByRole("tab", { name: "Bundled" });
    expect(bundledTab.getAttribute("aria-selected")).toBe("true");

    await waitFor(() => {
      const select = screen.getByLabelText("Recipe") as HTMLSelectElement;
      expect(select.value).toBe("bundled:proxima-code/engineer");
    });
  });

  it("writes bundled:<slug> when a bundled option is selected", async () => {
    const row = instance({
      wake_entries: [wakeEntry({ recipe_ref: "" })],
    });
    const { client, setWakeEntries, listBundledRecipes } = mockClient([row]);
    listBundledRecipes.mockResolvedValue({
      status: "ok",
      data: [
        { slug: "proxima-code/engineer", flavor_id: "proxima-code" },
      ],
    });

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    fireEvent.click(screen.getByRole("tab", { name: "Bundled" }));
    const select = await waitFor(() => {
      const node = screen.getByLabelText("Recipe") as HTMLSelectElement;
      const labels = Array.from(node.options).map((opt) => opt.textContent);
      if (!labels.includes("proxima-code/engineer")) {
        throw new Error(`bundled options not loaded: ${labels.join(",")}`);
      }
      return node;
    });
    fireEvent.change(select, {
      target: { value: "bundled:proxima-code/engineer" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(setWakeEntries).toHaveBeenCalled();
      expect(setWakeEntries.mock.calls[0][0].entries[0].recipe_ref).toBe(
        "bundled:proxima-code/engineer",
      );
    });
  });

  it("preserves recipe_ref when the user toggles tabs without selecting", async () => {
    const row = instance({
      wake_entries: [wakeEntry({ recipe_ref: "user:default.yaml" })],
    });
    const { client, setWakeEntries } = mockClient([row]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    fireEvent.click(screen.getByRole("tab", { name: "Bundled" }));
    fireEvent.click(screen.getByRole("tab", { name: "Private" }));

    await waitFor(() => {
      const select = screen.getByLabelText("Recipe") as HTMLSelectElement;
      expect(select.value).toBe("user:default.yaml");
    });

    expect(setWakeEntries).not.toHaveBeenCalled();
  });

  it("renders substrate tools from listMcpTools and toggles selection on save", async () => {
    const row = instance();
    const { client, setWakeEntries } = mockClient([row]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    const checkbox = await waitFor(() => {
      const labels = screen.getAllByText("proxima-code/code_search_chunks");
      const node = labels[0].closest("label")?.querySelector("input");
      if (!node) throw new Error("substrate tool checkbox not rendered");
      return node as HTMLInputElement;
    });

    expect(checkbox.checked).toBe(false);
    fireEvent.click(checkbox);
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(setWakeEntries).toHaveBeenCalled();
      const sent = setWakeEntries.mock.calls[0][0].entries[0];
      expect(sent.substrate_tool_palette).toEqual([
        "proxima-code/code_search_chunks",
      ]);
    });
  });

  it("hides the workspace tool picker when execution_mode is substrate_only", async () => {
    const row = instance();
    const { client } = mockClient([row]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    await waitFor(() => {
      expect(
        screen.getByText("proxima-code/code_search_chunks"),
      ).toBeTruthy();
    });

    expect(screen.queryByText("Workspace tool palette")).toBeNull();
    expect(screen.queryByText("proxima-workspace/shell")).toBeNull();
  });

  it("shows the workspace tool picker after switching execution_mode to workspace", async () => {
    const row = instance();
    const { client } = mockClient([row]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    const runtimeSummary = await waitFor(() => {
      const node = screen.getByText("Runtime");
      if (!(node instanceof HTMLElement)) {
        throw new Error("runtime section not rendered");
      }
      return node;
    });
    fireEvent.click(runtimeSummary);

    const executionSelect = await waitFor(() =>
      screen.getByDisplayValue("Substrate only"),
    );
    fireEvent.change(executionSelect, { target: { value: "workspace" } });

    await waitFor(() => {
      expect(screen.getByText("Workspace tool palette")).toBeTruthy();
      expect(screen.getByText("proxima-workspace/shell")).toBeTruthy();
    });
  });

  it("preserves workspace_tool_palette when toggling execution_mode back to substrate_only", async () => {
    const row = instance({
      wake_entries: [
        wakeEntry({
          execution_mode: "workspace",
          workspace_tool_palette: ["proxima-workspace/shell"],
        }),
      ],
    });
    const { client, setWakeEntries } = mockClient([row]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await selectPersonality("Engineer");
    await selectEntry("react-to-commit");

    await waitFor(() => {
      expect(screen.getByText("Workspace tool palette")).toBeTruthy();
    });

    const runtimeSummary = screen.getByText("Runtime");
    fireEvent.click(runtimeSummary);

    const executionSelect = screen.getByDisplayValue("Workspace");
    fireEvent.change(executionSelect, { target: { value: "substrate_only" } });

    await waitFor(() => {
      expect(screen.queryByText("Workspace tool palette")).toBeNull();
    });

    fireEvent.change(executionSelect, { target: { value: "workspace" } });

    await waitFor(() => {
      const labels = screen.getAllByText("proxima-workspace/shell");
      const checkbox = labels[0].closest("label")?.querySelector("input");
      expect(checkbox).toBeTruthy();
      expect((checkbox as HTMLInputElement).checked).toBe(true);
    });

    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => {
      expect(setWakeEntries).toHaveBeenCalled();
      const sent = setWakeEntries.mock.calls[0][0].entries[0];
      expect(sent.workspace_tool_palette).toEqual([
        "proxima-workspace/shell",
      ]);
      expect(sent.execution_mode).toBe("workspace");
    });
  });
});
