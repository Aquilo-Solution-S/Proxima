import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
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
});
