import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import * as THREE from "three";
import type {
  EdgeRow,
  GoalRow,
  MemoryRow,
  Owner,
  PersonalityInstanceTs,
} from "../../bindings";
import { GraphFilterProvider, createGraphFilterStore } from "../../graph-filter-store";
import {
  GraphProvider,
  MAX_SNAPSHOT_EDGES,
  type GraphSnapshot,
  type GraphStore,
} from "../../graph-store";
import { createHub } from "../../hub";
import {
  clearRegistriesForTests,
  registerGoalPayloadEditor,
  registerPayloadRenderer,
} from "../../registry";
import { Atlas } from "./index";
import type { AtlasEdge, AtlasNode } from "./types";

const goalWriteMock = vi.hoisted(() => vi.fn());
const goalReactivateMock = vi.hoisted(() => vi.fn());
const listPersonalityInstancesMock = vi.hoisted(() => vi.fn());

vi.mock("../../bindings", () => ({
  commands: {
    goalWrite: goalWriteMock,
    goalReactivate: goalReactivateMock,
    listPersonalityInstances: listPersonalityInstancesMock,
  },
}));

beforeAll(() => {
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
    fillText() {},
    strokeText() {},
  } as unknown as CanvasRenderingContext2D);
});

vi.mock("three", async () => {
  const actual = await vi.importActual<typeof import("three")>("three");
  class WebGLRenderer {
    domElement = document.createElement("canvas");
    setPixelRatio() {}
    setSize() {}
    render() {}
    dispose() {}
  }
  return { ...actual, WebGLRenderer };
});

const owner: Owner = {
  principal: { User: "00000000-0000-0000-0000-000000000000" },
  org_id: "00000000-0000-0000-0000-000000000000",
};

const memory = (id: string, kind: MemoryRow["kind"]): MemoryRow => ({
  id,
  kind,
  schema_id: "proxima-code/code-chunk-v1",
  schema_version: 1,
  owner,
  payload: [],
});

const edge = (id: string, source: string, target: string): EdgeRow => ({
  id,
  relation: "code/calls",
  relation_class: "Structural",
  source: { Memory: source },
  target: { Memory: target },
  payload: [],
});

const atlasNode = (
  id: string,
  kind: AtlasNode["kind"],
  title: string,
  x: number,
): AtlasNode => ({
  id,
  kind,
  schemaId: "proxima-code/code-chunk-v1",
  schemaVersion: 1,
  title,
  flavor: "code",
  x,
  y: 0,
});

const activeGoal = (id: string): GoalRow => ({
  id,
  schema_id: "proxima-goal/simple-text-v1",
  schema_version: 1,
  owner,
  title: "Planner handoff",
  text: "Emit execution requests.",
  state: "Active",
  parent_goal_ids: [],
  supersedes: null,
  payload: [],
});

const okWrite = () =>
  Promise.resolve({
    status: "ok" as const,
    data: {
      goal_id: "019dfa50-0000-7000-8000-000000000301",
      change_event_seq: "019dfa50-0000-7000-8000-000000000302",
      idempotent_replay: false,
    },
  });

const okReactivate = () =>
  Promise.resolve({
    status: "ok" as const,
    data: {
      event_id: "feed000000000000000000000000000000000000000000000000000000000000",
      memory_id: "019dfa50-0000-7000-8000-000000000303",
      change_event_seq: "019dfa50-0000-7000-8000-000000000304",
      idempotent_replay: false,
    },
  });

const wakeEntry = () => ({
  wake_entry_id: "019dfa50-0000-7000-8000-000000000305",
  trigger_kind: "on_memory" as const,
  trigger_id: "proxima-goal/goal-activated-v1",
  label: "plan execution requests",
  enabled: true,
  execution_mode: "substrate_only" as const,
  authored_by: "other" as const,
  probability_promille: 1000,
  goal_scope: "trigger_goal_assigned" as const,
  instructions: "",
  model_tier: "deep" as const,
  inference_target_ref: null,
  substrate_tool_palette: [],
  required_produced_schema_ids: [],
  max_rounds: 16,
  disabled_reason: null,
});

const planner = (id: string, display_name: string): PersonalityInstanceTs => ({
  owner,
  personality_instance_id: id,
  current_root_perspective_memory_id: "019dfa50-0000-7000-8000-000000000306",
  display_name,
  status: "active",
  wake_entries: [wakeEntry()],
});

const registerSimpleTextSchema = () => {
  registerPayloadRenderer({
    schemaId: "proxima-goal/simple-text-v1",
    schemaVersion: 1,
    kind: "Goal",
    flavor: "proxima-goal",
    codec: {
      decode: () => ({}),
      encode: () => new Uint8Array([0xa0]),
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

const atlasEdge = (id: string, src: string, tgt: string, kind = "code/calls"): AtlasEdge => ({
  id,
  src,
  tgt,
  kind,
});

const snapshot = (
  memories: MemoryRow[],
  edges: EdgeRow[],
  edgeSizeOverride?: number,
): GraphSnapshot => {
  const edgesById = new Map(edges.map((row) => [row.id, row]));
  const sizedEdges =
    edgeSizeOverride === undefined
      ? edgesById
      : (new Proxy(edgesById, {
          get(target, prop, receiver) {
            if (prop === "size") return edgeSizeOverride;
            const value = Reflect.get(target, prop, receiver);
            return typeof value === "function" ? value.bind(target) : value;
          },
        }) as ReadonlyMap<string, EdgeRow>);
  return {
    owner,
    schemas: [],
    memoriesById: new Map(memories.map((row) => [row.id, { row, payload: null }])),
    goalsById: new Map(),
    edgesById: sizedEdges,
    eventsBySeq: new Map(),
    pendingHydration: new Map(),
    decodeErrorsByEntity: new Map(),
    memoryProvenance: new Map(),
    streamStatus: "live",
    seqHighWater: null,
  };
};

describe("Atlas graph wiring", () => {
  afterEach(() => {
    cleanup();
    clearRegistriesForTests();
    goalWriteMock.mockReset();
    goalReactivateMock.mockReset();
    listPersonalityInstancesMock.mockReset();
  });

  it("uses GraphStore rows when nodes and edges props are omitted", () => {
    const fact = memory("019dfa40-0000-7000-8000-000000000001", "Fact");
    const abs = memory("019dfa40-0000-7000-8000-000000000002", "Abstraction");
    const store: GraphStore = {
      state: () => snapshot([fact, abs], [edge("019dfa40-0000-7000-8000-000000000003", abs.id, fact.id)]),
      refresh: () => Promise.resolve(),
    };
    const { container } = render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <Atlas hub={createHub([])} />
        </GraphFilterProvider>
      </GraphProvider>
    ));
    const statValues = Array.from(container.querySelectorAll(".atlas-stat .v")).map((el) => el.textContent);
    expect(statValues).toContain("2");
    expect(statValues).toContain("1");
  });

  it("hydrates empty payloads only after node selection", async () => {
    const fact = memory("019dfa40-0000-7000-8000-000000000004", "Fact");
    const hydrate = vi.fn().mockResolvedValue(undefined);
    const raycast = vi
      .spyOn(THREE.Raycaster.prototype, "intersectObjects")
      .mockImplementation((objects) => [{ object: objects[0] }] as THREE.Intersection[]);
    const store: GraphStore = {
      state: () => snapshot([fact], []),
      refresh: () => Promise.resolve(),
      hydrate,
    };
    const { container } = render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <Atlas hub={createHub([])} />
        </GraphFilterProvider>
      </GraphProvider>
    ));
    const canvas = container.querySelector(".atlas-canvas canvas")!;

    fireEvent.pointerMove(canvas);
    await Promise.resolve();
    expect(hydrate).not.toHaveBeenCalled();

    fireEvent.click(canvas);
    await waitFor(() =>
      expect(hydrate).toHaveBeenCalledWith({ memory_ids: [fact.id] }),
    );
    raycast.mockRestore();
  });

  it("keeps the selected id when hydration changes filtered payload size", async () => {
    const light = memory("019dfa40-0000-7000-8000-000000000005", "Fact");
    const full: MemoryRow = { ...light, payload: [1, 2, 3] };
    const filters = createGraphFilterStore();
    filters.setSizeRange({ minBytes: 0, maxBytes: 0 });
    let setGraphState: (next: GraphSnapshot) => void = () => {};
    const hydrate = vi.fn().mockImplementation(async () => {
      setGraphState(snapshot([full], []));
    });
    const raycast = vi
      .spyOn(THREE.Raycaster.prototype, "intersectObjects")
      .mockImplementation((objects) => [{ object: objects[0] }] as THREE.Intersection[]);
    const { container } = render(() => {
      const [graphState, setState] = createSignal(snapshot([light], []));
      setGraphState = setState;
      const store: GraphStore = {
        state: graphState,
        refresh: () => Promise.resolve(),
        hydrate,
      };
      return (
        <GraphProvider store={store}>
          <GraphFilterProvider store={filters}>
            <Atlas hub={createHub([])} />
          </GraphFilterProvider>
        </GraphProvider>
      );
    });
    const canvas = container.querySelector(".atlas-canvas canvas")!;
    const inspectorTitle = () => container.querySelector(".i-title")?.textContent;

    fireEvent.click(canvas);
    await waitFor(() => expect(hydrate).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(inspectorTitle()).toBeUndefined());

    filters.setSizeRange(null);
    await waitFor(() => expect(inspectorTitle()).toContain(light.schema_id));
    raycast.mockRestore();
  });

  it("displays the deterministic-projection overlay copy and never the embedding copy", () => {
    const store: GraphStore = {
      state: () => snapshot([], []),
      refresh: () => Promise.resolve(),
    };
    const { container } = render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <Atlas hub={createHub([])} />
        </GraphFilterProvider>
      </GraphProvider>
    ));
    expect(container.textContent).toMatch(/x,y = deterministic projection/);
    expect(container.textContent).not.toMatch(
      new RegExp("shared " + "embedding projection"),
    );
  });

  it("rewires the layer pill to the global filter store", () => {
    const fact = memory("019dfa40-0000-7000-8000-000000000010", "Fact");
    const abs = memory("019dfa40-0000-7000-8000-000000000011", "Abstraction");
    const filters = createGraphFilterStore();
    const store: GraphStore = {
      state: () => snapshot([fact, abs], []),
      refresh: () => Promise.resolve(),
    };
    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={filters}>
          <Atlas hub={createHub([])} />
        </GraphFilterProvider>
      </GraphProvider>
    ));
    fireEvent.click(screen.getByRole("button", { name: /Facts/ }));
    expect(filters.state().layers.has("Fact")).toBe(false);
  });

  it("hides only the clicked flavor when toggling from the all-visible default", () => {
    const fact = memory("019dfa40-0000-7000-8000-000000000020", "Fact");
    const filters = createGraphFilterStore();
    const store: GraphStore = {
      state: () => snapshot([fact], []),
      refresh: () => Promise.resolve(),
    };
    const hub = createHub([]);
    hub.registerFlavor("alpha", () => {});
    hub.registerFlavor("beta", () => {});

    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={filters}>
          <Atlas hub={hub} />
        </GraphFilterProvider>
      </GraphProvider>
    ));

    expect(filters.state().hiddenFlavorIds.size).toBe(0);
    const alphaPill = screen.getByRole("button", { name: /ƒ:alpha/ });
    const betaPill = screen.getByRole("button", { name: /ƒ:beta/ });
    expect(alphaPill.className).toMatch(/\bon\b/);
    expect(betaPill.className).toMatch(/\bon\b/);

    fireEvent.click(alphaPill);

    expect(filters.state().hiddenFlavorIds.has("alpha")).toBe(true);
    expect(filters.state().hiddenFlavorIds.has("beta")).toBe(false);
    expect(alphaPill.className).toMatch(/\boff\b/);
    expect(betaPill.className).toMatch(/\bon\b/);
  });

  it("disables the sole registered flavor when toggling from the all-visible default", () => {
    const fact = memory("019dfa40-0000-7000-8000-000000000030", "Fact");
    const filters = createGraphFilterStore();
    const store: GraphStore = {
      state: () => snapshot([fact], []),
      refresh: () => Promise.resolve(),
    };
    const hub = createHub([]);
    hub.registerFlavor("code", () => {});

    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={filters}>
          <Atlas hub={hub} />
        </GraphFilterProvider>
      </GraphProvider>
    ));

    const codePill = screen.getByRole("button", { name: /ƒ:code/ });
    expect(codePill.className).toMatch(/\bon\b/);
    fireEvent.click(codePill);
    expect(filters.state().hiddenFlavorIds.has("code")).toBe(true);
    expect(codePill.className).toMatch(/\boff\b/);
    fireEvent.click(codePill);
    expect(filters.state().hiddenFlavorIds.has("code")).toBe(false);
    expect(codePill.className).toMatch(/\bon\b/);
  });

  it("surfaces the snapshot-truncated status pill at the node window", () => {
    const memories = Array.from({ length: 5_000 }, (_, i) =>
      memory(`019dfa40-0000-7000-8000-${i.toString(16).padStart(12, "0")}`, "Fact"),
    );
    const store: GraphStore = {
      state: () => snapshot(memories, []),
      refresh: () => Promise.resolve(),
    };
    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <Atlas hub={createHub([])} />
        </GraphFilterProvider>
      </GraphProvider>
    ));
    expect(screen.getByText(/snapshot truncated at 5000 nodes/)).toBeTruthy();
  });

  it("does not count edges toward node-window truncation", () => {
    const fact = memory("019dfa40-0000-7000-8000-000000000001", "Fact");
    const abs = memory("019dfa40-0000-7000-8000-000000000002", "Abstraction");
    const store: GraphStore = {
      state: () =>
        snapshot(
          [fact, abs],
          [edge("019dfa41-0000-7000-8000-000000000001", abs.id, fact.id)],
          5_000,
        ),
      refresh: () => Promise.resolve(),
    };
    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <Atlas hub={createHub([])} />
        </GraphFilterProvider>
      </GraphProvider>
    ));
    expect(screen.queryByText(/snapshot truncated at 5000 nodes/)).toBeNull();
  });

  it("surfaces a separate edges-truncated pill at MAX_SNAPSHOT_EDGES", () => {
    const fact = memory("019dfa40-0000-7000-8000-000000000001", "Fact");
    const abs = memory("019dfa40-0000-7000-8000-000000000002", "Abstraction");
    const store: GraphStore = {
      state: () =>
        snapshot(
          [fact, abs],
          [edge("019dfa41-0000-7000-8000-000000000001", abs.id, fact.id)],
          MAX_SNAPSHOT_EDGES,
        ),
      refresh: () => Promise.resolve(),
    };
    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <Atlas hub={createHub([])} />
        </GraphFilterProvider>
      </GraphProvider>
    ));
    expect(screen.getByText(new RegExp(`edges truncated at ${MAX_SNAPSHOT_EDGES}`))).toBeTruthy();
  });

  it("tracks node selection history through edge clicks and keyboard navigation", () => {
    const fact = atlasNode("019dfa50-0000-7000-8000-000000000001", "Fact", "engine.rs", 0);
    const abs = atlasNode("019dfa50-0000-7000-8000-000000000002", "Abstraction", "call graph", 1);
    const perspective = atlasNode("019dfa50-0000-7000-8000-000000000003", "Perspective", "review", 2);
    const raycast = vi
      .spyOn(THREE.Raycaster.prototype, "intersectObjects")
      .mockImplementation((objects) => [{ object: objects[0] }] as THREE.Intersection[]);

    const { container } = render(() => (
      <GraphFilterProvider store={createGraphFilterStore()}>
        <Atlas
          hub={createHub([])}
          nodes={[fact, abs, perspective]}
          edges={[
            atlasEdge("019dfa50-0000-7000-8000-000000000011", fact.id, abs.id),
            atlasEdge("019dfa50-0000-7000-8000-000000000012", abs.id, perspective.id),
          ]}
        />
      </GraphFilterProvider>
    ));
    const inspectorTitle = () => container.querySelector(".i-title")?.textContent;

    fireEvent.click(document.querySelector(".atlas-canvas canvas")!);
    expect(inspectorTitle()).toBe("engine.rs");
    fireEvent.click(screen.getByText("call graph"));
    fireEvent.click(screen.getByText("review"));

    const back = screen.getByRole("button", { name: "Back" });
    const forward = screen.getByRole("button", { name: "Forward" });
    expect(back.hasAttribute("disabled")).toBe(false);
    expect(forward.hasAttribute("disabled")).toBe(true);

    fireEvent.click(back);
    expect(inspectorTitle()).toBe("call graph");
    expect(forward.hasAttribute("disabled")).toBe(false);

    fireEvent.keyDown(window, { key: "ArrowLeft", altKey: true });
    expect(inspectorTitle()).toBe("engine.rs");
    fireEvent.keyDown(window, { key: "ArrowRight", altKey: true });
    expect(inspectorTitle()).toBe("call graph");

    raycast.mockRestore();
  });

  it("shows motivated-by evidence chips for selected goal nodes", () => {
    const fact: AtlasNode = {
      ...atlasNode(
        "019dfa60-0000-7000-8000-000000000311",
        "Fact",
        "Read goal contract",
        0,
      ),
      schemaId: "proxima-code/commit-summary-v1",
      schemaVersion: 1,
      memory: {
        id: "019dfa60-0000-7000-8000-000000000311",
        kind: "Fact",
        schema_id: "proxima-code/commit-summary-v1",
        schema_version: 1,
        owner,
        payload: [],
      },
    };
    const abs: AtlasNode = {
      ...atlasNode(
        "019dfa60-0000-7000-8000-000000000312",
        "Abstraction",
        "Review summary",
        1,
      ),
      schemaId: "proxima-code/commit-summary-v1",
      schemaVersion: 1,
      memory: {
        id: "019dfa60-0000-7000-8000-000000000312",
        kind: "Abstraction",
        schema_id: "proxima-code/commit-summary-v1",
        schema_version: 1,
        owner,
        payload: [],
      },
    };
    const goalRow = activeGoal("019dfa60-0000-7000-8000-000000000301");
    const goalNode: AtlasNode = {
      ...atlasNode("019dfa60-0000-7000-8000-000000000300", "Goal", "Planner handoff", 2),
      kind: "Goal",
      schemaId: goalRow.schema_id,
      schemaVersion: goalRow.schema_version,
      title: goalRow.title,
      flavor: "proxima-goal",
      goal: goalRow,
      y: 0,
    };
    const raycast = vi
      .spyOn(THREE.Raycaster.prototype, "intersectObjects")
      .mockImplementation((objects) => [{ object: objects[0] }] as THREE.Intersection[]);

    render(() => (
      <GraphFilterProvider store={createGraphFilterStore()}>
        <Atlas
          hub={createHub([])}
          nodes={[goalNode, fact, abs]}
          edges={[
            atlasEdge(
              "019dfa60-0000-7000-8000-000000000401",
              goalNode.id,
              fact.id,
              "proxima-goal/motivated-by",
            ),
            atlasEdge(
              "019dfa60-0000-7000-8000-000000000402",
              goalNode.id,
              abs.id,
              "proxima-goal/motivated-by",
            ),
          ]}
        />
      </GraphFilterProvider>
    ));
    const canvas = document.querySelector(".atlas-canvas canvas")!;

    fireEvent.click(canvas);
    expect(screen.getByText("Evidence")).toBeTruthy();
    expect(screen.getByText("Read goal contract")).toBeTruthy();
    expect(screen.getByText("Review summary")).toBeTruthy();
    expect(screen.getByText("Fact · ƒ:code")).toBeTruthy();
    expect(screen.getByText("Abstraction · ƒ:code")).toBeTruthy();

    raycast.mockRestore();
  });

  it("does not show evidence chips for non-goal selected nodes", () => {
    const fact = atlasNode(
      "019dfa60-0000-7000-8000-000000000411",
      "Fact",
      "Read goal contract",
      0,
    );
    const abs = atlasNode(
      "019dfa60-0000-7000-8000-000000000412",
      "Abstraction",
      "Review summary",
      1,
    );
    const raycast = vi
      .spyOn(THREE.Raycaster.prototype, "intersectObjects")
      .mockImplementation((objects) => [{ object: objects[1] }] as THREE.Intersection[]);

    render(() => (
      <GraphFilterProvider store={createGraphFilterStore()}>
        <Atlas
          hub={createHub([])}
          nodes={[fact, abs]}
          edges={[
            atlasEdge(
              "019dfa60-0000-7000-8000-000000000501",
              fact.id,
              abs.id,
              "proxima-goal/motivated-by",
            ),
          ]}
        />
      </GraphFilterProvider>
    ));
    const canvas = document.querySelector(".atlas-canvas canvas")!;

    fireEvent.click(canvas);
    expect(screen.queryByText("Evidence")).toBeNull();

    raycast.mockRestore();
  });

  it("copies the selected inspector tab content", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    const previousClipboard = Object.getOwnPropertyDescriptor(
      navigator,
      "clipboard",
    );
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    try {
      const fact = {
        ...atlasNode("019dfa50-0000-7000-8000-000000000001", "Fact", "engine.rs", 0),
        memory: memory("019dfa50-0000-7000-8000-000000000001", "Fact"),
        payload: { body: "payload-content" },
      };
      const abs = atlasNode(
        "019dfa50-0000-7000-8000-000000000002",
        "Abstraction",
        "call graph",
        1,
      );
      const edge = atlasEdge("019dfa50-0000-7000-8000-000000000011", fact.id, abs.id);
      const raycast = vi
        .spyOn(THREE.Raycaster.prototype, "intersectObjects")
        .mockImplementation((objects) => [{ object: objects[0] }] as THREE.Intersection[]);

      render(() => (
        <GraphFilterProvider store={createGraphFilterStore()}>
          <Atlas hub={createHub([])} nodes={[fact, abs]} edges={[edge]} />
        </GraphFilterProvider>
      ));

      fireEvent.click(document.querySelector(".atlas-canvas canvas")!);

      fireEvent.click(screen.getByRole("button", { name: "Copy Payload" }));
      await waitFor(() => expect(writeText).toHaveBeenCalledTimes(1));
      expect(writeText.mock.calls[0][0]).toContain("payload-content");

      fireEvent.click(screen.getByRole("button", { name: "Edges" }));
      fireEvent.click(screen.getByRole("button", { name: "Copy Edges" }));
      await waitFor(() => expect(writeText).toHaveBeenCalledTimes(2));
      expect(writeText.mock.calls[1][0]).toContain(edge.id);

      fireEvent.click(screen.getByRole("button", { name: "Meta" }));
      fireEvent.click(screen.getByRole("button", { name: "Copy Meta" }));
      await waitFor(() => expect(writeText).toHaveBeenCalledTimes(3));
      expect(writeText.mock.calls[2][0]).toContain("payloadBytes");

      fireEvent.click(screen.getByRole("button", { name: "Raw" }));
      fireEvent.click(screen.getByRole("button", { name: "Copy Raw" }));
      await waitFor(() => expect(writeText).toHaveBeenCalledTimes(4));
      expect(writeText.mock.calls[3][0]).toContain(fact.id);

      raycast.mockRestore();
    } finally {
      if (previousClipboard) {
        Object.defineProperty(navigator, "clipboard", previousClipboard);
      } else {
        delete (navigator as { clipboard?: unknown }).clipboard;
      }
    }
  });

  it("pins selected node focus against hover changes until deselected", () => {
    const fact = atlasNode("019dfa50-0000-7000-8000-000000000001", "Fact", "engine.rs", 0);
    const abs = atlasNode("019dfa50-0000-7000-8000-000000000002", "Abstraction", "call graph", 1);
    let hitIndex: number | null = 1;
    const raycast = vi.spyOn(THREE.Raycaster.prototype, "intersectObjects").mockImplementation(
      (objects) =>
        hitIndex === null
          ? []
          : ([{ object: objects[hitIndex] }] as THREE.Intersection[]),
    );

    const { container } = render(() => (
      <GraphFilterProvider store={createGraphFilterStore()}>
        <Atlas
          hub={createHub([])}
          nodes={[fact, abs]}
          edges={[atlasEdge("019dfa50-0000-7000-8000-000000000011", fact.id, abs.id)]}
        />
      </GraphFilterProvider>
    ));
    const canvas = document.querySelector(".atlas-canvas canvas")!;
    const inspectorTitle = () => container.querySelector(".i-title")?.textContent;

    fireEvent.pointerMove(canvas);
    expect(inspectorTitle()).toBe("call graph");

    hitIndex = 0;
    fireEvent.click(canvas);
    expect(inspectorTitle()).toBe("engine.rs");

    hitIndex = 1;
    fireEvent.pointerMove(canvas);
    expect(inspectorTitle()).toBe("engine.rs");

    hitIndex = null;
    fireEvent.click(canvas);
    expect(inspectorTitle()).toBeUndefined();

    hitIndex = 1;
    fireEvent.pointerMove(canvas);
    expect(inspectorTitle()).toBe("call graph");

    raycast.mockRestore();
  });

  it("reactivates active goals from the inspector", async () => {
    const planner: PersonalityInstanceTs = {
      owner,
      personality_instance_id: "019dfa50-0000-7000-8000-000000000201",
      current_root_perspective_memory_id:
        "019dfa50-0000-7000-8000-000000000202",
      display_name: "Planner",
      status: "active",
      wake_entries: [
        {
          wake_entry_id: "019dfa50-0000-7000-8000-000000000203",
          trigger_kind: "on_memory",
          trigger_id: "proxima-goal/goal-activated-v1",
          label: "plan execution requests",
          enabled: true,
          execution_mode: "substrate_only",
          authored_by: "other",
          probability_promille: 1000,
          goal_scope: "trigger_goal_assigned",
          instructions: "",
          model_tier: "deep",
          inference_target_ref: null,
          substrate_tool_palette: [],
          required_produced_schema_ids: [],
          max_rounds: 16,
          disabled_reason: null,
        },
      ],
    };
    listPersonalityInstancesMock.mockResolvedValue({
      status: "ok",
      data: [planner],
    });
    goalReactivateMock.mockResolvedValue({
      status: "ok",
      data: {
        event_id: "feed000000000000000000000000000000000000000000000000000000000000",
        memory_id: "019dfa50-0000-7000-8000-000000000101",
        change_event_seq: "019dfa50-0000-7000-8000-000000000102",
        idempotent_replay: false,
      },
    });
    const row = activeGoal("019dfa50-0000-7000-8000-000000000100");
    const node: AtlasNode = {
      id: row.id,
      kind: "Goal",
      schemaId: row.schema_id,
      schemaVersion: row.schema_version,
      title: row.title,
      flavor: "goal",
      x: 0,
      y: 0,
      goal: row,
    };
    const raycast = vi
      .spyOn(THREE.Raycaster.prototype, "intersectObjects")
      .mockImplementation(
        (objects) => [{ object: objects[0] }] as THREE.Intersection[],
      );

    render(() => (
      <GraphFilterProvider store={createGraphFilterStore()}>
        <Atlas hub={createHub([])} nodes={[node]} edges={[]} />
      </GraphFilterProvider>
    ));
    const canvas = document.querySelector(".atlas-canvas canvas")!;
    fireEvent.pointerMove(canvas);

    fireEvent.click(screen.getByRole("button", { name: "Reactivate goal" }));
    await screen.findByRole("dialog", { name: "Assign goal" });
    await waitFor(() => expect(screen.getByText("Planner")).toBeTruthy());
    fireEvent.click(screen.getByRole("radio"));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    await waitFor(() => expect(goalReactivateMock).toHaveBeenCalledTimes(1));
    expect(goalReactivateMock).toHaveBeenCalledWith({
      principal: owner.principal,
      goal_id: row.id,
      target_personality_id: planner.personality_instance_id,
    });
    expect(
      screen.getByRole("button", { name: "Reactivate goal" }).textContent,
    ).toContain("Reactivated");
    raycast.mockRestore();
  });

  it("opens the goal dialog from Atlas chrome", async () => {
    registerSimpleTextSchema();
    listPersonalityInstancesMock.mockResolvedValue({
      status: "ok",
      data: [planner("019dfa50-0000-7000-8000-000000000311", "Planner")],
    });
    const store: GraphStore = {
      state: () => snapshot([], []),
      refresh: () => Promise.resolve(),
    };
    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <Atlas hub={createHub([])} />
        </GraphFilterProvider>
      </GraphProvider>
    ));

    fireEvent.click(screen.getByRole("button", { name: "New goal" }));

    expect(await screen.findByRole("dialog", { name: "Goal editor" })).toBeTruthy();
    expect(screen.getByLabelText("Type")).toBeTruthy();
    expect(await screen.findByText("Planner")).toBeTruthy();
  });

  it("renders registered goal editors in the Atlas goal type selector", async () => {
    registerSimpleTextSchema();
    registerPayloadRenderer({
      schemaId: "proxima-code/task-goal-v1",
      schemaVersion: 1,
      kind: "Goal",
      flavor: "proxima-code",
      codec: {
        decode: () => ({ repo: "" }),
        encode: () => new Uint8Array([0xa1, 0x64, 0x72, 0x65, 0x70, 0x6f, 0x60]),
      },
      renderer: {
        render: () => null,
      },
    });
    registerGoalPayloadEditor<{ repo: string }>({
      schemaId: "proxima-code/task-goal-v1",
      schemaVersion: 1,
      flavor: "proxima-code",
      label: "Task",
      defaults: () => ({ repo: "" }),
      component: () => null,
    });
    listPersonalityInstancesMock.mockResolvedValue({
      status: "ok",
      data: [planner("019dfa50-0000-7000-8000-000000000321", "Planner")],
    });
    const store: GraphStore = {
      state: () => snapshot([], []),
      refresh: () => Promise.resolve(),
    };
    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <Atlas hub={createHub([])} />
        </GraphFilterProvider>
      </GraphProvider>
    ));

    fireEvent.click(screen.getByRole("button", { name: "New goal" }));

    const type = (await screen.findByLabelText("Type")) as HTMLSelectElement;
    const options = Array.from(type.options).map((option) => option.textContent);
    expect(options).toEqual(["Simple text", "Task"]);
  });

  it("refreshes the graph and closes after creating an assigned Atlas goal", async () => {
    registerSimpleTextSchema();
    const target = planner(
      "019dfa50-0000-7000-8000-000000000331",
      "Planner",
    );
    listPersonalityInstancesMock.mockResolvedValue({
      status: "ok",
      data: [target],
    });
    goalWriteMock.mockImplementation(okWrite);
    goalReactivateMock.mockImplementation(okReactivate);
    const refresh = vi.fn().mockResolvedValue(undefined);
    const store: GraphStore = {
      state: () => snapshot([], []),
      refresh,
    };
    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <Atlas hub={createHub([])} />
        </GraphFilterProvider>
      </GraphProvider>
    ));

    fireEvent.click(screen.getByRole("button", { name: "New goal" }));
    await screen.findByText("Planner");
    fireEvent.input(screen.getByLabelText("Title"), {
      target: { value: "Atlas-created goal" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() => expect(goalWriteMock).toHaveBeenCalledOnce());
    await waitFor(() => expect(goalReactivateMock).toHaveBeenCalledOnce());
    expect(goalReactivateMock).toHaveBeenCalledWith({
      principal: owner.principal,
      goal_id: "019dfa50-0000-7000-8000-000000000301",
      target_personality_id: target.personality_instance_id,
    });
    await waitFor(() => expect(refresh).toHaveBeenCalledOnce());
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "Goal editor" })).toBeNull(),
    );
  });

  it("resizes the inspector column by dragging its handle", () => {
    const { container } = render(() => (
      <GraphFilterProvider store={createGraphFilterStore()}>
        <Atlas hub={createHub([])} nodes={[]} edges={[]} />
      </GraphFilterProvider>
    ));
    const body = container.querySelector(".atlas-body") as HTMLElement;
    Object.defineProperty(body, "clientWidth", { configurable: true, value: 1400 });

    fireEvent.pointerDown(screen.getByRole("button", { name: "Resize Atlas inspector" }), {
      button: 0,
      clientX: 1000,
    });
    fireEvent.pointerMove(window, { clientX: 900 });

    expect(body.getAttribute("style")).toContain("--atlas-inspector-width: 440px");

    fireEvent.pointerUp(window);
  });
});
