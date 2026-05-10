import {
  cleanup,
  fireEvent,
  render,
  screen,
} from "@solidjs/testing-library";
import { encode } from "cbor-x";
import { afterEach, describe, expect, it } from "vitest";
import type { EntityKind, GoalRow } from "../bindings";
import {
  GraphFilterProvider,
  createGraphFilterStore,
} from "../graph-filter-store";
import {
  GraphProvider,
  sentinelOwner,
  type DecodedMemory,
  type GraphSnapshot,
  type GraphStore,
} from "../graph-store";
import { createHub } from "../hub";
import { clearRegistriesForTests } from "../registry";
import { FullSurface } from "./surface";

const owner = sentinelOwner();

const memory = (
  id: string,
  kind: EntityKind,
  schemaId: string,
): DecodedMemory => ({
  row: {
    id,
    kind,
    schema_id: schemaId,
    schema_version: 1,
    owner,
    payload: Array.from(encode({})),
  },
  payload: {},
});

const snapshot = (memories: DecodedMemory[], goals: GoalRow[] = []): GraphSnapshot => ({
  owner,
  schemas: [],
  memoriesById: new Map(memories.map((m) => [m.row.id, m])),
  goalsById: new Map(goals.map((g) => [g.id, g])),
  edgesById: new Map(),
  eventsBySeq: new Map(),
  pendingHydration: new Map(),
  decodeErrorsByEntity: new Map(),
  memoryProvenance: new Map(),
  streamStatus: "live",
  seqHighWater: null,
});

interface RenderResult {
  filter: ReturnType<typeof createGraphFilterStore>;
}

const renderSurface = (memories: DecodedMemory[], goals: GoalRow[] = []): RenderResult => {
  const hub = createHub([]);
  const filter = createGraphFilterStore();
  const store: GraphStore = {
    state: () => snapshot(memories, goals),
    refresh: () => Promise.resolve(),
  };
  render(() => (
    <GraphProvider store={store}>
      <GraphFilterProvider store={filter}>
        <FullSurface hub={hub} />
      </GraphFilterProvider>
    </GraphProvider>
  ));
  return { filter };
};

describe("Surface — orchestration", () => {
  afterEach(() => {
    cleanup();
    clearRegistriesForTests();
  });

  it("starts on the All tab and shows pillar badges", () => {
    renderSurface([
      memory("m1", "Fact", "schema-a"),
      memory("m2", "Abstraction", "schema-b"),
    ]);
    expect(screen.getByRole("tab", { name: /All/ })).not.toBeNull();
    expect(screen.getByRole("tab", { name: /All/ }).getAttribute("aria-selected")).toBe("true");
    expect(screen.getByText("F")).not.toBeNull();
    expect(screen.getByText("A")).not.toBeNull();
  });

  it("switching to F tab hides Abstractions", () => {
    renderSurface([
      memory("m1", "Fact", "schema-a"),
      memory("m2", "Abstraction", "schema-b"),
    ]);
    fireEvent.click(screen.getByRole("tab", { name: /F 1/ }));
    expect(screen.getByText("schema-a")).not.toBeNull();
    expect(screen.queryByText("schema-b")).toBeNull();
  });

  it("toggles filter drawer on ⚙ Filters click", () => {
    renderSurface([memory("m1", "Fact", "schema-a")]);
    expect(screen.queryByRole("dialog", { name: /filters/i })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /filters/i }));
    expect(screen.getByRole("dialog", { name: /filters/i })).not.toBeNull();
  });

  it("checking a schema in drawer adds a chip and filters list", () => {
    renderSurface([
      memory("m1", "Fact", "schema-a"),
      memory("m2", "Fact", "schema-b"),
    ]);
    fireEvent.click(screen.getByRole("button", { name: /filters/i }));
    fireEvent.click(screen.getByLabelText(/schema-a/));
    fireEvent.click(screen.getByRole("button", { name: /done/i }));
    expect(screen.getByText(/schema: schema-a/)).not.toBeNull();
    expect(screen.queryByText("schema-b")).toBeNull();
  });

  it("removing a chip restores the row", () => {
    const { filter } = renderSurface([
      memory("m1", "Fact", "schema-a"),
      memory("m2", "Fact", "schema-b"),
    ]);
    filter.setSchema("schema-a", true);
    expect(screen.queryByText("schema-b")).toBeNull();
    fireEvent.click(screen.getByLabelText(/remove schema chip/i));
    expect(screen.getByText("schema-a")).not.toBeNull();
    expect(screen.getByText("schema-b")).not.toBeNull();
  });

  it("clicking a row opens the detail pane with payload + metadata", () => {
    renderSurface([memory("m1", "Fact", "schema-a")]);
    expect(screen.queryByText("PAYLOAD")).toBeNull();
    fireEvent.click(screen.getByText("schema-a").closest("[role='row']")!);
    expect(screen.getByText("PAYLOAD")).not.toBeNull();
    expect(screen.getByText("METADATA")).not.toBeNull();
  });

  it("G tab shows goals from goalsById and selecting one opens the detail pane", () => {
    const goal: GoalRow = {
      id: "g1",
      schema_id: "test/goal-v1",
      schema_version: 1,
      owner,
      state: "Active",
      title: "Test goal",
      text: "test",
      parent_goal_ids: [],
      supersedes: null,
      payload: [],
    };
    renderSurface([], [goal]);
    fireEvent.click(screen.getByRole("tab", { name: /G 1/ }));
    // The goal's schema id should appear in the row list
    expect(screen.queryByText("test/goal-v1")).not.toBeNull();
    // Click the row to open detail pane
    fireEvent.click(screen.getByText("test/goal-v1").closest("[role='row']")!);
    expect(screen.queryByText(/PAYLOAD/)).not.toBeNull();
    expect(screen.queryByText(/METADATA/)).not.toBeNull();
  });
});
