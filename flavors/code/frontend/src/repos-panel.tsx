import {
  createEffect,
  For,
  Show,
  createResource,
  createSignal,
  type Component,
  type JSX,
} from "solid-js";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  commands,
  type RepoEraseReceiptTs,
  type RepoRecordTs,
} from "@proxima/core";
import { formatPolledAt } from "@proxima/core";
import { formatCommandError } from "@proxima/core";
import { LoadingSurface, ProximaLoader } from "@proxima/core/primitives";
import { ingestStore, type RunRecord } from "./ingest-store";

type EraseState =
  | { kind: "idle" }
  | { kind: "running"; repoId: string; displayName: string }
  | { kind: "done"; receipt: RepoEraseReceiptTs }
  | { kind: "error"; repoId: string; message: string };

async function loadRepos(): Promise<RepoRecordTs[]> {
  const r = await commands.reposList();
  if (r.status === "error") {
    console.error("repos_list error:", r.error);
    const err = new Error(`repos_list: ${formatCommandError(r.error)}`);
    (err as Error & { cause?: unknown }).cause = r.error;
    throw err;
  }
  return r.data;
}

export const ReposPanel: Component = () => {
  const [repos, { mutate, refetch }] = createResource(loadRepos);
  const [globalError, setGlobalError] = createSignal<string | null>(null);
  const [erase, setErase] = createSignal<EraseState>({ kind: "idle" });

  createEffect(() => {
    for (const repo of repos() ?? []) void ingestStore.rehydrate(repo.repo_id);
  });

  const isErasing = (repoId: string): boolean => {
    const s = erase();
    return s.kind === "running" && s.repoId === repoId;
  };
  const eraseDoneReceipt = (): RepoEraseReceiptTs | null => {
    const s = erase();
    return s.kind === "done" ? s.receipt : null;
  };
  const anyBusy = (): boolean =>
    erase().kind === "running" ||
    (repos() ?? []).some((repo) => ingestStore.isRunning(repo.repo_id));

  const handleAdd = async (): Promise<void> => {
    setGlobalError(null);
    setErase({ kind: "idle" });
    const selected = await openDialog({
      directory: true,
      multiple: false,
      title: "Select a git repository",
    });
    if (selected === null) return;
    const path = Array.isArray(selected) ? selected[0]! : selected;
    const r = await commands.reposRegister(path, null);
    if (r.status === "error") {
      setGlobalError(formatCommandError(r.error));
      return;
    }
    refetch();
  };

  const handleDelete = async (repo: RepoRecordTs): Promise<void> => {
    setGlobalError(null);
    setErase({ kind: "idle" });
    setErase({
      kind: "running",
      repoId: repo.repo_id,
      displayName: repo.display_name,
    });
    const r = await commands.reposErase(repo.repo_id);
    if (r.status === "error") {
      const message = formatCommandError(r.error);
      setErase({ kind: "error", repoId: repo.repo_id, message });
      setGlobalError(message);
      return;
    }
    mutate((current) => current?.filter((r) => r.repo_id !== repo.repo_id));
    setErase({ kind: "done", receipt: r.data });
    void refetch();
  };

  const handleIngestNext = async (repo: RepoRecordTs): Promise<void> => {
    setGlobalError(null);
    setErase({ kind: "idle" });
    await ingestStore.start(repo.repo_id, 1);
  };

  const handleIngestAll = async (repo: RepoRecordTs): Promise<void> => {
    setGlobalError(null);
    setErase({ kind: "idle" });
    await ingestStore.start(repo.repo_id, null);
  };

  const handleSetTargetBranch = async (
    repo: RepoRecordTs,
    targetBranch: string | null,
  ): Promise<boolean> => {
    setGlobalError(null);
    setErase({ kind: "idle" });
    const r = await commands.codeSetRepoTargetBranch(
      repo.repo_id,
      targetBranch,
    );
    if (r.status === "error") {
      setGlobalError(formatCommandError(r.error));
      return false;
    }
    mutate((current) =>
      current?.map((item) => (item.repo_id === repo.repo_id ? r.data : item)),
    );
    return true;
  };

  return (
    <div class="proxima-code-panel">
      <header class="proxima-repos-header">
        <h2>Repos</h2>
        <button
          type="button"
          class="proxima-btn"
          disabled={anyBusy()}
          onClick={handleAdd}
        >
          Add Repo
        </button>
      </header>

      <Show when={globalError()}>
        {(msg) => <p class="proxima-error">{msg()}</p>}
      </Show>
      <Show when={erase().kind === "running"}>
        <LoadingSurface mode="inline" label="Deleting" size={36} />
      </Show>
      <Show when={eraseDoneReceipt()}>
        {(r) => (
          <p class="proxima-dim proxima-mono">
            deleted {r().facts_deleted.toString()} facts,{" "}
            {r().abstractions_deleted.toString()} abstractions,{" "}
            {r().edges_deleted.toString()} edges,{" "}
            {r().embeddings_deleted.toString()} embeddings
          </p>
        )}
      </Show>

      <Show
        when={!repos.loading}
        fallback={<LoadingSurface label="Loading repos" />}
      >
        <Show
          when={(repos() ?? []).length > 0}
          fallback={
            <p class="proxima-dim">
              No repos registered yet. Click "Add Repo" to point Proxima at a
              local git working tree.
            </p>
          }
        >
          <div class="proxima-repos-list">
            <For each={repos()}>
              {(repo) => (
                <RepoRow
                  repo={repo}
                  isErasing={isErasing(repo.repo_id)}
                  anyBusy={anyBusy()}
                  ingest={ingestStore.state[repo.repo_id]}
                  erase={erase()}
                  onIngestNext={() => handleIngestNext(repo)}
                  onIngestAll={() => handleIngestAll(repo)}
                  onSetTargetBranch={(targetBranch) =>
                    handleSetTargetBranch(repo, targetBranch)
                  }
                  onDelete={() => handleDelete(repo)}
                />
              )}
            </For>
          </div>
        </Show>
      </Show>
    </div>
  );
};

const RepoRow: Component<{
  repo: RepoRecordTs;
  isErasing: boolean;
  anyBusy: boolean;
  ingest: RunRecord | undefined;
  erase: EraseState;
  onIngestNext: () => void;
  onIngestAll: () => void;
  onSetTargetBranch: (targetBranch: string | null) => Promise<boolean>;
  onDelete: () => void;
}> = (props) => {
  const [confirmingDelete, setConfirmingDelete] = createSignal(false);
  const [confirmText, setConfirmText] = createSignal("");
  const [branchDraft, setBranchDraft] = createSignal(
    props.repo.target_branch ?? "",
  );
  const [branchSaving, setBranchSaving] = createSignal(false);
  createEffect(() => setBranchDraft(props.repo.target_branch ?? ""));

  const status = (): JSX.Element => {
    if (props.isErasing) return "deleting repo data...";
    if (confirmingDelete()) return "type repo name to confirm deletion";
    const ingestStatus = statusFromRecord(props.ingest, props.repo);
    if (ingestStatus !== null) return ingestStatus;
    const e = props.erase;
    if (e.kind === "error" && e.repoId === props.repo.repo_id) {
      return `error: ${e.message}`;
    }
    const target = props.repo.target_branch ?? "no target branch";
    return `${formatPolledAt(props.repo.last_polled_at)} - target ${target}`;
  };
  const running = (): boolean => ingestStore.isRunning(props.repo.repo_id);
  const ingestBtnDisabled = (): boolean => props.anyBusy || running();
  const deleteBtnDisabled = (): boolean => props.anyBusy || running();
  const branchBtnDisabled = (): boolean =>
    props.anyBusy || running() || branchSaving();
  const normalizedBranchDraft = (): string => branchDraft().trim();
  const branchUnchanged = (): boolean =>
    normalizedBranchDraft() === (props.repo.target_branch ?? "");
  const saveBranch = async (): Promise<void> => {
    if (branchBtnDisabled() || branchUnchanged()) return;
    setBranchSaving(true);
    const next = normalizedBranchDraft();
    const saved = await props.onSetTargetBranch(next === "" ? null : next);
    setBranchSaving(false);
    if (saved) setBranchDraft(next);
  };
  const clearBranch = async (): Promise<void> => {
    if (branchBtnDisabled() || props.repo.target_branch === null) return;
    setBranchSaving(true);
    const saved = await props.onSetTargetBranch(null);
    setBranchSaving(false);
    if (saved) setBranchDraft("");
  };
  const canConfirmDelete = (): boolean =>
    confirmText() === props.repo.display_name && !deleteBtnDisabled();
  const openDeleteConfirm = (): void => {
    setConfirmText("");
    setConfirmingDelete(true);
  };
  const cancelDeleteConfirm = (): void => {
    setConfirmText("");
    setConfirmingDelete(false);
  };
  const confirmDelete = (): void => {
    if (!canConfirmDelete()) return;
    props.onDelete();
  };

  return (
    <article class="proxima-repo-row">
      <div class="proxima-repo-head">
        <span class="proxima-repo-name">{props.repo.display_name}</span>
        <span class="proxima-dim proxima-mono">{props.repo.canonical_path}</span>
      </div>
      <div class="proxima-repo-status">{status()}</div>
      <div class="proxima-repo-actions">
        <Show
          when={confirmingDelete()}
          fallback={
            <>
              <button
                type="button"
                class="proxima-btn"
                disabled={ingestBtnDisabled()}
                onClick={props.onIngestNext}
              >
                Ingest Next
              </button>
              <button
                type="button"
                class="proxima-btn"
                disabled={ingestBtnDisabled()}
                onClick={props.onIngestAll}
              >
                Ingest All
              </button>
              <input
                type="text"
                class="proxima-repo-branch-input"
                value={branchDraft()}
                placeholder="target branch"
                disabled={branchBtnDisabled()}
                onInput={(e) => setBranchDraft(e.currentTarget.value)}
              />
              <button
                type="button"
                class="proxima-btn"
                disabled={branchBtnDisabled() || branchUnchanged()}
                onClick={saveBranch}
              >
                {branchSaving() ? "Saving..." : "Save Branch"}
              </button>
              <button
                type="button"
                class="proxima-btn"
                disabled={branchBtnDisabled() || props.repo.target_branch === null}
                onClick={clearBranch}
              >
                Clear
              </button>
              <button
                type="button"
                class="proxima-btn proxima-btn-danger"
                disabled={deleteBtnDisabled()}
                onClick={openDeleteConfirm}
              >
                {props.isErasing ? "Deleting..." : "Delete"}
              </button>
            </>
          }
        >
          <input
            type="text"
            class="proxima-repo-delete-confirm"
            value={confirmText()}
            placeholder={props.repo.display_name}
            disabled={deleteBtnDisabled()}
            onInput={(e) => setConfirmText(e.currentTarget.value)}
          />
          <button
            type="button"
            class="proxima-btn proxima-btn-danger"
            disabled={!canConfirmDelete()}
            onClick={confirmDelete}
          >
            Confirm
          </button>
          <button
            type="button"
            class="proxima-btn"
            disabled={deleteBtnDisabled()}
            onClick={cancelDeleteConfirm}
          >
            Cancel
          </button>
        </Show>
      </div>
    </article>
  );
};

function statusFromRecord(
  rec: RunRecord | undefined,
  repo: RepoRecordTs,
): JSX.Element | null {
  if (!rec?.run) return null;
  const r = rec.run;
  if (r.status === "succeeded") {
    return (
      `done - +${r.commits_emitted} commits, +${r.chunks_emitted} chunks, ` +
      `+${r.ast_edges_emitted} edges, +${r.abstractions_emitted} abstractions, ` +
      `+${r.embeddings_landed} embeddings, +${r.citations_emitted} citations`
    );
  }
  if (r.status === "failed") {
    return `failed ${r.stage}: ${r.error_message ?? rec.terminalError ?? "(no message)"}`;
  }
  if (r.stage === "facts" && rec.liveProgress) {
    const p = rec.liveProgress;
    return `running facts: commit ${p.commit_index + 1}/${p.total_commits}`;
  }
  if (rec.terminalError) return `error: ${rec.terminalError}`;
  if (repo.repo_id !== r.repo_id) return null;
  return (
    <span class="proxima-repo-running-status">
      <span>Running {r.stage}</span>
      <ProximaLoader size={28} class="proxima-repo-status-loader" />
    </span>
  );
}
