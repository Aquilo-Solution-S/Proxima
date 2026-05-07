import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { sentinelOwner } from "../graph-store";
import {
  clearRegistriesForTests,
  registerPayloadRenderer,
  registerPersonalityType,
} from "../registry";
import type {
  InstantiatePersonalityOutcomeTs,
  Owner,
  PersonalityInstanceTs,
  SetWakeConfigOutcomeTs,
  TombstonePersonalityOutcomeTs,
  WakeFilterTs,
} from "../bindings";
import {
  PersonalitiesView,
  type PersonalityCommandClient,
} from "./personalities";

const owner = sentinelOwner();

const ok = <T,>(data: T) =>
  Promise.resolve({ status: "ok" as const, data });

const onMemory = (probability = 1): WakeFilterTs => ({
  kind: "on_memory",
  version: 1,
  schema_id: "proxima-code/commit-summary-v1",
  authored_by: { kind: "any" },
  probability,
});

const onMemorySchema = (schemaId: string): WakeFilterTs => ({
  kind: "on_memory",
  version: 1,
  schema_id: schemaId,
  authored_by: { kind: "any" },
  probability: 1,
});

const instance = (
  overrides: Partial<PersonalityInstanceTs> = {},
): PersonalityInstanceTs => ({
  owner,
  personality_type_id: "proxima-code/engineer-v1",
  personality_instance_id: "018f0000-0000-7000-8000-000000000001",
  current_self_perspective_memory_id: "018f0000-0000-7000-8000-000000000101",
  display_name: "Engineer",
  status: "active",
  wake_filters: [onMemory()],
  flavor: {
    flavor_id: "proxima-code",
    display_name: "Code",
    package_version: "0.1.0",
    author: null,
    provenance: { kind: "builtin" },
  },
  ...overrides,
});

const mockClient = (
  initial: PersonalityInstanceTs[],
  afterRefresh: PersonalityInstanceTs[] = initial,
) => {
  const provisionOwner = vi.fn((_: Owner) => ok(null));
  const listPersonalityInstances = vi
    .fn()
    .mockResolvedValueOnce({ status: "ok", data: initial })
    .mockResolvedValue({ status: "ok", data: afterRefresh });
  const instantiatePersonality = vi.fn((_) =>
    ok<InstantiatePersonalityOutcomeTs>({
      instance_id: "018f0000-0000-7000-8000-000000000099",
    }),
  );
  const setWakeConfig = vi.fn((_) =>
    ok<SetWakeConfigOutcomeTs>({ status: "active" }),
  );
  const tombstonePersonality = vi.fn((_) =>
    ok<TombstonePersonalityOutcomeTs>({
      status: "tombstoned",
      idempotent_replay: false,
    }),
  );

  return {
    client: {
      provisionOwner,
      listPersonalityInstances,
      instantiatePersonality,
      setWakeConfig,
      tombstonePersonality,
    } satisfies PersonalityCommandClient,
    provisionOwner,
    listPersonalityInstances,
    instantiatePersonality,
    setWakeConfig,
    tombstonePersonality,
  };
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
      defaultDisplayName: "Engineer",
      defaultPurpose: "Develop perspectives on code changes",
    });
  });

  afterEach(() => {
    cleanup();
    clearRegistriesForTests();
    vi.restoreAllMocks();
  });

  it("renders one card per instance returned by ListPersonalityInstances", async () => {
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

  it("renders the flavor chip from the typed FlavorDescriptor", async () => {
    const { client } = mockClient([
      instance({
        flavor: {
          flavor_id: "proxima-code",
          display_name: "Code",
          package_version: "0.1.0",
          author: null,
          provenance: { kind: "builtin" },
        },
      }),
    ]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    const chip = await screen.findByTestId("personality-flavor-chip");
    expect(chip.textContent).toContain("Flavor Code");
  });

  it("creates a personality through the flavor-type dialog", async () => {
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
    expect(screen.getByText("Engineer")).toBeTruthy();
    expect(screen.getByText("proxima-code")).toBeTruthy();
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
        personality_type_id: "proxima-code/engineer-v1",
        payload_overrides: JSON.stringify({
          display_name: "Alice",
          purpose: "Test",
        }),
      });
    });
    expect(await screen.findByText("Alice")).toBeTruthy();
  });

  it("round-trips wake-config edits through SetWakeConfig", async () => {
    const row = instance();
    const updated = instance({ wake_filters: [onMemory(0.5)] });
    const { client, setWakeConfig } = mockClient([row], [updated]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await screen.findByText("Engineer");
    fireEvent.click(screen.getByRole("button", { name: "Edit wake config" }));
    const probability = screen.getByLabelText("Probability (promille)");
    expect(probability.getAttribute("min")).toBe("0");
    expect(probability.getAttribute("max")).toBe("1000");
    expect(probability.getAttribute("step")).toBe("1");
    fireEvent.input(probability, {
      target: { value: "500" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(setWakeConfig).toHaveBeenCalledWith({
        owner,
        personality_type_id: row.personality_type_id,
        personality_instance_id: row.personality_instance_id,
        wake_filters: [onMemory(0.5)],
      });
    });
  });

  it("edits OnMemory schema through a registry-backed select", async () => {
    const row = instance();
    const updated = instance({
      wake_filters: [onMemorySchema("proxima-code/code-chunk-v1")],
    });
    const { client, setWakeConfig } = mockClient([row], [updated]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await screen.findByText("Engineer");
    fireEvent.click(screen.getByRole("button", { name: "Edit wake config" }));

    const schema = screen.getByLabelText("Schema");
    expect(schema.tagName).toBe("SELECT");
    fireEvent.change(schema, {
      target: { value: "proxima-code/code-chunk-v1" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(setWakeConfig).toHaveBeenCalledWith({
        owner,
        personality_type_id: row.personality_type_id,
        personality_instance_id: row.personality_instance_id,
        wake_filters: [onMemorySchema("proxima-code/code-chunk-v1")],
      });
    });
  });

  it("tombstones an engineer after inline confirmation and removes it locally", async () => {
    const row = instance({ display_name: "Alice" });
    const { client, tombstonePersonality } = mockClient([row], []);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await screen.findByText("Alice");
    fireEvent.click(screen.getByRole("button", { name: "Tombstone" }));
    expect(
      screen.getByText("Tombstone Alice? Wakes stop; memories remain."),
    ).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Confirm tombstone" }));

    await waitFor(() => {
      expect(tombstonePersonality).toHaveBeenCalledWith({
        owner,
        personality_type_id: row.personality_type_id,
        personality_instance_id: row.personality_instance_id,
      });
    });
    await waitFor(() => expect(screen.queryByText("Alice")).toBeNull());
  });

  it("cancels tombstone confirmation without calling the command", async () => {
    const row = instance({ display_name: "Alice" });
    const { client, tombstonePersonality } = mockClient([row]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await screen.findByText("Alice");
    fireEvent.click(screen.getByRole("button", { name: "Tombstone" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(tombstonePersonality).not.toHaveBeenCalled();
    expect(screen.getByText("Alice")).toBeTruthy();
  });

  it("keeps the row and shows an error when tombstone fails", async () => {
    const row = instance({ display_name: "Alice" });
    const { client, tombstonePersonality } = mockClient([row]);
    tombstonePersonality.mockResolvedValueOnce({
      status: "error",
      error: { code: "internal", message: "db unavailable" },
    } as never);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    await screen.findByText("Alice");
    fireEvent.click(screen.getByRole("button", { name: "Tombstone" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm tombstone" }));

    expect(await screen.findByText(/db unavailable/)).toBeTruthy();
    expect(screen.getByText("Alice")).toBeTruthy();
  });

  it("renders needs_repair banner and opens re-edit with empty filters", async () => {
    const { client } = mockClient([
      instance({
        status: "needs_repair",
        wake_filters: [onMemory()],
      }),
    ]);

    render(() => <PersonalitiesView client={client} owner={owner} />);

    expect(await screen.findByText(/Wake config needs repair/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Re-edit" }));

    expect(screen.getByTestId("wake-filters-list").children).toHaveLength(0);
  });
});
