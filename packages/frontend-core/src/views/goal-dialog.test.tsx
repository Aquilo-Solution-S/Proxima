import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@solidjs/testing-library";
import { decode, encode } from "cbor-x";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { GoalDraft, GoalRow } from "../bindings";
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
}));

vi.mock("../bindings", () => ({
  commands: {
    goalWrite: mocks.goalWrite,
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

const emptySnapshot = (): GraphSnapshot => ({
  owner,
  schemas: [],
  memoriesById: new Map(),
  goalsById: new Map(),
  edgesById: new Map(),
  eventsBySeq: new Map(),
  pendingHydration: new Map(),
  decodeErrorsByEntity: new Map(),
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

const renderDialog = (
  proposal?: GoalRow,
  onClose: () => void = () => {},
  onAfterWrite: () => void = () => {},
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
});
