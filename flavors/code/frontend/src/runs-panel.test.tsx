import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RunsPanel } from "./runs-panel";
import type {
  CommandError,
  RepoRecordTs,
  WorkspaceDecisionRecordTs,
  WorkspaceRunDiffTs,
  WorkspaceReviewRecordTs,
  WorkspaceRunRecordTs,
} from "@proxima/core";

const mocks = vi.hoisted(() => ({
  reposList: vi.fn(),
  codeListWorkspaceRuns: vi.fn(),
  codeGetWorkspaceRunDiff: vi.fn(),
  codeMergeWorkspaceRun: vi.fn(),
  codeDecideWorkspaceRun: vi.fn(),
}));

vi.mock("@proxima/core", () => ({
  commands: {
    reposList: mocks.reposList,
    codeListWorkspaceRuns: mocks.codeListWorkspaceRuns,
    codeGetWorkspaceRunDiff: mocks.codeGetWorkspaceRunDiff,
    codeMergeWorkspaceRun: mocks.codeMergeWorkspaceRun,
    codeDecideWorkspaceRun: mocks.codeDecideWorkspaceRun,
  },
  formatCommandError: (error: CommandError) => {
    if (error.kind === "invalid_uuid") return `Invalid UUID: ${error.data.value}`;
    return String(error.kind);
  },
}));

vi.mock("@proxima/core/primitives", () => ({
  LoadingSurface: (props: { label?: string }) => (
    <div data-testid="loading">{props.label ?? "Loading"}</div>
  ),
}));

const ok = <T,>(data: T) => Promise.resolve({ status: "ok" as const, data });

const runDiff = (
  overrides: Partial<WorkspaceRunDiffTs> = {},
): WorkspaceRunDiffTs => ({
  range:
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa..bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  stat: " src/chips.tsx | 3 ++-",
  files: ["src/chips.tsx"],
  patch:
    "diff --git a/src/chips.tsx b/src/chips.tsx\n+added evidence chip\n-old chip\n",
  patch_truncated: false,
  max_patch_bytes: 98304,
  ...overrides,
});

const repo = (overrides: Partial<RepoRecordTs> = {}): RepoRecordTs => ({
  repo_id: "018f0000-0000-7000-8000-000000000001",
  canonical_path: "/repos/proxima",
  display_name: "Proxima",
  target_branch: "main",
  has_been_polled: true,
  last_polled_at: "2026-05-11T12:00:00Z",
  created_at: "2026-05-11T11:00:00Z",
  ...overrides,
});

const review = (
  overrides: Partial<WorkspaceReviewRecordTs> = {},
): WorkspaceReviewRecordTs => ({
  memory_id: "018f0000-0000-7000-8000-000000000101",
  workspace_run_memory_id: "018f0000-0000-7000-8000-000000000201",
  execution_request_memory_id: "018f0000-0000-7000-8000-000000000301",
  verdict: "approved",
  round_index: 0,
  summary: "looks correct",
  findings: [],
  correction_instructions: null,
  verification_summary: "cargo test passed",
  reviewed_at: "2026-05-11T12:02:00Z",
  created_at: "2026-05-11T12:02:00Z",
  ...overrides,
});

const decision = (
  overrides: Partial<WorkspaceDecisionRecordTs> = {},
): WorkspaceDecisionRecordTs => ({
  memory_id: "018f0000-0000-7000-8000-000000000401",
  workspace_run_memory_id: "018f0000-0000-7000-8000-000000000201",
  decision: "rejected",
  decided_at: "2026-05-11T12:03:00Z",
  reason_text: "wrong behavior",
  decided_by_owner_id: "018f0000-0000-7000-8000-000000000501",
  ...overrides,
});

const workspaceRun = (
  overrides: Partial<WorkspaceRunRecordTs> = {},
): WorkspaceRunRecordTs => ({
  memory_id: "018f0000-0000-7000-8000-000000000201",
  wake_invocation_id: "018f0000-0000-7000-8000-000000000202",
  repo_id: "018f0000-0000-7000-8000-000000000001",
  execution_request_title: "Add evidence chips",
  target_branch: "main",
  worktree_path: "/tmp/proxima-worker",
  branch_name: "proxima/wake/018f0000",
  parent_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  head_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  diff_stat_json: {
    files_changed: 1,
    insertions: 2,
    deletions: 1,
    files: [{ path: "src/chips.tsx", insertions: 2, deletions: 1 }],
  },
  exit_code: 0,
  stdout_tail: null,
  stderr_tail: null,
  duration_ms: 1200,
  created_at: "2026-05-11T12:01:00Z",
  latest_review: review(),
  latest_decision: null,
  ...overrides,
});

describe("RunsPanel", () => {
  beforeEach(() => {
    mocks.reposList.mockResolvedValue(ok([repo()]));
    mocks.codeListWorkspaceRuns.mockResolvedValue(ok([workspaceRun()]));
    mocks.codeMergeWorkspaceRun.mockResolvedValue(
      ok({
        run_memory_id: "018f0000-0000-7000-8000-000000000201",
        decision_memory_id: "018f0000-0000-7000-8000-000000000402",
        repo_id: "018f0000-0000-7000-8000-000000000001",
        target_branch: "main",
        old_target_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        new_target_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      }),
    );
    mocks.codeDecideWorkspaceRun.mockResolvedValue(
      ok("018f0000-0000-7000-8000-000000000403"),
    );
    mocks.codeGetWorkspaceRunDiff.mockResolvedValue(
      ok(runDiff()),
    );
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("shows approved run as merge proposal", async () => {
    render(() => <RunsPanel />);

    expect(await screen.findByText("Add evidence chips")).toBeTruthy();
    expect(screen.getByText("approved")).toBeTruthy();
    expect(screen.getByText("1 file")).toBeTruthy();
    expect(screen.getByText("+2")).toBeTruthy();
    expect(screen.getByText("-1")).toBeTruthy();
    expect(screen.getByText("cargo test passed")).toBeTruthy();
    expect(
      (screen.getByRole("button", { name: "Merge" }) as HTMLButtonElement)
        .disabled,
    ).toBe(false);
    expect(
      (screen.getByRole("button", { name: "Discard" }) as HTMLButtonElement)
        .disabled,
    ).toBe(false);
    expect(
      (
        screen.getByRole("button", {
          name: "Request Retry",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(false);
  });

  it("disables merge before verifier approval", async () => {
    mocks.codeListWorkspaceRuns.mockResolvedValue(
      ok([workspaceRun({ latest_review: null })]),
    );
    render(() => <RunsPanel />);

    expect(await screen.findByText("pending review")).toBeTruthy();
    expect(
      (screen.getByRole("button", { name: "Merge" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });

  it("calls merge command and refetches runs", async () => {
    render(() => <RunsPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "Merge" }));

    await waitFor(() => {
      expect(mocks.codeMergeWorkspaceRun).toHaveBeenCalledWith(
        "018f0000-0000-7000-8000-000000000201",
      );
      expect(mocks.codeListWorkspaceRuns).toHaveBeenCalledTimes(2);
    });
  });

  it("loads and renders the workspace diff on demand", async () => {
    render(() => <RunsPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "Show diff" }));

    expect(await screen.findByText("src/chips.tsx")).toBeTruthy();
    expect(screen.getByText(/\+added evidence chip/)).toBeTruthy();
    expect(mocks.codeGetWorkspaceRunDiff).toHaveBeenCalledWith(
      "018f0000-0000-7000-8000-000000000201",
    );
  });

  it("renders unified patches as per-file diff sections with counts", async () => {
    mocks.codeGetWorkspaceRunDiff.mockResolvedValue(
      ok(
        runDiff({
          files: ["src/a.ts", "src/b.ts"],
          patch: [
            "diff --git a/src/a.ts b/src/a.ts",
            "--- a/src/a.ts",
            "+++ b/src/a.ts",
            "@@ -1 +1 @@",
            "-old",
            "+new",
            "diff --git a/src/b.ts b/src/b.ts",
            "--- /dev/null",
            "+++ b/src/b.ts",
            "@@ -0,0 +1,2 @@",
            "+one",
            "+two",
          ].join("\n"),
        }),
      ),
    );
    const { container } = render(() => <RunsPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "Show diff" }));

    await screen.findByText("src/a.ts");
    expect(screen.getByText("src/b.ts")).toBeTruthy();
    const sections = container.querySelectorAll(".proxima-run-diff-file");
    expect(sections).toHaveLength(2);
    expect(sections[0]?.textContent).toContain("+1");
    expect(sections[0]?.textContent).toContain("-1");
    expect(sections[1]?.textContent).toContain("+2");
    expect(sections[1]?.textContent).toContain("-0");
  });

  it("keeps the truncation notice in the diff monitor", async () => {
    mocks.codeGetWorkspaceRunDiff.mockResolvedValue(
      ok(runDiff({ patch_truncated: true, max_patch_bytes: 128 })),
    );
    render(() => <RunsPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "Show diff" }));

    expect(await screen.findByText("truncated at 128 bytes")).toBeTruthy();
  });

  it("discards a run with reason and refetches", async () => {
    render(() => <RunsPanel />);

    fireEvent.input(await screen.findByPlaceholderText("decision reason"), {
      target: { value: "not the requested change" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));

    await waitFor(() => {
      expect(mocks.codeDecideWorkspaceRun).toHaveBeenCalledWith(
        "018f0000-0000-7000-8000-000000000201",
        "rejected",
        "not the requested change",
      );
      expect(mocks.codeListWorkspaceRuns).toHaveBeenCalledTimes(2);
    });
  });

  it("requests retry with reason and refetches", async () => {
    render(() => <RunsPanel />);

    fireEvent.input(await screen.findByPlaceholderText("decision reason"), {
      target: { value: "try the smaller fix" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Request Retry" }));

    await waitFor(() => {
      expect(mocks.codeDecideWorkspaceRun).toHaveBeenCalledWith(
        "018f0000-0000-7000-8000-000000000201",
        "retry_requested",
        "try the smaller fix",
      );
      expect(mocks.codeListWorkspaceRuns).toHaveBeenCalledTimes(2);
    });
  });

  it("hides closed runs by default", async () => {
    mocks.codeListWorkspaceRuns.mockResolvedValue(
      ok([
        workspaceRun({
          latest_review: review({
            verdict: "needs_user",
            summary: "veto cap reached",
            verification_summary: "manual review required",
            findings: [
              {
                severity: "high",
                file_path: "src/chips.tsx",
                line: 12,
                message: "still incomplete",
              },
            ],
          }),
          latest_decision: decision(),
        }),
      ]),
    );
    render(() => <RunsPanel />);

    expect(await screen.findByText("No open workspace runs for this repo.")).toBeTruthy();
    expect(screen.queryByText("Add evidence chips")).toBeNull();
  });

  it("renders closed runs when requested", async () => {
    mocks.codeListWorkspaceRuns.mockResolvedValue(
      ok([
        workspaceRun({
          latest_review: review({
            verdict: "needs_user",
            summary: "veto cap reached",
            verification_summary: "manual review required",
            findings: [
              {
                severity: "high",
                file_path: "src/chips.tsx",
                line: 12,
                message: "still incomplete",
              },
            ],
          }),
          latest_decision: decision(),
        }),
      ]),
    );
    render(() => <RunsPanel />);

    fireEvent.click(await screen.findByLabelText("Show closed"));

    expect(await screen.findByText("needs_user")).toBeTruthy();
    expect(screen.getByText("discarded")).toBeTruthy();
    expect(screen.getByText("1 finding")).toBeTruthy();
    expect(
      (screen.getByRole("button", { name: "Merge" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    expect(
      (screen.getByRole("button", { name: "Discard" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    expect(
      (
        screen.getByRole("button", {
          name: "Request Retry",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
  });
});
