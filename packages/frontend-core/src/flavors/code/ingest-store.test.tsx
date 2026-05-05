import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ingestStore } from "./ingest-store";
import type { RepoIngestionRunTs } from "../../bindings";

const mocks = vi.hoisted(() => ({
  repoIngestStart: vi.fn(),
  repoIngestStatus: vi.fn(),
  repoIngestSubscribe: vi.fn(),
}));

vi.mock("../../bindings", () => ({
  commands: {
    repoIngestStart: mocks.repoIngestStart,
    repoIngestStatus: mocks.repoIngestStatus,
    repoIngestSubscribe: mocks.repoIngestSubscribe,
  },
}));

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class Channel<T> {
    onmessage: ((message: T) => void) | null = null;
  },
}));

const ok = <T,>(data: T) => Promise.resolve({ status: "ok" as const, data });

const run = (overrides: Partial<RepoIngestionRunTs> = {}): RepoIngestionRunTs => ({
  run_id: "018f0000-0000-7000-8000-000000000101",
  repo_id: "repo-A",
  status: "running",
  stage: "facts",
  commits_emitted: 0,
  files_emitted: 0,
  chunks_emitted: 0,
  chunks_reused: 0,
  chunks_tombstoned: 0,
  ast_edges_emitted: 0,
  abstractions_emitted: 0,
  embeddings_landed: 0,
  citations_emitted: 0,
  error_message: null,
  started_at: "2026-05-05T22:03:34Z",
  updated_at: "2026-05-05T22:03:35Z",
  finished_at: null,
  ...overrides,
});

describe("ingestStore.rehydrate", () => {
  beforeEach(() => {
    ingestStore.resetForTests();
    mocks.repoIngestStatus.mockResolvedValue(ok(run()));
    mocks.repoIngestSubscribe.mockResolvedValue(ok(null));
    mocks.repoIngestStart.mockResolvedValue(ok(run()));
  });

  afterEach(() => {
    ingestStore.resetForTests();
    vi.clearAllMocks();
  });

  it("seeds state from repoIngestStatus when a run is active", async () => {
    await ingestStore.rehydrate("repo-A");

    expect(ingestStore.state["repo-A"]?.run?.status).toBe("running");
    expect(ingestStore.isRunning("repo-A")).toBe(true);
    expect(mocks.repoIngestSubscribe).toHaveBeenCalledWith(
      "repo-A",
      expect.any(Object),
    );
  });

  it("does not double-subscribe", async () => {
    await ingestStore.rehydrate("repo-A");
    await ingestStore.rehydrate("repo-A");

    expect(mocks.repoIngestStatus).toHaveBeenCalledTimes(1);
    expect(mocks.repoIngestSubscribe).toHaveBeenCalledTimes(1);
  });
});
