import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ingestStore } from "./ingest-store";
import { ReposPanel } from "./repos-panel";
import type {
  CommandError,
  RepoEraseReceiptTs,
  RepoRecordTs,
} from "@proxima/core";

const mocks = vi.hoisted(() => ({
  reposList: vi.fn(),
  reposErase: vi.fn(),
  reposRegister: vi.fn(),
  repoIngestStart: vi.fn(),
  repoIngestStatus: vi.fn(),
  repoIngestSubscribe: vi.fn(),
  openDialog: vi.fn(),
}));

vi.mock("@proxima/core", () => ({
  commands: {
    reposList: mocks.reposList,
    reposErase: mocks.reposErase,
    reposRegister: mocks.reposRegister,
    repoIngestStart: mocks.repoIngestStart,
    repoIngestStatus: mocks.repoIngestStatus,
    repoIngestSubscribe: mocks.repoIngestSubscribe,
  },
  formatCommandError: (error: CommandError) => {
    if (error.kind === "unknown_repo") return `Unknown repo: ${error.data.repo_id}`;
    return String(error.kind);
  },
  formatPolledAt: () => "never",
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: mocks.openDialog,
}));

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class Channel<T> {
    onmessage: ((message: T) => void) | null = null;
  },
}));

vi.mock("@proxima/core/primitives", () => ({
  LoadingSurface: (props: { label?: string }) => (
    <div data-testid="loading">{props.label ?? "Loading"}</div>
  ),
  ProximaLoader: () => <div data-testid="proxima-loader" />,
}));

const ok = <T,>(data: T) => Promise.resolve({ status: "ok" as const, data });
const error = (errorValue: CommandError) =>
  Promise.resolve({ status: "error" as const, error: errorValue });

const repo = (overrides: Partial<RepoRecordTs> = {}): RepoRecordTs => ({
  repo_id: "018f0000-0000-7000-8000-000000000001",
  canonical_path: "/repos/proxima",
  display_name: "Proxima",
  target_branch: "main",
  has_been_polled: false,
  last_polled_at: null,
  created_at: "2026-05-05T12:00:00Z",
  ...overrides,
});

const receipt = (
  overrides: Partial<RepoEraseReceiptTs> = {},
): RepoEraseReceiptTs => ({
  repo_id: "018f0000-0000-7000-8000-000000000001",
  completed_at: "2026-05-05T12:01:00Z",
  facts_deleted: 2,
  abstractions_deleted: 1,
  edges_deleted: 3,
  embeddings_deleted: 1,
  events_deleted: 2,
  citation_mappings_deleted: 2,
  cited_objects_deleted: 1,
  source_batches_deleted: 1,
  f2a_rows_deleted: 1,
  repo_record_deleted: true,
  ...overrides,
});

const run = () => ({
  run_id: "018f0000-0000-7000-8000-000000000101",
  repo_id: "018f0000-0000-7000-8000-000000000001",
  status: "running" as const,
  stage: "facts" as const,
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
  started_at: "2026-05-05T12:00:00Z",
  updated_at: "2026-05-05T12:00:01Z",
  finished_at: null,
});

describe("ReposPanel", () => {
  beforeEach(() => {
    ingestStore.resetForTests();
    mocks.reposList.mockResolvedValue(ok([repo()]));
    mocks.reposErase.mockResolvedValue(ok(receipt()));
    mocks.reposRegister.mockResolvedValue(ok(repo()));
    mocks.repoIngestStart.mockResolvedValue(ok(run()));
    mocks.repoIngestStatus.mockResolvedValue(ok(null));
    mocks.repoIngestSubscribe.mockResolvedValue(ok(null));
    mocks.openDialog.mockResolvedValue(null);
  });

  afterEach(() => {
    cleanup();
    ingestStore.resetForTests();
    vi.clearAllMocks();
  });

  it("renders repos returned by the backend", async () => {
    render(() => <ReposPanel />);

    expect(await screen.findByText("Proxima")).toBeTruthy();
    expect(screen.getByText("/repos/proxima")).toBeTruthy();
  });

  it("opens inline delete confirmation and requires the exact repo name", async () => {
    render(() => <ReposPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "Delete" }));

    const confirm = screen.getByRole("button", { name: "Confirm" });
    expect((confirm as HTMLButtonElement).disabled).toBe(true);

    fireEvent.input(screen.getByPlaceholderText("Proxima"), {
      target: { value: "proxima" },
    });
    expect((confirm as HTMLButtonElement).disabled).toBe(true);

    fireEvent.input(screen.getByPlaceholderText("Proxima"), {
      target: { value: "Proxima" },
    });
    expect((confirm as HTMLButtonElement).disabled).toBe(false);
  });

  it("erases a repo and removes it from the local list immediately", async () => {
    render(() => <ReposPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "Delete" }));
    fireEvent.input(screen.getByPlaceholderText("Proxima"), {
      target: { value: "Proxima" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    await waitFor(() => {
      expect(mocks.reposErase).toHaveBeenCalledWith(
        "018f0000-0000-7000-8000-000000000001",
      );
      expect(screen.queryByText("Proxima")).toBeNull();
    });
    expect(
      screen.getByText(
        /deleted 2 facts, 1 abstractions, 3 edges, 1 embeddings/,
      ),
    ).toBeTruthy();
  });

  it("keeps the repo visible and shows an error when erase fails", async () => {
    mocks.reposErase.mockResolvedValue(
      error({
        kind: "unknown_repo",
        data: { repo_id: "018f0000-0000-7000-8000-000000000001" },
      }),
    );
    render(() => <ReposPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "Delete" }));
    fireEvent.input(screen.getByPlaceholderText("Proxima"), {
      target: { value: "Proxima" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(await screen.findByText("Proxima")).toBeTruthy();
    expect(
      await screen.findByText(
        "Unknown repo: 018f0000-0000-7000-8000-000000000001",
      ),
    ).toBeTruthy();
  });

  it("keeps ingest state across unmount and remount", async () => {
    const { unmount } = render(() => <ReposPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "Ingest All" }));

    await waitFor(() => {
      expect(mocks.repoIngestStart).toHaveBeenCalledWith(
        "018f0000-0000-7000-8000-000000000001",
        null,
      );
      expect(screen.getByText("Running facts")).toBeTruthy();
      expect(
        (
          screen.getByRole("button", {
            name: "Ingest All",
          }) as HTMLButtonElement
        ).disabled,
      ).toBe(true);
    });

    unmount();
    render(() => <ReposPanel />);

    expect(await screen.findByText("Running facts")).toBeTruthy();
    expect(
      (
        screen.getByRole("button", {
          name: "Ingest Next",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
    expect(mocks.repoIngestSubscribe).toHaveBeenCalledTimes(1);
  });

  it("starts one-commit ingest from Ingest Next", async () => {
    render(() => <ReposPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "Ingest Next" }));

    await waitFor(() => {
      expect(mocks.repoIngestStart).toHaveBeenCalledWith(
        "018f0000-0000-7000-8000-000000000001",
        1,
      );
    });
  });
});
