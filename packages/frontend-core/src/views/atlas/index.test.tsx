import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import * as THREE from "three";
import type { EdgeRow, MemoryRow, Owner } from "../../bindings";
import { GraphFilterProvider, createGraphFilterStore } from "../../graph-filter-store";
import {
  GraphProvider,
  MAX_SNAPSHOT_EDGES,
  type GraphSnapshot,
  type GraphStore,
} from "../../graph-store";
import { createHub } from "../../hub";
import { Atlas } from "./index";
import type { AtlasEdge, AtlasNode } from "./types";

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
  owner,
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

const atlasEdge = (id: string, src: string, tgt: string): AtlasEdge => ({
  id,
  src,
  tgt,
  kind: "code/calls",
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
    streamStatus: "live",
    seqHighWater: null,
  };
};

describe("Atlas graph wiring", () => {
  afterEach(() => cleanup());

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
