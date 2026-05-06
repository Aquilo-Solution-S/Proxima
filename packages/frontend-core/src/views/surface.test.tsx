import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@solidjs/testing-library";
import { decode, encode } from "cbor-x";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ChangeEvent, EntityKind, MemoryRow } from "../bindings";
import type { EngineClient } from "../client";
import {
  GraphFilterProvider,
  createGraphFilterStore,
} from "../graph-filter-store";
import {
  GraphProvider,
  createGraphStore,
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

const owner = sentinelOwner();

const row = (
  id: string,
  schemaId: string,
  payload: Record<string, unknown>,
  kind: EntityKind = "Fact",
): MemoryRow => ({
  id,
  kind,
  schema_id: schemaId,
  schema_version: 1,
  owner,
  payload: Array.from(encode(payload)),
});

const snapshot = (
  memories: MemoryRow[],
  events: ChangeEvent[] = [],
): GraphSnapshot => ({
  owner,
  schemas: [],
  memoriesById: new Map(
    memories.map((memory) => [
      memory.id,
      {
        row: memory,
        payload: createHubWithCode()
          .codecFor(memory.schema_id, memory.schema_version)!
          .decode(new Uint8Array(memory.payload)),
      },
    ]),
  ),
  goalsById: new Map(),
  edgesById: new Map(),
  eventsBySeq: new Map(events.map((event) => [event.seq, event])),
  pendingHydration: new Map(),
  decodeErrorsByEntity: new Map(),
  streamStatus: "live",
  seqHighWater: null,
});

const createHubWithCode = () => {
  registerPayloadRenderer({
    schemaId: "proxima-code/file-revision-v1",
    schemaVersion: 1,
    flavor: "proxima-code",
    codec: {
      decode: (bytes) => decode(bytes),
      encode: (value) => encode(value),
    },
    renderer: {
      render: (props) => {
        const payload = props.payload as Record<string, unknown>;
        return <div>{String(payload.file_path ?? "")}</div>;
      },
    },
  });
  registerPayloadRenderer({
    schemaId: "proxima-code/code-chunk-v1",
    schemaVersion: 1,
    flavor: "proxima-code",
    codec: {
      decode: (bytes) => decode(bytes),
      encode: (value) => encode(value),
    },
    renderer: {
      render: (props) => {
        const payload = props.payload as Record<string, unknown>;
        return (
          <div>
            <span>
              {String(payload.file_path ?? "")}:{String(payload.line_range_start)}
              -{String(payload.line_range_end)}
            </span>
            <pre class="code-payload-snippet">
              <code>{String(payload.text ?? "")}</code>
            </pre>
          </div>
        );
      },
    },
  });
  registerPayloadRenderer({
    schemaId: "proxima-code/commit-summary-v1",
    schemaVersion: 1,
    flavor: "proxima-code",
    codec: {
      decode: (bytes) => decode(bytes),
      encode: (value) => encode(value),
    },
    renderer: {
      render: (props) => {
        const payload = props.payload as Record<string, unknown>;
        return <div>{String(payload.summary ?? "")}</div>;
      },
    },
  });
  const hub = createHub([]);
  return hub;
};

const event = (seq: string): ChangeEvent => ({
  seq,
  owner,
  kind: {
    EntityAppend: {
      entity_kind: "Fact",
      entity: { Memory: `019dfa35-0000-7000-8000-${seq.slice(-12)}` },
      schema_id: "test/fact_blob",
      schema_version: 1,
      supersedes: null,
    },
  },
});

const clientWithHistory = (
  seedEvents: ChangeEvent[],
  onSubscribe?: (handler: (event: ChangeEvent) => void) => void,
): EngineClient => ({
  schema: async () => ({ schemas: [] }),
  query: async () => ({
    memories: [],
    goals: [],
    edges: [],
    seq_high_water: seedEvents[0]?.seq ?? null,
  }),
  eventHistory: async () => ({
    events: seedEvents,
    seq_high_water: seedEvents[0]?.seq ?? null,
  }),
  subscribe: async (_req, handler) => {
    onSubscribe?.(handler);
    return { unsubscribe: vi.fn() };
  },
  goalWrite: async () => {
    throw new Error("not used");
  },
  eventIngest: async () => {
    throw new Error("not used");
  },
});

const renderSurfaceWithClient = (client: EngineClient) => {
  const hub = createHubWithCode();
  const store = createGraphStore(client, hub, owner);
  render(() => (
    <GraphProvider store={store}>
      <GraphFilterProvider store={createGraphFilterStore()}>
        <FullSurface hub={hub} />
      </GraphFilterProvider>
    </GraphProvider>
  ));
  fireEvent.click(screen.getByRole("button", { name: "Expand Event stream" }));
};

const eventRows = (): NodeListOf<Element> =>
  document.querySelectorAll(".event-row");

describe("FullSurface fact explorer", () => {
  afterEach(() => {
    cleanup();
    clearRegistriesForTests();
  });

  it("updates decoded payload content when selecting another fact", async () => {
    const hub = createHubWithCode();
    const factA = row("019df9e1-cb61-7031-8e93-6facbe711cb2", "proxima-code/file-revision-v1", {
      repo_id: "018f0000-0000-7000-8000-000000000001",
      file_path: "src/lib.rs",
      language: "Rust",
      content_sha256: new Uint8Array(32).fill(1),
      size_bytes: 194,
      indexed_commit_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      state: "Present",
    });
    const factB = row("019df9e1-cb61-7031-8e93-6facbe711cb3", "proxima-code/code-chunk-v1", {
      repo_id: "018f0000-0000-7000-8000-000000000001",
      file_path: "src/chunker.rs",
      chunk_index: 3,
      text: "fn selected_chunk() {}",
      language: "Rust",
      chunk_type: "function",
      byte_range_start: 10,
      byte_range_end: 32,
      line_range_start: 7,
      line_range_end: 9,
      state: "Present",
    });
    const store: GraphStore = {
      state: () => snapshot([factA, factB]),
      refresh: () => Promise.resolve(),
    };

    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <FullSurface hub={hub} />
        </GraphFilterProvider>
      </GraphProvider>
    ));

    expect(screen.getByText("src/lib.rs")).toBeTruthy();

    fireEvent.click(screen.getByTitle("proxima-code/code-chunk-v1"));

    expect(await screen.findByText("src/chunker.rs:7-9")).toBeTruthy();
    expect(
      document.querySelector(".code-payload-snippet code")?.textContent,
    ).toBe("fn selected_chunk() {}");
    expect(screen.queryByText("src/lib.rs")).toBeNull();
  });

  it("selects virtualized fact rows on pointer down before click", async () => {
    const hub = createHubWithCode();
    const factA = row("019df9e1-cb61-7031-8e93-6facbe711cb8", "proxima-code/file-revision-v1", {
      repo_id: "018f0000-0000-7000-8000-000000000001",
      file_path: "src/initial.rs",
      language: "Rust",
      content_sha256: new Uint8Array(32).fill(1),
      size_bytes: 194,
      indexed_commit_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      state: "Present",
    });
    const factB = row("019df9e1-cb61-7031-8e93-6facbe711cb9", "proxima-code/code-chunk-v1", {
      repo_id: "018f0000-0000-7000-8000-000000000001",
      file_path: "src/pointer.rs",
      chunk_index: 3,
      text: "fn pointer_selected() {}",
      language: "Rust",
      chunk_type: "function",
      byte_range_start: 10,
      byte_range_end: 32,
      line_range_start: 11,
      line_range_end: 12,
      state: "Present",
    });
    const store: GraphStore = {
      state: () => snapshot([factA, factB]),
      refresh: () => Promise.resolve(),
    };

    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <FullSurface hub={hub} />
        </GraphFilterProvider>
      </GraphProvider>
    ));

    expect(screen.getByText("src/initial.rs")).toBeTruthy();

    fireEvent.pointerDown(screen.getByTitle("proxima-code/code-chunk-v1"), {
      button: 0,
    });

    expect(await screen.findByText("src/pointer.rs:11-12")).toBeTruthy();
    expect(screen.queryByText("src/initial.rs")).toBeNull();
  });

  it("updates decoded payload content when selecting another abstraction", async () => {
    const hub = createHubWithCode();
    const abstractionA = row(
      "019df9e1-cb61-7031-8e93-6facbe711cb4",
      "proxima-code/commit-summary-v1",
      {
        repo_id: "018f0000-0000-7000-8000-000000000001",
        commit_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        summary: "First abstraction summary",
        change_kind: "feature",
        key_files: ["src/a.rs"],
      },
      "Abstraction",
    );
    const abstractionB = row(
      "019df9e1-cb61-7031-8e93-6facbe711cb5",
      "proxima-code/commit-summary-v1",
      {
        repo_id: "018f0000-0000-7000-8000-000000000001",
        commit_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        summary: "Selected abstraction summary",
        change_kind: "fix",
        key_files: ["src/b.rs"],
      },
      "Abstraction",
    );
    const store: GraphStore = {
      state: () => snapshot([abstractionA, abstractionB]),
      refresh: () => Promise.resolve(),
    };

    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <FullSurface hub={hub} />
        </GraphFilterProvider>
      </GraphProvider>
    ));

    expect(screen.getByText("First abstraction summary")).toBeTruthy();

    fireEvent.click(
      screen.getAllByTitle("proxima-code/commit-summary-v1")[1]!,
    );

    expect(await screen.findByText("Selected abstraction summary")).toBeTruthy();
    expect(screen.queryByText("First abstraction summary")).toBeNull();
  });

  it("applies the global layer filter to Surface", () => {
    const hub = createHubWithCode();
    const fact = row("019dfa30-0000-7000-8000-000000000001", "proxima-code/code-chunk-v1", {
      repo_id: "018f0000-0000-7000-8000-000000000001",
      file_path: "src/visible.rs",
      chunk_index: 0,
      text: "fn visible() {}",
      language: "Rust",
      chunk_type: "function",
      byte_range_start: 0,
      byte_range_end: 16,
      line_range_start: 1,
      line_range_end: 1,
      state: "Present",
    });
    const abstraction = row(
      "019dfa30-0000-7000-8000-000000000002",
      "proxima-code/commit-summary-v1",
      {
        repo_id: "018f0000-0000-7000-8000-000000000001",
        commit_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        summary: "hidden abstraction",
        change_kind: "feature",
        key_files: [],
      },
      "Abstraction",
    );
    const store: GraphStore = {
      state: () => snapshot([fact, abstraction]),
      refresh: () => Promise.resolve(),
    };
    const filters = createGraphFilterStore();
    filters.setLayer("Abstraction", false);
    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={filters}>
          <FullSurface hub={hub} />
        </GraphFilterProvider>
      </GraphProvider>
    ));
    expect(screen.getByText("src/visible.rs:1-1")).toBeTruthy();
    expect(screen.queryByText("hidden abstraction")).toBeNull();
  });

  it("collapses and expands traversal sections independently", () => {
    const hub = createHubWithCode();
    const fact = row(
      "019dfa31-0000-7000-8000-000000000001",
      "proxima-code/code-chunk-v1",
      {
        repo_id: "018f0000-0000-7000-8000-000000000001",
        file_path: "src/collapsible.rs",
        chunk_index: 0,
        text: "fn collapsible() {}",
        language: "Rust",
        chunk_type: "function",
        byte_range_start: 0,
        byte_range_end: 21,
        line_range_start: 1,
        line_range_end: 1,
        state: "Present",
      },
    );
    const store: GraphStore = {
      state: () => snapshot([fact]),
      refresh: () => Promise.resolve(),
    };

    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <FullSurface hub={hub} />
        </GraphFilterProvider>
      </GraphProvider>
    ));

    expect(screen.getByText("src/collapsible.rs:1-1")).toBeTruthy();
    expect(screen.getByText("No perspectives")).toBeTruthy();

    fireEvent.click(
      screen.getByRole("button", { name: "Collapse Facts section" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Collapse Perspective section" }),
    );

    expect(screen.queryByText("src/collapsible.rs:1-1")).toBeNull();
    expect(screen.queryByText("No perspectives")).toBeNull();

    fireEvent.click(
      screen.getByRole("button", { name: "Expand Facts section" }),
    );

    expect(screen.getByText("src/collapsible.rs:1-1")).toBeTruthy();
    expect(screen.queryByText("No perspectives")).toBeNull();
  });

  it("expands event rows with protocol and hydrated entity details", () => {
    const hub = createHubWithCode();
    const fact = row("019dfa32-0000-7000-8000-000000000001", "proxima-code/code-chunk-v1", {
      repo_id: "018f0000-0000-7000-8000-000000000001",
      file_path: "src/event.rs",
      chunk_index: 2,
      text: "fn from_event() {}",
      language: "Rust",
      chunk_type: "function",
      byte_range_start: 30,
      byte_range_end: 48,
      line_range_start: 4,
      line_range_end: 5,
      state: "Present",
    });
    const eventSeq = "019dfa32-0000-7000-8000-000000000002";
    const event: ChangeEvent = {
      seq: eventSeq,
      owner,
      kind: {
        EntityAppend: {
          entity_kind: "Fact",
          entity: { Memory: fact.id },
          schema_id: fact.schema_id,
          schema_version: fact.schema_version,
          supersedes: null,
        },
      },
    };
    const store: GraphStore = {
      state: () => snapshot([fact], [event]),
      refresh: () => Promise.resolve(),
    };

    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <FullSurface hub={hub} />
        </GraphFilterProvider>
      </GraphProvider>
    ));

    expect(screen.queryByText(eventSeq)).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Expand Event stream" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Expand event 019dfa32 Fact" }),
    );

    expect(screen.getByText(eventSeq)).toBeTruthy();
    expect(
      screen.getAllByText("proxima-code/code-chunk-v1 v1").length,
    ).toBeGreaterThan(1);
    expect(screen.getAllByText(fact.id).length).toBeGreaterThan(1);
    expect(screen.getAllByText("src/event.rs:4-5").length).toBeGreaterThan(1);
  });

  it("virtualizes long fact lists in the Surface lanes", () => {
    const hub = createHubWithCode();
    const facts = Array.from({ length: 5000 }, (_, index) => {
      const suffix = index.toString().padStart(12, "0");
      return row(`019dfa37-0000-7000-8000-${suffix}`, "proxima-code/code-chunk-v1", {
        repo_id: "018f0000-0000-7000-8000-000000000001",
        file_path: `src/file-${index.toString().padStart(4, "0")}.rs`,
        chunk_index: index,
        text: `fn chunk_${index}() {}`,
        language: "Rust",
        chunk_type: "function",
        byte_range_start: index,
        byte_range_end: index + 1,
        line_range_start: 1,
        line_range_end: 1,
        state: "Present",
      });
    });
    const memoriesById = new Map(
      facts.map((memory, index) => [
        memory.id,
        {
          row: memory,
          payload: {
            file_path: `src/file-${index.toString().padStart(4, "0")}.rs`,
            line_range_start: 1,
            line_range_end: 1,
            text: `fn chunk_${index}() {}`,
          },
        },
      ]),
    );
    const store: GraphStore = {
      state: () => ({
        ...snapshot([]),
        memoriesById,
      }),
      refresh: () => Promise.resolve(),
    };

    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <FullSurface hub={hub} />
        </GraphFilterProvider>
      </GraphProvider>
    ));

    expect(document.querySelectorAll(".fact-list-item").length).toBeGreaterThan(0);
    expect(document.querySelectorAll(".fact-list-item").length).toBeLessThan(100);
    expect(screen.getByText("src/file-0000.rs:1-1")).toBeTruthy();
  });

  it("virtualizes long Event stream lists", () => {
    const hub = createHub([]);
    const events = Array.from({ length: 5000 }, (_, index) =>
      event(`019dfa38-0000-7000-8000-${index.toString().padStart(12, "0")}`),
    );
    const store: GraphStore = {
      state: () => snapshot([], events),
      refresh: () => Promise.resolve(),
    };

    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <FullSurface hub={hub} />
        </GraphFilterProvider>
      </GraphProvider>
    ));

    fireEvent.click(screen.getByRole("button", { name: "Expand Event stream" }));

    expect(eventRows().length).toBeGreaterThan(0);
    expect(eventRows().length).toBeLessThan(100);
    expect(eventRows()[0]?.textContent).toContain("019dfa38");
  });

  it("resizes the Event stream rail from the left-edge separator", () => {
    const hub = createHub([]);
    const store: GraphStore = {
      state: () => snapshot([]),
      refresh: () => Promise.resolve(),
    };

    render(() => (
      <GraphProvider store={store}>
        <GraphFilterProvider store={createGraphFilterStore()}>
          <FullSurface hub={hub} />
        </GraphFilterProvider>
      </GraphProvider>
    ));

    fireEvent.click(screen.getByRole("button", { name: "Expand Event stream" }));
    const surface = document.querySelector(".surface-body") as HTMLElement;
    const separator = screen.getByRole("separator", { name: "Resize Event stream" });

    fireEvent.pointerDown(separator, { button: 0, clientX: 900 });
    fireEvent.pointerMove(window, { clientX: 820 });
    fireEvent.pointerUp(window);

    expect(surface.getAttribute("style")).toContain("--surface-event-width: 440px");
  });

  it("renders historical events before any live append", async () => {
    const seedEvents = [
      event("019dfa34-0000-7000-8000-000000000002"),
      event("019dfa33-0000-7000-8000-000000000001"),
    ];
    renderSurfaceWithClient(clientWithHistory(seedEvents));

    await waitFor(() => expect(eventRows()).toHaveLength(2));
    const rows = Array.from(eventRows());
    expect(rows[0]?.textContent).toContain("019dfa34");
    expect(rows[1]?.textContent).toContain("019dfa33");
  });

  it("dedupes a live event whose seq is already in the seed", async () => {
    const seed = event("019dfa36-0000-7000-8000-000000000005");
    const live = { emit: undefined as ((event: ChangeEvent) => void) | undefined };
    renderSurfaceWithClient(
      clientWithHistory([seed], (handler) => {
        live.emit = handler;
      }),
    );

    await waitFor(() => expect(eventRows()).toHaveLength(1));
    if (live.emit === undefined) throw new Error("subscription callback not captured");
    live.emit(seed);

    await waitFor(() => expect(eventRows()).toHaveLength(1));
  });
});
