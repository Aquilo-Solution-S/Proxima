import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { encode } from "cbor-x";
import { afterEach, describe, expect, it } from "vitest";
import type { EntityKind, MemoryRow } from "../bindings";
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
import { registerCode } from "../flavors/code";
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

const snapshot = (memories: MemoryRow[]): GraphSnapshot => ({
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
  eventsBySeq: new Map(),
  pendingHydration: new Map(),
  decodeErrorsByEntity: new Map(),
  streamStatus: "live",
  seqHighWater: null,
});

const createHubWithCode = () => {
  const hub = createHub([]);
  hub.registerFlavor("code", registerCode);
  return hub;
};

describe("FullSurface fact explorer", () => {
  afterEach(() => cleanup());

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
    expect(screen.getByText("fn selected_chunk() {}")).toBeTruthy();
    expect(screen.queryByText("src/lib.rs")).toBeNull();
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
});
