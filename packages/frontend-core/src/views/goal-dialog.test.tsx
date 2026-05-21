import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@solidjs/testing-library";
import { decode, encode } from "cbor-x";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  GoalDraft,
  GoalRow,
  PersonalityInstanceTs,
  WakeEntryTs,
} from "../bindings";
import {
  GraphProvider,
  sentinelOwner,
  type GraphSnapshot,
  type GraphStore,
} from "../graph-store";
import { createHub } from "../hub";
import {
  clearRegistriesForTests,
  registerGoalPayloadEditor,
  registerPayloadRenderer,
} from "../registry";
import { GoalDialog } from "./goal-dialog";

const mocks = vi.hoisted(() => ({
  goalWrite: vi.fn(),
  goalReactivate: vi.fn(),
  listPersonalityInstances: vi.fn(),
}));

vi.mock("../bindings", () => ({
  commands: {
    goalWrite: mocks.goalWrite,
    goalReactivate: mocks.goalReactivate,
    listPersonalityInstances: mocks.listPersonalityInstances,
  },
}));

const owner = sentinelOwner();

const okWrite = () =>
  Promise.resolve({
    status: "ok" as const,
    data: {
      goal_id: "00000000-0000-0000-0000-0000000000aa",
      change_event_seq: "00000000-0000-0000-0000-0000000000ab",
      idempotent_replay: false,
    },
  });

const okReactivate = () =>
  Promise.resolve({
    status: "ok" as const,
    data: {
      event_id: "feed000000000000000000000000000000000000000000000000000000000000",
      memory_id: "00000000-0000-0000-0000-0000000000ac",
      change_event_seq: "00000000-0000-0000-0000-0000000000ad",
      idempotent_replay: false,
    },
  });

const failedReactivate = () =>
  Promise.resolve({
    status: "error" as const,
    error: {
      code: "Internal" as const,
      message: "activation failed",
      request_id: null,
    },
  });

const emptySnapshot = (): GraphSnapshot => ({
  owner,
  schemas: [],
  memoriesById: new Map(),
  goalsById: new Map(),
  edgesById: new Map(),
  eventsBySeq: new Map(),
  pendingHydration: new Map(),
  decodeErrorsByEntity: new Map(),
  memoryProvenance: new Map(),
  streamStatus: "live",
  seqHighWater: null,
});

const registerSimpleTextSchema = () => {
  registerPayloadRenderer({
    schemaId: "proxima-goal/simple-text-v1",
    schemaVersion: 1,
    kind: "Goal",
    flavor: "proxima-goal",
    codec: {
      decode: (bytes) => decode(bytes),
      encode: (value) => encode(value),
    },
    renderer: {
      render: () => null,
    },
  });
  registerGoalPayloadEditor<Record<string, never>>({
    schemaId: "proxima-goal/simple-text-v1",
    schemaVersion: 1,
    flavor: "proxima-goal",
    label: "Simple text",
    defaults: () => ({}),
    component: () => null,
  });
};

const wakeEntry = (enabled = true): WakeEntryTs => ({
  wake_entry_id: "00000000-0000-0000-0000-000000000201",
  trigger_kind: "on_memory" as const,
  trigger_id: "proxima-goal/goal-activated-v1",
  label: "plan execution requests",
  enabled,
  execution_mode: "substrate_only" as const,
  authored_by: "other" as const,
  probability_promille: 1000,
  goal_scope: "trigger_goal_assigned" as const,
  instructions: "",
  model_tier: "deep" as const,
  inference_target_ref: null,
  substrate_tool_palette: [],
  workspace_tool_palette: [],
  workspace_binding: null,
  max_rounds: 16,
  disabled_reason: null,
});

const personality = (
  id: string,
  display_name: string,
  wake_entries = [wakeEntry()],
): PersonalityInstanceTs => ({
  owner,
  personality_instance_id: id,
  current_root_perspective_memory_id: `${id.slice(0, 24)}000000000999`,
  display_name,
  status: "active",
  wake_entries,
});

const renderDialog = (
  proposal?: GoalRow,
  onClose: () => void = () => {},
  onAfterWrite: () => void = () => {},
  assignmentMode?: "goal-reactive",
) => {
  const hub = createHub([]);
  const store: GraphStore = {
    state: () => emptySnapshot(),
    refresh: () => Promise.resolve(),
  };
  render(() => (
    <GraphProvider store={store}>
      <GoalDialog
        hub={hub}
        proposal={proposal}
        assignmentMode={assignmentMode}
        onClose={onClose}
        onAfterWrite={onAfterWrite}
      />
    </GraphProvider>
  ));
  return { hub };
};

describe("GoalDialog", () => {
  afterEach(() => {
    cleanup();
    clearRegistriesForTests();
    mocks.goalWrite.mockReset();
    mocks.goalReactivate.mockReset();
    mocks.listPersonalityInstances.mockReset();
  });

  it("creates a fresh Active goal under User authorship", async () => {
    registerSimpleTextSchema();
    mocks.goalWrite.mockImplementation(okWrite);
    const onClose = vi.fn();
    const onAfterWrite = vi.fn();
    renderDialog(undefined, onClose, onAfterWrite);

    fireEvent.input(screen.getByLabelText("Title"), {
      target: { value: "Ship goal dialog" },
    });
    fireEvent.input(screen.getByLabelText("Text"), {
      target: { value: "Detailed goal body" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() => expect(mocks.goalWrite).toHaveBeenCalledOnce());
    const draft = mocks.goalWrite.mock.calls[0]![0] as GoalDraft;
    expect(draft.state).toBe("Active");
    expect(draft.supersedes_goal_id).toBeNull();
    expect(draft.authorship).toBe("User");
    expect(draft.title).toBe("Ship goal dialog");
    expect(draft.text).toBe("Detailed goal body");
    expect(draft.schema_id).toBe("proxima-goal/simple-text-v1");
    expect(decode(new Uint8Array(draft.payload))).toEqual({});
    expect(onAfterWrite).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("modify-then-accept supersedes the proposal with edited payload", async () => {
    registerSimpleTextSchema();
    mocks.goalWrite.mockImplementation(okWrite);
    const proposal: GoalRow = {
      id: "00000000-0000-0000-0000-0000000000ff",
      schema_id: "proxima-goal/simple-text-v1",
      schema_version: 1,
      owner,
      title: "Original title",
      text: "Original",
      state: "Proposed",
      parent_goal_ids: [],
      supersedes: null,
      payload: Array.from(encode({})),
    };
    renderDialog(proposal);

    expect((screen.getByLabelText("Title") as HTMLInputElement).value).toBe(
      "Original title",
    );
    fireEvent.input(screen.getByLabelText("Title"), {
      target: { value: "Edited title" },
    });
    fireEvent.input(screen.getByLabelText("Text"), {
      target: { value: "Edited then accepted" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Accept" }));

    await waitFor(() => expect(mocks.goalWrite).toHaveBeenCalledOnce());
    const draft = mocks.goalWrite.mock.calls[0]![0] as GoalDraft;
    expect(draft.state).toBe("Active");
    expect(draft.supersedes_goal_id).toBe(proposal.id);
    expect(draft.title).toBe("Edited title");
    expect(draft.text).toBe("Edited then accepted");
  });

  it("falls back to a placeholder when no editors are registered", () => {
    renderDialog();
    expect(
      screen.getByText("No goal payload editors registered."),
    ).toBeTruthy();
  });

  it("creates a goal and assigns it to multiple selected personalities", async () => {
    registerSimpleTextSchema();
    const planner = personality(
      "00000000-0000-0000-0000-000000000211",
      "Planner",
    );
    const executor = personality(
      "00000000-0000-0000-0000-000000000212",
      "Executor",
    );
    mocks.listPersonalityInstances.mockResolvedValue({
      status: "ok",
      data: [planner, executor],
    });
    mocks.goalWrite.mockImplementation(okWrite);
    mocks.goalReactivate.mockImplementation(okReactivate);
    const onClose = vi.fn();
    const onAfterWrite = vi.fn();
    renderDialog(undefined, onClose, onAfterWrite, "goal-reactive");

    await screen.findByText("Planner");
    fireEvent.input(screen.getByLabelText("Title"), {
      target: { value: "Ship assigned goal" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() => expect(mocks.goalWrite).toHaveBeenCalledOnce());
    await waitFor(() => expect(mocks.goalReactivate).toHaveBeenCalledTimes(2));
    expect(mocks.goalReactivate).toHaveBeenNthCalledWith(1, {
      owner,
      goal_id: "00000000-0000-0000-0000-0000000000aa",
      target_personality_id: planner.personality_instance_id,
    });
    expect(mocks.goalReactivate).toHaveBeenNthCalledWith(2, {
      owner,
      goal_id: "00000000-0000-0000-0000-0000000000aa",
      target_personality_id: executor.personality_instance_id,
    });
    expect(onAfterWrite).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("filters assignment targets to goal-reactive personalities", async () => {
    registerSimpleTextSchema();
    const planner = personality(
      "00000000-0000-0000-0000-000000000221",
      "Planner",
    );
    const disabled = personality(
      "00000000-0000-0000-0000-000000000222",
      "Disabled",
      [wakeEntry(false)],
    );
    const wrongScope = personality(
      "00000000-0000-0000-0000-000000000223",
      "Wrong scope",
      [{ ...wakeEntry(), goal_scope: "none" as const }],
    );
    mocks.listPersonalityInstances.mockResolvedValue({
      status: "ok",
      data: [planner, disabled, wrongScope],
    });
    renderDialog(undefined, vi.fn(), vi.fn(), "goal-reactive");

    await screen.findByText("Planner");
    expect(screen.queryByText("Disabled")).toBeNull();
    expect(screen.queryByText("Wrong scope")).toBeNull();
  });

  it("keeps the created goal and retries only failed assignments", async () => {
    registerSimpleTextSchema();
    const planner = personality(
      "00000000-0000-0000-0000-000000000231",
      "Planner",
    );
    const executor = personality(
      "00000000-0000-0000-0000-000000000232",
      "Executor",
    );
    mocks.listPersonalityInstances.mockResolvedValue({
      status: "ok",
      data: [planner, executor],
    });
    mocks.goalWrite.mockImplementation(okWrite);
    mocks.goalReactivate
      .mockImplementationOnce(okReactivate)
      .mockImplementationOnce(failedReactivate)
      .mockImplementationOnce(okReactivate);
    const onClose = vi.fn();
    const onAfterWrite = vi.fn();
    renderDialog(undefined, onClose, onAfterWrite, "goal-reactive");

    await screen.findByText("Planner");
    fireEvent.input(screen.getByLabelText("Title"), {
      target: { value: "Partially assigned goal" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() => expect(mocks.goalReactivate).toHaveBeenCalledTimes(2));
    expect(await screen.findByText(/assignment failed for: Executor/)).toBeTruthy();
    expect(onClose).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Retry" }));

    await waitFor(() => expect(mocks.goalReactivate).toHaveBeenCalledTimes(3));
    expect(mocks.goalWrite).toHaveBeenCalledOnce();
    expect(mocks.goalReactivate).toHaveBeenNthCalledWith(3, {
      owner,
      goal_id: "00000000-0000-0000-0000-0000000000aa",
      target_personality_id: executor.personality_instance_id,
    });
    await waitFor(() => expect(onAfterWrite).toHaveBeenCalledOnce());
    expect(onClose).toHaveBeenCalledOnce();
  });
});
