import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ReposPanel } from "./repos-panel";
import type {
  CommandError,
  RepoEraseReceiptTs,
  RepoRecordTs,
} from "../../bindings";

const mocks = vi.hoisted(() => ({
  reposList: vi.fn(),
  reposErase: vi.fn(),
  reposRegister: vi.fn(),
  repoIngest: vi.fn(),
  openDialog: vi.fn(),
}));

vi.mock("../../bindings", () => ({
  commands: {
    reposList: mocks.reposList,
    reposErase: mocks.reposErase,
    reposRegister: mocks.reposRegister,
    repoIngest: mocks.repoIngest,
  },
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: mocks.openDialog,
}));

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class Channel<T> {
    onmessage: ((message: T) => void) | null = null;
  },
}));

vi.mock("../../primitives", () => ({
  LoadingSurface: (props: { label?: string }) => (
    <div data-testid="loading">{props.label ?? "Loading"}</div>
  ),
}));

const ok = <T,>(data: T) => Promise.resolve({ status: "ok" as const, data });
const error = (errorValue: CommandError) =>
  Promise.resolve({ status: "error" as const, error: errorValue });

const repo = (overrides: Partial<RepoRecordTs> = {}): RepoRecordTs => ({
  repo_id: "018f0000-0000-7000-8000-000000000001",
  canonical_path: "/repos/proxima",
  display_name: "Proxima",
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

describe("ReposPanel", () => {
  beforeEach(() => {
    mocks.reposList.mockResolvedValue(ok([repo()]));
    mocks.reposErase.mockResolvedValue(ok(receipt()));
    mocks.reposRegister.mockResolvedValue(ok(repo()));
    mocks.repoIngest.mockResolvedValue(ok(null));
    mocks.openDialog.mockResolvedValue(null);
  });

  afterEach(() => {
    cleanup();
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
});
