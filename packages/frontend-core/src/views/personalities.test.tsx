import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { sentinelOwner } from "../graph-store";
import type {
  InstantiatePersonalityOutcomeTs,
  Owner,
  PersonalityInstanceTs,
  SetWakeConfigOutcomeTs,
  WakeFilterTs,
} from "../bindings";
import {
  EngineerInstancesPanel,
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

  return {
    client: {
      provisionOwner,
      listPersonalityInstances,
      instantiatePersonality,
      setWakeConfig,
    } satisfies PersonalityCommandClient,
    provisionOwner,
    listPersonalityInstances,
    instantiatePersonality,
    setWakeConfig,
  };
};

describe("EngineerInstancesPanel", () => {
  afterEach(() => {
    cleanup();
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

    render(() => <EngineerInstancesPanel client={client} owner={owner} />);

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

    render(() => <EngineerInstancesPanel client={client} owner={owner} />);

    const chip = await screen.findByTestId("personality-flavor-chip");
    expect(chip.textContent).toContain("Flavor Code");
  });

  it("posts the create-engineer request and adds the new instance", async () => {
    const alice = instance({
      display_name: "Alice",
      personality_instance_id: "018f0000-0000-7000-8000-000000000003",
    });
    const { client, instantiatePersonality } = mockClient([], [alice]);

    render(() => <EngineerInstancesPanel client={client} owner={owner} />);

    const createButton = await screen.findByRole("button", {
      name: "Create another Engineer",
    });
    await waitFor(() => {
      expect(createButton.hasAttribute("disabled")).toBe(false);
    });
    fireEvent.input(screen.getByLabelText("Display name"), {
      target: { value: "Alice" },
    });
    fireEvent.input(screen.getByLabelText("Purpose"), {
      target: { value: "Test" },
    });
    fireEvent.click(createButton);

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

    render(() => <EngineerInstancesPanel client={client} owner={owner} />);

    await screen.findByText("Engineer");
    fireEvent.click(screen.getByRole("button", { name: "Edit wake config" }));
    fireEvent.input(screen.getByLabelText("Probability"), {
      target: { value: "0.5" },
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

  it("renders needs_repair banner and opens re-edit with empty filters", async () => {
    const { client } = mockClient([
      instance({
        status: "needs_repair",
        wake_filters: [onMemory()],
      }),
    ]);

    render(() => <EngineerInstancesPanel client={client} owner={owner} />);

    expect(await screen.findByText(/Wake config needs repair/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Re-edit" }));

    expect(screen.getByTestId("wake-filters-list").children).toHaveLength(0);
  });
});
