import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { decode, encode } from "cbor-x";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { GoalDraft, GoalRow } from "../bindings";
import {
  GraphFilterProvider,
  createGraphFilterStore,
} from "../graph-filter-store";
import {
  GraphProvider,
  sentinelOwner,
  type GraphSnapshot,
  type GraphStore,
} from "../graph-store";
import { createHub } from "../hub";
import {
  clearRegistriesForTests,
  registerPayloadRenderer,
} from "../registry";
import { FullSurface } from "./surface";

const mocks = vi.hoisted(() => ({
  goalWrite: vi.fn(),
}));

vi.mock("../bindings", () => ({
  commands: {
    goalWrite: mocks.goalWrite,
  },
}));

const owner = sentinelOwner();

const proposedGoal = (overrides: Partial<GoalRow> = {}): GoalRow => ({
  id: "00000000-0000-0000-0000-000000000001",
  schema_id: "proxima-goal/simple-text-v1",
  schema_version: 1,
  owner,
  text: "Refactor the chunker",
  state: "Proposed",
  parent_goal_ids: [],
  supersedes: null,
  payload: Array.from(encode({ text: "Refactor the chunker" })),
  ...overrides,
});

const snapshot = (goals: GoalRow[]): GraphSnapshot => ({
  owner,
  schemas: [],
  memoriesById: new Map(),
  goalsById: new Map(goals.map((g) => [g.id, g])),
  edgesById: new Map(),
  eventsBySeq: new Map(),
  pendingHydration: new Map(),
  decodeErrorsByEntity: new Map(),
  streamStatus: "live",
  seqHighWater: null,
});

const okWrite = () =>
  Promise.resolve({
    status: "ok" as const,
    data: {
      goal_id: "00000000-0000-0000-0000-000000000099",
      change_event_seq: "00000000-0000-0000-0000-000000000098",
      idempotent_replay: false,
    },
  });

const renderWithGoals = (goals: GoalRow[]) => {
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
      render: (props) => {
        const payload = props.payload as { text: string };
        return <p class="goal-typed-text">{payload.text}</p>;
      },
    },
  });
  const hub = createHub([]);
  const store: GraphStore = {
    state: () => snapshot(goals),
    refresh: () => Promise.resolve(),
  };
  render(() => (
    <GraphProvider store={store}>
      <GraphFilterProvider store={createGraphFilterStore()}>
        <FullSurface hub={hub} />
      </GraphFilterProvider>
    </GraphProvider>
  ));
  fireEvent.click(screen.getByRole("button", { name: "Expand Goal DAG" }));
};

describe("Goal rail proposed section", () => {
  afterEach(() => {
    cleanup();
    clearRegistriesForTests();
    mocks.goalWrite.mockReset();
  });

  it("renders Proposed goals via the registered payload renderer", () => {
    renderWithGoals([proposedGoal()]);
    expect(screen.getByText("Refactor the chunker")).toBeTruthy();
    expect(screen.getByText("Proposed")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Accept proposal" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Modify proposal" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Decline proposal" })).toBeTruthy();
  });

  it("Accept supersedes the proposal with state=Active", async () => {
    mocks.goalWrite.mockImplementation(okWrite);
    const goal = proposedGoal();
    renderWithGoals([goal]);
    fireEvent.click(screen.getByRole("button", { name: "Accept proposal" }));
    await waitFor(() => expect(mocks.goalWrite).toHaveBeenCalledOnce());
    const draft = mocks.goalWrite.mock.calls[0]![0] as GoalDraft;
    expect(draft.state).toBe("Active");
    expect(draft.supersedes_goal_id).toBe(goal.id);
    expect(draft.authorship).toBe("User");
  });

  it("Decline supersedes the proposal with state=Rejected", async () => {
    mocks.goalWrite.mockImplementation(okWrite);
    const goal = proposedGoal();
    renderWithGoals([goal]);
    fireEvent.click(screen.getByRole("button", { name: "Decline proposal" }));
    await waitFor(() => expect(mocks.goalWrite).toHaveBeenCalledOnce());
    expect((mocks.goalWrite.mock.calls[0]![0] as GoalDraft).state).toBe(
      "Rejected",
    );
  });

  it("hides the Proposed section when there are no proposals", () => {
    renderWithGoals([]);
    expect(screen.queryByRole("button", { name: "Accept proposal" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Decline proposal" })).toBeNull();
    expect(screen.getByText("No goals")).toBeTruthy();
  });
});
