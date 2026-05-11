import {
  For,
  Show,
  createEffect,
  createMemo,
  createResource,
  createSignal,
  type Component,
} from "solid-js";
import {
  commands,
  formatCommandError,
  type RepoRecordTs,
  type WorkspaceDecisionRecordTs,
  type WorkspaceRunDiffTs,
  type WorkspaceReviewRecordTs,
  type WorkspaceRunRecordTs,
} from "@proxima/core";
import { LoadingSurface } from "@proxima/core/primitives";
import { highlightedCode, languageFromPath } from "./code-highlight";

type ActionState =
  | { kind: "idle" }
  | { kind: "merging"; runId: string }
  | { kind: "discarding"; runId: string }
  | { kind: "retrying"; runId: string }
  | { kind: "error"; message: string };

async function loadRepos(): Promise<RepoRecordTs[]> {
  const r = await commands.reposList();
  if (r.status === "error") {
    const err = new Error(formatCommandError(r.error));
    (err as Error & { cause?: unknown }).cause = r.error;
    throw err;
  }
  return r.data;
}

async function loadRuns(repoId: string): Promise<WorkspaceRunRecordTs[]> {
  const r = await commands.codeListWorkspaceRuns(repoId, 50);
  if (r.status === "error") {
    const err = new Error(formatCommandError(r.error));
    (err as Error & { cause?: unknown }).cause = r.error;
    throw err;
  }
  return r.data;
}

async function loadRunDiff(runId: string): Promise<WorkspaceRunDiffTs> {
  const r = await commands.codeGetWorkspaceRunDiff(runId);
  if (r.status === "error") {
    const err = new Error(formatCommandError(r.error));
    (err as Error & { cause?: unknown }).cause = r.error;
    throw err;
  }
  return r.data;
}

interface DiffFileSection {
  key: string;
  path: string;
  patch: string;
  insertions: number;
  deletions: number;
}

type DiffLineKind = "addition" | "deletion" | "context" | "hunk" | "meta";

interface DiffLine {
  key: string;
  kind: DiffLineKind;
  prefix: string;
  content: string;
}

const stripDiffPathPrefix = (value: string): string =>
  value.replace(/^(?:a|b)\//, "");

const pathFromDiffHeader = (header: string): string | null => {
  const match = /^diff --git a\/(.+) b\/(.+)$/.exec(header);
  if (match) return match[2]!;
  return null;
};

const pathFromFileHeader = (line: string): string | null => {
  const value = line.slice(4).trim();
  if (value === "/dev/null") return null;
  const path = stripDiffPathPrefix(value.split(/\t/, 1)[0] ?? value);
  return path === "" ? null : path;
};

const diffFilePath = (lines: string[], fallback: string): string => {
  const fromDiff = pathFromDiffHeader(lines[0] ?? "");
  if (fromDiff !== null) return fromDiff;
  const plus = lines.find((line) => line.startsWith("+++ "));
  if (plus) {
    const path = pathFromFileHeader(plus);
    if (path !== null) return path;
  }
  const minus = lines.find((line) => line.startsWith("--- "));
  if (minus) {
    const path = pathFromFileHeader(minus);
    if (path !== null) return path;
  }
  return fallback;
};

const diffLineCounts = (
  lines: string[],
): Pick<DiffFileSection, "insertions" | "deletions"> => {
  let insertions = 0;
  let deletions = 0;
  for (const line of lines) {
    if (line.startsWith("+") && !line.startsWith("+++")) insertions += 1;
    if (line.startsWith("-") && !line.startsWith("---")) deletions += 1;
  }
  return { insertions, deletions };
};

const parseUnifiedDiff = (
  patch: string,
  fallbackFiles: string[],
): DiffFileSection[] => {
  if (patch.trim() === "") return [];
  const sections: string[][] = [];
  let current: string[] = [];
  for (const line of patch.split(/\r?\n/)) {
    if (line.startsWith("diff --git ") && current.length > 0) {
      sections.push(current);
      current = [];
    }
    current.push(line);
  }
  if (current.length > 0) sections.push(current);

  return sections.map((lines, index) => {
    const fallback = fallbackFiles[index] ?? `patch-${index + 1}`;
    const counts = diffLineCounts(lines);
    const path = diffFilePath(lines, fallback);
    return {
      key: `${index}:${path}`,
      path,
      patch: lines.join("\n"),
      ...counts,
    };
  });
};

const diffLineKind = (line: string): DiffLineKind => {
  if (line.startsWith("@@")) return "hunk";
  if (
    line.startsWith("diff --git ") ||
    line.startsWith("index ") ||
    line.startsWith("--- ") ||
    line.startsWith("+++ ") ||
    line.startsWith("new file mode ") ||
    line.startsWith("deleted file mode ") ||
    line.startsWith("similarity index ") ||
    line.startsWith("rename from ") ||
    line.startsWith("rename to ")
  ) {
    return "meta";
  }
  if (line.startsWith("+")) return "addition";
  if (line.startsWith("-")) return "deletion";
  return "context";
};

const parseDiffLines = (patch: string): DiffLine[] =>
  patch.split(/\r?\n/).map((line, index) => {
    const kind = diffLineKind(line);
    const hasDiffPrefix =
      kind === "addition" || kind === "deletion" || kind === "context";
    return {
      key: `${index}:${line}`,
      kind,
      prefix: hasDiffPrefix ? line.slice(0, 1) || " " : "",
      content: hasDiffPrefix ? line.slice(1) : line,
    };
  });

export const RunsPanel: Component = () => {
  const [repos] = createResource(loadRepos);
  const [selectedRepoId, setSelectedRepoId] = createSignal<string | null>(null);
  const [showClosed, setShowClosed] = createSignal(false);
  const [action, setAction] = createSignal<ActionState>({ kind: "idle" });
  const [runs, { refetch: refetchRuns }] = createResource(
    selectedRepoId,
    loadRuns,
  );

  createEffect(() => {
    const list = repos();
    if (!list) return;
    const selected = selectedRepoId();
    if (selected && list.some((repo) => repo.repo_id === selected)) return;
    setSelectedRepoId(list[0]?.repo_id ?? null);
  });

  const selectedRepo = (): RepoRecordTs | undefined =>
    (repos() ?? []).find((repo) => repo.repo_id === selectedRepoId());
  const visibleRuns = (): WorkspaceRunRecordTs[] =>
    (runs() ?? []).filter((run) => showClosed() || run.latest_decision === null);

  const mergeRun = async (run: WorkspaceRunRecordTs): Promise<void> => {
    setAction({ kind: "merging", runId: run.memory_id });
    const r = await commands.codeMergeWorkspaceRun(run.memory_id);
    if (r.status === "error") {
      setAction({
        kind: "error",
        message: `merge failed: ${formatCommandError(r.error)}`,
      });
      return;
    }
    setAction({ kind: "idle" });
    void refetchRuns();
  };

  const discardRun = async (
    run: WorkspaceRunRecordTs,
    reason: string,
  ): Promise<void> => {
    setAction({ kind: "discarding", runId: run.memory_id });
    const trimmed = reason.trim();
    const r = await commands.codeDecideWorkspaceRun(
      run.memory_id,
      "rejected",
      trimmed === "" ? null : trimmed,
    );
    if (r.status === "error") {
      setAction({
        kind: "error",
        message: `discard failed: ${formatCommandError(r.error)}`,
      });
      return;
    }
    setAction({ kind: "idle" });
    void refetchRuns();
  };

  const requestRetry = async (
    run: WorkspaceRunRecordTs,
    reason: string,
  ): Promise<void> => {
    setAction({ kind: "retrying", runId: run.memory_id });
    const trimmed = reason.trim();
    const r = await commands.codeDecideWorkspaceRun(
      run.memory_id,
      "retry_requested",
      trimmed === "" ? null : trimmed,
    );
    if (r.status === "error") {
      setAction({
        kind: "error",
        message: `retry request failed: ${formatCommandError(r.error)}`,
      });
      return;
    }
    setAction({ kind: "idle" });
    void refetchRuns();
  };

  return (
    <div class="proxima-code-panel">
      <header class="proxima-runs-header">
        <div>
          <h2>Runs</h2>
          <Show when={selectedRepo()}>
            {(repo) => (
              <p class="proxima-dim proxima-mono">{repo().canonical_path}</p>
            )}
          </Show>
        </div>
        <div class="proxima-runs-toolbar">
          <label class="proxima-runs-toggle">
            <input
              type="checkbox"
              checked={showClosed()}
              onChange={(e) => setShowClosed(e.currentTarget.checked)}
            />
            <span>Show closed</span>
          </label>
          <Show when={(repos() ?? []).length > 1}>
            <select
              class="proxima-runs-repo-select"
              value={selectedRepoId() ?? ""}
              aria-label="Repo"
              onChange={(e) => {
                setAction({ kind: "idle" });
                setSelectedRepoId(e.currentTarget.value);
              }}
            >
              <For each={repos()}>
                {(repo) => (
                  <option value={repo.repo_id}>{repo.display_name}</option>
                )}
              </For>
            </select>
          </Show>
        </div>
      </header>

      <Show when={action().kind === "error"}>
        <p class="proxima-error">{(action() as { message: string }).message}</p>
      </Show>
      <Show when={repos.error}>
        <p class="proxima-error">{String(repos.error)}</p>
      </Show>
      <Show when={runs.error}>
        <p class="proxima-error">{String(runs.error)}</p>
      </Show>

      <Show
        when={!repos.loading}
        fallback={<LoadingSurface label="Loading repos" />}
      >
        <Show
          when={(repos() ?? []).length > 0}
          fallback={<p class="proxima-dim">No repos registered.</p>}
        >
          <Show
            when={!runs.loading}
            fallback={<LoadingSurface label="Loading workspace runs" />}
          >
            <Show
              when={visibleRuns().length > 0}
              fallback={
                <p class="proxima-dim">
                  {showClosed()
                    ? "No workspace runs for this repo."
                    : "No open workspace runs for this repo."}
                </p>
              }
            >
              <div class="proxima-runs-list">
                <For each={visibleRuns()}>
                  {(run) => (
                    <RunRow
                      run={run}
                      action={action()}
                      onMerge={() => mergeRun(run)}
                      onDiscard={(reason) => discardRun(run, reason)}
                      onRetry={(reason) => requestRetry(run, reason)}
                    />
                  )}
                </For>
              </div>
            </Show>
          </Show>
        </Show>
      </Show>
    </div>
  );
};

const RunRow: Component<{
  run: WorkspaceRunRecordTs;
  action: ActionState;
  onMerge: () => void;
  onDiscard: (reason: string) => void;
  onRetry: (reason: string) => void;
}> = (props) => {
  const [reason, setReason] = createSignal("");
  const [showDiff, setShowDiff] = createSignal(false);
  const [runDiff] = createResource(
    () => (showDiff() ? props.run.memory_id : null),
    loadRunDiff,
  );
  const terminalDecision = (): WorkspaceDecisionRecordTs | null =>
    props.run.latest_decision;
  const review = (): WorkspaceReviewRecordTs | null => props.run.latest_review;
  const isBusy = (): boolean =>
    (props.action.kind === "merging" ||
      props.action.kind === "discarding" ||
      props.action.kind === "retrying") &&
    props.action.runId === props.run.memory_id;
  const anyActionBusy = (): boolean =>
    props.action.kind === "merging" ||
    props.action.kind === "discarding" ||
    props.action.kind === "retrying";
  const canMerge = (): boolean =>
    review()?.verdict === "approved" && terminalDecision() === null;
  const canDecide = (): boolean => terminalDecision() === null;
  const title = (): string =>
    props.run.execution_request_title ??
    `Workspace run ${props.run.memory_id.slice(0, 12)}`;
  const reviewLabel = (): string => review()?.verdict ?? "pending review";
  const decisionLabel = (): string | null => {
    switch (terminalDecision()?.decision) {
      case "rejected":
        return "discarded";
      case "retry_requested":
        return "retry requested";
      case "accepted":
        return "accepted";
      case "merged":
        return "merged";
      default:
        return null;
    }
  };
  const diff = () => props.run.diff_stat_json;

  return (
    <article class="proxima-run-row">
      <div class="proxima-run-main">
        <div class="proxima-run-title-line">
          <span class="proxima-run-title">{title()}</span>
          <span
            classList={{
              "proxima-run-badge": true,
              [`verdict-${reviewLabel()}`]: true,
            }}
          >
            {reviewLabel()}
          </span>
          <Show when={decisionLabel()}>
            {(decision) => (
              <span class="proxima-run-badge decision">{decision()}</span>
            )}
          </Show>
        </div>
        <div class="proxima-run-meta proxima-mono">
          <span class="proxima-run-branch">{props.run.branch_name}</span>
          <span class="proxima-run-sha">{props.run.head_sha.slice(0, 12)}</span>
          <span class="proxima-run-stat">
            {diff().files_changed} {diff().files_changed === 1 ? "file" : "files"}
          </span>
          <span class="proxima-run-stat">+{diff().insertions}</span>
          <span class="proxima-run-stat">-{diff().deletions}</span>
        </div>
        <Show when={review()}>
          {(latest) => (
            <div class="proxima-run-review">
              <Show when={latest().verification_summary}>
                {(summary) => <p>{summary()}</p>}
              </Show>
              <p>
                {latest().findings.length}{" "}
                {latest().findings.length === 1 ? "finding" : "findings"}
              </p>
            </div>
          )}
        </Show>
        <div class="proxima-run-diff-toggle">
          <button
            type="button"
            class="proxima-link-button"
            onClick={() => setShowDiff((value) => !value)}
          >
            {showDiff() ? "Hide diff" : "Show diff"}
          </button>
        </div>
        <Show when={showDiff()}>
          <div class="proxima-run-diff">
            <Show
              when={!runDiff.loading}
              fallback={<p class="proxima-dim">Loading diff...</p>}
            >
              <Show
                when={runDiff()}
                fallback={
                  <p class="proxima-error">
                    {runDiff.error instanceof Error
                      ? runDiff.error.message
                      : String(runDiff.error)}
                  </p>
                }
              >
                {(diff) => <RunDiffMonitor diff={diff()} />}
              </Show>
            </Show>
          </div>
        </Show>
      </div>
      <div class="proxima-run-actions">
        <button
          type="button"
          class="proxima-btn"
          disabled={!canMerge() || anyActionBusy()}
          onClick={props.onMerge}
        >
          {props.action.kind === "merging" && isBusy() ? "Merging..." : "Merge"}
        </button>
        <textarea
          class="proxima-run-decline-reason"
          placeholder="decision reason"
          value={reason()}
          rows={2}
          disabled={!canDecide() || anyActionBusy()}
          onInput={(e) => setReason(e.currentTarget.value)}
        />
        <button
          type="button"
          class="proxima-btn"
          disabled={!canDecide() || anyActionBusy()}
          onClick={() => props.onDiscard(reason())}
        >
          {props.action.kind === "discarding" && isBusy()
            ? "Discarding..."
            : "Discard"}
        </button>
        <button
          type="button"
          class="proxima-btn"
          disabled={!canDecide() || anyActionBusy()}
          onClick={() => props.onRetry(reason())}
        >
          {props.action.kind === "retrying" && isBusy()
            ? "Requesting..."
            : "Request Retry"}
        </button>
      </div>
    </article>
  );
};

const RunDiffMonitor: Component<{ diff: WorkspaceRunDiffTs }> = (props) => {
  const sections = createMemo(() =>
    parseUnifiedDiff(props.diff.patch, props.diff.files),
  );

  return (
    <>
      <div class="proxima-run-diff-head proxima-mono">
        <span>{props.diff.range}</span>
        <Show when={props.diff.patch_truncated}>
          <span>truncated at {props.diff.max_patch_bytes} bytes</span>
        </Show>
      </div>
      <div class="proxima-run-diff-monitor">
        <Show
          when={sections().length > 0}
          fallback={<p class="proxima-run-diff-empty">No patch content.</p>}
        >
          <For each={sections()}>
            {(section) => <RunDiffFileSection section={section} />}
          </For>
        </Show>
      </div>
    </>
  );
};

const RunDiffFileSection: Component<{ section: DiffFileSection }> = (props) => {
  const language = createMemo(() => languageFromPath(props.section.path));
  const lines = createMemo(() => parseDiffLines(props.section.patch));

  return (
    <details class="proxima-run-diff-file" open>
      <summary class="proxima-run-diff-file-head">
        <span class="proxima-run-diff-file-path">{props.section.path}</span>
        <span class="proxima-run-diff-file-stat">+{props.section.insertions}</span>
        <span class="proxima-run-diff-file-stat">-{props.section.deletions}</span>
      </summary>
      <pre class="proxima-run-diff-patch">
        <code class="proxima-run-diff-code">
          <For each={lines()}>
            {(line) => <RunDiffLine line={line} language={language()} />}
          </For>
        </code>
      </pre>
    </details>
  );
};

const RunDiffLine: Component<{ line: DiffLine; language: string | null }> = (
  props,
) => {
  const highlighted = createMemo(() => {
    if (props.line.kind === "hunk" || props.line.kind === "meta") {
      return { html: "", language: "diff" };
    }
    return highlightedCode(props.line.content, props.language);
  });

  return (
    <span
      classList={{
        "proxima-run-diff-line": true,
        [`line-${props.line.kind}`]: true,
      }}
    >
      <span class="proxima-run-diff-prefix">{props.line.prefix}</span>
      <Show
        when={props.line.kind !== "hunk" && props.line.kind !== "meta"}
        fallback={
          <span class="proxima-run-diff-content">{props.line.content}</span>
        }
      >
        <span
          class={`proxima-run-diff-content hljs language-${highlighted().language}`}
          innerHTML={highlighted().html}
        />
      </Show>
    </span>
  );
};
